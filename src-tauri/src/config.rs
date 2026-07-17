//! Durable app config and device identity for jotainchatttttttt.
//!
//! History and config live under the OS app data directory and are intentionally
//! NOT deleted when the user removes the .app (uninstall does not wipe data).

use crate::diagnostics::{self, DiagLevel, LogicPoint};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, Runtime};
use uuid::Uuid;

pub const APP_NAME: &str = "jotainchatttttttt";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const BUNDLE_ID: &str = "com.jotain.jotainchatttttttt";
pub const CONFIG_FILE_NAME: &str = "config.json";
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// Default relative folder under the user's Downloads for received files.
pub const DEFAULT_SAVE_SUBDIR: &str = "jotainchatttttttt";

/// Display name constraints.
pub const DISPLAY_NAME_MIN_CHARS: usize = 1;
pub const DISPLAY_NAME_MAX_CHARS: usize = 32;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub bundle_id: String,
    pub platform: String,
    /// Explicit product policy: this build has no auto-updater.
    pub auto_update: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppPaths {
    /// Durable config + SQLite history (survives uninstall of the .app).
    pub app_data_dir: String,
    /// Suggested default directory for received files.
    pub default_save_dir: String,
    /// Absolute path to the durable config JSON file.
    pub config_path: String,
    pub history_note: String,
}

/// Public identity snapshot for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub device_id: String,
    pub display_name: String,
    /// True after the user has finished first-run naming.
    pub onboarding_complete: bool,
    /// Suggested name for first-run (e.g. computer name).
    pub suggested_display_name: String,
}

/// On-disk config. `device_id` is stable for the life of this data directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserConfig {
    pub version: u32,
    pub device_id: String,
    pub display_name: String,
    pub onboarding_complete: bool,
    /// Optional override; empty means use default Downloads subdir.
    #[serde(default)]
    pub save_dir_override: Option<String>,
    /// Play system sounds for incoming messages / file transfer events.
    #[serde(default = "default_true")]
    pub sound_enabled: bool,
    /// When a peer session comes up, auto-resume interrupted inbound transfers.
    #[serde(default = "default_true")]
    pub auto_resume_transfers: bool,
}

fn default_true() -> bool {
    true
}

/// UI-facing preferences (sound, etc.).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preferences {
    pub sound_enabled: bool,
    pub auto_resume_transfers: bool,
}

impl UserConfig {
    pub fn new_with_device_id(device_id: String) -> Self {
        Self {
            version: CONFIG_SCHEMA_VERSION,
            device_id,
            display_name: String::new(),
            onboarding_complete: false,
            save_dir_override: None,
            sound_enabled: true,
            auto_resume_transfers: true,
        }
    }

    pub fn to_identity(&self) -> Identity {
        Identity {
            device_id: self.device_id.clone(),
            display_name: self.display_name.clone(),
            onboarding_complete: self.onboarding_complete,
            suggested_display_name: suggested_display_name(),
        }
    }

    pub fn to_preferences(&self) -> Preferences {
        Preferences {
            sound_enabled: self.sound_enabled,
            auto_resume_transfers: self.auto_resume_transfers,
        }
    }
}

impl AppInfo {
    pub fn current() -> Self {
        Self {
            name: APP_NAME.to_string(),
            version: APP_VERSION.to_string(),
            bundle_id: BUNDLE_ID.to_string(),
            platform: std::env::consts::OS.to_string(),
            auto_update: false,
        }
    }
}

pub fn app_data_dir_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("resolve app data dir: {e}"))?;
    ensure_dir(&dir)?;
    Ok(dir)
}

pub fn config_file_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(CONFIG_FILE_NAME)
}

pub fn resolve_paths<R: Runtime>(app: &AppHandle<R>) -> Result<AppPaths, String> {
    let app_data_dir = app_data_dir_path(app)?;
    let config_path = config_file_path(&app_data_dir);
    let default_save_dir = default_downloads_save_dir()?;

    Ok(AppPaths {
        app_data_dir: app_data_dir.display().to_string(),
        default_save_dir: default_save_dir.display().to_string(),
        config_path: config_path.display().to_string(),
        history_note: "Chat history and identity are stored in the app data directory and are not removed when you delete the application. Clear history from Settings, or delete this folder manually.".to_string(),
    })
}

/// Load config from disk, or create a new one with a fresh `device_id`.
pub fn load_or_create(app_data_dir: &Path) -> Result<UserConfig, String> {
    ensure_dir(app_data_dir)?;
    let path = config_file_path(app_data_dir);

    if path.exists() {
        let raw = fs::read_to_string(&path)
            .map_err(|e| format!("read config {}: {e}", path.display()))?;

        let mut cfg = match serde_json::from_str::<UserConfig>(&raw) {
            Ok(cfg) => cfg,
            Err(parse_err) => {
                // Do not crash the whole app on a corrupt file; quarantine and recreate.
                // device_id is lost in this rare case — preferred over unstartable app.
                quarantine_corrupt_config(&path)?;
                let fresh = UserConfig::new_with_device_id(Uuid::new_v4().to_string());
                save_config(app_data_dir, &fresh)?;
                diagnostics::log_console(
                    LogicPoint::CfgLoadCorrupt,
                    DiagLevel::Error,
                    format!(
                        "corrupt config quarantined ({parse_err}); new device_id={}",
                        fresh.device_id
                    ),
                );
                return Ok(fresh);
            }
        };

        let mut dirty = false;
        let mut repairs = Vec::new();

        // Never regenerate device_id for an existing file; repair empty only.
        if cfg.device_id.trim().is_empty() {
            cfg.device_id = Uuid::new_v4().to_string();
            dirty = true;
            repairs.push("empty device_id regenerated");
        }
        if cfg.version != CONFIG_SCHEMA_VERSION {
            cfg.version = CONFIG_SCHEMA_VERSION;
            dirty = true;
            repairs.push("schema version bumped");
        }
        // Inconsistent: marked complete but no usable name → force first-run again.
        if cfg.onboarding_complete && normalize_display_name(&cfg.display_name).is_err() {
            cfg.onboarding_complete = false;
            cfg.display_name.clear();
            dirty = true;
            repairs.push("onboarding reset (empty name)");
        }
        // Inconsistent: has name but onboarding not marked → treat as complete.
        if !cfg.onboarding_complete && normalize_display_name(&cfg.display_name).is_ok() {
            cfg.onboarding_complete = true;
            dirty = true;
            repairs.push("onboarding marked complete from name");
        }

        if dirty {
            save_config(app_data_dir, &cfg)?;
            diagnostics::log_console(
                LogicPoint::CfgLoadRepair,
                DiagLevel::Warn,
                format!("config repaired: {}", repairs.join("; ")),
            );
        }
        return Ok(cfg);
    }

    let cfg = UserConfig::new_with_device_id(Uuid::new_v4().to_string());
    save_config(app_data_dir, &cfg)?;
    Ok(cfg)
}

pub fn save_config(app_data_dir: &Path, cfg: &UserConfig) -> Result<(), String> {
    ensure_dir(app_data_dir)?;
    let path = config_file_path(app_data_dir);
    let raw = serde_json::to_string_pretty(cfg).map_err(|e| format!("serialize config: {e}"))?;
    // Atomic-ish write: temp then rename. Use a sibling name that cannot collide
    // with `with_extension` edge cases on multi-dot paths.
    let tmp = app_data_dir.join(format!("{CONFIG_FILE_NAME}.tmp"));
    if let Err(e) = fs::write(&tmp, raw.as_bytes()) {
        diagnostics::log_console(
            LogicPoint::CfgSaveFail,
            DiagLevel::Error,
            format!("write {}: {e}", tmp.display()),
        );
        return Err(format!("write {}: {e}", tmp.display()));
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        diagnostics::log_console(
            LogicPoint::CfgSaveFail,
            DiagLevel::Error,
            format!("rename config into place: {e}"),
        );
        return Err(format!("rename config into place: {e}"));
    }
    Ok(())
}

fn quarantine_corrupt_config(path: &Path) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let bak = path.with_extension(format!("json.corrupt.{stamp}"));
    fs::rename(path, &bak).map_err(|e| format!("quarantine corrupt config: {e}"))?;
    Ok(())
}

/// Validate and normalize a display name. Returns trimmed name or error.
pub fn normalize_display_name(raw: &str) -> Result<String, String> {
    let name = raw.trim();
    let chars: Vec<char> = name.chars().collect();
    let len = chars.len();

    if len < DISPLAY_NAME_MIN_CHARS {
        return Err("Display name cannot be empty.".to_string());
    }
    if len > DISPLAY_NAME_MAX_CHARS {
        return Err(format!(
            "Display name must be at most {DISPLAY_NAME_MAX_CHARS} characters."
        ));
    }
    // Reject control characters.
    if name.chars().any(|c| c.is_control()) {
        return Err("Display name cannot contain control characters.".to_string());
    }
    Ok(name.to_string())
}

/// Clamp a string to at most `DISPLAY_NAME_MAX_CHARS` Unicode scalars (for suggestions).
pub fn clamp_display_name_hint(raw: &str) -> String {
    let trimmed = raw.trim();
    trimmed.chars().take(DISPLAY_NAME_MAX_CHARS).collect()
}

/// Set display name and mark onboarding complete. Persists to disk.
/// On disk failure, leaves `cfg` unchanged (rollback).
pub fn set_display_name(app_data_dir: &Path, cfg: &mut UserConfig, raw: &str) -> Result<(), String> {
    let name = normalize_display_name(raw)?;
    let previous = cfg.clone();
    cfg.display_name = name;
    cfg.onboarding_complete = true;
    if let Err(e) = save_config(app_data_dir, cfg) {
        *cfg = previous;
        return Err(e);
    }
    Ok(())
}

pub fn set_sound_enabled(
    app_data_dir: &Path,
    cfg: &mut UserConfig,
    enabled: bool,
) -> Result<(), String> {
    let previous = cfg.clone();
    cfg.sound_enabled = enabled;
    if let Err(e) = save_config(app_data_dir, cfg) {
        *cfg = previous;
        return Err(e);
    }
    Ok(())
}

pub fn set_auto_resume_transfers(
    app_data_dir: &Path,
    cfg: &mut UserConfig,
    enabled: bool,
) -> Result<(), String> {
    let previous = cfg.clone();
    cfg.auto_resume_transfers = enabled;
    if let Err(e) = save_config(app_data_dir, cfg) {
        *cfg = previous;
        return Err(e);
    }
    Ok(())
}

pub fn suggested_display_name() -> String {
    let fallback = match std::env::consts::OS {
        "macos" => "Mac",
        "windows" => "Windows PC",
        _ => "Computer",
    };
    let raw = computer_name()
        .or_else(host_name)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string());
    let clamped = clamp_display_name_hint(&raw);
    if clamped.is_empty() {
        fallback.to_string()
    } else {
        clamped
    }
}

fn computer_name() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("COMPUTERNAME").ok().filter(|s| !s.trim().is_empty())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

fn host_name() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Strip .local suffix common on macOS / mDNS.
    let s = s.strip_suffix(".local").unwrap_or(&s).to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn ensure_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("create {}: {e}", path.display()))
}

fn default_downloads_save_dir() -> Result<PathBuf, String> {
    let home = dirs_home().ok_or_else(|| "could not resolve home directory".to_string())?;
    Ok(home.join("Downloads").join(DEFAULT_SAVE_SUBDIR))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("jotainchat-test-{n}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn app_info_disables_auto_update() {
        let info = AppInfo::current();
        assert!(!info.auto_update);
        assert_eq!(info.name, APP_NAME);
        assert_eq!(info.bundle_id, BUNDLE_ID);
    }

    #[test]
    fn normalize_display_name_trims_and_rejects_empty() {
        assert_eq!(normalize_display_name("  Alice  ").unwrap(), "Alice");
        assert!(normalize_display_name("   ").is_err());
        assert!(normalize_display_name("").is_err());
    }

    #[test]
    fn normalize_display_name_max_length() {
        let ok = "a".repeat(DISPLAY_NAME_MAX_CHARS);
        assert!(normalize_display_name(&ok).is_ok());
        let too_long = "a".repeat(DISPLAY_NAME_MAX_CHARS + 1);
        assert!(normalize_display_name(&too_long).is_err());
    }

    #[test]
    fn load_or_create_persists_stable_device_id() {
        let dir = temp_dir();
        let first = load_or_create(&dir).unwrap();
        assert!(!first.device_id.is_empty());
        assert!(!first.onboarding_complete);
        assert!(first.display_name.is_empty());

        let second = load_or_create(&dir).unwrap();
        assert_eq!(first.device_id, second.device_id);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_display_name_completes_onboarding() {
        let dir = temp_dir();
        let mut cfg = load_or_create(&dir).unwrap();
        set_display_name(&dir, &mut cfg, "  Studio Mac  ").unwrap();
        assert_eq!(cfg.display_name, "Studio Mac");
        assert!(cfg.onboarding_complete);

        let reloaded = load_or_create(&dir).unwrap();
        assert_eq!(reloaded.display_name, "Studio Mac");
        assert!(reloaded.onboarding_complete);
        assert_eq!(reloaded.device_id, cfg.device_id);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_display_name_rolls_back_memory_on_save_failure() {
        // Point save at a path that cannot be a directory write target.
        let mut cfg = UserConfig::new_with_device_id(Uuid::new_v4().to_string());
        let before = cfg.clone();
        // Use a file path as "directory" so create_dir_all / write fails.
        let blocker = temp_dir().join("not-a-dir");
        fs::write(&blocker, b"x").unwrap();
        let err = set_display_name(&blocker, &mut cfg, "Alice");
        assert!(err.is_err());
        assert_eq!(cfg.display_name, before.display_name);
        assert_eq!(cfg.onboarding_complete, before.onboarding_complete);
        let _ = fs::remove_file(&blocker);
    }

    #[test]
    fn load_repairs_onboarding_without_name() {
        let dir = temp_dir();
        let mut cfg = load_or_create(&dir).unwrap();
        cfg.onboarding_complete = true;
        cfg.display_name = String::new();
        save_config(&dir, &cfg).unwrap();

        let loaded = load_or_create(&dir).unwrap();
        assert!(!loaded.onboarding_complete);
        assert!(loaded.display_name.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_repairs_name_without_onboarding_flag() {
        let dir = temp_dir();
        let mut cfg = load_or_create(&dir).unwrap();
        cfg.display_name = "Bob".into();
        cfg.onboarding_complete = false;
        save_config(&dir, &cfg).unwrap();

        let loaded = load_or_create(&dir).unwrap();
        assert!(loaded.onboarding_complete);
        assert_eq!(loaded.display_name, "Bob");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_config_is_quarantined_and_recreated() {
        let dir = temp_dir();
        let path = config_file_path(&dir);
        fs::write(&path, b"{not json").unwrap();
        let cfg = load_or_create(&dir).unwrap();
        assert!(!cfg.device_id.is_empty());
        assert!(!cfg.onboarding_complete);
        // Original moved aside
        let bak_exists = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("corrupt")
            });
        assert!(bak_exists);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn suggested_name_never_exceeds_max() {
        let long = "名".repeat(50);
        let clamped = clamp_display_name_hint(&long);
        assert_eq!(clamped.chars().count(), DISPLAY_NAME_MAX_CHARS);
    }
}
