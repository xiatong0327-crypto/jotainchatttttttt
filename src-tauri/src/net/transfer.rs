//! File transfer: control signaling + TCP data plane on DATA_PORT.
//!
//! Receive policy: **confirm by default** — bytes only after FileAccept.

use crate::db::{ChatMessage, TransferRow};
use crate::diagnostics::{self, LogicPoint};
use crate::discovery::DATA_PORT;
use crate::fsutil;
use crate::net::frame::{read_frame, write_frame};
use crate::net::protocol::{FileCard, WireMessage};
use crate::net::session;
use crate::sound::{self, SoundKind};
use crate::state::AppState;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const CHUNK: usize = 256 * 1024;
const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024 * 1024; // 50 GiB soft cap
const PUSH_CONNECT_RETRIES: u32 = 8;
const PUSH_RETRY_DELAY: Duration = Duration::from_millis(250);
/// Throttle SQLite/UI card rewrites during transfer.
const PROGRESS_DB_EVERY: u64 = 1024 * 1024; // 1 MiB
/// After Accept / before first data byte; demote to interrupted so user can retry later.
const FIRST_DATA_WAIT: Duration = Duration::from_secs(20);
/// After fileResume until data body or reject (same as FIRST_DATA_WAIT).
const RESUME_WAIT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataHeader {
    file_id: String,
    token: String,
    size: u64,
    name: String,
    /// Byte offset into the full file; missing on old peers → 0.
    #[serde(default)]
    offset: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DataTrailer {
    sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferProgress {
    pub file_id: String,
    pub message_id: String,
    pub peer_id: String,
    pub bytes_done: u64,
    pub size: u64,
    pub state: String,
}

struct PendingInbound {
    file_id: String,
    message_id: String,
    peer_id: String,
    name: String,
    size: u64,
    #[allow(dead_code)]
    mime: String,
    token: String,
    cancel: Arc<AtomicBool>,
    /// Confirm-by-default: data plane must not run until user Accept.
    accepted: bool,
    /// Reserved final path (placeholder until complete).
    dest_path: Option<PathBuf>,
    /// Reserved partial path for inbound bytes.
    partial_path: Option<PathBuf>,
    bytes_done: u64,
    /// Expected whole-file SHA-256 from FileOffer (if provided).
    expected_sha256: Option<String>,
}

#[derive(Clone)]
struct PendingOutbound {
    file_id: String,
    message_id: String,
    peer_id: String,
    path: PathBuf,
    name: String,
    size: u64,
    #[allow(dead_code)]
    mime: String,
    token: String,
    /// Discovery address at offer time; re-resolved at push when possible.
    peer_address: String,
    cancel: Arc<AtomicBool>,
    /// Source mtime when cached_sha256 was computed.
    source_mtime_ms: Option<u128>,
    /// Precomputed whole-file hash from offer time (reuse for trailer if source unchanged).
    cached_sha256: Option<String>,
}

struct ResumeInflight {
    generation: u64,
    #[allow(dead_code)]
    message_id: String,
    peer_id: String,
    #[allow(dead_code)]
    offset: u64,
}

pub struct TransferRegistry {
    /// Incoming offers waiting for Accept (key: file_id).
    inbound: HashMap<String, PendingInbound>,
    /// Outgoing offers waiting for Accept (key: file_id).
    outbound: HashMap<String, PendingOutbound>,
    /// Active cancel flags (file_id).
    active_cancel: HashMap<String, Arc<AtomicBool>>,
    /// INV-5: data-plane thread running for this file_id.
    active_files: HashSet<String>,
    /// Receiver waiting for data after fileResume.
    resume_inflight: HashMap<String, ResumeInflight>,
    /// Monotonic generation for resume/first-data timers.
    timer_generation: AtomicU64,
    /// Peers with an auto-resume currently scheduled or in-flight (serial 1/peer).
    auto_resume_peers: HashSet<String>,
}

impl TransferRegistry {
    pub fn new() -> Self {
        Self {
            inbound: HashMap::new(),
            outbound: HashMap::new(),
            active_cancel: HashMap::new(),
            active_files: HashSet::new(),
            resume_inflight: HashMap::new(),
            timer_generation: AtomicU64::new(1),
            auto_resume_peers: HashSet::new(),
        }
    }

    fn next_generation(&self) -> u64 {
        self.timer_generation.fetch_add(1, Ordering::SeqCst)
    }
}

// --- Persistence helpers (PR-R3) ---

fn db_upsert_transfer<R: Runtime>(app: &AppHandle<R>, row: &TransferRow) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Err(e) = state.db.upsert_transfer(row) {
            diagnostics::warn(app, LogicPoint::DbQueryFail, format!("upsert transfer: {e}"));
        }
    }
}

fn db_delete_transfer<R: Runtime>(app: &AppHandle<R>, file_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        let _ = state.db.delete_transfer(file_id);
    }
}

fn db_progress<R: Runtime>(app: &AppHandle<R>, file_id: &str, bytes_done: u64, state: &str) {
    if let Some(st) = app.try_state::<AppState>() {
        let _ = st.db.update_transfer_progress(file_id, bytes_done, state);
    }
}

fn db_state_err<R: Runtime>(
    app: &AppHandle<R>,
    file_id: &str,
    state: &str,
    bytes_done: Option<u64>,
    error: Option<&str>,
) {
    if let Some(st) = app.try_state::<AppState>() {
        let _ = st.db.update_transfer_state(file_id, state, bytes_done, error);
    }
}

fn transfer_now_ms() -> i64 {
    now_ms()
}

fn make_transfer_row(
    file_id: &str,
    role: &str,
    peer_id: &str,
    message_id: &str,
    path: &str,
    partial_path: Option<String>,
    name: &str,
    size: u64,
    mime: &str,
    token: &str,
    bytes_done: u64,
    state: &str,
    error: Option<String>,
    created_at: Option<i64>,
) -> TransferRow {
    let ts = transfer_now_ms();
    TransferRow {
        file_id: file_id.into(),
        role: role.into(),
        peer_id: peer_id.into(),
        message_id: message_id.into(),
        path: path.into(),
        partial_path,
        name: name.into(),
        size,
        mime: mime.into(),
        token: token.into(),
        bytes_done,
        state: state.into(),
        source_mtime: None,
        error,
        created_at: created_at.unwrap_or(ts),
        updated_at: ts,
    }
}

/// Load durable transfers into memory before network starts.
pub fn hydrate_transfers<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let rows = match state.db.list_resumable_transfers() {
        Ok(r) => r,
        Err(e) => {
            diagnostics::error(app, LogicPoint::XferHydrate, format!("list failed: {e}"));
            return;
        }
    };
    let mut hydrated = 0u32;
    let mut demoted = 0u32;
    let Ok(mut reg) = state.transfers.lock() else {
        return;
    };
    for row in rows {
        // Terminal-ish cleanups that shouldn't be resumable.
        if matches!(
            row.state.as_str(),
            "completed" | "cancelled" | "rejected" | "failed"
        ) {
            let _ = state.db.delete_transfer(&row.file_id);
            continue;
        }

        let mut state_name = row.state.clone();
        // Crash mid-transfer / mid-accept: demote to interrupted for UI + resume.
        if state_name == "transferring" || state_name == "accepted" {
            state_name = "interrupted".into();
            demoted += 1;
            let _ = state.db.update_transfer_state(
                &row.file_id,
                "interrupted",
                Some(row.bytes_done),
                Some("app_restart"),
            );
        }

        if row.role == "recv" {
            let dest = if row.path.is_empty() {
                None
            } else {
                Some(PathBuf::from(&row.path))
            };
            let partial = row.partial_path.as_ref().map(PathBuf::from);
            // Skip recv without token or without reservation if past offered.
            if state_name != "offered" && (dest.is_none() || partial.is_none()) {
                diagnostics::warn(
                    app,
                    LogicPoint::XferHydrate,
                    format!("drop recv incomplete reservation file={}", row.file_id),
                );
                let _ = state.db.delete_transfer(&row.file_id);
                update_file_message(app, &row.message_id, &row.peer_id, |card| {
                    card.state = "failed".into();
                    card.error = Some("state lost after restart; re-send required".into());
                    card.resume_capable = Some(false);
                });
                continue;
            }
            // Ensure partial exists for resume-capable rows.
            if let (Some(ref d), Some(ref p)) = (&dest, &partial) {
                if !p.exists() {
                    if d.exists() {
                        let _ = File::create(p);
                    } else {
                        // Lost both files.
                        let _ = state.db.delete_transfer(&row.file_id);
                        update_file_message(app, &row.message_id, &row.peer_id, |card| {
                            card.state = "failed".into();
                            card.error = Some("partial missing after restart".into());
                            card.resume_capable = Some(false);
                        });
                        continue;
                    }
                }
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let accepted = state_name != "offered";
            // Recover expected hash from FileCard if present (offer-time).
            let expected_sha = state
                .db
                .get_message(&row.message_id)
                .ok()
                .flatten()
                .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
                .and_then(|c| c.sha256);
            reg.inbound.insert(
                row.file_id.clone(),
                PendingInbound {
                    file_id: row.file_id.clone(),
                    message_id: row.message_id.clone(),
                    peer_id: row.peer_id.clone(),
                    name: row.name.clone(),
                    size: row.size,
                    mime: row.mime.clone(),
                    token: row.token.clone(),
                    cancel: cancel.clone(),
                    accepted,
                    dest_path: dest,
                    partial_path: partial.clone(),
                    bytes_done: row.bytes_done,
                    expected_sha256: expected_sha,
                },
            );
            if accepted {
                reg.active_cancel.insert(row.file_id.clone(), cancel);
            }
            update_file_message(app, &row.message_id, &row.peer_id, |card| {
                card.state = state_name.clone();
                card.bytes_done = row.bytes_done;
                card.local_path = partial.map(|p| p.display().to_string());
                card.error = row.error.clone().or(Some("app_restart".into()));
                card.resume_capable = Some(accepted);
            });
            hydrated += 1;
        } else if row.role == "send" {
            if row.path.is_empty() || !PathBuf::from(&row.path).exists() {
                diagnostics::warn(
                    app,
                    LogicPoint::XferHydrate,
                    format!("drop send missing source file={}", row.file_id),
                );
                let _ = state.db.delete_transfer(&row.file_id);
                update_file_message(app, &row.message_id, &row.peer_id, |card| {
                    card.state = "failed".into();
                    card.error = Some("source missing after restart".into());
                    card.resume_capable = Some(false);
                });
                continue;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            let cached = state
                .db
                .get_message(&row.message_id)
                .ok()
                .flatten()
                .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
                .and_then(|c| c.sha256);
            reg.outbound.insert(
                row.file_id.clone(),
                PendingOutbound {
                    file_id: row.file_id.clone(),
                    message_id: row.message_id.clone(),
                    peer_id: row.peer_id.clone(),
                    path: PathBuf::from(&row.path),
                    name: row.name.clone(),
                    size: row.size,
                    mime: row.mime.clone(),
                    token: row.token.clone(),
                    peer_address: String::new(),
                    cancel,
                    source_mtime_ms: row.source_mtime.map(|m| m as u128),
                    cached_sha256: cached,
                },
            );
            update_file_message(app, &row.message_id, &row.peer_id, |card| {
                card.state = state_name.clone();
                card.bytes_done = row.bytes_done;
                card.local_path = Some(row.path.clone());
                card.error = row.error.clone().or(Some("app_restart".into()));
                card.resume_capable = Some(true);
            });
            hydrated += 1;
        }
    }
    drop(reg);
    diagnostics::info(
        app,
        LogicPoint::XferHydrate,
        format!("hydrated={hydrated} demoted={demoted}"),
    );
}

/// Session became ready: debounce then auto-resume recv transfers for this peer.
pub fn on_peer_session_up<R: Runtime>(app: &AppHandle<R>, peer_id: &str) {
    let app2 = app.clone();
    let peer = peer_id.to_string();
    thread::Builder::new()
        .name("jotain-auto-resume".into())
        .spawn(move || {
            thread::sleep(Duration::from_millis(1500));
            try_auto_resume_for_peer(&app2, &peer);
        })
        .ok();
}

/// After network starts, scan already-connected sessions (none at cold start usually).
pub fn scan_connected_peers_for_auto_resume<R: Runtime>(app: &AppHandle<R>) {
    let app2 = app.clone();
    thread::Builder::new()
        .name("jotain-auto-resume-scan".into())
        .spawn(move || {
            thread::sleep(Duration::from_millis(2000));
            let peers = session::list_session_peers(&app2).unwrap_or_default();
            for s in peers {
                try_auto_resume_for_peer(&app2, &s.peer_id);
            }
        })
        .ok();
}

fn auto_resume_enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.try_state::<AppState>()
        .and_then(|s| s.config.lock().ok().map(|c| c.auto_resume_transfers))
        .unwrap_or(true)
}

fn try_auto_resume_for_peer<R: Runtime>(app: &AppHandle<R>, peer_id: &str) {
    if !auto_resume_enabled(app) {
        return;
    }
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // Session still up?
    let connected = session::list_session_peers(app)
        .map(|list| list.iter().any(|s| s.peer_id == peer_id))
        .unwrap_or(false);
    if !connected {
        return;
    }

    {
        let Ok(mut reg) = state.transfers.lock() else {
            return;
        };
        if reg.auto_resume_peers.contains(peer_id) {
            return;
        }
        // Already have in-flight data/resume for this peer?
        let busy = reg
            .resume_inflight
            .values()
            .any(|r| r.peer_id == peer_id)
            || reg
                .inbound
                .values()
                .any(|p| p.peer_id == peer_id && reg.active_files.contains(&p.file_id));
        if busy {
            return;
        }
        reg.auto_resume_peers.insert(peer_id.to_string());
    }

    // Prefer registry inbound interrupted/accepted.
    let candidate = {
        let Ok(reg) = state.transfers.lock() else {
            clear_auto_peer(app, peer_id);
            return;
        };
        reg.inbound
            .values()
            .filter(|p| {
                p.peer_id == peer_id
                    && p.accepted
                    && p.partial_path.is_some()
                    && !reg.active_files.contains(&p.file_id)
                    && !reg.resume_inflight.contains_key(&p.file_id)
            })
            .min_by_key(|p| p.bytes_done) // finish nearly-done first
            .map(|p| (p.message_id.clone(), p.file_id.clone()))
    };

    let Some((message_id, file_id)) = candidate else {
        clear_auto_peer(app, peer_id);
        return;
    };

    // Confirm card state.
    let ok_state = state
        .db
        .get_message(&message_id)
        .ok()
        .flatten()
        .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
        .map(|c| c.state == "interrupted" || c.state == "accepted")
        .unwrap_or(false);
    if !ok_state {
        clear_auto_peer(app, peer_id);
        return;
    }

    diagnostics::info(
        app,
        LogicPoint::XferResume,
        format!("auto-resume peer={peer_id} file={file_id}"),
    );
    let result = resume_file(app, &message_id, peer_id);
    if let Err(e) = result {
        diagnostics::warn(
            app,
            LogicPoint::XferResumeFail,
            format!("auto-resume fail file={file_id}: {e}"),
        );
        clear_auto_peer(app, peer_id);
        // Limited backoff retry once for unknown_file/busy.
        if e.contains("busy") || e.contains("Not connected") {
            let app2 = app.clone();
            let peer = peer_id.to_string();
            thread::spawn(move || {
                thread::sleep(Duration::from_secs(2));
                try_auto_resume_for_peer(&app2, &peer);
            });
        }
    } else {
        // Keep auto_resume_peers until resume settles (data start / timeout / reject).
        // Cleared when resume_inflight removed + not active.
        let app2 = app.clone();
        let peer = peer_id.to_string();
        let fid = file_id;
        thread::spawn(move || {
            for _ in 0..40 {
                thread::sleep(Duration::from_millis(500));
                let done = if let Some(state) = app2.try_state::<AppState>() {
                    if let Ok(reg) = state.transfers.lock() {
                        !reg.resume_inflight.contains_key(&fid)
                            && !reg.active_files.contains(&fid)
                    } else {
                        true
                    }
                } else {
                    true
                };
                if done {
                    break;
                }
            }
            clear_auto_peer(&app2, &peer);
            // Chain next file for this peer.
            try_auto_resume_for_peer(&app2, &peer);
        });
    }
}

fn clear_auto_peer<R: Runtime>(app: &AppHandle<R>, peer_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            reg.auto_resume_peers.remove(peer_id);
        }
    }
}

fn active_files_insert<R: Runtime>(app: &AppHandle<R>, file_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            reg.active_files.insert(file_id.to_string());
        }
    }
}

fn active_files_remove<R: Runtime>(app: &AppHandle<R>, file_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            reg.active_files.remove(file_id);
        }
    }
}

fn clear_resume_inflight<R: Runtime>(app: &AppHandle<R>, file_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            reg.resume_inflight.remove(file_id);
        }
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("open for hash: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).map_err(|e| format!("read for hash: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn mtime_ms(meta: &fs::Metadata) -> Option<u128> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
}

fn align_partial_logged<R: Runtime>(app: &AppHandle<R>, partial: &Path) -> Result<u64, String> {
    let before = fs::metadata(partial).map(|m| m.len()).unwrap_or(0);
    let aligned = fsutil::prepare_partial_for_resume(partial)?;
    if aligned < before {
        diagnostics::info(
            app,
            LogicPoint::XferAlign,
            format!(
                "dirty tail truncate partial={} from={before} to={aligned}",
                partial.display()
            ),
        );
    }
    Ok(aligned)
}

pub fn start_data_plane<R: Runtime>(app: AppHandle<R>) {
    thread::Builder::new()
        .name("jotain-data-accept".into())
        .spawn(move || data_accept_loop(app))
        .expect("spawn data accept");
}

fn data_accept_loop<R: Runtime>(app: AppHandle<R>) {
    let listener = match TcpListener::bind(("0.0.0.0", DATA_PORT)) {
        Ok(l) => {
            diagnostics::info(
                &app,
                LogicPoint::XferListen,
                format!("TCP data plane listening 0.0.0.0:{DATA_PORT}"),
            );
            l
        }
        Err(e) => {
            diagnostics::error(
                &app,
                LogicPoint::XferListenFail,
                format!("data port {DATA_PORT} bind failed: {e}"),
            );
            return;
        }
    };
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                let app2 = app.clone();
                thread::Builder::new()
                    .name("jotain-data-in".into())
                    .spawn(move || {
                        if let Err(e) = handle_inbound_data(app2.clone(), stream) {
                            diagnostics::warn(
                                &app2,
                                LogicPoint::XferDataFail,
                                format!("inbound data from {addr}: {e}"),
                            );
                        }
                    })
                    .ok();
            }
            Err(e) => {
                diagnostics::error(
                    &app,
                    LogicPoint::XferListenFail,
                    format!("data accept error: {e}"),
                );
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

/// Format host:port correctly for IPv4 and IPv6.
fn socket_addr_str(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn connect_data_with_retry(
    peer_address: &str,
    cancel: &AtomicBool,
) -> Result<TcpStream, String> {
    let addr = socket_addr_str(peer_address, DATA_PORT);
    let sock: std::net::SocketAddr = addr
        .parse()
        .map_err(|e| format!("bad data addr {addr}: {e}"))?;
    let mut last_err = String::new();
    for attempt in 1..=PUSH_CONNECT_RETRIES {
        if cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        match TcpStream::connect_timeout(&sock, CONNECT_TIMEOUT) {
            Ok(s) => {
                s.set_nodelay(true).ok();
                // Keep writes from hanging forever on dead peer.
                s.set_write_timeout(Some(Duration::from_secs(120))).ok();
                return Ok(s);
            }
            Err(e) => {
                last_err = format!("data connect {addr} attempt {attempt}: {e}");
                thread::sleep(PUSH_RETRY_DELAY);
            }
        }
    }
    Err(last_err)
}

/// Sender: after FileAccept or FileResume, push file bytes `[offset..size)` to peer DATA_PORT.
fn push_file_to_peer<R: Runtime>(
    app: AppHandle<R>,
    peer_address: String,
    path: PathBuf,
    file_id: String,
    message_id: String,
    peer_id: String,
    name: String,
    size: u64,
    token: String,
    cancel: Arc<AtomicBool>,
    offset: u64,
    cached_sha256: Option<String>,
    cached_mtime_ms: Option<u128>,
) {
    // Brief pause so receiver finishes Accept/Resume bookkeeping before data hits.
    thread::sleep(Duration::from_millis(150));

    let path_keep = path.clone();
    let token_keep = token.clone();
    let peer_address_keep = peer_address.clone();
    let name_keep = name.clone();
    let cached_keep = cached_sha256.clone();
    let cached_mtime_keep = cached_mtime_ms;

    active_files_insert(&app, &file_id);

    let result = (|| -> Result<(), String> {
        if offset >= size {
            return Err("offset_invalid".into());
        }

        // Re-resolve right before connect (IP may change between Accept and push).
        let host = resolve_peer_address(&app, &peer_id).unwrap_or(peer_address.clone());
        let mut stream = connect_data_with_retry(&host, &cancel)?;

        // Pre-stat: size must still match offer.
        let meta_before = fs::metadata(&path).map_err(|e| format!("stat source: {e}"))?;
        if meta_before.len() != size {
            return Err("source_changed".into());
        }
        let mtime_before = mtime_ms(&meta_before);

        let header = DataHeader {
            file_id: file_id.clone(),
            token: token.clone(),
            size,
            name: name.clone(),
            offset,
        };
        let hb = serde_json::to_vec(&header).map_err(|e| e.to_string())?;
        write_frame(&mut stream, &hb)?;

        let mut file = File::open(&path).map_err(|e| format!("open source: {e}"))?;
        if offset > 0 {
            file.seek(SeekFrom::Start(offset))
                .map_err(|e| format!("seek source: {e}"))?;
        }

        let mut buf = vec![0u8; CHUNK];
        let mut done: u64 = offset;
        let mut last_ui = offset;
        let mut last_db = offset;
        let to_send = size - offset;
        let mut sent: u64 = 0;

        while sent < to_send {
            if cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            let want = std::cmp::min((to_send - sent) as usize, CHUNK);
            let n = file
                .read(&mut buf[..want])
                .map_err(|e| format!("read source: {e}"))?;
            if n == 0 {
                return Err(format!(
                    "unexpected EOF reading source at {done}/{size}"
                ));
            }
            stream
                .write_all(&buf[..n])
                .map_err(|e| format!("write data: {e}"))?;
            sent += n as u64;
            done += n as u64;
            if done - last_ui >= CHUNK as u64 || done == size {
                last_ui = done;
                emit_progress(
                    &app,
                    &file_id,
                    &message_id,
                    &peer_id,
                    done,
                    size,
                    "transferring",
                );
            }
            if done - last_db >= PROGRESS_DB_EVERY || done == size {
                last_db = done;
                update_file_message(&app, &message_id, &peer_id, |card| {
                    card.bytes_done = done;
                    card.state = "transferring".into();
                });
                db_progress(&app, &file_id, done, "transferring");
            }
        }

        stream.flush().map_err(|e| format!("flush data: {e}"))?;

        // Re-stat before whole-file hash / trailer (INV: source mutation window).
        let meta_after = fs::metadata(&path).map_err(|e| format!("re-stat source: {e}"))?;
        if meta_after.len() != size {
            return Err("source_changed".into());
        }
        if mtime_before.is_some() && mtime_ms(&meta_after) != mtime_before {
            return Err("source_changed".into());
        }

        // Whole-file SHA-256. Reuse offer-time cache only if mtime+size still match.
        let digest = match (&cached_sha256, cached_mtime_ms, mtime_before) {
            (Some(c), Some(cm), Some(mb))
                if cm == mb && mtime_ms(&meta_after) == Some(cm) =>
            {
                c.clone()
            }
            _ => sha256_file(&path)?,
        };
        let trailer = DataTrailer {
            sha256: digest.clone(),
        };
        let tb = serde_json::to_vec(&trailer).map_err(|e| e.to_string())?;
        write_frame(&mut stream, &tb)?;
        let _ = stream.shutdown(Shutdown::Write);

        update_file_message(&app, &message_id, &peer_id, |card| {
            card.bytes_done = size;
            card.state = "completed".into();
            card.sha256 = Some(digest);
            card.local_path = Some(path.display().to_string());
            card.error = None;
            card.resume_capable = None;
        });
        db_delete_transfer(&app, &file_id);
        emit_progress(
            &app,
            &file_id,
            &message_id,
            &peer_id,
            size,
            size,
            "completed",
        );
        diagnostics::info(
            &app,
            LogicPoint::XferComplete,
            format!(
                "send complete file={file_id} name={name} size={size} offset={offset} host={host}"
            ),
        );
        sound::play(&app, SoundKind::FileDone);
        Ok(())
    })();

    let last_done = app
        .try_state::<AppState>()
        .and_then(|state| state.db.get_message(&message_id).ok().flatten())
        .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
        .map(|c| c.bytes_done.max(offset))
        .unwrap_or(offset);

    active_files_remove(&app, &file_id);

    if let Err(e) = result {
        let cancelled = e == "cancelled";
        let state_name = if cancelled {
            "cancelled"
        } else {
            "interrupted"
        };
        diagnostics::warn(
            &app,
            if cancelled {
                LogicPoint::XferCancel
            } else {
                LogicPoint::XferInterrupt
            },
            format!("send file={file_id} offset={offset}: {e}"),
        );
        update_file_message(&app, &message_id, &peer_id, |card| {
            card.state = state_name.into();
            card.error = Some(e.clone());
            if card.bytes_done < last_done {
                card.bytes_done = last_done;
            }
            card.resume_capable = Some(!cancelled);
        });
        if cancelled {
            db_delete_transfer(&app, &file_id);
        } else {
            db_state_err(
                &app,
                &file_id,
                "interrupted",
                Some(last_done),
                Some(&e),
            );
        }
        emit_progress(
            &app,
            &file_id,
            &message_id,
            &peer_id,
            last_done,
            size,
            state_name,
        );
        if let Some(state) = app.try_state::<AppState>() {
            if let Ok(mut reg) = state.transfers.lock() {
                reg.active_cancel.remove(&file_id);
                if cancelled {
                    reg.outbound.remove(&file_id);
                } else {
                    reg.outbound.insert(
                        file_id.clone(),
                        PendingOutbound {
                            file_id: file_id.clone(),
                            message_id: message_id.clone(),
                            peer_id: peer_id.clone(),
                            path: path_keep.clone(),
                            name: name_keep.clone(),
                            size,
                            mime: String::new(),
                            token: token_keep.clone(),
                            peer_address: peer_address_keep.clone(),
                            cancel: Arc::new(AtomicBool::new(false)),
                            source_mtime_ms: cached_mtime_keep,
                            cached_sha256: cached_keep.clone(),
                        },
                    );
                }
            }
        }
        if !cancelled {
            sound::play(&app, SoundKind::FileAlert);
        }
    } else if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            reg.active_cancel.remove(&file_id);
            reg.outbound.remove(&file_id);
        }
    }
}

fn handle_inbound_data<R: Runtime>(app: AppHandle<R>, mut stream: TcpStream) -> Result<(), String> {
    // Idle timeout only for header; bulk read uses long timeout below.
    stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
    stream.set_nodelay(true).ok();

    let header_bytes = read_frame(&mut stream).map_err(|e| e.to_string())?;
    let header: DataHeader =
        serde_json::from_slice(&header_bytes).map_err(|e| format!("data header: {e}"))?;

    let pending = {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "no state".to_string())?;
        let mut reg = state
            .transfers
            .lock()
            .map_err(|_| "transfers lock".to_string())?;
        if reg.active_files.contains(&header.file_id) {
            let _ = stream.shutdown(Shutdown::Both);
            return Err("busy".into());
        }
        let accepted = reg
            .inbound
            .get(&header.file_id)
            .map(|p| p.accepted)
            .unwrap_or(false);
        if !accepted {
            diagnostics::warn(
                &app,
                LogicPoint::XferOfferReject,
                format!(
                    "data before Accept rejected file={} (confirm-by-default)",
                    header.file_id
                ),
            );
            let _ = stream.shutdown(Shutdown::Both);
            return Err("file not accepted yet".into());
        }
        let p = reg
            .inbound
            .remove(&header.file_id)
            .ok_or_else(|| "no pending accept for file".to_string())?;
        if p.token != header.token {
            reg.inbound.insert(header.file_id.clone(), p);
            return Err("token mismatch".into());
        }
        if p.size != header.size {
            reg.inbound.insert(header.file_id.clone(), p);
            return Err("size mismatch".into());
        }
        reg.resume_inflight.remove(&header.file_id);
        reg.active_cancel
            .insert(header.file_id.clone(), p.cancel.clone());
        reg.active_files.insert(header.file_id.clone());
        p
    };

    // Long timeout for multi-GB transfers (reset activity via continuous reads).
    stream.set_read_timeout(Some(Duration::from_secs(600))).ok();

    let dest = match pending.dest_path.clone() {
        Some(d) => d,
        None => {
            active_files_remove(&app, &pending.file_id);
            return Err("no reserved dest (accept path missing)".into());
        }
    };
    let partial = match pending.partial_path.clone() {
        Some(p) => p,
        None => {
            active_files_remove(&app, &pending.file_id);
            return Err("no reserved partial (accept path missing)".into());
        }
    };

    // INV-1: disk-aligned offset is ground truth.
    let expected = match align_partial_logged(&app, &partial) {
        Ok(o) => o,
        Err(e) => {
            reinsert_inbound_interrupted(&app, &pending, &dest, &partial, 0);
            active_files_remove(&app, &pending.file_id);
            return Err(e);
        }
    };
    if header.offset != expected {
        diagnostics::warn(
            &app,
            LogicPoint::XferResumeFail,
            format!(
                "offset_mismatch file={} header={} expected={}",
                header.file_id, header.offset, expected
            ),
        );
        let _ = stream.shutdown(Shutdown::Both);
        reinsert_inbound_interrupted(&app, &pending, &dest, &partial, expected);
        update_file_message(&app, &pending.message_id, &pending.peer_id, |card| {
            card.state = "interrupted".into();
            card.bytes_done = expected;
            card.error = Some("offset_mismatch".into());
            card.resume_capable = Some(true);
            card.local_path = Some(partial.display().to_string());
        });
        active_files_remove(&app, &pending.file_id);
        return Err("offset_mismatch".into());
    }

    update_file_message(&app, &pending.message_id, &pending.peer_id, |card| {
        card.state = "transferring".into();
        card.local_path = Some(partial.display().to_string());
        card.bytes_done = header.offset;
        card.error = None;
    });

    let mut last_done: u64 = header.offset;
    let result = (|| -> Result<String, String> {
        if pending.cancel.load(Ordering::SeqCst) {
            return Err("cancelled".into());
        }
        let mut out = fsutil::open_partial_write(&partial)?;
        if header.offset == 0 {
            out.set_len(0).map_err(|e| format!("truncate partial: {e}"))?;
        } else {
            out.seek(SeekFrom::Start(header.offset))
                .map_err(|e| format!("seek partial: {e}"))?;
        }

        let mut remaining = header.size.saturating_sub(header.offset);
        let mut buf = vec![0u8; CHUNK];
        let mut done: u64 = header.offset;
        let mut last_ui = header.offset;
        let mut last_db = header.offset;

        while remaining > 0 {
            if pending.cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            let want = std::cmp::min(remaining as usize, CHUNK);
            let n = stream
                .read(&mut buf[..want])
                .map_err(|e| format!("read data: {e}"))?;
            if n == 0 {
                return Err(format!(
                    "unexpected EOF after {done}/{} bytes",
                    header.size
                ));
            }
            out.write_all(&buf[..n])
                .map_err(|e| format!("write partial: {e}"))?;
            remaining -= n as u64;
            done += n as u64;
            last_done = done;
            if done - last_ui >= CHUNK as u64 || remaining == 0 {
                last_ui = done;
                emit_progress(
                    &app,
                    &pending.file_id,
                    &pending.message_id,
                    &pending.peer_id,
                    done,
                    header.size,
                    "transferring",
                );
            }
            if done - last_db >= PROGRESS_DB_EVERY || remaining == 0 {
                last_db = done;
                update_file_message(&app, &pending.message_id, &pending.peer_id, |card| {
                    card.bytes_done = done;
                    card.state = "transferring".into();
                    card.local_path = Some(partial.display().to_string());
                });
                db_progress(&app, &pending.file_id, done, "transferring");
            }
        }
        out.flush().ok();
        drop(out);

        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .ok();
        let trailer_bytes = read_frame(&mut stream).map_err(|e| {
            format!("read trailer (after {done} bytes): {e}")
        })?;
        let trailer: DataTrailer =
            serde_json::from_slice(&trailer_bytes).map_err(|e| format!("trailer: {e}"))?;

        // Whole-file rehash of partial (not session-range hash).
        let digest = sha256_file(&partial)?;
        if digest != trailer.sha256 {
            fsutil::cleanup_reservation(Some(&dest), Some(&partial));
            return Err(format!(
                "sha256 mismatch local={digest} remote={}",
                trailer.sha256
            ));
        }
        // Cross-check optional offer-time hash (PR-R4).
        if let Some(ref expected) = pending.expected_sha256 {
            if &digest != expected {
                fsutil::cleanup_reservation(Some(&dest), Some(&partial));
                return Err(format!(
                    "sha256 mismatch local={digest} offer={expected}"
                ));
            }
        }
        fsutil::finalize_partial_to_dest(&partial, &dest)?;
        Ok(digest)
    })();

    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            reg.active_cancel.remove(&pending.file_id);
            reg.active_files.remove(&pending.file_id);
            reg.resume_inflight.remove(&pending.file_id);
        }
    }

    match result {
        Ok(digest) => {
            let path_str = dest.display().to_string();
            update_file_message(&app, &pending.message_id, &pending.peer_id, |card| {
                card.bytes_done = header.size;
                card.state = "completed".into();
                card.sha256 = Some(digest);
                card.local_path = Some(path_str);
                card.error = None;
                card.resume_capable = None;
            });
            db_delete_transfer(&app, &pending.file_id);
            emit_progress(
                &app,
                &pending.file_id,
                &pending.message_id,
                &pending.peer_id,
                header.size,
                header.size,
                "completed",
            );
            diagnostics::info(
                &app,
                LogicPoint::XferComplete,
                format!(
                    "recv complete file={} name={} path={} offset={}",
                    pending.file_id,
                    pending.name,
                    dest.display(),
                    header.offset
                ),
            );
            sound::play(&app, SoundKind::FileDone);
            Ok(())
        }
        Err(e) => {
            let cancelled = e == "cancelled";
            let hash_fail = e.starts_with("sha256 mismatch");
            let st = if cancelled {
                "cancelled"
            } else if hash_fail {
                "failed"
            } else {
                "interrupted"
            };
            if cancelled || hash_fail {
                fsutil::cleanup_reservation(Some(&dest), Some(&partial));
                db_delete_transfer(&app, &pending.file_id);
            } else {
                db_state_err(
                    &app,
                    &pending.file_id,
                    "interrupted",
                    Some(last_done),
                    Some(&e),
                );
            }
            diagnostics::warn(
                &app,
                if st == "interrupted" {
                    LogicPoint::XferInterrupt
                } else {
                    LogicPoint::XferDataFail
                },
                format!("recv file={}: {e}", pending.file_id),
            );
            update_file_message(&app, &pending.message_id, &pending.peer_id, |card| {
                card.state = st.into();
                card.error = Some(e.clone());
                if !cancelled && !hash_fail {
                    card.bytes_done = last_done;
                    card.local_path = Some(partial.display().to_string());
                    card.resume_capable = Some(true);
                } else {
                    card.resume_capable = Some(false);
                }
            });
            emit_progress(
                &app,
                &pending.file_id,
                &pending.message_id,
                &pending.peer_id,
                if cancelled || hash_fail { 0 } else { last_done },
                header.size,
                st,
            );
            if st == "interrupted" {
                reinsert_inbound_interrupted(&app, &pending, &dest, &partial, last_done);
                sound::play(&app, SoundKind::FileAlert);
            } else if st == "failed" {
                sound::play(&app, SoundKind::FileAlert);
            }
            Err(e)
        }
    }
}

fn reinsert_inbound_interrupted<R: Runtime>(
    app: &AppHandle<R>,
    pending: &PendingInbound,
    dest: &Path,
    partial: &Path,
    bytes_done: u64,
) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            let cancel = Arc::new(AtomicBool::new(false));
            reg.inbound.insert(
                pending.file_id.clone(),
                PendingInbound {
                    file_id: pending.file_id.clone(),
                    message_id: pending.message_id.clone(),
                    peer_id: pending.peer_id.clone(),
                    name: pending.name.clone(),
                    size: pending.size,
                    mime: pending.mime.clone(),
                    token: pending.token.clone(),
                    cancel: cancel.clone(),
                    accepted: true,
                    dest_path: Some(dest.to_path_buf()),
                    partial_path: Some(partial.to_path_buf()),
                    bytes_done,
                    expected_sha256: pending.expected_sha256.clone(),
                },
            );
            reg.active_cancel
                .insert(pending.file_id.clone(), cancel);
        }
    }
}

// --- Control-plane hooks ---

pub fn on_file_offer_wire<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    file_id: String,
    message_id: String,
    name: String,
    size: u64,
    mime: String,
    token: String,
    ts: i64,
    sha256: Option<String>,
) {
    let name = match fsutil::safe_basename(&name) {
        Ok(n) => n,
        Err(e) => {
            diagnostics::warn(app, LogicPoint::XferOfferReject, format!("bad name: {e}"));
            return;
        }
    };
    if size > MAX_FILE_SIZE {
        diagnostics::warn(
            app,
            LogicPoint::XferOfferReject,
            format!("file too large size={size}"),
        );
        return;
    }

    let card = FileCard {
        file_id: file_id.clone(),
        name: name.clone(),
        size,
        mime: mime.clone(),
        bytes_done: 0,
        state: "offered".into(),
        local_path: None,
        // Store offer-time expected hash (verified on complete).
        sha256: sha256.clone(),
        error: None,
        resume_capable: None,
    };
    let body = serde_json::to_string(&card).unwrap_or_default();
    let created = if ts > 0 { ts } else { now_ms() };
    let msg = ChatMessage {
        id: message_id.clone(),
        peer_id: peer_id.to_string(),
        direction: "in".into(),
        msg_type: "file".into(),
        body,
        created_at: created,
        status: "received".into(),
    };

    if let Some(state) = app.try_state::<AppState>() {
        match state.db.insert_message(&msg) {
            Ok(true) => {
                let cancel = Arc::new(AtomicBool::new(false));
                if let Ok(mut reg) = state.transfers.lock() {
                    reg.inbound.insert(
                        file_id.clone(),
                        PendingInbound {
                            file_id: file_id.clone(),
                            message_id: message_id.clone(),
                            peer_id: peer_id.to_string(),
                            name: name.clone(),
                            size,
                            mime: mime.clone(),
                            token: token.clone(),
                            cancel,
                            accepted: false,
                            dest_path: None,
                            partial_path: None,
                            bytes_done: 0,
                            expected_sha256: sha256.clone(),
                        },
                    );
                }
                db_upsert_transfer(
                    app,
                    &make_transfer_row(
                        &file_id,
                        "recv",
                        peer_id,
                        &message_id,
                        "",
                        None,
                        &name,
                        size,
                        &mime,
                        &token,
                        0,
                        "offered",
                        None,
                        Some(created),
                    ),
                );
                diagnostics::info(
                    app,
                    LogicPoint::XferOfferIn,
                    format!("offer in file={file_id} peer={peer_id} size={size}"),
                );
                sound::play(app, SoundKind::FileOffer);
                let _ = app.emit("message", &msg);
            }
            Ok(false) => {}
            Err(e) => diagnostics::error(app, LogicPoint::MsgPersistFail, e),
        }
    }
}

pub fn on_file_accept_wire<R: Runtime>(
    app: &AppHandle<R>,
    _peer_id: &str,
    file_id: String,
    message_id: String,
) {
    spawn_outbound_push(app, &file_id, &message_id, 0, "accept");
}

/// Shared sender path for first Accept and FileResume.
fn spawn_outbound_push<R: Runtime>(
    app: &AppHandle<R>,
    file_id: &str,
    message_id: &str,
    offset: u64,
    reason: &str,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let out = {
        let Ok(mut reg) = state.transfers.lock() else {
            return;
        };
        if reg.active_files.contains(file_id) {
            diagnostics::warn(
                app,
                LogicPoint::XferResumeFail,
                format!("push busy file={file_id} reason={reason}"),
            );
            return;
        }
        // Keep a clone in map until push finishes so resume can reject busy;
        // remove on success; re-insert on interrupt is handled inside push.
        let Some(out) = reg.outbound.get(file_id).cloned() else {
            diagnostics::warn(
                app,
                LogicPoint::XferAcceptFail,
                format!("push for unknown outbound file={file_id} ({reason})"),
            );
            return;
        };
        reg.active_cancel
            .insert(file_id.to_string(), out.cancel.clone());
        out
    };

    let peer_address = resolve_peer_address(app, &out.peer_id).unwrap_or(out.peer_address.clone());

    diagnostics::info(
        app,
        if offset > 0 {
            LogicPoint::XferResume
        } else {
            LogicPoint::XferAccept
        },
        format!(
            "peer {reason} file={file_id} offset={offset} → push to {peer_address}"
        ),
    );
    update_file_message(app, message_id, &out.peer_id, |card| {
        card.state = "transferring".into();
        card.bytes_done = offset;
        card.error = None;
    });

    let app2 = app.clone();
    let file_id_owned = file_id.to_string();
    thread::Builder::new()
        .name("jotain-data-push".into())
        .spawn(move || {
            push_file_to_peer(
                app2,
                peer_address,
                out.path,
                file_id_owned,
                out.message_id,
                out.peer_id,
                out.name,
                out.size,
                out.token,
                out.cancel,
                offset,
                out.cached_sha256,
                out.source_mtime_ms,
            );
        })
        .ok();
}

/// Best-effort address for data plane: session map first, then discovery online peer.
fn resolve_peer_address<R: Runtime>(app: &AppHandle<R>, peer_id: &str) -> Option<String> {
    let state = app.try_state::<AppState>()?;
    if let Ok(sessions) = state.sessions.lock() {
        if let Some(s) = sessions.list().into_iter().find(|s| s.peer_id == peer_id) {
            if !s.address.is_empty() {
                return Some(s.address);
            }
        }
    }
    if let Ok(peers) = state.peers.lock() {
        return peers
            .list()
            .into_iter()
            .find(|p| p.device_id == peer_id && p.online)
            .map(|p| p.address);
    }
    None
}

pub fn on_file_reject_wire<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    file_id: String,
    message_id: String,
) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut reg) = state.transfers.lock() {
            reg.outbound.remove(&file_id);
        }
    }
    db_delete_transfer(app, &file_id);
    diagnostics::info(
        app,
        LogicPoint::XferReject,
        format!("peer rejected file={file_id}"),
    );
    update_file_message(app, &message_id, peer_id, |card| {
        card.state = "rejected".into();
    });
}

pub fn on_file_cancel_wire<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    file_id: String,
    message_id: String,
) {
    // Same disk + registry cleanup as local cancel; do not re-send Cancel wire.
    let _ = finalize_cancel(app, &file_id, peer_id, false);
    // finalize_cancel updates message_id from registry; ensure card if registry was empty.
    update_file_message(app, &message_id, peer_id, |card| {
        card.state = "cancelled".into();
        card.resume_capable = Some(false);
    });
    diagnostics::info(
        app,
        LogicPoint::XferCancel,
        format!("peer cancelled file={file_id}"),
    );
}

// --- Commands ---

pub fn pick_and_send_file<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
) -> Result<ChatMessage, String> {
    // NSOpenPanel must run on the main thread on macOS.
    let path = pick_file_on_main_thread(app)?;

    let meta = fs::metadata(&path).map_err(|e| format!("stat file: {e}"))?;
    if !meta.is_file() {
        return Err("Not a regular file.".into());
    }
    let size = meta.len();
    if size == 0 {
        return Err("Cannot send empty file.".into());
    }
    if size > MAX_FILE_SIZE {
        return Err(format!("File too large (max {MAX_FILE_SIZE} bytes)."));
    }

    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| "bad file name".to_string())?;
    let name = fsutil::safe_basename(name)?;
    let mime = mime_guess(&name);

    // Prefer live session IP (same path as chat); fall back to discovery.
    let peer_address = resolve_peer_address(app, peer_id)
        .ok_or_else(|| "Peer address unknown. Wait until linked (green).".to_string())?;

    // Need control session for signaling.
    if session::list_session_peers(app)?
        .iter()
        .all(|s| s.peer_id != peer_id)
    {
        return Err("Not connected to peer. Wait until status shows connected.".into());
    }

    let file_id = Uuid::new_v4().to_string();
    let message_id = Uuid::new_v4().to_string();
    let token = random_token();
    let ts = now_ms();

    // Pre-hash for Offer + trailer cache (whole-file integrity; may take a moment for large files).
    diagnostics::info(
        app,
        LogicPoint::XferOfferOut,
        format!("computing sha256 before offer size={size} path={}", path.display()),
    );
    let meta_for_hash = fs::metadata(&path).map_err(|e| format!("stat file: {e}"))?;
    let offer_mtime = mtime_ms(&meta_for_hash);
    let file_sha = sha256_file(&path).map_err(|e| format!("Could not checksum file: {e}"))?;

    let card = FileCard {
        file_id: file_id.clone(),
        name: name.clone(),
        size,
        mime: mime.clone(),
        bytes_done: 0,
        state: "offered".into(),
        local_path: Some(path.display().to_string()),
        sha256: Some(file_sha.clone()),
        error: None,
        resume_capable: None,
    };
    let body = serde_json::to_string(&card).map_err(|e| e.to_string())?;
    let mut msg = ChatMessage {
        id: message_id.clone(),
        peer_id: peer_id.to_string(),
        direction: "out".into(),
        msg_type: "file".into(),
        body,
        created_at: ts,
        status: "pending".into(),
    };

    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;
    state.db.insert_message(&msg)?;
    let _ = app.emit("message", &msg);

    let cancel = Arc::new(AtomicBool::new(false));
    {
        let mut reg = state
            .transfers
            .lock()
            .map_err(|_| "transfers lock".to_string())?;
        reg.outbound.insert(
            file_id.clone(),
            PendingOutbound {
                file_id: file_id.clone(),
                message_id: message_id.clone(),
                peer_id: peer_id.to_string(),
                path: path.clone(),
                name: name.clone(),
                size,
                mime: mime.clone(),
                token: token.clone(),
                peer_address: peer_address.clone(),
                cancel,
                source_mtime_ms: offer_mtime,
                cached_sha256: Some(file_sha.clone()),
            },
        );
    }

    db_upsert_transfer(
        app,
        &make_transfer_row(
            &file_id,
            "send",
            peer_id,
            &message_id,
            &path.display().to_string(),
            None,
            &name,
            size,
            &mime,
            &token,
            0,
            "offered",
            None,
            Some(ts),
        ),
    );

    let wire = WireMessage::FileOffer {
        file_id: file_id.clone(),
        message_id: message_id.clone(),
        name,
        size,
        mime,
        token,
        ts,
        sha256: Some(file_sha),
    };
    session::send_wire_to_peer(app, peer_id, &wire).map_err(|e| {
        let _ = state.db.update_status(&message_id, "failed");
        update_file_message(app, &message_id, peer_id, |card| {
            card.state = "failed".into();
            card.error = Some(e.clone());
        });
        if let Ok(mut reg) = state.transfers.lock() {
            reg.outbound.remove(&file_id);
        }
        db_delete_transfer(app, &file_id);
        diagnostics::error(
            app,
            LogicPoint::XferDataFail,
            format!("offer wire send failed file={file_id}: {e}"),
        );
        format!("Could not send file offer: {e}")
    })?;

    msg.status = "sent".into();
    state.db.update_status(&message_id, "sent")?;
    // Refresh body from card (still offered)
    if let Ok(Some(m)) = state.db.get_message(&message_id) {
        msg.body = m.body;
    }
    diagnostics::info(
        app,
        LogicPoint::XferOfferOut,
        format!(
            "offer out file={file_id} peer={peer_id} size={size} addr={peer_address}"
        ),
    );
    let _ = app.emit("message", &msg);
    Ok(msg)
}

pub fn accept_file<R: Runtime>(app: &AppHandle<R>, message_id: &str, peer_id: &str) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;

    // Mark offer accepted + reserve exclusive dest/partial. Data plane requires accepted flag.
    let (file_id, dest, partial) = {
        let mut reg = state
            .transfers
            .lock()
            .map_err(|_| "transfers lock".to_string())?;
        let entry = reg
            .inbound
            .values_mut()
            .find(|p| p.message_id == message_id && p.peer_id == peer_id)
            .ok_or_else(|| "No pending file offer for this message.".to_string())?;
        if entry.accepted {
            return Err("File already accepted.".into());
        }
        let save_dir = fsutil::default_save_dir()?;
        let (dest, partial) = fsutil::reserve_dest(&save_dir, &entry.name).map_err(|e| {
            diagnostics::error(app, LogicPoint::XferReserve, format!("reserve_dest: {e}"));
            e
        })?;
        diagnostics::info(
            app,
            LogicPoint::XferReserve,
            format!(
                "reserved dest={} partial={}",
                dest.display(),
                partial.display()
            ),
        );
        entry.accepted = true;
        entry.dest_path = Some(dest.clone());
        entry.partial_path = Some(partial.clone());
        entry.bytes_done = 0;
        let file_id = entry.file_id.clone();
        let cancel = entry.cancel.clone();
        // Allow cancel between Accept and first data byte.
        reg.active_cancel.insert(file_id.clone(), cancel);
        (file_id, dest, partial)
    };

    // Persist paths + token for restart resume.
    let (token, name, size, mime) = {
        let reg = state.transfers.lock().map_err(|_| "transfers lock".to_string())?;
        let e = reg
            .inbound
            .get(&file_id)
            .ok_or_else(|| "inbound missing after accept".to_string())?;
        (
            e.token.clone(),
            e.name.clone(),
            e.size,
            e.mime.clone(),
        )
    };
    db_upsert_transfer(
        app,
        &make_transfer_row(
            &file_id,
            "recv",
            peer_id,
            message_id,
            &dest.display().to_string(),
            Some(partial.display().to_string()),
            &name,
            size,
            &mime,
            &token,
            0,
            "accepted",
            None,
            None,
        ),
    );

    update_file_message(app, message_id, peer_id, |card| {
        card.state = "accepted".into();
        card.local_path = Some(partial.display().to_string());
        card.bytes_done = 0;
        card.error = None;
        card.resume_capable = Some(true);
    });

    let wire = WireMessage::FileAccept {
        file_id: file_id.clone(),
        message_id: message_id.to_string(),
    };
    session::send_wire_to_peer(app, peer_id, &wire).map_err(|e| {
        // Roll back reservation if Accept wire fails.
        fsutil::cleanup_reservation(Some(&dest), Some(&partial));
        if let Ok(mut reg) = state.transfers.lock() {
            if let Some(entry) = reg.inbound.get_mut(&file_id) {
                entry.accepted = false;
                entry.dest_path = None;
                entry.partial_path = None;
            }
            reg.active_cancel.remove(&file_id);
        }
        db_state_err(app, &file_id, "offered", Some(0), Some(&e));
        update_file_message(app, message_id, peer_id, |card| {
            card.state = "offered".into();
            card.local_path = None;
            card.resume_capable = None;
            card.error = Some(e.clone());
        });
        e
    })?;
    diagnostics::info(
        app,
        LogicPoint::XferAccept,
        format!("local accept file={file_id} peer={peer_id}"),
    );

    spawn_first_data_wait(
        app.clone(),
        file_id,
        message_id.to_string(),
        peer_id.to_string(),
    );
    Ok(())
}

/// If no data plane bytes arrive after Accept, demote to interrupted@0 (keep token/paths).
fn spawn_first_data_wait<R: Runtime>(
    app: AppHandle<R>,
    file_id: String,
    message_id: String,
    peer_id: String,
) {
    let gen = app
        .try_state::<AppState>()
        .and_then(|s| s.transfers.lock().ok().map(|r| r.next_generation()))
        .unwrap_or(0);
    thread::Builder::new()
        .name("jotain-first-data-wait".into())
        .spawn(move || {
            thread::sleep(FIRST_DATA_WAIT);
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            // If resume_inflight superseded or data active, skip.
            if let Ok(reg) = state.transfers.lock() {
                if reg.active_files.contains(&file_id) {
                    return;
                }
                if reg
                    .resume_inflight
                    .get(&file_id)
                    .is_some_and(|r| r.generation != gen)
                {
                    // A newer resume wait owns this file.
                }
                if !reg.inbound.contains_key(&file_id) {
                    return;
                }
            }
            let still_waiting = state
                .db
                .get_message(&message_id)
                .ok()
                .flatten()
                .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
                .map(|c| c.state == "accepted")
                .unwrap_or(false);
            if !still_waiting {
                return;
            }
            diagnostics::warn(
                &app,
                LogicPoint::XferFirstDataTimeout,
                format!("first data timeout file={file_id} peer={peer_id}"),
            );
            update_file_message(&app, &message_id, &peer_id, |card| {
                card.state = "interrupted".into();
                card.error = Some("first_data_timeout".into());
                card.bytes_done = 0;
                card.resume_capable = Some(true);
            });
            db_state_err(
                &app,
                &file_id,
                "interrupted",
                Some(0),
                Some("first_data_timeout"),
            );
            emit_progress(&app, &file_id, &message_id, &peer_id, 0, 0, "interrupted");
            sound::play(&app, SoundKind::FileAlert);
        })
        .ok();
}

fn spawn_resume_wait<R: Runtime>(
    app: AppHandle<R>,
    file_id: String,
    message_id: String,
    peer_id: String,
    generation: u64,
    size: u64,
    offset: u64,
) {
    thread::Builder::new()
        .name("jotain-resume-wait".into())
        .spawn(move || {
            thread::sleep(RESUME_WAIT);
            let Some(state) = app.try_state::<AppState>() else {
                return;
            };
            let Ok(mut reg) = state.transfers.lock() else {
                return;
            };
            let Some(inf) = reg.resume_inflight.get(&file_id) else {
                return;
            };
            if inf.generation != generation {
                return;
            }
            if reg.active_files.contains(&file_id) {
                return;
            }
            reg.resume_inflight.remove(&file_id);
            drop(reg);
            diagnostics::warn(
                &app,
                LogicPoint::XferResumeFail,
                format!("resume_timeout file={file_id} offset={offset}"),
            );
            update_file_message(&app, &message_id, &peer_id, |card| {
                card.state = "interrupted".into();
                card.error = Some("resume_timeout".into());
                card.bytes_done = offset;
                card.resume_capable = Some(true);
            });
            db_state_err(
                &app,
                &file_id,
                "interrupted",
                Some(offset),
                Some("resume_timeout"),
            );
            emit_progress(
                &app,
                &file_id,
                &message_id,
                &peer_id,
                offset,
                size,
                "interrupted",
            );
            sound::play(&app, SoundKind::FileAlert);
        })
        .ok();
}

/// Receiver: resume interrupted / accepted transfer from disk-aligned offset.
pub fn resume_file<R: Runtime>(
    app: &AppHandle<R>,
    message_id: &str,
    peer_id: &str,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;

    // Session required.
    if session::list_session_peers(app)?
        .iter()
        .all(|s| s.peer_id != peer_id)
    {
        return Err("Not connected to peer. Wait until status shows connected.".into());
    }

    let card_state = state
        .db
        .get_message(message_id)
        .ok()
        .flatten()
        .and_then(|m| {
            if m.peer_id != peer_id {
                return None;
            }
            serde_json::from_str::<FileCard>(&m.body).ok()
        })
        .ok_or_else(|| "Message not found.".to_string())?;

    if card_state.state != "interrupted" && card_state.state != "accepted" {
        return Err(format!(
            "Cannot resume in state '{}'.",
            card_state.state
        ));
    }

    let (file_id, token, partial, size, gen, offset) = {
        let mut reg = state
            .transfers
            .lock()
            .map_err(|_| "transfers lock".to_string())?;
        if reg.resume_inflight.contains_key(&card_state.file_id) {
            // Idempotent: already waiting for data.
            return Ok(());
        }
        if reg.active_files.contains(&card_state.file_id) {
            return Err("Transfer already in progress.".into());
        }
        let entry = reg
            .inbound
            .get_mut(&card_state.file_id)
            .ok_or_else(|| {
                "No resumable transfer in memory (restart app requires PR-R3 persistence)."
                    .to_string()
            })?;
        if entry.peer_id != peer_id || entry.message_id != message_id {
            return Err("Transfer does not match this chat.".into());
        }
        if !entry.accepted {
            return Err("File was never accepted.".into());
        }
        let partial = entry
            .partial_path
            .clone()
            .ok_or_else(|| "Missing partial path.".to_string())?;
        if !partial.exists() {
            // Recreate empty partial if reservation was lost but dest placeholder remains.
            if entry.dest_path.is_some() {
                let _ = File::create(&partial);
            } else {
                return Err("Partial file missing.".into());
            }
        }
        let file_id = entry.file_id.clone();
        let token = entry.token.clone();
        let size = entry.size;
        // Release lock before disk align.
        drop(reg);

        let offset = align_partial_logged(app, &partial)?;

        let mut reg = state
            .transfers
            .lock()
            .map_err(|_| "transfers lock".to_string())?;
        if let Some(entry) = reg.inbound.get_mut(&file_id) {
            entry.bytes_done = offset;
        }
        let gen = reg.next_generation();
        reg.resume_inflight.insert(
            file_id.clone(),
            ResumeInflight {
                generation: gen,
                message_id: message_id.to_string(),
                peer_id: peer_id.to_string(),
                offset,
            },
        );
        (file_id, token, partial, size, gen, offset)
    };

    update_file_message(app, message_id, peer_id, |card| {
        card.state = "transferring".into();
        card.bytes_done = offset;
        card.error = None;
        card.resume_capable = Some(true);
        card.local_path = Some(partial.display().to_string());
    });
    db_progress(app, &file_id, offset, "transferring");
    emit_progress(
        app,
        &file_id,
        message_id,
        peer_id,
        offset,
        size,
        "transferring",
    );

    let wire = WireMessage::FileResume {
        file_id: file_id.clone(),
        message_id: message_id.to_string(),
        resume_offset: offset,
        token,
    };
    session::send_wire_to_peer(app, peer_id, &wire).map_err(|e| {
        clear_resume_inflight(app, &file_id);
        update_file_message(app, message_id, peer_id, |card| {
            card.state = "interrupted".into();
            card.error = Some(e.clone());
            card.resume_capable = Some(true);
        });
        e
    })?;

    diagnostics::info(
        app,
        LogicPoint::XferResume,
        format!("local resume file={file_id} offset={offset} peer={peer_id}"),
    );

    spawn_resume_wait(
        app.clone(),
        file_id,
        message_id.to_string(),
        peer_id.to_string(),
        gen,
        size,
        offset,
    );
    Ok(())
}

pub fn on_file_resume_wire<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    file_id: String,
    message_id: String,
    resume_offset: u64,
    token: String,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };

    let reject = |reason: &str, detail: Option<String>| {
        let wire = WireMessage::FileResumeReject {
            file_id: file_id.clone(),
            message_id: message_id.clone(),
            reason: reason.into(),
            detail,
        };
        let _ = session::send_wire_to_peer(app, peer_id, &wire);
        diagnostics::warn(
            app,
            LogicPoint::XferResumeFail,
            format!("resume reject file={file_id} reason={reason}"),
        );
    };

    let out = {
        let Ok(reg) = state.transfers.lock() else {
            return;
        };
        if reg.active_files.contains(&file_id) {
            drop(reg);
            reject("busy", Some("push already in flight".into()));
            return;
        }
        match reg.outbound.get(&file_id) {
            Some(o) => o.clone(),
            None => {
                drop(reg);
                reject("unknown_file", None);
                return;
            }
        }
    };

    if out.peer_id != peer_id {
        reject("unknown_file", Some("peer mismatch".into()));
        return;
    }
    if out.token != token {
        reject("token_mismatch", None);
        return;
    }
    if resume_offset >= out.size {
        reject(
            "offset_invalid",
            Some(format!("offset={resume_offset} size={}", out.size)),
        );
        return;
    }
    if !out.path.exists() {
        reject("source_missing", None);
        return;
    }
    match fs::metadata(&out.path) {
        Ok(m) if m.len() != out.size => {
            reject(
                "source_changed",
                Some(format!("len={} expected={}", m.len(), out.size)),
            );
            return;
        }
        Ok(_) => {}
        Err(e) => {
            reject("source_missing", Some(e.to_string()));
            return;
        }
    }

    diagnostics::info(
        app,
        LogicPoint::XferResume,
        format!(
            "serving resume file={file_id} offset={resume_offset} peer={peer_id}"
        ),
    );
    spawn_outbound_push(app, &file_id, &message_id, resume_offset, "resume");
}

pub fn on_file_resume_reject_wire<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    file_id: String,
    message_id: String,
    reason: String,
    detail: Option<String>,
) {
    clear_resume_inflight(app, &file_id);
    let msg = match detail {
        Some(d) if !d.is_empty() => format!("{reason}: {d}"),
        _ => reason.clone(),
    };
    diagnostics::warn(
        app,
        LogicPoint::XferResumeFail,
        format!("resume rejected file={file_id} reason={msg}"),
    );
    let bytes = app
        .try_state::<AppState>()
        .and_then(|s| s.db.get_message(&message_id).ok().flatten())
        .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
        .map(|c| c.bytes_done)
        .unwrap_or(0);
    let can_retry = !matches!(
        reason.as_str(),
        "source_missing" | "source_changed" | "token_mismatch" | "offset_invalid"
    );
    update_file_message(app, &message_id, peer_id, |card| {
        card.state = "interrupted".into();
        card.error = Some(msg);
        card.resume_capable = Some(can_retry);
    });
    let size = app
        .try_state::<AppState>()
        .and_then(|s| s.db.get_message(&message_id).ok().flatten())
        .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
        .map(|c| c.size)
        .unwrap_or(0);
    emit_progress(
        app,
        &file_id,
        &message_id,
        peer_id,
        bytes,
        size,
        "interrupted",
    );
    sound::play(app, SoundKind::FileAlert);
}

pub fn reject_file<R: Runtime>(app: &AppHandle<R>, message_id: &str, peer_id: &str) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;
    let file_id = {
        let mut reg = state
            .transfers
            .lock()
            .map_err(|_| "transfers lock".to_string())?;
        let id = reg
            .inbound
            .values()
            .find(|p| p.message_id == message_id && p.peer_id == peer_id && !p.accepted)
            .map(|p| p.file_id.clone());
        if let Some(ref fid) = id {
            reg.inbound.remove(fid);
            reg.active_cancel.remove(fid);
        }
        id
    }
    .ok_or_else(|| "No pending file offer to reject.".to_string())?;

    update_file_message(app, message_id, peer_id, |card| {
        card.state = "rejected".into();
    });
    db_delete_transfer(app, &file_id);
    let wire = WireMessage::FileReject {
        file_id: file_id.clone(),
        message_id: message_id.to_string(),
    };
    session::send_wire_to_peer(app, peer_id, &wire)?;
    diagnostics::info(
        app,
        LogicPoint::XferReject,
        format!("local reject file={file_id}"),
    );
    Ok(())
}

pub fn cancel_file<R: Runtime>(app: &AppHandle<R>, file_id: &str, peer_id: &str) -> Result<(), String> {
    finalize_cancel(app, file_id, peer_id, true)
}

/// Stop I/O, delete reservation files, drop registry, mark cancelled.
/// `send_wire`: local user cancel sends FileCancel; history cleanup may skip if peer gone.
pub fn finalize_cancel<R: Runtime>(
    app: &AppHandle<R>,
    file_id: &str,
    peer_id: &str,
    send_wire: bool,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;
    let (message_id, dest, partial) = {
        let mut reg = state
            .transfers
            .lock()
            .map_err(|_| "transfers lock".to_string())?;
        if let Some(c) = reg.active_cancel.get(file_id) {
            c.store(true, Ordering::SeqCst);
        }
        if let Some(p) = reg.inbound.get(file_id) {
            p.cancel.store(true, Ordering::SeqCst);
        }
        if let Some(p) = reg.outbound.get(file_id) {
            p.cancel.store(true, Ordering::SeqCst);
        }
        let dest = reg
            .inbound
            .get(file_id)
            .and_then(|p| p.dest_path.clone());
        let partial = reg
            .inbound
            .get(file_id)
            .and_then(|p| p.partial_path.clone());
        let mid = reg
            .inbound
            .get(file_id)
            .map(|p| p.message_id.clone())
            .or_else(|| reg.outbound.get(file_id).map(|p| p.message_id.clone()));
        reg.inbound.remove(file_id);
        reg.outbound.remove(file_id);
        reg.active_cancel.remove(file_id);
        reg.resume_inflight.remove(file_id);
        // Bump generation so pending timers become no-ops.
        let _ = reg.next_generation();
        (mid, dest, partial)
    };

    db_delete_transfer(app, file_id);

    // Disk cleanup for receiver reservation / interrupted partial.
    if dest.is_some() || partial.is_some() {
        fsutil::cleanup_reservation(dest.as_deref(), partial.as_deref());
    } else if let Some(ref mid) = message_id {
        // Interrupted cards may have local_path but registry already empty after edge cases.
        if let Ok(Some(msg)) = state.db.get_message(mid) {
            if let Ok(card) = serde_json::from_str::<FileCard>(&msg.body) {
                if let Some(lp) = card.local_path {
                    let p = PathBuf::from(&lp);
                    if lp.ends_with(".partial") {
                        let dest_guess = PathBuf::from(lp.trim_end_matches(".partial"));
                        fsutil::cleanup_reservation(Some(&dest_guess), Some(&p));
                    }
                }
            }
        }
    }

    if let Some(mid) = message_id {
        update_file_message(app, &mid, peer_id, |card| {
            card.state = "cancelled".into();
            card.error = None;
            card.resume_capable = Some(false);
        });
        if send_wire {
            let wire = WireMessage::FileCancel {
                file_id: file_id.to_string(),
                message_id: mid,
            };
            let _ = session::send_wire_to_peer(app, peer_id, &wire);
        }
    }
    diagnostics::info(app, LogicPoint::XferCancel, format!("local cancel file={file_id}"));
    Ok(())
}

// Note: history helpers below already call finalize_cancel which deletes transfers.

/// Drop inbound/outbound for a message (history delete). Offered: registry only.
/// Accepted/interrupted/transferring: full finalize_cancel.
pub fn on_history_delete_message<R: Runtime>(
    app: &AppHandle<R>,
    message_id: &str,
    peer_id: &str,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let card_state = state
        .db
        .get_message(message_id)
        .ok()
        .flatten()
        .and_then(|m| serde_json::from_str::<FileCard>(&m.body).ok())
        .map(|c| (c.file_id, c.state));

    // Prefer registry lookup if message body already gone (delete before call).
    let from_reg = {
        let Ok(reg) = state.transfers.lock() else {
            return;
        };
        reg.inbound
            .values()
            .find(|p| p.message_id == message_id && p.peer_id == peer_id)
            .map(|p| {
                (
                    p.file_id.clone(),
                    if p.accepted {
                        "accepted".to_string()
                    } else {
                        "offered".to_string()
                    },
                    p.dest_path.clone(),
                    p.partial_path.clone(),
                )
            })
            .or_else(|| {
                reg.outbound
                    .values()
                    .find(|p| p.message_id == message_id && p.peer_id == peer_id)
                    .map(|p| (p.file_id.clone(), "offered".to_string(), None, None))
            })
    };

    if let Some((file_id, st, dest, partial)) = from_reg {
        if st == "offered" {
            if let Ok(mut reg) = state.transfers.lock() {
                if let Some(p) = reg.inbound.remove(&file_id) {
                    p.cancel.store(true, Ordering::SeqCst);
                }
                reg.outbound.remove(&file_id);
                reg.active_cancel.remove(&file_id);
            }
            db_delete_transfer(app, &file_id);
            // Best-effort Reject/Cancel so sender drops.
            let wire = WireMessage::FileReject {
                file_id,
                message_id: message_id.to_string(),
            };
            let _ = session::send_wire_to_peer(app, peer_id, &wire);
            let _ = dest;
            let _ = partial;
        } else {
            let _ = finalize_cancel(app, &file_id, peer_id, true);
        }
        return;
    }

    if let Some((file_id, st)) = card_state {
        match st.as_str() {
            "offered" => {
                if let Ok(mut reg) = state.transfers.lock() {
                    reg.inbound.remove(&file_id);
                    reg.outbound.remove(&file_id);
                    reg.active_cancel.remove(&file_id);
                }
                db_delete_transfer(app, &file_id);
            }
            "accepted" | "transferring" | "interrupted" => {
                let _ = finalize_cancel(app, &file_id, peer_id, true);
            }
            "completed" | "failed" | "cancelled" | "rejected" => {
                db_delete_transfer(app, &file_id);
            }
            _ => {
                db_delete_transfer(app, &file_id);
            }
        }
    } else if let Ok(Some(row)) = state.db.get_transfer_by_message(message_id) {
        // Card already gone; clean transfer by message.
        let _ = finalize_cancel(app, &row.file_id, peer_id, false);
        db_delete_transfer(app, &row.file_id);
    }
}

/// Clear all in-flight transfers for a peer (clear thread / clear all).
pub fn on_history_clear_peer<R: Runtime>(app: &AppHandle<R>, peer_id: &str) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let file_ids: Vec<(String, bool)> = {
        let Ok(reg) = state.transfers.lock() else {
            return;
        };
        let mut v = Vec::new();
        for p in reg.inbound.values() {
            if p.peer_id == peer_id {
                v.push((p.file_id.clone(), p.accepted));
            }
        }
        for p in reg.outbound.values() {
            if p.peer_id == peer_id {
                v.push((p.file_id.clone(), true));
            }
        }
        v
    };
    for (fid, accepted) in file_ids {
        if accepted {
            let _ = finalize_cancel(app, &fid, peer_id, false);
        } else if let Ok(mut reg) = state.transfers.lock() {
            reg.inbound.remove(&fid);
            reg.outbound.remove(&fid);
            reg.active_cancel.remove(&fid);
        }
    }
}

pub fn on_history_clear_all<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let items: Vec<(String, String, bool)> = {
        let Ok(reg) = state.transfers.lock() else {
            return;
        };
        let mut v = Vec::new();
        for p in reg.inbound.values() {
            v.push((p.file_id.clone(), p.peer_id.clone(), p.accepted));
        }
        for p in reg.outbound.values() {
            v.push((p.file_id.clone(), p.peer_id.clone(), true));
        }
        v
    };
    for (fid, peer, accepted) in items {
        if accepted {
            let _ = finalize_cancel(app, &fid, &peer, false);
        } else if let Ok(mut reg) = state.transfers.lock() {
            reg.inbound.remove(&fid);
            reg.outbound.remove(&fid);
            reg.active_cancel.remove(&fid);
        }
    }
}

// --- helpers ---

fn update_file_message<R: Runtime>(
    app: &AppHandle<R>,
    message_id: &str,
    peer_id: &str,
    f: impl FnOnce(&mut FileCard),
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Ok(Some(mut msg)) = state.db.get_message(message_id) else {
        return;
    };
    if msg.peer_id != peer_id {
        return;
    }
    let mut card: FileCard = serde_json::from_str(&msg.body).unwrap_or(FileCard {
        file_id: String::new(),
        name: String::new(),
        size: 0,
        mime: String::new(),
        bytes_done: 0,
        state: "failed".into(),
        local_path: None,
        sha256: None,
        error: Some("corrupt card".into()),
        resume_capable: None,
    });
    f(&mut card);
    msg.body = serde_json::to_string(&card).unwrap_or_default();
    let _ = state.db.update_body_and_status(&msg.id, &msg.body, &msg.status);
    let _ = app.emit("message", &msg);
}

fn emit_progress<R: Runtime>(
    app: &AppHandle<R>,
    file_id: &str,
    message_id: &str,
    peer_id: &str,
    bytes_done: u64,
    size: u64,
    state: &str,
) {
    let _ = app.emit(
        "transfer-progress",
        TransferProgress {
            file_id: file_id.into(),
            message_id: message_id.into(),
            peer_id: peer_id.into(),
            bytes_done,
            size,
            state: state.into(),
        },
    );
}

fn random_token() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn mime_guess(name: &str) -> String {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" | "md" => "text/plain",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
    .into()
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Open native file dialog on the UI/main thread (required on macOS).
fn pick_file_on_main_thread<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let path = rfd::FileDialog::new()
            .set_title("Send file via jotainchatttttttt")
            .pick_file();
        let _ = tx.send(path);
    })
    .map_err(|e| format!("Could not open file dialog: {e}"))?;

    match rx.recv() {
        Ok(Some(p)) => Ok(p),
        Ok(None) => Err("No file selected.".into()),
        Err(_) => Err("File dialog failed.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_inbound_defaults_not_accepted() {
        let p = PendingInbound {
            file_id: "f".into(),
            message_id: "m".into(),
            peer_id: "p".into(),
            name: "a.txt".into(),
            size: 1,
            mime: "text/plain".into(),
            token: "t".into(),
            cancel: Arc::new(AtomicBool::new(false)),
            accepted: false,
            dest_path: None,
            partial_path: None,
            bytes_done: 0,
            expected_sha256: None,
        };
        assert!(!p.accepted);
    }

    #[test]
    fn data_header_roundtrip() {
        let h = DataHeader {
            file_id: "f".into(),
            token: "abc".into(),
            size: 99,
            name: "x.bin".into(),
            offset: 0,
        };
        let b = serde_json::to_vec(&h).unwrap();
        let back: DataHeader = serde_json::from_slice(&b).unwrap();
        assert_eq!(back.size, 99);
        assert_eq!(back.token, "abc");
        assert_eq!(back.offset, 0);
    }

    #[test]
    fn data_header_missing_offset_defaults_zero() {
        let raw = r#"{"fileId":"f","token":"t","size":1,"name":"a"}"#;
        let h: DataHeader = serde_json::from_str(raw).unwrap();
        assert_eq!(h.offset, 0);
    }

    #[test]
    fn socket_addr_formats_ipv4_and_ipv6() {
        assert_eq!(socket_addr_str("192.168.1.5", 48767), "192.168.1.5:48767");
        assert_eq!(
            socket_addr_str("fe80::1", 48767),
            "[fe80::1]:48767"
        );
        assert_eq!(
            socket_addr_str("[fe80::1]", 48767),
            "[fe80::1]:48767"
        );
    }
}
