//! Local SQLite message history (durable; survives uninstall of the .app).

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DB_FILE_NAME: &str = "messages.db";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub peer_id: String,
    pub direction: String, // "in" | "out"
    pub msg_type: String,  // "text"
    pub body: String,
    pub created_at: i64,
    pub status: String, // pending | sent | failed | received
}

pub struct Database {
    conn: Mutex<Connection>,
}

impl Database {
    pub fn open(app_data_dir: &Path) -> Result<Self, String> {
        let path = db_path(app_data_dir);
        let conn = Connection::open(&path).map_err(|e| format!("open db {}: {e}", path.display()))?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS messages (
              id TEXT PRIMARY KEY NOT NULL,
              peer_id TEXT NOT NULL,
              direction TEXT NOT NULL,
              msg_type TEXT NOT NULL,
              body TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              status TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_peer_created
              ON messages(peer_id, created_at);
            CREATE TABLE IF NOT EXISTS transfers (
              file_id TEXT PRIMARY KEY NOT NULL,
              role TEXT NOT NULL,
              peer_id TEXT NOT NULL,
              message_id TEXT NOT NULL,
              path TEXT NOT NULL,
              partial_path TEXT,
              name TEXT NOT NULL,
              size INTEGER NOT NULL,
              mime TEXT NOT NULL DEFAULT '',
              token TEXT NOT NULL,
              bytes_done INTEGER NOT NULL DEFAULT 0,
              state TEXT NOT NULL,
              source_mtime INTEGER,
              error TEXT,
              created_at INTEGER NOT NULL,
              updated_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_transfers_peer_state
              ON transfers(peer_id, state);
            CREATE INDEX IF NOT EXISTS idx_transfers_message
              ON transfers(message_id);
            CREATE TABLE IF NOT EXISTS chat_groups (
              id TEXT PRIMARY KEY NOT NULL,
              name TEXT NOT NULL,
              join_code TEXT NOT NULL,
              creator_id TEXT NOT NULL,
              created_at INTEGER NOT NULL,
              active INTEGER NOT NULL DEFAULT 1
            );
            CREATE TABLE IF NOT EXISTS chat_group_members (
              group_id TEXT NOT NULL,
              device_id TEXT NOT NULL,
              display_name TEXT NOT NULL,
              PRIMARY KEY (group_id, device_id)
            );
            CREATE INDEX IF NOT EXISTS idx_chat_groups_code ON chat_groups(join_code);
            ",
        )
        .map_err(|e| format!("migrate db: {e}"))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn insert_message(&self, msg: &ChatMessage) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO messages
                 (id, peer_id, direction, msg_type, body, created_at, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    msg.id,
                    msg.peer_id,
                    msg.direction,
                    msg.msg_type,
                    msg.body,
                    msg.created_at,
                    msg.status
                ],
            )
            .map_err(|e| format!("insert message: {e}"))?;
        Ok(changed > 0)
    }

    pub fn update_status(&self, id: &str, status: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE messages SET status = ?1 WHERE id = ?2",
            params![status, id],
        )
        .map_err(|e| format!("update status: {e}"))?;
        Ok(())
    }

    pub fn update_body_and_status(
        &self,
        id: &str,
        body: &str,
        status: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE messages SET body = ?1, status = ?2 WHERE id = ?3",
            params![body, status, id],
        )
        .map_err(|e| format!("update body: {e}"))?;
        Ok(())
    }

    pub fn get_message(&self, id: &str) -> Result<Option<ChatMessage>, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.query_row(
            "SELECT id, peer_id, direction, msg_type, body, created_at, status
             FROM messages WHERE id = ?1",
            params![id],
            row_to_message,
        )
        .optional()
        .map_err(|e| format!("get message: {e}"))
    }

    /// Latest `limit` messages for a peer, returned oldest→newest for UI.
    pub fn list_for_peer(&self, peer_id: &str, limit: i64) -> Result<Vec<ChatMessage>, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        // Fetch newest first, then reverse so the chat UI can append chronologically.
        let mut stmt = conn
            .prepare(
                "SELECT id, peer_id, direction, msg_type, body, created_at, status
                 FROM messages
                 WHERE peer_id = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )
            .map_err(|e| format!("prepare list: {e}"))?;
        let rows = stmt
            .query_map(params![peer_id, limit], row_to_message)
            .map_err(|e| format!("query list: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("row: {e}"))?);
        }
        out.reverse();
        Ok(out)
    }

    /// Delete message only if it belongs to `peer_id` (guards wrong-thread deletes).
    pub fn delete_message_for_peer(&self, id: &str, peer_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n = conn
            .execute(
                "DELETE FROM messages WHERE id = ?1 AND peer_id = ?2",
                params![id, peer_id],
            )
            .map_err(|e| format!("delete message: {e}"))?;
        Ok(n > 0)
    }

    /// Delete one message by id. Returns true if a row was removed.
    pub fn delete_message(&self, id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n = conn
            .execute("DELETE FROM messages WHERE id = ?1", params![id])
            .map_err(|e| format!("delete message: {e}"))?;
        Ok(n > 0)
    }

    /// Delete all messages for a peer thread. Returns rows deleted.
    pub fn clear_peer(&self, peer_id: &str) -> Result<u64, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n = conn
            .execute("DELETE FROM messages WHERE peer_id = ?1", params![peer_id])
            .map_err(|e| format!("clear peer: {e}"))?;
        Ok(n as u64)
    }

    /// Delete all messages. Returns rows deleted.
    pub fn clear_all(&self) -> Result<u64, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n = conn
            .execute("DELETE FROM messages", [])
            .map_err(|e| format!("clear all: {e}"))?;
        Ok(n as u64)
    }

    pub fn count_all(&self) -> Result<u64, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .map_err(|e| format!("count all: {e}"))?;
        Ok(n as u64)
    }

    pub fn count_for_peer(&self, peer_id: &str) -> Result<u64, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages WHERE peer_id = ?1",
                params![peer_id],
                |r| r.get(0),
            )
            .map_err(|e| format!("count peer: {e}"))?;
        Ok(n as u64)
    }

    // --- transfers (resumable file jobs) ---

    pub fn upsert_transfer(&self, row: &TransferRow) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.execute(
            "INSERT INTO transfers
             (file_id, role, peer_id, message_id, path, partial_path, name, size, mime,
              token, bytes_done, state, source_mtime, error, created_at, updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(file_id) DO UPDATE SET
               role=excluded.role,
               peer_id=excluded.peer_id,
               message_id=excluded.message_id,
               path=excluded.path,
               partial_path=excluded.partial_path,
               name=excluded.name,
               size=excluded.size,
               mime=excluded.mime,
               token=excluded.token,
               bytes_done=excluded.bytes_done,
               state=excluded.state,
               source_mtime=excluded.source_mtime,
               error=excluded.error,
               updated_at=excluded.updated_at",
            params![
                row.file_id,
                row.role,
                row.peer_id,
                row.message_id,
                row.path,
                row.partial_path,
                row.name,
                row.size as i64,
                row.mime,
                row.token,
                row.bytes_done as i64,
                row.state,
                row.source_mtime,
                row.error,
                row.created_at,
                row.updated_at,
            ],
        )
        .map_err(|e| format!("upsert transfer: {e}"))?;
        Ok(())
    }

    pub fn get_transfer(&self, file_id: &str) -> Result<Option<TransferRow>, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.query_row(
            "SELECT file_id, role, peer_id, message_id, path, partial_path, name, size, mime,
                    token, bytes_done, state, source_mtime, error, created_at, updated_at
             FROM transfers WHERE file_id = ?1",
            params![file_id],
            row_to_transfer,
        )
        .optional()
        .map_err(|e| format!("get transfer: {e}"))
    }

    pub fn get_transfer_by_message(&self, message_id: &str) -> Result<Option<TransferRow>, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.query_row(
            "SELECT file_id, role, peer_id, message_id, path, partial_path, name, size, mime,
                    token, bytes_done, state, source_mtime, error, created_at, updated_at
             FROM transfers WHERE message_id = ?1",
            params![message_id],
            row_to_transfer,
        )
        .optional()
        .map_err(|e| format!("get transfer by message: {e}"))
    }

    /// Jobs that may still need resume or registry presence.
    pub fn list_resumable_transfers(&self) -> Result<Vec<TransferRow>, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT file_id, role, peer_id, message_id, path, partial_path, name, size, mime,
                        token, bytes_done, state, source_mtime, error, created_at, updated_at
                 FROM transfers
                 WHERE state IN ('offered','accepted','transferring','interrupted')
                 ORDER BY updated_at ASC",
            )
            .map_err(|e| format!("prepare resumable: {e}"))?;
        let rows = stmt
            .query_map([], row_to_transfer)
            .map_err(|e| format!("query resumable: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("row: {e}"))?);
        }
        Ok(out)
    }

    pub fn list_resumable_for_peer(&self, peer_id: &str) -> Result<Vec<TransferRow>, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT file_id, role, peer_id, message_id, path, partial_path, name, size, mime,
                        token, bytes_done, state, source_mtime, error, created_at, updated_at
                 FROM transfers
                 WHERE peer_id = ?1
                   AND state IN ('accepted','transferring','interrupted')
                 ORDER BY updated_at ASC",
            )
            .map_err(|e| format!("prepare peer transfers: {e}"))?;
        let rows = stmt
            .query_map(params![peer_id], row_to_transfer)
            .map_err(|e| format!("query peer transfers: {e}"))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| format!("row: {e}"))?);
        }
        Ok(out)
    }

    pub fn update_transfer_progress(
        &self,
        file_id: &str,
        bytes_done: u64,
        state: &str,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let now = now_ms();
        conn.execute(
            "UPDATE transfers SET bytes_done = ?1, state = ?2, updated_at = ?3, error = NULL
             WHERE file_id = ?4",
            params![bytes_done as i64, state, now, file_id],
        )
        .map_err(|e| format!("update transfer progress: {e}"))?;
        Ok(())
    }

    pub fn update_transfer_state(
        &self,
        file_id: &str,
        state: &str,
        bytes_done: Option<u64>,
        error: Option<&str>,
    ) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let now = now_ms();
        match bytes_done {
            Some(b) => {
                conn.execute(
                    "UPDATE transfers SET state = ?1, bytes_done = ?2, error = ?3, updated_at = ?4
                     WHERE file_id = ?5",
                    params![state, b as i64, error, now, file_id],
                )
            }
            None => conn.execute(
                "UPDATE transfers SET state = ?1, error = ?2, updated_at = ?3 WHERE file_id = ?4",
                params![state, error, now, file_id],
            ),
        }
        .map_err(|e| format!("update transfer state: {e}"))?;
        Ok(())
    }

    pub fn delete_transfer(&self, file_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.execute("DELETE FROM transfers WHERE file_id = ?1", params![file_id])
            .map_err(|e| format!("delete transfer: {e}"))?;
        Ok(())
    }

    pub fn delete_transfers_for_peer(&self, peer_id: &str) -> Result<u64, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n = conn
            .execute("DELETE FROM transfers WHERE peer_id = ?1", params![peer_id])
            .map_err(|e| format!("delete peer transfers: {e}"))?;
        Ok(n as u64)
    }

    pub fn delete_all_transfers(&self) -> Result<u64, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let n = conn
            .execute("DELETE FROM transfers", [])
            .map_err(|e| format!("delete all transfers: {e}"))?;
        Ok(n as u64)
    }

    pub fn upsert_group(&self, g: &crate::net::group::GroupInfo) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let active: i64 = if g.active { 1 } else { 0 };
        conn.execute(
            "INSERT INTO chat_groups (id, name, join_code, creator_id, created_at, active)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               join_code = excluded.join_code,
               creator_id = excluded.creator_id,
               active = excluded.active",
            params![
                g.id,
                g.name,
                g.join_code,
                g.creator_id,
                now_ms(),
                active
            ],
        )
        .map_err(|e| format!("upsert group: {e}"))?;
        conn.execute(
            "DELETE FROM chat_group_members WHERE group_id = ?1",
            params![g.id],
        )
        .map_err(|e| format!("clear group members: {e}"))?;
        for m in &g.members {
            conn.execute(
                "INSERT INTO chat_group_members (group_id, device_id, display_name)
                 VALUES (?1, ?2, ?3)",
                params![g.id, m.device_id, m.display_name],
            )
            .map_err(|e| format!("insert group member: {e}"))?;
        }
        Ok(())
    }

    pub fn mark_group_left(&self, group_id: &str) -> Result<(), String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        conn.execute(
            "UPDATE chat_groups SET active = 0 WHERE id = ?1",
            params![group_id],
        )
        .map_err(|e| format!("mark group left: {e}"))?;
        Ok(())
    }

    pub fn list_groups(&self) -> Result<Vec<crate::net::group::GroupInfo>, String> {
        let conn = self.conn.lock().map_err(|_| "db lock poisoned".to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT id, name, join_code, creator_id, active FROM chat_groups ORDER BY name",
            )
            .map_err(|e| format!("prepare groups: {e}"))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(|e| format!("query groups: {e}"))?;

        let mut out = Vec::new();
        for r in rows {
            let (id, name, join_code, creator_id, active) =
                r.map_err(|e| format!("group row: {e}"))?;
            let mut mstmt = conn
                .prepare(
                    "SELECT device_id, display_name FROM chat_group_members WHERE group_id = ?1",
                )
                .map_err(|e| format!("prepare members: {e}"))?;
            let members = mstmt
                .query_map(params![id], |row| {
                    Ok(crate::net::protocol::GroupMemberWire {
                        device_id: row.get(0)?,
                        display_name: row.get(1)?,
                    })
                })
                .map_err(|e| format!("query members: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("member row: {e}"))?;
            out.push(crate::net::group::GroupInfo {
                id,
                name,
                join_code,
                creator_id,
                members,
                active: active != 0,
            });
        }
        Ok(out)
    }
}

/// Durable transfer job (token + paths for resume across restarts).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferRow {
    pub file_id: String,
    /// `send` | `recv`
    pub role: String,
    pub peer_id: String,
    pub message_id: String,
    /// send: source path; recv: final dest path
    pub path: String,
    pub partial_path: Option<String>,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub token: String,
    pub bytes_done: u64,
    pub state: String,
    pub source_mtime: Option<i64>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChatMessage> {
    Ok(ChatMessage {
        id: row.get(0)?,
        peer_id: row.get(1)?,
        direction: row.get(2)?,
        msg_type: row.get(3)?,
        body: row.get(4)?,
        created_at: row.get(5)?,
        status: row.get(6)?,
    })
}

fn row_to_transfer(row: &rusqlite::Row<'_>) -> rusqlite::Result<TransferRow> {
    let size: i64 = row.get(7)?;
    let bytes_done: i64 = row.get(10)?;
    Ok(TransferRow {
        file_id: row.get(0)?,
        role: row.get(1)?,
        peer_id: row.get(2)?,
        message_id: row.get(3)?,
        path: row.get(4)?,
        partial_path: row.get(5)?,
        name: row.get(6)?,
        size: size as u64,
        mime: row.get(8)?,
        token: row.get(9)?,
        bytes_done: bytes_done as u64,
        state: row.get(11)?,
        source_mtime: row.get(12)?,
        error: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn db_path(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(DB_FILE_NAME)
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
        let dir = std::env::temp_dir().join(format!("jotain-db-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample(id: &str, peer: &str) -> ChatMessage {
        ChatMessage {
            id: id.into(),
            peer_id: peer.into(),
            direction: "out".into(),
            msg_type: "text".into(),
            body: "hi".into(),
            created_at: 100,
            status: "sent".into(),
        }
    }

    #[test]
    fn insert_is_idempotent_and_lists() {
        let dir = temp_dir();
        let db = Database::open(&dir).unwrap();
        let msg = sample("m1", "peer");
        assert!(db.insert_message(&msg).unwrap());
        assert!(!db.insert_message(&msg).unwrap());
        db.update_status("m1", "sent").unwrap();
        let list = db.list_for_peer("peer", 100).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].status, "sent");

        // list_for_peer returns newest window in chronological order
        for i in 0..5 {
            let mut m = sample(&format!("n{i}"), "peer");
            m.created_at = 200 + i;
            db.insert_message(&m).unwrap();
        }
        let window = db.list_for_peer("peer", 3).unwrap();
        assert_eq!(window.len(), 3);
        assert_eq!(window[0].id, "n2");
        assert_eq!(window[2].id, "n4");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_message_and_clear_peer_and_all() {
        let dir = temp_dir();
        let db = Database::open(&dir).unwrap();
        db.insert_message(&sample("a1", "p1")).unwrap();
        db.insert_message(&sample("a2", "p1")).unwrap();
        db.insert_message(&sample("b1", "p2")).unwrap();

        assert!(db.delete_message_for_peer("a1", "p1").unwrap());
        assert!(!db.delete_message_for_peer("a1", "p1").unwrap());
        // Wrong peer must not delete
        assert!(!db.delete_message_for_peer("a2", "p2").unwrap());
        assert_eq!(db.count_for_peer("p1").unwrap(), 1);

        assert_eq!(db.clear_peer("p1").unwrap(), 1);
        assert_eq!(db.count_for_peer("p1").unwrap(), 0);
        assert_eq!(db.count_for_peer("p2").unwrap(), 1);

        assert_eq!(db.clear_all().unwrap(), 1);
        assert_eq!(db.count_all().unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transfers_upsert_list_delete() {
        let dir = temp_dir();
        let db = Database::open(&dir).unwrap();
        let now = 1_700_000_000_000i64;
        let row = TransferRow {
            file_id: "f1".into(),
            role: "recv".into(),
            peer_id: "p1".into(),
            message_id: "m1".into(),
            path: "/tmp/a".into(),
            partial_path: Some("/tmp/a.partial".into()),
            name: "a.bin".into(),
            size: 1000,
            mime: "application/octet-stream".into(),
            token: "tok".into(),
            bytes_done: 100,
            state: "interrupted".into(),
            source_mtime: None,
            error: Some("eof".into()),
            created_at: now,
            updated_at: now,
        };
        db.upsert_transfer(&row).unwrap();
        db.upsert_transfer(&TransferRow {
            bytes_done: 200,
            updated_at: now + 1,
            error: None,
            ..row.clone()
        })
        .unwrap();
        let got = db.get_transfer("f1").unwrap().unwrap();
        assert_eq!(got.bytes_done, 200);
        assert!(got.error.is_none());
        assert_eq!(db.list_resumable_transfers().unwrap().len(), 1);
        assert_eq!(db.list_resumable_for_peer("p1").unwrap().len(), 1);
        db.update_transfer_progress("f1", 300, "transferring")
            .unwrap();
        assert_eq!(db.get_transfer("f1").unwrap().unwrap().bytes_done, 300);
        db.delete_transfer("f1").unwrap();
        assert!(db.get_transfer("f1").unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
