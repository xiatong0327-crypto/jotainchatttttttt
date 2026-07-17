//! LAN group chat (text only).
//!
//! - Create group → 6-char join code (share out-of-band).
//! - Join: enter code → request flooded to connected peers → member with matching
//!   group verifies code and returns roster.
//! - Leave: notify members, drop local membership (history kept under peer_id `g:{id}`).
//! - **No file transfer** in groups (UI + API reject).

use crate::db::ChatMessage;
use crate::diagnostics::{self, LogicPoint};
use crate::net::protocol::{validate_text_body, GroupMemberWire, WireMessage};
use crate::net::session;
use crate::state::AppState;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use uuid::Uuid;

pub const GROUP_PEER_PREFIX: &str = "g:";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupInfo {
    pub id: String,
    pub name: String,
    pub join_code: String,
    pub creator_id: String,
    pub members: Vec<GroupMemberWire>,
    /// Still a member (false after leave; history may remain).
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupTextBody {
    pub from_device_id: String,
    pub from_name: String,
    pub text: String,
}

pub struct GroupRegistry {
    /// Active groups keyed by id.
    by_id: Mutex<HashMap<String, GroupInfo>>,
}

impl GroupRegistry {
    pub fn new() -> Self {
        Self {
            by_id: Mutex::new(HashMap::new()),
        }
    }

    pub fn list(&self) -> Vec<GroupInfo> {
        let map = self.by_id.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<_> = map.values().cloned().collect();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        v
    }

    pub fn get(&self, id: &str) -> Option<GroupInfo> {
        self.by_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
    }

    pub fn upsert(&self, g: GroupInfo) {
        if let Ok(mut map) = self.by_id.lock() {
            map.insert(g.id.clone(), g);
        }
    }

    pub fn remove_active(&self, id: &str) {
        if let Ok(mut map) = self.by_id.lock() {
            if let Some(g) = map.get_mut(id) {
                g.active = false;
            }
        }
    }

    pub fn find_by_join_code(&self, code: &str) -> Option<GroupInfo> {
        let code = normalize_code(code);
        let map = self.by_id.lock().unwrap_or_else(|e| e.into_inner());
        map.values()
            .find(|g| g.active && normalize_code(&g.join_code) == code)
            .cloned()
    }
}

pub fn peer_key(group_id: &str) -> String {
    format!("{GROUP_PEER_PREFIX}{group_id}")
}

pub fn parse_group_peer_id(peer_id: &str) -> Option<&str> {
    peer_id.strip_prefix(GROUP_PEER_PREFIX)
}

pub fn is_group_peer_id(peer_id: &str) -> bool {
    peer_id.starts_with(GROUP_PEER_PREFIX)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn normalize_code(code: &str) -> String {
    code.trim().to_uppercase().replace([' ', '-'], "")
}

fn generate_join_code() -> String {
    // Avoid ambiguous 0/O, 1/I/L
    const CHARS: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..6)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

fn self_identity<R: Runtime>(app: &AppHandle<R>) -> Result<(String, String), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;
    let cfg = state
        .config
        .lock()
        .map_err(|_| "config lock".to_string())?;
    let name = if cfg.display_name.trim().is_empty() {
        crate::config::suggested_display_name()
    } else {
        cfg.display_name.clone()
    };
    Ok((cfg.device_id.clone(), name))
}

fn emit_groups<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AppState>() {
        let list = state.groups.list();
        let _ = app.emit("groups-updated", &list);
    }
}

fn persist_group<R: Runtime>(app: &AppHandle<R>, g: &GroupInfo) {
    if let Some(state) = app.try_state::<AppState>() {
        let _ = state.db.upsert_group(g);
        state.groups.upsert(g.clone());
        emit_groups(app);
    }
}

/// Load groups from SQLite into registry (call at startup).
pub fn hydrate_groups<R: Runtime>(app: &AppHandle<R>) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    match state.db.list_groups() {
        Ok(list) => {
            for g in list {
                state.groups.upsert(g);
            }
            emit_groups(app);
            diagnostics::info(
                app,
                LogicPoint::AppStart,
                format!("hydrated {} groups", state.groups.list().len()),
            );
        }
        Err(e) => {
            diagnostics::warn(app, LogicPoint::DbQueryFail, format!("hydrate groups: {e}"));
        }
    }
}

pub fn create_group<R: Runtime>(app: &AppHandle<R>, name: &str) -> Result<GroupInfo, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Group name cannot be empty.".into());
    }
    if name.chars().count() > 40 {
        return Err("Group name too long (max 40 characters).".into());
    }
    let (self_id, self_name) = self_identity(app)?;
    let id = Uuid::new_v4().to_string();
    let join_code = generate_join_code();
    let g = GroupInfo {
        id: id.clone(),
        name: name.to_string(),
        join_code: join_code.clone(),
        creator_id: self_id.clone(),
        members: vec![GroupMemberWire {
            device_id: self_id,
            display_name: self_name,
        }],
        active: true,
    };
    persist_group(app, &g);
    diagnostics::info(
        app,
        LogicPoint::MsgSend,
        format!("group created id={id} code={join_code}"),
    );
    Ok(g)
}

/// Flood join request to all connected peers. A member with matching code replies.
pub fn join_group<R: Runtime>(app: &AppHandle<R>, join_code: &str) -> Result<GroupInfo, String> {
    let code = normalize_code(join_code);
    if code.len() < 4 || code.len() > 12 {
        return Err("Join code should be 6 characters (letters/numbers).".into());
    }
    // Already in a group with this code?
    if let Some(state) = app.try_state::<AppState>() {
        if let Some(g) = state.groups.find_by_join_code(&code) {
            if g.active {
                return Ok(g);
            }
        }
    }

    let (self_id, self_name) = self_identity(app)?;
    // Temporary group id unknown — use empty and let responders fill; we use a probe id
    let probe_id = Uuid::new_v4().to_string();
    let wire = WireMessage::GroupJoinRequest {
        group_id: probe_id.clone(),
        join_code: code.clone(),
        device_id: self_id,
        display_name: self_name,
    };

    let peers = session::list_session_peers(app)?;
    if peers.is_empty() {
        return Err(
            "No connected peers. Connect to at least one group member on the LAN first.".into(),
        );
    }

    let mut sent = 0usize;
    for p in &peers {
        if session::send_wire_to_peer(app, &p.peer_id, &wire).is_ok() {
            sent += 1;
        }
    }
    if sent == 0 {
        return Err("Could not reach any peer. Wait until someone is connected (green).".into());
    }

    diagnostics::info(
        app,
        LogicPoint::MsgSend,
        format!("group join request code={code} sent_to={sent}"),
    );

    // Async: JoinOk handler will persist. Poll briefly for result.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        if let Some(state) = app.try_state::<AppState>() {
            if let Some(g) = state.groups.find_by_join_code(&code) {
                if g.active {
                    return Ok(g);
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(120));
    }
    Err(
        "No group accepted the code. Check the code, or ensure a group member is online and connected."
            .into(),
    )
}

pub fn leave_group<R: Runtime>(app: &AppHandle<R>, group_id: &str) -> Result<(), String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;
    let mut g = state
        .groups
        .get(group_id)
        .ok_or_else(|| "Group not found.".to_string())?;
    if !g.active {
        return Ok(());
    }
    let (self_id, _) = self_identity(app)?;

    let leave = WireMessage::GroupLeave {
        group_id: group_id.to_string(),
        device_id: self_id.clone(),
    };
    for m in &g.members {
        if m.device_id != self_id {
            let _ = session::send_wire_to_peer(app, &m.device_id, &leave);
        }
    }

    g.members.retain(|m| m.device_id != self_id);
    g.active = false;
    let _ = state.db.mark_group_left(group_id);
    state.groups.remove_active(group_id);
    // Keep inactive snapshot for history listing title
    state.groups.upsert(g);
    emit_groups(app);
    diagnostics::info(
        app,
        LogicPoint::MsgSend,
        format!("left group id={group_id}"),
    );
    Ok(())
}

pub fn list_groups<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<GroupInfo>, String> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;
    Ok(state.groups.list().into_iter().filter(|g| g.active).collect())
}

pub fn send_group_text<R: Runtime>(
    app: &AppHandle<R>,
    group_id: &str,
    body: &str,
) -> Result<ChatMessage, String> {
    validate_text_body(body)?;
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| "no state".to_string())?;
    let g = state
        .groups
        .get(group_id)
        .filter(|g| g.active)
        .ok_or_else(|| "You are not in this group.".to_string())?;
    let (self_id, self_name) = self_identity(app)?;
    if !g.members.iter().any(|m| m.device_id == self_id) {
        return Err("You are not a member of this group.".into());
    }

    let id = Uuid::new_v4().to_string();
    let ts = now_ms();
    let payload = GroupTextBody {
        from_device_id: self_id.clone(),
        from_name: self_name.clone(),
        text: body.to_string(),
    };
    let body_json = serde_json::to_string(&payload).map_err(|e| e.to_string())?;
    let peer_key = peer_key(group_id);
    let mut msg = ChatMessage {
        id: id.clone(),
        peer_id: peer_key,
        direction: "out".into(),
        msg_type: "gtext".into(),
        body: body_json,
        created_at: ts,
        status: "pending".into(),
    };
    state.db.insert_message(&msg)?;
    let _ = app.emit("message", &msg);

    let wire = WireMessage::GroupText {
        group_id: group_id.to_string(),
        id: id.clone(),
        from_device_id: self_id.clone(),
        from_name: self_name,
        body: body.to_string(),
        ts,
    };

    let mut ok_any = false;
    for m in &g.members {
        if m.device_id == self_id {
            continue;
        }
        if session::send_wire_to_peer(app, &m.device_id, &wire).is_ok() {
            ok_any = true;
        }
    }

    if ok_any || g.members.len() <= 1 {
        msg.status = "sent".into();
        state.db.update_status(&id, "sent")?;
    } else {
        msg.status = "failed".into();
        state.db.update_status(&id, "failed")?;
        let _ = app.emit("message", &msg);
        return Err(
            "No group members reachable. They must be connected (green) on the LAN.".into(),
        );
    }
    let _ = app.emit("message", &msg);
    Ok(msg)
}

// --- Wire handlers ---

pub fn on_group_join_request<R: Runtime>(
    app: &AppHandle<R>,
    from_peer: &str,
    group_id_probe: String,
    join_code: String,
    device_id: String,
    display_name: String,
) {
    let code = normalize_code(&join_code);
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // Silent ignore if we don't know this code — another peer may own the group.
    let _ = group_id_probe;
    let Some(mut g) = state.groups.find_by_join_code(&code) else {
        return;
    };
    if !g.active {
        let _ = session::send_wire_to_peer(
            app,
            from_peer,
            &WireMessage::GroupJoinReject {
                group_id: g.id.clone(),
                reason: "group_inactive".into(),
            },
        );
        return;
    }

    // Code matches this group — ignore probe id from joiner.
    if !g.members.iter().any(|m| m.device_id == device_id) {
        g.members.push(GroupMemberWire {
            device_id: device_id.clone(),
            display_name: display_name.clone(),
        });
    } else {
        // Update name
        for m in &mut g.members {
            if m.device_id == device_id {
                m.display_name = display_name.clone();
            }
        }
    }
    persist_group(app, &g);

    let ok = WireMessage::GroupJoinOk {
        group_id: g.id.clone(),
        name: g.name.clone(),
        join_code: g.join_code.clone(),
        creator_id: g.creator_id.clone(),
        members: g.members.clone(),
    };
    let _ = session::send_wire_to_peer(app, from_peer, &ok);

    // Fan-out roster to other members
    let update = WireMessage::GroupMemberUpdate {
        group_id: g.id.clone(),
        name: g.name.clone(),
        join_code: g.join_code.clone(),
        creator_id: g.creator_id.clone(),
        members: g.members.clone(),
    };
    let (self_id, _) = self_identity(app).unwrap_or_default();
    for m in &g.members {
        if m.device_id != self_id && m.device_id != device_id {
            let _ = session::send_wire_to_peer(app, &m.device_id, &update);
        }
    }

    diagnostics::info(
        app,
        LogicPoint::MsgSend,
        format!("group join accepted group={} device={}", g.id, device_id),
    );
}

pub fn on_group_join_ok<R: Runtime>(
    app: &AppHandle<R>,
    group_id: String,
    name: String,
    join_code: String,
    creator_id: String,
    members: Vec<GroupMemberWire>,
) {
    let g = GroupInfo {
        id: group_id.clone(),
        name,
        join_code,
        creator_id,
        members,
        active: true,
    };
    persist_group(app, &g);
    diagnostics::info(
        app,
        LogicPoint::MsgSend,
        format!("joined group id={group_id}"),
    );
}

pub fn on_group_join_reject<R: Runtime>(app: &AppHandle<R>, group_id: String, reason: String) {
    diagnostics::warn(
        app,
        LogicPoint::MsgSendFail,
        format!("group join reject id={group_id} reason={reason}"),
    );
}

pub fn on_group_member_update<R: Runtime>(
    app: &AppHandle<R>,
    group_id: String,
    name: String,
    join_code: String,
    creator_id: String,
    members: Vec<GroupMemberWire>,
) {
    let (self_id, _) = match self_identity(app) {
        Ok(v) => v,
        Err(_) => return,
    };
    let still_in = members.iter().any(|m| m.device_id == self_id);
    let g = GroupInfo {
        id: group_id,
        name,
        join_code,
        creator_id,
        members,
        active: still_in,
    };
    if still_in {
        persist_group(app, &g);
    } else if let Some(state) = app.try_state::<AppState>() {
        let _ = state.db.mark_group_left(&g.id);
        state.groups.upsert(g);
        emit_groups(app);
    }
}

pub fn on_group_leave<R: Runtime>(app: &AppHandle<R>, group_id: String, device_id: String) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    let Some(mut g) = state.groups.get(&group_id) else {
        return;
    };
    g.members.retain(|m| m.device_id != device_id);
    persist_group(app, &g);
}

pub fn on_group_text<R: Runtime>(
    app: &AppHandle<R>,
    _from_peer: &str,
    group_id: String,
    id: String,
    from_device_id: String,
    from_name: String,
    body: String,
    ts: i64,
) {
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    // Only accept if we are in the group
    let Some(g) = state.groups.get(&group_id) else {
        return;
    };
    if !g.active {
        return;
    }
    let (self_id, _) = self_identity(app).unwrap_or_default();
    if from_device_id == self_id {
        return;
    }
    if !g.members.iter().any(|m| m.device_id == self_id) {
        return;
    }

    let payload = GroupTextBody {
        from_device_id,
        from_name,
        text: body,
    };
    let body_json = serde_json::to_string(&payload).unwrap_or_default();
    let created = if ts > 0 { ts } else { now_ms() };
    let msg = ChatMessage {
        id,
        peer_id: peer_key(&group_id),
        direction: "in".into(),
        msg_type: "gtext".into(),
        body: body_json,
        created_at: created,
        status: "received".into(),
    };
    match state.db.insert_message(&msg) {
        Ok(true) => {
            let _ = app.emit("message", &msg);
            crate::sound::play(app, crate::sound::SoundKind::Message);
        }
        Ok(false) => {}
        Err(e) => diagnostics::error(app, LogicPoint::MsgPersistFail, e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_key_roundtrip() {
        let id = "abc-123";
        let k = peer_key(id);
        assert!(is_group_peer_id(&k));
        assert_eq!(parse_group_peer_id(&k), Some(id));
    }

    #[test]
    fn join_code_normalize() {
        assert_eq!(normalize_code(" ab-cd "), "ABCD");
    }
}
