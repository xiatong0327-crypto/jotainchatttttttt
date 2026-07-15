//! Shared application state.

use crate::config::UserConfig;
use crate::db::Database;
use crate::diagnostics::DiagnosticsLog;
use crate::discovery::{DiscoveryState, PeerTable};
use crate::net::session::SessionMap;
use crate::net::transfer::TransferRegistry;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AppState {
    pub app_data_dir: PathBuf,
    pub config: Mutex<UserConfig>,
    pub peers: Mutex<PeerTable>,
    pub discovery: DiscoveryState,
    pub db: Database,
    pub sessions: Mutex<SessionMap>,
    pub transfers: Mutex<TransferRegistry>,
    pub diagnostics: DiagnosticsLog,
}
