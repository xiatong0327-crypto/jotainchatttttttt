mod config;
mod db;
mod diagnostics;
mod discovery;
mod fsutil;
mod net;
mod sound;
mod state;

use config::{AppInfo, AppPaths, Identity, Preferences};
use db::ChatMessage;
use diagnostics::{DiagEntry, LogicPoint};
use discovery::{DiscoveryStatus, PeerInfo};
use net::session::SessionInfo;
use serde::Serialize;
use state::AppState;
use tauri::{AppHandle, Emitter, Manager, State};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryChanged {
    /// "message" | "peer" | "all"
    scope: String,
    peer_id: Option<String>,
    message_id: Option<String>,
    deleted: u64,
}

#[tauri::command]
fn get_app_info() -> AppInfo {
    AppInfo::current()
}

#[tauri::command]
fn get_app_paths(app: tauri::AppHandle) -> Result<AppPaths, String> {
    config::resolve_paths(&app)
}

#[tauri::command]
fn get_identity(state: State<'_, AppState>) -> Result<Identity, String> {
    let cfg = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    Ok(cfg.to_identity())
}

#[tauri::command]
fn get_preferences(state: State<'_, AppState>) -> Result<Preferences, String> {
    let cfg = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    Ok(cfg.to_preferences())
}

#[tauri::command]
fn set_sound_enabled(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<Preferences, String> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    config::set_sound_enabled(&state.app_data_dir, &mut cfg, enabled)?;
    let _ = &app; // keep app available for future prefs events
    Ok(cfg.to_preferences())
}

#[tauri::command]
fn set_auto_resume_transfers(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
) -> Result<Preferences, String> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    config::set_auto_resume_transfers(&state.app_data_dir, &mut cfg, enabled)?;
    let _ = &app;
    Ok(cfg.to_preferences())
}

#[tauri::command]
fn preview_sound(app: AppHandle, kind: String) -> Result<(), String> {
    sound::play_kind_str(&app, &kind)
}

#[tauri::command]
fn set_display_name(app: AppHandle, state: State<'_, AppState>, name: String) -> Result<Identity, String> {
    let mut cfg = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;
    match config::set_display_name(&state.app_data_dir, &mut cfg, &name) {
        Ok(()) => {
            diagnostics::info(
                &app,
                LogicPoint::CfgSetName,
                format!("display_name set to {:?}", cfg.display_name),
            );
            Ok(cfg.to_identity())
        }
        Err(e) => {
            diagnostics::error(
                &app,
                LogicPoint::CfgSetNameFail,
                format!("set_display_name failed: {e}"),
            );
            Err(e)
        }
    }
}

#[tauri::command]
fn list_peers(app: tauri::AppHandle) -> Result<Vec<PeerInfo>, String> {
    discovery::list_peers_snapshot(&app)
}

#[tauri::command]
fn get_discovery_status(app: tauri::AppHandle) -> Result<DiscoveryStatus, String> {
    discovery::status_snapshot(&app)
}

#[tauri::command]
fn list_messages(
    app: AppHandle,
    state: State<'_, AppState>,
    peer_id: String,
    limit: Option<i64>,
) -> Result<Vec<ChatMessage>, String> {
    let limit = limit.unwrap_or(500).clamp(1, 2000);
    match state.db.list_for_peer(&peer_id, limit) {
        Ok(rows) => Ok(rows),
        Err(e) => {
            diagnostics::error(
                &app,
                LogicPoint::DbQueryFail,
                format!("list_messages peer={peer_id}: {e}"),
            );
            Err(e)
        }
    }
}

#[tauri::command]
fn send_text(app: tauri::AppHandle, peer_id: String, body: String) -> Result<ChatMessage, String> {
    net::send_text_to_peer(&app, &peer_id, &body)
}

#[tauri::command]
fn create_group(app: AppHandle, name: String) -> Result<net::group::GroupInfo, String> {
    net::group::create_group(&app, &name)
}

#[tauri::command]
fn join_group(app: AppHandle, join_code: String) -> Result<net::group::GroupInfo, String> {
    net::group::join_group(&app, &join_code)
}

#[tauri::command]
fn leave_group(app: AppHandle, group_id: String) -> Result<(), String> {
    net::group::leave_group(&app, &group_id)
}

#[tauri::command]
fn list_groups(app: AppHandle) -> Result<Vec<net::group::GroupInfo>, String> {
    net::group::list_groups(&app)
}

#[tauri::command]
fn send_group_text(
    app: AppHandle,
    group_id: String,
    body: String,
) -> Result<ChatMessage, String> {
    net::group::send_group_text(&app, &group_id, &body)
}

#[tauri::command]
fn pick_and_send_file(app: AppHandle, peer_id: String) -> Result<ChatMessage, String> {
    net::transfer::pick_and_send_file(&app, &peer_id)
}

/// Drag-drop: offer a file by absolute path on this Mac.
#[tauri::command]
fn send_file_from_path(
    app: AppHandle,
    peer_id: String,
    path: String,
) -> Result<ChatMessage, String> {
    // Drag-drop / path send always requires Accept (including image files).
    net::transfer::send_file_from_path(&app, &peer_id, std::path::PathBuf::from(path), false)
}

/// Stage bytes then offer.
/// `as_screenshot_paste=true` only for ⌘V clipboard screenshots (may auto-accept ≤2 MiB).
/// Drag/drop/file paths must never use this with true — they always need Accept.
#[tauri::command]
fn send_file_bytes(
    app: AppHandle,
    peer_id: String,
    file_name: String,
    mime: String,
    base64_data: String,
    as_screenshot_paste: bool,
) -> Result<ChatMessage, String> {
    use base64::Engine;
    // Accept raw base64 or data-URL prefix.
    let b64 = base64_data
        .split_once(',')
        .map(|(_, d)| d)
        .unwrap_or(&base64_data)
        .trim();
    let data = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64))
        .map_err(|e| format!("Invalid base64 in clipboard payload: {e}"))?;
    net::transfer::send_file_bytes(
        &app,
        &peer_id,
        &file_name,
        &mime,
        &data,
        as_screenshot_paste,
    )
}

/// Load a small local image as a data-URL for chat preview (screenshots ≤2 MiB).
#[tauri::command]
fn read_local_image_preview(path: String) -> Result<String, String> {
    net::transfer::read_local_image_preview(&path)
}

#[tauri::command]
fn accept_file(app: AppHandle, message_id: String, peer_id: String) -> Result<(), String> {
    net::transfer::accept_file(&app, &message_id, &peer_id)
}

#[tauri::command]
fn reject_file(app: AppHandle, message_id: String, peer_id: String) -> Result<(), String> {
    net::transfer::reject_file(&app, &message_id, &peer_id)
}

#[tauri::command]
fn cancel_file(app: AppHandle, file_id: String, peer_id: String) -> Result<(), String> {
    net::transfer::cancel_file(&app, &file_id, &peer_id)
}

#[tauri::command]
fn resume_file(app: AppHandle, message_id: String, peer_id: String) -> Result<(), String> {
    net::transfer::resume_file(&app, &message_id, &peer_id)
}

#[tauri::command]
fn list_sessions(app: tauri::AppHandle) -> Result<Vec<SessionInfo>, String> {
    net::list_session_peers(&app)
}

#[tauri::command]
fn history_stats(state: State<'_, AppState>) -> Result<HistoryStats, String> {
    Ok(HistoryStats {
        total_messages: state.db.count_all()?,
    })
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryStats {
    total_messages: u64,
}

#[tauri::command]
fn delete_message(
    app: AppHandle,
    state: State<'_, AppState>,
    message_id: String,
    peer_id: String,
) -> Result<bool, String> {
    // Transfer cleanup before row delete (needs FileCard / registry).
    net::transfer::on_history_delete_message(&app, &message_id, &peer_id);
    match state.db.delete_message_for_peer(&message_id, &peer_id) {
        Ok(deleted) => {
            if deleted {
                diagnostics::info(
                    &app,
                    LogicPoint::HistDelete,
                    format!("deleted message {message_id} peer={peer_id}"),
                );
                let _ = app.emit(
                    "history-changed",
                    HistoryChanged {
                        scope: "message".into(),
                        peer_id: Some(peer_id),
                        message_id: Some(message_id),
                        deleted: 1,
                    },
                );
            }
            Ok(deleted)
        }
        Err(e) => {
            diagnostics::error(
                &app,
                LogicPoint::HistDeleteFail,
                format!("delete_message {message_id}: {e}"),
            );
            Err(e)
        }
    }
}

#[tauri::command]
fn clear_thread(app: AppHandle, state: State<'_, AppState>, peer_id: String) -> Result<u64, String> {
    net::transfer::on_history_clear_peer(&app, &peer_id);
    match state.db.clear_peer(&peer_id) {
        Ok(n) => {
            diagnostics::warn(
                &app,
                LogicPoint::HistClearPeer,
                format!("cleared thread peer={peer_id} deleted={n}"),
            );
            let _ = app.emit(
                "history-changed",
                HistoryChanged {
                    scope: "peer".into(),
                    peer_id: Some(peer_id),
                    message_id: None,
                    deleted: n,
                },
            );
            Ok(n)
        }
        Err(e) => {
            diagnostics::error(
                &app,
                LogicPoint::HistDeleteFail,
                format!("clear_thread peer={peer_id}: {e}"),
            );
            Err(e)
        }
    }
}

#[tauri::command]
fn clear_all_history(app: AppHandle, state: State<'_, AppState>) -> Result<u64, String> {
    net::transfer::on_history_clear_all(&app);
    match state.db.clear_all() {
        Ok(n) => {
            diagnostics::warn(
                &app,
                LogicPoint::HistClearAll,
                format!("cleared all history deleted={n}"),
            );
            let _ = app.emit(
                "history-changed",
                HistoryChanged {
                    scope: "all".into(),
                    peer_id: None,
                    message_id: None,
                    deleted: n,
                },
            );
            Ok(n)
        }
        Err(e) => {
            diagnostics::error(
                &app,
                LogicPoint::HistDeleteFail,
                format!("clear_all_history: {e}"),
            );
            Err(e)
        }
    }
}

#[tauri::command]
fn list_diagnostics(
    state: State<'_, AppState>,
    limit: Option<usize>,
) -> Result<Vec<DiagEntry>, String> {
    let limit = limit.unwrap_or(100).clamp(1, 200);
    Ok(state.diagnostics.list_newest_first(limit))
}

#[tauri::command]
fn clear_diagnostics(state: State<'_, AppState>) -> Result<(), String> {
    state.diagnostics.clear();
    Ok(())
}

/// Open a local file with the default OS app.
#[tauri::command]
fn open_local_path(path: String) -> Result<(), String> {
    open_path_os(&path, false)
}

/// Reveal a local file in the OS file manager (Finder / Explorer).
#[tauri::command]
fn reveal_in_finder(path: String) -> Result<(), String> {
    open_path_os(&path, true)
}

fn open_path_os(path: &str, reveal: bool) -> Result<(), String> {
    use std::path::Path;
    use std::process::Command;

    let p = Path::new(path);
    if !p.exists() {
        return Err("File not found on this computer (moved or deleted).".into());
    }

    #[cfg(target_os = "macos")]
    {
        let status = if reveal {
            Command::new("open")
                .args(["-R", path])
                .status()
                .map_err(|e| format!("Could not open Finder: {e}"))?
        } else {
            Command::new("open")
                .arg(path)
                .status()
                .map_err(|e| format!("Could not open file: {e}"))?
        };
        if status.success() {
            Ok(())
        } else {
            Err(if reveal {
                "Finder failed to reveal the file.".into()
            } else {
                "Failed to open the file.".into()
            })
        }
    }

    #[cfg(target_os = "windows")]
    {
        let status = if reveal {
            // Select file in Explorer
            Command::new("explorer")
                .arg(format!("/select,{path}"))
                .status()
                .map_err(|e| format!("Could not open Explorer: {e}"))?
        } else {
            Command::new("cmd")
                .args(["/C", "start", "", path])
                .status()
                .map_err(|e| format!("Could not open file: {e}"))?
        };
        // explorer /select often returns non-zero even on success — treat spawn OK as success
        if status.success() || reveal {
            Ok(())
        } else {
            Err("Failed to open the file.".into())
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::path::PathBuf;
        let status = if reveal {
            // Best-effort: open containing folder
            let parent = p
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| PathBuf::from("."));
            Command::new("xdg-open")
                .arg(&parent)
                .status()
                .map_err(|e| format!("Could not open file manager: {e}"))?
        } else {
            Command::new("xdg-open")
                .arg(path)
                .status()
                .map_err(|e| format!("Could not open file: {e}"))?
        };
        if status.success() {
            Ok(())
        } else {
            Err("Failed to open the file.".into())
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    diagnostics::log_console(
        LogicPoint::AppStart,
        diagnostics::DiagLevel::Info,
        "application starting",
    );

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
                let _ = window.unminimize();
            }
        }))
        .invoke_handler(tauri::generate_handler![
            get_app_info,
            get_app_paths,
            get_identity,
            get_preferences,
            set_sound_enabled,
            set_auto_resume_transfers,
            preview_sound,
            set_display_name,
            list_peers,
            get_discovery_status,
            list_messages,
            send_text,
            create_group,
            join_group,
            leave_group,
            list_groups,
            send_group_text,
            pick_and_send_file,
            send_file_from_path,
            send_file_bytes,
            read_local_image_preview,
            accept_file,
            reject_file,
            cancel_file,
            resume_file,
            list_sessions,
            history_stats,
            delete_message,
            clear_thread,
            clear_all_history,
            list_diagnostics,
            clear_diagnostics,
            open_local_path,
            reveal_in_finder
        ])
        .setup(|app| {
            let app_data_dir = config::app_data_dir_path(app.handle())?;
            let user_config = match config::load_or_create(&app_data_dir) {
                Ok(c) => c,
                Err(e) => {
                    diagnostics::log_console(
                        LogicPoint::CfgLoad,
                        diagnostics::DiagLevel::Error,
                        format!("config load failed: {e}"),
                    );
                    return Err(Box::<dyn std::error::Error>::from(e));
                }
            };

            let database = match db::Database::open(&app_data_dir) {
                Ok(db) => db,
                Err(e) => {
                    diagnostics::log_console(
                        LogicPoint::DbOpenFail,
                        diagnostics::DiagLevel::Error,
                        format!("db open failed: {e}"),
                    );
                    return Err(Box::<dyn std::error::Error>::from(e));
                }
            };

            let device_id = user_config.device_id.clone();
            let onboarding = user_config.onboarding_complete;

            app.manage(AppState {
                app_data_dir: app_data_dir.clone(),
                config: std::sync::Mutex::new(user_config),
                peers: std::sync::Mutex::new(discovery::PeerTable::new()),
                discovery: discovery::DiscoveryState::new(),
                db: database,
                sessions: std::sync::Mutex::new(net::session::SessionMap::new()),
                transfers: std::sync::Mutex::new(net::transfer::TransferRegistry::new()),
                groups: net::group::GroupRegistry::new(),
                diagnostics: diagnostics::DiagnosticsLog::new(),
            });

            // After manage: entries enter the in-app ring buffer + stderr.
            diagnostics::info(
                app.handle(),
                LogicPoint::CfgLoad,
                format!("config loaded device_id={device_id} onboarding={onboarding}"),
            );
            diagnostics::info(
                app.handle(),
                LogicPoint::DbOpen,
                format!("messages.db open under {}", app_data_dir.display()),
            );
            diagnostics::info(
                app.handle(),
                LogicPoint::AppStateReady,
                "AppState ready; hydrating transfers then starting network",
            );

            // PR-R3: hydrate transfers BEFORE discovery/session/data so auto-resume
            // has registry + token when sessions come up.
            net::transfer::hydrate_transfers(app.handle());
            net::group::hydrate_groups(app.handle());

            discovery::start_discovery_thread(app.handle().clone());
            net::start_control_plane(app.handle().clone());
            net::start_data_plane(app.handle().clone());
            net::transfer::scan_connected_peers_for_auto_resume(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
