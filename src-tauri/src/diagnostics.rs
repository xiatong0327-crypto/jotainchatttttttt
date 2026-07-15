//! Structured diagnostics at critical logic nodes.
//!
//! Every entry has a stable **code** (e.g. `SESS-PING-FAIL`) so runtime bugs can
//! be traced to a specific code path and fixed one node at a time.
//!
//! Format on stderr: `[JC][<CODE>][<LEVEL>] <message>`
//! In-app: Settings → Diagnostics (ring buffer).

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, Runtime};

const MAX_ENTRIES: usize = 200;

/// Stable logic-node identifiers. Prefer adding new codes over reusing old ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum LogicPoint {
    // Lifecycle
    AppStart,
    AppStateReady,
    // Config / identity
    CfgLoad,
    CfgLoadCorrupt,
    CfgLoadRepair,
    CfgSave,
    CfgSaveFail,
    CfgSetName,
    CfgSetNameFail,
    // Discovery
    DiscBind,
    DiscBindFail,
    DiscAnnounce,
    DiscAnnounceFail,
    DiscRecvFail,
    DiscPeerSeen,
    DiscPeerExpire,
    DiscStop,
    // TCP control plane
    TcpListen,
    TcpListenFail,
    TcpAccept,
    TcpAcceptFail,
    TcpDialStart,
    TcpDialFail,
    TcpDialSpawnFail,
    TcpHelloFail,
    TcpHelloOk,
    TcpArbDrop,
    TcpArbReplace,
    TcpSessionUp,
    TcpSessionDown,
    TcpPingFail,
    TcpReconcileDrop,
    TcpReadFail,
    TcpFrameBad,
    // Messaging
    MsgSend,
    MsgSendFail,
    MsgRecv,
    MsgRecvDup,
    MsgRecvReject,
    MsgPersistFail,
    // History
    HistDelete,
    HistClearPeer,
    HistClearAll,
    HistDeleteFail,
    // DB
    DbOpen,
    DbOpenFail,
    DbQueryFail,
    // File transfer
    XferListen,
    XferListenFail,
    XferOfferOut,
    XferOfferIn,
    XferOfferReject,
    XferAccept,
    XferAcceptFail,
    XferReject,
    XferCancel,
    XferDataFail,
    XferComplete,
    XferInterrupt,
    XferAlign,
    XferReserve,
    XferFirstDataTimeout,
    XferResume,
    XferResumeFail,
    XferHydrate,
}

impl LogicPoint {
    pub fn code(self) -> &'static str {
        match self {
            Self::AppStart => "APP-START",
            Self::AppStateReady => "APP-STATE-READY",
            Self::CfgLoad => "CFG-LOAD",
            Self::CfgLoadCorrupt => "CFG-LOAD-CORRUPT",
            Self::CfgLoadRepair => "CFG-LOAD-REPAIR",
            Self::CfgSave => "CFG-SAVE",
            Self::CfgSaveFail => "CFG-SAVE-FAIL",
            Self::CfgSetName => "CFG-SET-NAME",
            Self::CfgSetNameFail => "CFG-SET-NAME-FAIL",
            Self::DiscBind => "DISC-BIND",
            Self::DiscBindFail => "DISC-BIND-FAIL",
            Self::DiscAnnounce => "DISC-ANNOUNCE",
            Self::DiscAnnounceFail => "DISC-ANNOUNCE-FAIL",
            Self::DiscRecvFail => "DISC-RECV-FAIL",
            Self::DiscPeerSeen => "DISC-PEER-SEEN",
            Self::DiscPeerExpire => "DISC-PEER-EXPIRE",
            Self::DiscStop => "DISC-STOP",
            Self::TcpListen => "TCP-LISTEN",
            Self::TcpListenFail => "TCP-LISTEN-FAIL",
            Self::TcpAccept => "TCP-ACCEPT",
            Self::TcpAcceptFail => "TCP-ACCEPT-FAIL",
            Self::TcpDialStart => "TCP-DIAL-START",
            Self::TcpDialFail => "TCP-DIAL-FAIL",
            Self::TcpDialSpawnFail => "TCP-DIAL-SPAWN-FAIL",
            Self::TcpHelloFail => "TCP-HELLO-FAIL",
            Self::TcpHelloOk => "TCP-HELLO-OK",
            Self::TcpArbDrop => "TCP-ARB-DROP",
            Self::TcpArbReplace => "TCP-ARB-REPLACE",
            Self::TcpSessionUp => "TCP-SESSION-UP",
            Self::TcpSessionDown => "TCP-SESSION-DOWN",
            Self::TcpPingFail => "TCP-PING-FAIL",
            Self::TcpReconcileDrop => "TCP-RECONCILE-DROP",
            Self::TcpReadFail => "TCP-READ-FAIL",
            Self::TcpFrameBad => "TCP-FRAME-BAD",
            Self::MsgSend => "MSG-SEND",
            Self::MsgSendFail => "MSG-SEND-FAIL",
            Self::MsgRecv => "MSG-RECV",
            Self::MsgRecvDup => "MSG-RECV-DUP",
            Self::MsgRecvReject => "MSG-RECV-REJECT",
            Self::MsgPersistFail => "MSG-PERSIST-FAIL",
            Self::HistDelete => "HIST-DELETE",
            Self::HistClearPeer => "HIST-CLEAR-PEER",
            Self::HistClearAll => "HIST-CLEAR-ALL",
            Self::HistDeleteFail => "HIST-DELETE-FAIL",
            Self::DbOpen => "DB-OPEN",
            Self::DbOpenFail => "DB-OPEN-FAIL",
            Self::DbQueryFail => "DB-QUERY-FAIL",
            Self::XferListen => "XFER-LISTEN",
            Self::XferListenFail => "XFER-LISTEN-FAIL",
            Self::XferOfferOut => "XFER-OFFER-OUT",
            Self::XferOfferIn => "XFER-OFFER-IN",
            Self::XferOfferReject => "XFER-OFFER-REJECT",
            Self::XferAccept => "XFER-ACCEPT",
            Self::XferAcceptFail => "XFER-ACCEPT-FAIL",
            Self::XferReject => "XFER-REJECT",
            Self::XferCancel => "XFER-CANCEL",
            Self::XferDataFail => "XFER-DATA-FAIL",
            Self::XferComplete => "XFER-COMPLETE",
            Self::XferInterrupt => "XFER-INTERRUPT",
            Self::XferAlign => "XFER-ALIGN",
            Self::XferReserve => "XFER-RESERVE",
            Self::XferFirstDataTimeout => "XFER-FIRST-DATA-TIMEOUT",
            Self::XferResume => "XFER-RESUME",
            Self::XferResumeFail => "XFER-RESUME-FAIL",
            Self::XferHydrate => "XFER-HYDRATE",
        }
    }

    pub fn area(self) -> &'static str {
        match self {
            Self::AppStart | Self::AppStateReady => "lifecycle",
            Self::CfgLoad
            | Self::CfgLoadCorrupt
            | Self::CfgLoadRepair
            | Self::CfgSave
            | Self::CfgSaveFail
            | Self::CfgSetName
            | Self::CfgSetNameFail => "config",
            Self::DiscBind
            | Self::DiscBindFail
            | Self::DiscAnnounce
            | Self::DiscAnnounceFail
            | Self::DiscRecvFail
            | Self::DiscPeerSeen
            | Self::DiscPeerExpire
            | Self::DiscStop => "discovery",
            Self::TcpListen
            | Self::TcpListenFail
            | Self::TcpAccept
            | Self::TcpAcceptFail
            | Self::TcpDialStart
            | Self::TcpDialFail
            | Self::TcpDialSpawnFail
            | Self::TcpHelloFail
            | Self::TcpHelloOk
            | Self::TcpArbDrop
            | Self::TcpArbReplace
            | Self::TcpSessionUp
            | Self::TcpSessionDown
            | Self::TcpPingFail
            | Self::TcpReconcileDrop
            | Self::TcpReadFail
            | Self::TcpFrameBad => "session",
            Self::MsgSend
            | Self::MsgSendFail
            | Self::MsgRecv
            | Self::MsgRecvDup
            | Self::MsgRecvReject
            | Self::MsgPersistFail => "messaging",
            Self::HistDelete
            | Self::HistClearPeer
            | Self::HistClearAll
            | Self::HistDeleteFail => "history",
            Self::DbOpen | Self::DbOpenFail | Self::DbQueryFail => "database",
            Self::XferListen
            | Self::XferListenFail
            | Self::XferOfferOut
            | Self::XferOfferIn
            | Self::XferOfferReject
            | Self::XferAccept
            | Self::XferAcceptFail
            | Self::XferReject
            | Self::XferCancel
            | Self::XferDataFail
            | Self::XferComplete
            | Self::XferInterrupt
            | Self::XferAlign
            | Self::XferReserve
            | Self::XferFirstDataTimeout
            | Self::XferResume
            | Self::XferResumeFail
            | Self::XferHydrate => "transfer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagLevel {
    Info,
    Warn,
    Error,
}

impl DiagLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagEntry {
    pub ts_ms: u64,
    pub code: String,
    pub area: String,
    pub level: DiagLevel,
    pub message: String,
}

pub struct DiagnosticsLog {
    entries: Mutex<VecDeque<DiagEntry>>,
}

impl DiagnosticsLog {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(MAX_ENTRIES)),
        }
    }

    pub fn push(&self, entry: DiagEntry) {
        if let Ok(mut q) = self.entries.lock() {
            if q.len() >= MAX_ENTRIES {
                q.pop_front();
            }
            q.push_back(entry);
        }
    }

    pub fn list_newest_first(&self, limit: usize) -> Vec<DiagEntry> {
        let Ok(q) = self.entries.lock() else {
            return Vec::new();
        };
        q.iter().rev().take(limit.max(1)).cloned().collect()
    }

    pub fn clear(&self) {
        if let Ok(mut q) = self.entries.lock() {
            q.clear();
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Log without AppHandle (startup / early failures).
pub fn log_console(point: LogicPoint, level: DiagLevel, message: impl AsRef<str>) {
    let msg = message.as_ref();
    eprintln!(
        "[JC][{}][{}] {}",
        point.code(),
        level.as_str(),
        msg
    );
}

/// Record + stderr + optional UI event (warn/error only, to avoid flood).
pub fn emit_diag<R: Runtime>(
    app: &AppHandle<R>,
    point: LogicPoint,
    level: DiagLevel,
    message: impl Into<String>,
) {
    let message = message.into();
    let entry = DiagEntry {
        ts_ms: now_ms(),
        code: point.code().to_string(),
        area: point.area().to_string(),
        level,
        message: message.clone(),
    };

    log_console(point, level, &message);

    if let Some(state) = app.try_state::<crate::state::AppState>() {
        state.diagnostics.push(entry.clone());
    }

    // UI only for warn/error so Settings can light up without info spam.
    if matches!(level, DiagLevel::Warn | DiagLevel::Error) {
        let _ = app.emit("diagnostic", &entry);
    }
}

pub fn info<R: Runtime>(app: &AppHandle<R>, point: LogicPoint, message: impl Into<String>) {
    emit_diag(app, point, DiagLevel::Info, message);
}

pub fn warn<R: Runtime>(app: &AppHandle<R>, point: LogicPoint, message: impl Into<String>) {
    emit_diag(app, point, DiagLevel::Warn, message);
}

pub fn error<R: Runtime>(app: &AppHandle<R>, point: LogicPoint, message: impl Into<String>) {
    emit_diag(app, point, DiagLevel::Error, message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_stable() {
        assert_eq!(LogicPoint::TcpPingFail.code(), "TCP-PING-FAIL");
        assert_eq!(LogicPoint::DiscBindFail.area(), "discovery");
    }

    #[test]
    fn ring_buffer_caps() {
        let log = DiagnosticsLog::new();
        for i in 0..(MAX_ENTRIES + 50) {
            log.push(DiagEntry {
                ts_ms: i as u64,
                code: "T".into(),
                area: "t".into(),
                level: DiagLevel::Info,
                message: format!("{i}"),
            });
        }
        let all = log.list_newest_first(1000);
        assert_eq!(all.len(), MAX_ENTRIES);
        assert_eq!(all[0].message, format!("{}", MAX_ENTRIES + 49));
    }
}
