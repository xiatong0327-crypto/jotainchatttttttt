//! TCP control sessions: accept, dial, arbitration, text send/receive.

use crate::db::ChatMessage;
use crate::diagnostics::{self, LogicPoint};
use crate::discovery::{PeerInfo, CONTROL_PORT, PROTOCOL_VERSION};
use crate::net::frame::{read_frame, write_frame, FrameError};
use crate::net::group;
use crate::net::protocol::{validate_text_body, WireMessage};
use crate::net::transfer;
use crate::state::AppState;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const READ_IDLE: Duration = Duration::from_secs(45);
const DIAL_INTERVAL: Duration = Duration::from_secs(2);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub peer_id: String,
    pub display_name: String,
    pub address: String,
    pub connected: bool,
}

struct LiveSession {
    peer_id: String,
    display_name: String,
    address: String,
    writer: Mutex<TcpStream>,
    /// True if we initiated the TCP connection (we are dialer).
    we_dialed: bool,
    stop: AtomicBool,
}

pub struct SessionMap {
    by_peer: HashMap<String, Arc<LiveSession>>,
    /// Peers with an in-flight outbound dial (prevents dial storms).
    dialing: HashSet<String>,
}

impl SessionMap {
    pub fn new() -> Self {
        Self {
            by_peer: HashMap::new(),
            dialing: HashSet::new(),
        }
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        let mut v: Vec<SessionInfo> = self
            .by_peer
            .values()
            .map(|s| SessionInfo {
                peer_id: s.peer_id.clone(),
                display_name: s.display_name.clone(),
                address: s.address.clone(),
                connected: true,
            })
            .collect();
        v.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        v
    }

    fn get(&self, peer_id: &str) -> Option<Arc<LiveSession>> {
        self.by_peer.get(peer_id).cloned()
    }

    /// Returns true if dial may proceed (marked as dialing).
    fn try_begin_dial(&mut self, peer_id: &str) -> bool {
        if self.by_peer.contains_key(peer_id) || self.dialing.contains(peer_id) {
            return false;
        }
        self.dialing.insert(peer_id.to_string());
        true
    }

    fn end_dial(&mut self, peer_id: &str) {
        self.dialing.remove(peer_id);
    }
}

/// Lower device_id is the preferred dialer (design §5.3).
pub fn is_preferred_dialer(self_id: &str, peer_id: &str) -> bool {
    self_id < peer_id
}

/// Connection is on the correct side when we_dialed matches preferred-dialer role.
pub fn is_correct_connection_side(we_dialed: bool, self_id: &str, peer_id: &str) -> bool {
    we_dialed == is_preferred_dialer(self_id, peer_id)
}

pub fn start_control_plane<R: Runtime>(app: AppHandle<R>) {
    // Accept loop
    let app_accept = app.clone();
    thread::Builder::new()
        .name("jotain-tcp-accept".into())
        .spawn(move || accept_loop(app_accept))
        .expect("spawn accept");

    // Dial + keepalive loop
    let app_dial = app.clone();
    thread::Builder::new()
        .name("jotain-tcp-dial".into())
        .spawn(move || dial_loop(app_dial))
        .expect("spawn dial");
}

fn accept_loop<R: Runtime>(app: AppHandle<R>) {
    let listener = match TcpListener::bind(("0.0.0.0", CONTROL_PORT)) {
        Ok(l) => {
            diagnostics::info(
                &app,
                LogicPoint::TcpListen,
                format!("TCP control listening 0.0.0.0:{CONTROL_PORT}"),
            );
            l
        }
        Err(e) => {
            diagnostics::error(
                &app,
                LogicPoint::TcpListenFail,
                format!("TCP bind {CONTROL_PORT} failed: {e}"),
            );
            return;
        }
    };
    let _ = listener.set_nonblocking(false);
    loop {
        match listener.accept() {
            Ok((stream, addr)) => {
                diagnostics::info(
                    &app,
                    LogicPoint::TcpAccept,
                    format!("inbound TCP from {addr}"),
                );
                let app2 = app.clone();
                thread::Builder::new()
                    .name("jotain-tcp-in".into())
                    .spawn(move || {
                        if let Err(e) = handle_connection(app2.clone(), stream, addr, false) {
                            diagnostics::warn(
                                &app2,
                                LogicPoint::TcpHelloFail,
                                format!("inbound session error from {addr}: {e}"),
                            );
                        }
                    })
                    .ok();
            }
            Err(e) => {
                diagnostics::error(
                    &app,
                    LogicPoint::TcpAcceptFail,
                    format!("accept error: {e}"),
                );
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

fn dial_loop<R: Runtime>(app: AppHandle<R>) {
    loop {
        // Order matters after sleep/wake:
        // 1) drop half-open / offline sessions so UI is honest and redial can proceed
        // 2) ping survivors (failed write also drops)
        // 3) dial preferred peers that are online and not yet connected
        reconcile_sessions_with_peers(&app);
        ping_sessions(&app);
        try_dial_peers(&app);
        thread::sleep(DIAL_INTERVAL);
    }
}

/// Drop TCP sessions that no longer match discovery reality.
///
/// Covers: peer left, peer offline TTL, DHCP IP change after sleep/wake,
/// and ghost "connected" rows that would block redial forever.
fn reconcile_sessions_with_peers<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let peers = {
        let Ok(table) = state.peers.lock() else {
            return;
        };
        table.list()
    };
    let by_id: HashMap<String, PeerInfo> = peers
        .into_iter()
        .map(|p| (p.device_id.clone(), p))
        .collect();

    let to_drop: Vec<Arc<LiveSession>> = {
        let Ok(map) = state.sessions.lock() else {
            return;
        };
        map.by_peer
            .values()
            .filter(|s| {
                match by_id.get(&s.peer_id) {
                    // Still advertised and same IP — keep.
                    Some(p) if p.online && p.address == s.address => false,
                    // Offline, gone, or IP changed after roam/wake — drop.
                    _ => true,
                }
            })
            .cloned()
            .collect()
    };

    if to_drop.is_empty() {
        return;
    }
    for s in &to_drop {
        diagnostics::warn(
            app,
            LogicPoint::TcpReconcileDrop,
            format!(
                "drop session peer={} addr={} (offline/IP change/missing)",
                s.peer_id, s.address
            ),
        );
    }
    drop_sessions(app, &to_drop);
}

fn drop_sessions<R: Runtime>(app: &AppHandle<R>, sessions: &[Arc<LiveSession>]) {
    if sessions.is_empty() {
        return;
    }
    for s in sessions {
        s.stop.store(true, Ordering::SeqCst);
        if let Ok(w) = s.writer.lock() {
            let _ = w.shutdown(Shutdown::Both);
        }
    }
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut map) = state.sessions.lock() {
            for s in sessions {
                if map
                    .by_peer
                    .get(&s.peer_id)
                    .is_some_and(|cur| Arc::ptr_eq(cur, s))
                {
                    map.by_peer.remove(&s.peer_id);
                    diagnostics::info(
                        app,
                        LogicPoint::TcpSessionDown,
                        format!("session removed peer={} addr={}", s.peer_id, s.address),
                    );
                }
                map.end_dial(&s.peer_id);
            }
        }
    }
    emit_sessions(app);
}

fn try_dial_peers<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let self_id = {
        let Ok(cfg) = state.config.lock() else {
            return;
        };
        cfg.device_id.clone()
    };
    let peers = {
        let Ok(table) = state.peers.lock() else {
            return;
        };
        table.list()
    };

    for peer in peers {
        if !peer.online {
            continue;
        }
        if !is_preferred_dialer(&self_id, &peer.device_id) {
            continue;
        }
        {
            let Ok(mut sessions) = state.sessions.lock() else {
                continue;
            };
            if !sessions.try_begin_dial(&peer.device_id) {
                continue;
            }
        }

        let port = if peer.control_port == 0 {
            CONTROL_PORT
        } else {
            peer.control_port
        };
        let addr_str = format!("{}:{}", peer.address, port);
        let sock_addr: SocketAddr = match addr_str.parse() {
            Ok(a) => a,
            Err(_) => {
                clear_dialing(app, &peer.device_id);
                continue;
            }
        };

        let peer_id = peer.device_id.clone();
        diagnostics::info(
            app,
            LogicPoint::TcpDialStart,
            format!(
                "dial peer={} addr={}:{}",
                peer_id, peer.address, port
            ),
        );
        match TcpStream::connect_timeout(&sock_addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                let app2 = app.clone();
                let peer_hint = peer_id.clone();
                let spawn_result = thread::Builder::new()
                    .name("jotain-tcp-out".into())
                    .spawn(move || {
                        if let Err(e) = handle_connection(app2.clone(), stream, sock_addr, true) {
                            diagnostics::warn(
                                &app2,
                                LogicPoint::TcpHelloFail,
                                format!("outbound session to {peer_hint} error: {e}"),
                            );
                        }
                        clear_dialing(&app2, &peer_hint);
                    });
                // If thread failed to start, dialing must not stick forever (solo/long-run).
                if spawn_result.is_err() {
                    diagnostics::error(
                        app,
                        LogicPoint::TcpDialSpawnFail,
                        format!("spawn outbound thread failed peer={peer_id}"),
                    );
                    clear_dialing(app, &peer_id);
                }
            }
            Err(e) => {
                // Peer may not be listening yet; retry later.
                diagnostics::warn(
                    app,
                    LogicPoint::TcpDialFail,
                    format!("connect timeout/fail peer={peer_id} {sock_addr}: {e}"),
                );
                clear_dialing(app, &peer_id);
            }
        }
    }
}

fn clear_dialing<R: Runtime>(app: &AppHandle<R>, peer_id: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut sessions) = state.sessions.lock() {
            sessions.end_dial(peer_id);
        }
    }
}

fn ping_sessions<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let sessions: Vec<Arc<LiveSession>> = {
        let Ok(map) = state.sessions.lock() else {
            return;
        };
        map.by_peer.values().cloned().collect()
    };
    if sessions.is_empty() {
        return;
    }
    let ping = match WireMessage::Ping.to_bytes() {
        Ok(b) => b,
        Err(_) => return,
    };
    let mut dead: Vec<Arc<LiveSession>> = Vec::new();
    for s in sessions {
        let failed = match s.writer.lock() {
            Ok(mut w) => write_frame(&mut *w, &ping).is_err(),
            Err(_) => true,
        };
        // Write failure after sleep → dead half-open socket; drop so we can redial.
        if failed {
            diagnostics::warn(
                app,
                LogicPoint::TcpPingFail,
                format!("ping write failed peer={} addr={}", s.peer_id, s.address),
            );
            dead.push(s);
        }
    }
    drop_sessions(app, &dead);
}

fn handle_connection<R: Runtime>(
    app: AppHandle<R>,
    stream: TcpStream,
    addr: SocketAddr,
    we_dialed: bool,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(HELLO_TIMEOUT))
        .map_err(|e| e.to_string())?;
    stream
        .set_nodelay(true)
        .map_err(|e| e.to_string())?;

    let mut writer = stream
        .try_clone()
        .map_err(|e| format!("clone stream: {e}"))?;
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .map_err(|e| format!("clone stream read: {e}"))?,
    );

    // Identity for hello
    let (self_id, self_name) = {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "no state".to_string())?;
        let cfg = state
            .config
            .lock()
            .map_err(|_| "config lock".to_string())?;
        let name = if cfg.display_name.is_empty() {
            "Mac".to_string()
        } else {
            cfg.display_name.clone()
        };
        (cfg.device_id.clone(), name)
    };

    // Always send hello first.
    let hello = WireMessage::hello(self_id.clone(), self_name)
        .to_bytes()
        .map_err(|e| e)?;
    write_frame(&mut writer, &hello)?;

    // Read peer hello
    let peer_hello_bytes = read_frame(&mut reader).map_err(|e| e.to_string())?;
    let peer_hello = WireMessage::from_bytes(&peer_hello_bytes)?;
    let (peer_id, peer_name, peer_v) = match peer_hello {
        WireMessage::Hello {
            v,
            device_id,
            display_name,
        } => (device_id, display_name, v),
        _ => return Err("expected hello".into()),
    };

    if peer_v != PROTOCOL_VERSION {
        let msg = format!(
            "incompatible protocol version {peer_v} (ours {PROTOCOL_VERSION}) from {addr}"
        );
        diagnostics::error(&app, LogicPoint::TcpHelloFail, &msg);
        return Err(msg);
    }
    if peer_id == self_id {
        diagnostics::warn(&app, LogicPoint::TcpHelloFail, "hello peer_id equals self");
        return Err("peer is self".into());
    }

    diagnostics::info(
        &app,
        LogicPoint::TcpHelloOk,
        format!(
            "hello ok peer={} name={:?} we_dialed={} from={}",
            peer_id, peer_name, we_dialed, addr
        ),
    );

    // Arbitration if dual connect: keep the side matching preferred dialer.
    let live = {
        let state = app
            .try_state::<AppState>()
            .ok_or_else(|| "no state".to_string())?;
        let mut sessions = state
            .sessions
            .lock()
            .map_err(|_| "sessions lock".to_string())?;

        // Outbound dial attempt finished (success path clears later too).
        sessions.end_dial(&peer_id);

        let new_correct = is_correct_connection_side(we_dialed, &self_id, &peer_id);

        if let Some(existing) = sessions.by_peer.get(&peer_id).cloned() {
            let existing_correct =
                is_correct_connection_side(existing.we_dialed, &self_id, &peer_id);
            if existing_correct && !new_correct {
                diagnostics::info(
                    &app,
                    LogicPoint::TcpArbDrop,
                    format!("arb drop new (keep existing) peer={peer_id} we_dialed={we_dialed}"),
                );
                let _ = stream.shutdown(Shutdown::Both);
                return Ok(());
            }
            if existing_correct && new_correct {
                diagnostics::info(
                    &app,
                    LogicPoint::TcpArbDrop,
                    format!("arb drop new (race keep first) peer={peer_id}"),
                );
                let _ = stream.shutdown(Shutdown::Both);
                return Ok(());
            }
            // Existing is wrong side — replace.
            diagnostics::warn(
                &app,
                LogicPoint::TcpArbReplace,
                format!("arb replace existing session peer={peer_id}"),
            );
            existing.stop.store(true, Ordering::SeqCst);
            if let Ok(w) = existing.writer.lock() {
                let _ = w.shutdown(Shutdown::Both);
            }
            sessions.by_peer.remove(&peer_id);
        } else if !new_correct {
            diagnostics::info(
                &app,
                LogicPoint::TcpArbDrop,
                format!("arb drop new (wrong side) peer={peer_id} we_dialed={we_dialed}"),
            );
            let _ = stream.shutdown(Shutdown::Both);
            return Ok(());
        }

        let live = Arc::new(LiveSession {
            peer_id: peer_id.clone(),
            display_name: peer_name.clone(),
            address: addr.ip().to_string(),
            writer: Mutex::new(writer),
            we_dialed,
            stop: AtomicBool::new(false),
        });
        sessions.by_peer.insert(peer_id.clone(), live.clone());
        live
    };

    diagnostics::info(
        &app,
        LogicPoint::TcpSessionUp,
        format!(
            "session up peer={} name={:?} addr={} we_dialed={}",
            peer_id, peer_name, live.address, we_dialed
        ),
    );
    emit_sessions(&app);
    // PR-R3: auto-resume interrupted inbound transfers for this peer.
    transfer::on_peer_session_up(&app, &peer_id);

    // Longer read timeout for session lifetime
    stream
        .set_read_timeout(Some(READ_IDLE))
        .map_err(|e| e.to_string())?;

    // Reader loop
    loop {
        if live.stop.load(Ordering::SeqCst) {
            break;
        }
        // Dropped from map by a replacement connection.
        {
            let Some(state) = app.try_state::<AppState>() else {
                break;
            };
            let Ok(sessions) = state.sessions.lock() else {
                break;
            };
            match sessions.get(&peer_id) {
                Some(current) if Arc::ptr_eq(&current, &live) => {}
                _ => break,
            }
        }

        let frame = match read_frame(&mut reader) {
            Ok(f) => f,
            // Idle between frames. (Pings keep this rare while peer is alive.)
            Err(FrameError::Timeout) => continue,
            Err(FrameError::TooLarge(n)) => {
                diagnostics::error(
                    &app,
                    LogicPoint::TcpFrameBad,
                    format!("oversized frame from {peer_id}: {n}"),
                );
                break;
            }
            Err(FrameError::Io(e)) => {
                diagnostics::warn(
                    &app,
                    LogicPoint::TcpReadFail,
                    format!("read fail peer={peer_id}: {e}"),
                );
                break;
            }
        };

        match WireMessage::from_bytes(&frame) {
            Ok(WireMessage::Text { id, body, ts }) => {
                handle_incoming_text(&app, &peer_id, id, body, ts);
            }
            Ok(WireMessage::FileOffer {
                file_id,
                message_id,
                name,
                size,
                mime,
                token,
                ts,
                sha256,
                auto_accept,
            }) => {
                transfer::on_file_offer_wire(
                    &app,
                    &peer_id,
                    file_id,
                    message_id,
                    name,
                    size,
                    mime,
                    token,
                    ts,
                    sha256,
                    auto_accept,
                );
            }
            Ok(WireMessage::FileAccept {
                file_id,
                message_id,
            }) => {
                transfer::on_file_accept_wire(&app, &peer_id, file_id, message_id);
            }
            Ok(WireMessage::FileReject {
                file_id,
                message_id,
            }) => {
                transfer::on_file_reject_wire(&app, &peer_id, file_id, message_id);
            }
            Ok(WireMessage::FileCancel {
                file_id,
                message_id,
            }) => {
                transfer::on_file_cancel_wire(&app, &peer_id, file_id, message_id);
            }
            Ok(WireMessage::FileResume {
                file_id,
                message_id,
                resume_offset,
                token,
            }) => {
                transfer::on_file_resume_wire(
                    &app,
                    &peer_id,
                    file_id,
                    message_id,
                    resume_offset,
                    token,
                );
            }
            Ok(WireMessage::FileResumeReject {
                file_id,
                message_id,
                reason,
                detail,
            }) => {
                transfer::on_file_resume_reject_wire(
                    &app,
                    &peer_id,
                    file_id,
                    message_id,
                    reason,
                    detail,
                );
            }
            Ok(WireMessage::Ping) => {
                if let Ok(mut w) = live.writer.lock() {
                    if let Ok(bytes) = WireMessage::Pong.to_bytes() {
                        let _ = write_frame(&mut *w, &bytes);
                    }
                }
            }
            Ok(WireMessage::Pong) => {}
            Ok(WireMessage::Hello { .. }) => {
                // ignore extra hellos
            }
            Ok(WireMessage::GroupJoinRequest {
                group_id,
                join_code,
                device_id,
                display_name,
            }) => {
                group::on_group_join_request(
                    &app,
                    &peer_id,
                    group_id,
                    join_code,
                    device_id,
                    display_name,
                );
            }
            Ok(WireMessage::GroupJoinOk {
                group_id,
                name,
                join_code,
                creator_id,
                members,
            }) => {
                group::on_group_join_ok(
                    &app, group_id, name, join_code, creator_id, members,
                );
            }
            Ok(WireMessage::GroupJoinReject { group_id, reason }) => {
                group::on_group_join_reject(&app, group_id, reason);
            }
            Ok(WireMessage::GroupMemberUpdate {
                group_id,
                name,
                join_code,
                creator_id,
                members,
            }) => {
                group::on_group_member_update(
                    &app, group_id, name, join_code, creator_id, members,
                );
            }
            Ok(WireMessage::GroupLeave {
                group_id,
                device_id,
            }) => {
                group::on_group_leave(&app, group_id, device_id);
            }
            Ok(WireMessage::GroupText {
                group_id,
                id,
                from_device_id,
                from_name,
                body,
                ts,
            }) => {
                group::on_group_text(
                    &app,
                    &peer_id,
                    group_id,
                    id,
                    from_device_id,
                    from_name,
                    body,
                    ts,
                );
            }
            Err(e) => {
                diagnostics::warn(
                    &app,
                    LogicPoint::TcpFrameBad,
                    format!("bad frame from {peer_id}: {e}"),
                );
            }
        }
    }

    // Cleanup only if we still own the map entry (do not clobber a replacement).
    {
        if let Some(state) = app.try_state::<AppState>() {
            if let Ok(mut sessions) = state.sessions.lock() {
                if sessions
                    .by_peer
                    .get(&peer_id)
                    .is_some_and(|s| Arc::ptr_eq(s, &live))
                {
                    sessions.by_peer.remove(&peer_id);
                }
            }
        }
        let _ = stream.shutdown(Shutdown::Both);
    }
    emit_sessions(&app);
    Ok(())
}

fn handle_incoming_text<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    id: String,
    body: String,
    ts: i64,
) {
    if let Err(e) = validate_text_body(&body) {
        diagnostics::warn(
            app,
            LogicPoint::MsgRecvReject,
            format!("reject inbound text peer={peer_id}: {e}"),
        );
        return;
    }
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let created = if ts > 0 { ts } else { now_ms() };
    let msg = ChatMessage {
        id: id.clone(),
        peer_id: peer_id.to_string(),
        direction: "in".into(),
        msg_type: "text".into(),
        body,
        created_at: created,
        status: "received".into(),
    };
    match state.db.insert_message(&msg) {
        Ok(true) => {
            diagnostics::info(
                app,
                LogicPoint::MsgRecv,
                format!("recv text id={id} peer={peer_id} chars={}", msg.body.chars().count()),
            );
            crate::sound::play(app, crate::sound::SoundKind::Message);
            let _ = app.emit("message", &msg);
        }
        Ok(false) => {
            diagnostics::info(
                app,
                LogicPoint::MsgRecvDup,
                format!("duplicate inbound id={id} peer={peer_id}"),
            );
        }
        Err(e) => {
            diagnostics::error(
                app,
                LogicPoint::MsgPersistFail,
                format!("persist inbound id={id}: {e}"),
            );
        }
    }
}

pub fn send_text_to_peer<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    body: &str,
) -> Result<ChatMessage, String> {
    // Group chats use a dedicated API (mesh + no files).
    if group::is_group_peer_id(peer_id) {
        let gid = group::parse_group_peer_id(peer_id).unwrap_or(peer_id);
        return group::send_group_text(app, gid, body);
    }
    validate_text_body(body)?;
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state missing".to_string())?;

    let id = Uuid::new_v4().to_string();
    let created = now_ms();
    let mut msg = ChatMessage {
        id: id.clone(),
        peer_id: peer_id.to_string(),
        direction: "out".into(),
        msg_type: "text".into(),
        body: body.to_string(),
        created_at: created,
        status: "pending".into(),
    };

    // Persist before send so history survives crash (design).
    state.db.insert_message(&msg)?;
    let _ = app.emit("message", &msg);

    let session = {
        let sessions = state
            .sessions
            .lock()
            .map_err(|_| "sessions lock poisoned".to_string())?;
        sessions.get(peer_id)
    };

    let Some(session) = session else {
        msg.status = "failed".into();
        state.db.update_status(&id, "failed")?;
        let _ = app.emit("message", &msg);
        let err = "Not connected to peer. Wait until they are online.".to_string();
        diagnostics::warn(
            app,
            LogicPoint::MsgSendFail,
            format!("send fail no session peer={peer_id} id={id}"),
        );
        return Err(err);
    };

    let wire = WireMessage::text(id.clone(), body.to_string(), created).to_bytes()?;
    let send_result = {
        let mut w = session
            .writer
            .lock()
            .map_err(|_| "writer lock poisoned".to_string())?;
        write_frame(&mut *w, &wire)
    };

    match send_result {
        Ok(()) => {
            msg.status = "sent".into();
            state.db.update_status(&id, "sent")?;
            diagnostics::info(
                app,
                LogicPoint::MsgSend,
                format!(
                    "sent text id={id} peer={peer_id} chars={}",
                    body.chars().count()
                ),
            );
            let _ = app.emit("message", &msg);
            Ok(msg)
        }
        Err(e) => {
            msg.status = "failed".into();
            state.db.update_status(&id, "failed")?;
            let _ = app.emit("message", &msg);
            let err = format!("Send failed: {e}");
            diagnostics::error(
                app,
                LogicPoint::MsgSendFail,
                format!("send wire fail peer={peer_id} id={id}: {e}"),
            );
            Err(err)
        }
    }
}

pub fn list_session_peers<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<SessionInfo>, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state missing".to_string())?;
    let sessions = state
        .sessions
        .lock()
        .map_err(|_| "sessions lock poisoned".to_string())?;
    Ok(sessions.list())
}

/// Send an already-built control-plane frame to a connected peer.
pub fn send_wire_to_peer<R: Runtime>(
    app: &AppHandle<R>,
    peer_id: &str,
    msg: &WireMessage,
) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state missing".to_string())?;
    let session = {
        let sessions = state
            .sessions
            .lock()
            .map_err(|_| "sessions lock poisoned".to_string())?;
        sessions.get(peer_id)
    }
    .ok_or_else(|| "Not connected to peer.".to_string())?;
    let bytes = msg.to_bytes()?;
    let mut w = session
        .writer
        .lock()
        .map_err(|_| "writer lock poisoned".to_string())?;
    write_frame(&mut *w, &bytes)
}

fn emit_sessions<R: Runtime>(app: &AppHandle<R>) {
    if let Ok(list) = list_session_peers(app) {
        let _ = app.emit("sessions-updated", list);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_dialer_is_lexicographically_lower() {
        assert!(is_preferred_dialer("aaa", "bbb"));
        assert!(!is_preferred_dialer("bbb", "aaa"));
    }

    #[test]
    fn correct_side_matches_preferred_role() {
        // Lower id should dial.
        assert!(is_correct_connection_side(true, "aaa", "bbb"));
        assert!(!is_correct_connection_side(false, "aaa", "bbb"));
        // Higher id should only accept.
        assert!(is_correct_connection_side(false, "bbb", "aaa"));
        assert!(!is_correct_connection_side(true, "bbb", "aaa"));
    }

    #[test]
    fn session_should_drop_when_peer_offline_or_ip_changes() {
        // Pure policy check used by reconcile_sessions_with_peers.
        fn should_drop(online: bool, peer_addr: &str, session_addr: &str) -> bool {
            !(online && peer_addr == session_addr)
        }
        assert!(!should_drop(true, "10.0.0.2", "10.0.0.2"));
        assert!(should_drop(false, "10.0.0.2", "10.0.0.2")); // offline after sleep TTL
        assert!(should_drop(true, "10.0.0.9", "10.0.0.2")); // DHCP/roam
        assert!(should_drop(false, "10.0.0.9", "10.0.0.2"));
    }
}
