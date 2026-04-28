//! macOS-specific routing setup.

use std::path::PathBuf;

use gnosis_vpn_lib::shell_command_ext::Logs;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::manager::{RouteCmd, RouteEvent, RouteManager};
use super::route_ops_macos::DarwinRouteOps;
use crate::wg_tooling;

/// Creates the route manager for macOS.
pub(crate) fn create_manager(
    state_home: PathBuf,
) -> (CancellationToken, mpsc::Sender<RouteCmd>, mpsc::Receiver<RouteEvent>, RouteManager) {
    RouteManager::new(state_home, DarwinRouteOps)
}

/// Clean up from any previous unclean shutdown.
pub async fn reset_on_startup(state_home: PathBuf) {
    let _ = wg_tooling::down(state_home, Logs::Suppress).await;
}
