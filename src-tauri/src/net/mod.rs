//! LAN control plane (TCP) for 1:1 messaging + file signaling + group text mesh.

pub mod frame;
pub mod group;
pub mod protocol;
pub mod session;
pub mod transfer;

pub use session::{list_session_peers, send_text_to_peer, start_control_plane};
pub use transfer::start_data_plane;
