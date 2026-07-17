//! UDP LAN peer discovery.
//!
//! Broadcasts an announce beacon and listens for peers on the same network.
//! `device_id` is the stable identity; IP addresses are treated as ephemeral.

use crate::config;
use crate::diagnostics::{self, LogicPoint};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, Runtime};

/// Fixed discovery port (documented for firewall / Local Network).
pub const DISCOVERY_PORT: u16 = 48765;
/// TCP control plane (chat + file signaling).
pub const CONTROL_PORT: u16 = 48766;
/// TCP data plane (file bytes).
pub const DATA_PORT: u16 = 48767;
pub const PROTOCOL_VERSION: u32 = 1;

const ANNOUNCE_INTERVAL: Duration = Duration::from_millis(1500);
const PEER_ONLINE_TTL: Duration = Duration::from_secs(6);
const PEER_RETAIN_TTL: Duration = Duration::from_secs(30);
const RECV_TIMEOUT: Duration = Duration::from_millis(400);
const MAX_PACKET: usize = 2048;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AnnouncePacket {
    pub v: u32,
    #[serde(rename = "type")]
    pub kind: String,
    pub device_id: String,
    pub display_name: String,
    pub os: String,
    pub control_port: u16,
}

impl AnnouncePacket {
    pub fn new(device_id: String, display_name: String) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            kind: "announce".into(),
            device_id,
            display_name,
            os: std::env::consts::OS.into(),
            control_port: CONTROL_PORT,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerInfo {
    pub device_id: String,
    pub display_name: String,
    pub os: String,
    pub control_port: u16,
    pub address: String,
    pub online: bool,
    /// Milliseconds since epoch for UI; last time we heard an announce.
    pub last_seen_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryStatus {
    pub running: bool,
    pub port: u16,
    pub control_port: u16,
    pub protocol_version: u32,
    pub last_error: Option<String>,
    pub peer_count: usize,
    pub hint: String,
}

struct PeerEntry {
    info: PeerInfo,
    last_seen: Instant,
}

pub struct PeerTable {
    peers: HashMap<String, PeerEntry>,
}

impl PeerTable {
    pub fn new() -> Self {
        Self {
            peers: HashMap::new(),
        }
    }

    pub fn upsert_from_announce(
        &mut self,
        packet: &AnnouncePacket,
        from: SocketAddr,
        now: Instant,
    ) {
        let address = match from {
            SocketAddr::V4(v4) => v4.ip().to_string(),
            SocketAddr::V6(v6) => v6.ip().to_string(),
        };
        let last_seen_ms = wall_ms();
        self.peers.insert(
            packet.device_id.clone(),
            PeerEntry {
                info: PeerInfo {
                    device_id: packet.device_id.clone(),
                    display_name: packet.display_name.clone(),
                    os: packet.os.clone(),
                    control_port: packet.control_port,
                    address,
                    online: true,
                    last_seen_ms,
                },
                last_seen: now,
            },
        );
    }

    /// Refresh online/offline flags; drop peers retained too long while offline.
    /// Returns (changed, expired_ids_for_logging).
    pub fn refresh(&mut self, now: Instant) -> (bool, Vec<String>) {
        let mut changed = false;
        let mut drop_ids = Vec::new();
        let mut expired_log = Vec::new();

        for (id, entry) in self.peers.iter_mut() {
            let age = now.saturating_duration_since(entry.last_seen);
            let should_online = age <= PEER_ONLINE_TTL;
            if entry.info.online != should_online {
                entry.info.online = should_online;
                changed = true;
                if !should_online {
                    expired_log.push(format!("{id}@{}", entry.info.address));
                }
            }
            if age > PEER_RETAIN_TTL {
                drop_ids.push(id.clone());
            }
        }

        for id in drop_ids {
            self.peers.remove(&id);
            changed = true;
        }
        (changed, expired_log)
    }

    pub fn list(&self) -> Vec<PeerInfo> {
        let mut list: Vec<PeerInfo> = self.peers.values().map(|e| e.info.clone()).collect();
        list.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                })
                .then_with(|| a.device_id.cmp(&b.device_id))
        });
        list
    }

    pub fn online_count(&self) -> usize {
        self.peers.values().filter(|e| e.info.online).count()
    }

    pub fn contains(&self, device_id: &str) -> bool {
        self.peers.contains_key(device_id)
    }
}

pub struct DiscoveryState {
    pub status: Mutex<DiscoveryStatus>,
    pub stop: AtomicBool,
}

impl DiscoveryState {
    pub fn new() -> Self {
        Self {
            status: Mutex::new(DiscoveryStatus {
                running: false,
                port: DISCOVERY_PORT,
                control_port: CONTROL_PORT,
                protocol_version: PROTOCOL_VERSION,
                last_error: None,
                peer_count: 0,
                hint: default_hint(),
            }),
            stop: AtomicBool::new(false),
        }
    }
}

fn default_hint() -> String {
    format!(
        "Peers appear when another device runs jotainchatttttttt on the same Wi‑Fi \
         (Mac or Windows). On macOS allow Local Network if prompted; on Windows allow \
         Private network / firewall for this app. \
         Ports: UDP {DISCOVERY_PORT}, TCP {CONTROL_PORT}/{DATA_PORT}."
    )
}

fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Parse and validate an announce packet. Returns None if ignore.
pub fn parse_announce(bytes: &[u8], self_id: &str) -> Option<AnnouncePacket> {
    let text = std::str::from_utf8(bytes).ok()?;
    let packet: AnnouncePacket = serde_json::from_str(text).ok()?;
    if packet.kind != "announce" {
        return None;
    }
    if packet.v != PROTOCOL_VERSION {
        return None;
    }
    if packet.device_id.trim().is_empty() {
        return None;
    }
    // Self-filter
    if packet.device_id == self_id {
        return None;
    }
    Some(packet)
}

pub fn start_discovery_thread<R: Runtime>(app: AppHandle<R>) {
    std::thread::Builder::new()
        .name("jotain-discovery".into())
        .spawn(move || discovery_loop(app))
        .expect("spawn discovery thread");
}

fn discovery_loop<R: Runtime>(app: AppHandle<R>) {
    let socket = match bind_socket() {
        Ok(s) => {
            diagnostics::info(
                &app,
                LogicPoint::DiscBind,
                format!("UDP discovery bound 0.0.0.0:{DISCOVERY_PORT}"),
            );
            s
        }
        Err(e) => {
            let msg = format!("UDP bind failed on port {DISCOVERY_PORT}: {e}");
            diagnostics::error(&app, LogicPoint::DiscBindFail, &msg);
            set_error(&app, msg);
            return;
        }
    };

    {
        if let Some(state) = app.try_state::<AppState>() {
            if let Ok(mut st) = state.discovery.status.lock() {
                st.running = true;
                st.last_error = None;
            }
        }
        let _ = app.emit(
            "discovery-status",
            status_snapshot(&app).unwrap_or_else(|_| DiscoveryStatus {
                running: true,
                port: DISCOVERY_PORT,
                control_port: CONTROL_PORT,
                protocol_version: PROTOCOL_VERSION,
                last_error: None,
                peer_count: 0,
                hint: default_hint(),
            }),
        );
    }

    let mut last_announce = Instant::now()
        .checked_sub(ANNOUNCE_INTERVAL)
        .unwrap_or_else(Instant::now);
    let mut buf = [0u8; MAX_PACKET];

    loop {
        let stop = app
            .try_state::<AppState>()
            .map(|s| s.discovery.stop.load(Ordering::Relaxed))
            .unwrap_or(true); // Option map is fine
        if stop {
            diagnostics::info(&app, LogicPoint::DiscStop, "discovery stop flag set");
            break;
        }

        // Receive with short timeout so we can announce periodically.
        match socket.recv_from(&mut buf) {
            Ok((n, from)) => {
                handle_packet(&app, &buf[..n], from);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                let msg = format!("UDP recv error: {e}");
                diagnostics::error(&app, LogicPoint::DiscRecvFail, &msg);
                set_error(&app, msg);
                std::thread::sleep(Duration::from_millis(500));
            }
        }

        let now = Instant::now();
        if now.duration_since(last_announce) >= ANNOUNCE_INTERVAL {
            last_announce = now;
            if let Err(e) = send_announce(&app, &socket) {
                let msg = format!("UDP announce error: {e}");
                diagnostics::error(&app, LogicPoint::DiscAnnounceFail, &msg);
                set_error(&app, msg);
            }
            refresh_and_emit(&app);
        }
    }

    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut st) = state.discovery.status.lock() {
            st.running = false;
        }
    }
    diagnostics::warn(&app, LogicPoint::DiscStop, "discovery loop exited");
}

fn bind_socket() -> std::io::Result<UdpSocket> {
    let socket = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, DISCOVERY_PORT)))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(RECV_TIMEOUT))?;
    // Best-effort reuse if OS supports it (helps recover after crash).
    let _ = socket.set_reuse_address(true);
    Ok(socket)
}

fn send_announce<R: Runtime>(app: &AppHandle<R>, socket: &UdpSocket) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state missing".to_string())?; // Option
    let cfg = state
        .config
        .lock()
        .map_err(|_| "config lock poisoned".to_string())?;

    let display = if cfg.display_name.trim().is_empty() {
        // Avoid advertising empty name pre-onboarding.
        config::suggested_display_name()
    } else {
        cfg.display_name.clone()
    };

    let packet = AnnouncePacket::new(cfg.device_id.clone(), display);
    let payload =
        serde_json::to_vec(&packet).map_err(|e| format!("serialize announce: {e}"))?;

    // Global broadcast — works on most home Wi‑Fi LANs.
    let dest = SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT));
    socket
        .send_to(&payload, dest)
        .map_err(|e| format!("send broadcast: {e}"))?;

    // Solo / long-run: clear sticky errors after a successful announce so a
    // transient network blip (common around sleep/wake) does not linger forever.
    if let Ok(mut st) = state.discovery.status.lock() {
        if st.last_error.is_some() {
            st.last_error = None;
            st.hint = default_hint();
        }
    }

    Ok(())
}

fn handle_packet<R: Runtime>(app: &AppHandle<R>, bytes: &[u8], from: SocketAddr) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let self_id = {
        let Ok(cfg) = state.config.lock() else {
            return;
        };
        cfg.device_id.clone()
    };

    let Some(packet) = parse_announce(bytes, &self_id) else {
        return;
    };

    let mut changed = false;
    if let Ok(mut table) = state.peers.lock() {
        let is_new = !table.contains(&packet.device_id);
        table.upsert_from_announce(&packet, from, Instant::now());
        changed = true;
        let (_c, _exp) = table.refresh(Instant::now());
        if let Ok(mut st) = state.discovery.status.lock() {
            st.peer_count = table.online_count();
            st.last_error = None;
        }
        if is_new {
            diagnostics::info(
                app,
                LogicPoint::DiscPeerSeen,
                format!(
                    "new peer name={:?} id={} addr={} ctrl={}",
                    packet.display_name, packet.device_id, from.ip(), packet.control_port
                ),
            );
        }
    }

    if changed {
        emit_peers(app);
    }
}

fn refresh_and_emit<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let mut changed = false;
    if let Ok(mut table) = state.peers.lock() {
        let (c, expired) = table.refresh(Instant::now());
        changed = c;
        if !expired.is_empty() {
            diagnostics::info(
                app,
                LogicPoint::DiscPeerExpire,
                format!("peers went offline: {}", expired.join(", ")),
            );
        }
        if let Ok(mut st) = state.discovery.status.lock() {
            st.peer_count = table.online_count();
        }
    }
    if changed {
        emit_peers(app);
    }
    // Always refresh discovery-status peer_count for UI timer consumers.
    if let Ok(status) = status_snapshot(app) {
        let _ = app.emit("discovery-status", status);
    }
}

fn emit_peers<R: Runtime>(app: &AppHandle<R>) {
    if let Ok(list) = list_peers_snapshot(app) {
        let _ = app.emit("peers-updated", list);
    }
}

fn set_error<R: Runtime>(app: &AppHandle<R>, msg: String) {
    if let Some(state) = app.try_state::<AppState>() {
        if let Ok(mut st) = state.discovery.status.lock() {
            st.last_error = Some(msg.clone());
            st.hint = format!(
                "{}. If the list stays empty: same Wi‑Fi, disable AP/client isolation, allow Local Network for jotainchatttttttt.",
                msg
            );
        }
    }
    if let Ok(status) = status_snapshot(app) {
        let _ = app.emit("discovery-status", status);
    }
}

pub fn list_peers_snapshot<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<PeerInfo>, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state missing".to_string())?;
    let table = state
        .peers
        .lock()
        .map_err(|_| "peers lock poisoned".to_string())?;
    Ok(table.list())
}

pub fn status_snapshot<R: Runtime>(app: &AppHandle<R>) -> Result<DiscoveryStatus, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "app state missing".to_string())?;
    let st = state
        .discovery
        .status
        .lock()
        .map_err(|_| "discovery status lock poisoned".to_string())?;
    Ok(st.clone())
}

// --- UdpSocket reuse helper (std has no set_reuse_address on all platforms the same way) ---
trait UdpReuse {
    fn set_reuse_address(&self, on: bool) -> std::io::Result<()>;
}

#[cfg(unix)]
impl UdpReuse for UdpSocket {
    fn set_reuse_address(&self, on: bool) -> std::io::Result<()> {
        use std::os::unix::io::AsRawFd;
        let fd = self.as_raw_fd();
        let val: libc::c_int = if on { 1 } else { 0 };
        let ret = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                &val as *const _ as *const libc::c_void,
                std::mem::size_of_val(&val) as libc::socklen_t,
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

#[cfg(not(unix))]
impl UdpReuse for UdpSocket {
    fn set_reuse_address(&self, _on: bool) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddrV4;

    #[test]
    fn parse_announce_filters_self_and_bad() {
        let self_id = "aaa";
        let good = AnnouncePacket::new("bbb".into(), "Bob".into());
        let bytes = serde_json::to_vec(&good).unwrap();
        let parsed = parse_announce(&bytes, self_id).unwrap();
        assert_eq!(parsed.device_id, "bbb");

        let me = AnnouncePacket::new(self_id.into(), "Me".into());
        let bytes = serde_json::to_vec(&me).unwrap();
        assert!(parse_announce(&bytes, self_id).is_none());

        assert!(parse_announce(b"not-json", self_id).is_none());
    }

    #[test]
    fn parse_rejects_wrong_version() {
        let mut p = AnnouncePacket::new("x".into(), "X".into());
        p.v = 99;
        let bytes = serde_json::to_vec(&p).unwrap();
        assert!(parse_announce(&bytes, "other").is_none());
    }

    #[test]
    fn peer_table_ttl_and_sort() {
        let mut table = PeerTable::new();
        let now = Instant::now();
        let pkt = AnnouncePacket::new("id-1".into(), "Zed".into());
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 5), DISCOVERY_PORT));
        table.upsert_from_announce(&pkt, addr, now);

        let pkt2 = AnnouncePacket::new("id-2".into(), "Ann".into());
        let addr2 = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 6), DISCOVERY_PORT));
        table.upsert_from_announce(&pkt2, addr2, now);

        let list = table.list();
        assert_eq!(list[0].display_name, "Ann");
        assert_eq!(list[1].display_name, "Zed");

        // Expire online
        let later = now + PEER_ONLINE_TTL + Duration::from_secs(1);
        let (c1, exp) = table.refresh(later);
        assert!(c1);
        assert!(!exp.is_empty());
        assert!(!table.list()[0].online);

        // Drop after retain
        let much_later = now + PEER_RETAIN_TTL + Duration::from_secs(1);
        let (c2, _) = table.refresh(much_later);
        assert!(c2);
        assert!(table.list().is_empty());
    }
}
