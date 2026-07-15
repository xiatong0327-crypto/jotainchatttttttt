//! Notification sounds for chat / file transfer.
//!
//! Dual path:
//! 1. Emit `play-sound` → frontend Web Audio (main path in Tauri UI)
//! 2. `/usr/bin/afplay` system sound (backup; full path, detached)
//!
//! Respects `UserConfig.sound_enabled` (default true).

use crate::state::AppState;
use serde::Serialize;
use std::process::{Command, Stdio};
use tauri::{AppHandle, Emitter, Manager, Runtime};

#[derive(Debug, Clone, Copy)]
pub enum SoundKind {
    Message,
    FileOffer,
    FileDone,
    FileAlert,
}

impl SoundKind {
    fn system_path(self) -> &'static str {
        match self {
            Self::Message => "/System/Library/Sounds/Glass.aiff",
            Self::FileOffer => "/System/Library/Sounds/Submarine.aiff",
            Self::FileDone => "/System/Library/Sounds/Hero.aiff",
            Self::FileAlert => "/System/Library/Sounds/Basso.aiff",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::FileOffer => "file_offer",
            Self::FileDone => "file_done",
            Self::FileAlert => "file_alert",
        }
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PlaySoundEvent {
    kind: String,
}

fn sound_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<AppState>()
        .and_then(|s| s.config.lock().ok().map(|c| c.sound_enabled))
        .unwrap_or(true)
}

pub fn play<R: Runtime>(app: &AppHandle<R>, kind: SoundKind) {
    if !sound_enabled(app) {
        return;
    }
    emit_and_afplay(app, kind);
}

fn emit_and_afplay<R: Runtime>(app: &AppHandle<R>, kind: SoundKind) {
    let _ = app.emit(
        "play-sound",
        PlaySoundEvent {
            kind: kind.as_str().to_string(),
        },
    );
    play_afplay(kind.system_path());
}

fn play_afplay(path: &str) {
    // Prefer absolute path. Detach fully so the child is not tied to our lifetime.
    match Command::new("/usr/bin/afplay")
        .arg("-v")
        .arg("1.0")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            // Don't wait; drop handle so process continues independently.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(_) => {
            // Fallback: osascript beep (always available)
            let _ = Command::new("/usr/bin/osascript")
                .args(["-e", "beep 1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
}

/// Settings preview — always attempts sound (even if notifications muted),
/// so the user can hear samples while toggling.
pub fn play_kind_str<R: Runtime>(app: &AppHandle<R>, kind: &str) -> Result<(), String> {
    let k = match kind {
        "message" => SoundKind::Message,
        "file_offer" => SoundKind::FileOffer,
        "file_done" => SoundKind::FileDone,
        "file_alert" => SoundKind::FileAlert,
        _ => return Err(format!("unknown sound kind: {kind}")),
    };
    emit_and_afplay(app, k);
    Ok(())
}
