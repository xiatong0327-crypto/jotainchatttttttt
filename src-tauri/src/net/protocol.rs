//! Wire protocol messages for the control plane.

use crate::discovery::PROTOCOL_VERSION;
use serde::{Deserialize, Serialize};

pub const MAX_TEXT_CHARS: usize = 16_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WireMessage {
    #[serde(rename = "hello")]
    Hello {
        v: u32,
        #[serde(rename = "deviceId")]
        device_id: String,
        #[serde(rename = "displayName")]
        display_name: String,
    },
    #[serde(rename = "text")]
    Text {
        id: String,
        body: String,
        ts: i64,
    },
    /// Sender offers a file; receiver must Accept before bytes flow
    /// unless `auto_accept` is true (paste screenshot only).
    #[serde(rename = "fileOffer")]
    FileOffer {
        #[serde(rename = "fileId")]
        file_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        name: String,
        size: u64,
        mime: String,
        token: String,
        ts: i64,
        /// Optional whole-file SHA-256 (hex). Old peers omit; receiver cross-checks trailer when present.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sha256: Option<String>,
        /// When true, receiver may start without user Accept (screenshot paste only).
        /// Omitted / false for File picker, drag-drop, and normal image files.
        #[serde(default, rename = "autoAccept", skip_serializing_if = "std::ops::Not::not")]
        auto_accept: bool,
    },
    #[serde(rename = "fileAccept")]
    FileAccept {
        #[serde(rename = "fileId")]
        file_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "fileReject")]
    FileReject {
        #[serde(rename = "fileId")]
        file_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    #[serde(rename = "fileCancel")]
    FileCancel {
        #[serde(rename = "fileId")]
        file_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
    },
    /// Receiver asks sender to push from resume_offset (same token as offer).
    #[serde(rename = "fileResume")]
    FileResume {
        #[serde(rename = "fileId")]
        file_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "resumeOffset")]
        resume_offset: u64,
        token: String,
    },
    /// Sender cannot serve resume (busy, missing source, bad token, …).
    #[serde(rename = "fileResumeReject")]
    FileResumeReject {
        #[serde(rename = "fileId")]
        file_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        reason: String,
        #[serde(default)]
        detail: Option<String>,
    },
    #[serde(rename = "ping")]
    Ping,
    #[serde(rename = "pong")]
    Pong,

    // --- Group chat (mesh over 1:1 control sessions; no file transfer) ---
    /// Ask every connected peer: join this group if you know the code.
    #[serde(rename = "groupJoinRequest")]
    GroupJoinRequest {
        #[serde(rename = "groupId")]
        group_id: String,
        #[serde(rename = "joinCode")]
        join_code: String,
        #[serde(rename = "deviceId")]
        device_id: String,
        #[serde(rename = "displayName")]
        display_name: String,
    },
    /// Sent to joiner with full roster after code verifies.
    #[serde(rename = "groupJoinOk")]
    GroupJoinOk {
        #[serde(rename = "groupId")]
        group_id: String,
        name: String,
        #[serde(rename = "joinCode")]
        join_code: String,
        #[serde(rename = "creatorId")]
        creator_id: String,
        members: Vec<GroupMemberWire>,
    },
    #[serde(rename = "groupJoinReject")]
    GroupJoinReject {
        #[serde(rename = "groupId")]
        group_id: String,
        reason: String,
    },
    /// Member left or roster changed.
    #[serde(rename = "groupMemberUpdate")]
    GroupMemberUpdate {
        #[serde(rename = "groupId")]
        group_id: String,
        name: String,
        #[serde(rename = "joinCode")]
        join_code: String,
        #[serde(rename = "creatorId")]
        creator_id: String,
        members: Vec<GroupMemberWire>,
    },
    #[serde(rename = "groupLeave")]
    GroupLeave {
        #[serde(rename = "groupId")]
        group_id: String,
        #[serde(rename = "deviceId")]
        device_id: String,
    },
    /// Text to a group (fan-out by each member over 1:1 sessions).
    #[serde(rename = "groupText")]
    GroupText {
        #[serde(rename = "groupId")]
        group_id: String,
        id: String,
        #[serde(rename = "fromDeviceId")]
        from_device_id: String,
        #[serde(rename = "fromName")]
        from_name: String,
        body: String,
        ts: i64,
    },
}

/// Member snapshot on the wire / in group roster.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GroupMemberWire {
    #[serde(rename = "deviceId")]
    pub device_id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
}

impl WireMessage {
    pub fn hello(device_id: String, display_name: String) -> Self {
        Self::Hello {
            v: PROTOCOL_VERSION,
            device_id,
            display_name,
        }
    }

    pub fn text(id: String, body: String, ts: i64) -> Self {
        Self::Text { id, body, ts }
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("serialize wire: {e}"))
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("parse wire: {e}"))
    }
}

/// Persisted / UI file card (JSON in message.body for msg_type=file).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCard {
    pub file_id: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub bytes_done: u64,
    /// offered | accepted | rejected | cancelled | transferring | completed | failed | interrupted
    pub state: String,
    pub local_path: Option<String>,
    pub sha256: Option<String>,
    pub error: Option<String>,
    /// Receiver may Resume (PR-R2 wire; R1 sets for interrupted/accepted demote).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_capable: Option<bool>,
    /// Paste screenshot: no Accept UI on receiver (wire autoAccept).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_accept: bool,
}

pub fn validate_text_body(body: &str) -> Result<(), String> {
    if body.is_empty() {
        return Err("Message cannot be empty.".into());
    }
    if body.chars().count() > MAX_TEXT_CHARS {
        return Err(format!(
            "Message too long (max {MAX_TEXT_CHARS} characters)."
        ));
    }
    if body
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
    {
        return Err("Message contains invalid control characters.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let m = WireMessage::hello("a".into(), "Alice".into());
        let b = m.to_bytes().unwrap();
        let back = WireMessage::from_bytes(&b).unwrap();
        match back {
            WireMessage::Hello { v, device_id, .. } => {
                assert_eq!(v, PROTOCOL_VERSION);
                assert_eq!(device_id, "a");
            }
            _ => panic!("wrong type"),
        }
    }

    #[test]
    fn file_offer_roundtrip() {
        let m = WireMessage::FileOffer {
            file_id: "f1".into(),
            message_id: "m1".into(),
            name: "a.png".into(),
            size: 10,
            mime: "image/png".into(),
            token: "tok".into(),
            ts: 1,
            sha256: Some("deadbeef".into()),
            auto_accept: true,
        };
        let back = WireMessage::from_bytes(&m.to_bytes().unwrap()).unwrap();
        match back {
            WireMessage::FileOffer {
                name,
                size,
                sha256,
                auto_accept,
                ..
            } => {
                assert_eq!(name, "a.png");
                assert_eq!(size, 10);
                assert_eq!(sha256.as_deref(), Some("deadbeef"));
                assert!(auto_accept);
            }
            _ => panic!("wrong"),
        }
    }

    #[test]
    fn file_offer_without_sha256_compat() {
        let raw = r#"{"type":"fileOffer","fileId":"f","messageId":"m","name":"a","size":1,"mime":"x","token":"t","ts":1}"#;
        let m = WireMessage::from_bytes(raw.as_bytes()).unwrap();
        match m {
            WireMessage::FileOffer {
                sha256,
                size,
                auto_accept,
                ..
            } => {
                assert!(sha256.is_none());
                assert_eq!(size, 1);
                assert!(!auto_accept);
            }
            _ => panic!("wrong"),
        }
    }

    #[test]
    fn text_validation() {
        assert!(validate_text_body("hi").is_ok());
        assert!(validate_text_body("").is_err());
        assert!(validate_text_body(&"x".repeat(MAX_TEXT_CHARS + 1)).is_err());
    }

    #[test]
    fn file_card_old_json_compat() {
        let raw = r#"{"fileId":"f","name":"a","size":1,"mime":"x","bytesDone":0,"state":"offered"}"#;
        let card: FileCard = serde_json::from_str(raw).unwrap();
        assert_eq!(card.state, "offered");
        assert!(card.resume_capable.is_none());
    }

    #[test]
    fn file_resume_roundtrip() {
        let m = WireMessage::FileResume {
            file_id: "f1".into(),
            message_id: "m1".into(),
            resume_offset: 262144,
            token: "ab".into(),
        };
        let back = WireMessage::from_bytes(&m.to_bytes().unwrap()).unwrap();
        match back {
            WireMessage::FileResume {
                resume_offset,
                token,
                ..
            } => {
                assert_eq!(resume_offset, 262144);
                assert_eq!(token, "ab");
            }
            _ => panic!("wrong type"),
        }
    }

    #[test]
    fn file_resume_reject_roundtrip() {
        let m = WireMessage::FileResumeReject {
            file_id: "f1".into(),
            message_id: "m1".into(),
            reason: "busy".into(),
            detail: Some("push in flight".into()),
        };
        let back = WireMessage::from_bytes(&m.to_bytes().unwrap()).unwrap();
        match back {
            WireMessage::FileResumeReject { reason, detail, .. } => {
                assert_eq!(reason, "busy");
                assert_eq!(detail.as_deref(), Some("push in flight"));
            }
            _ => panic!("wrong type"),
        }
    }
}
