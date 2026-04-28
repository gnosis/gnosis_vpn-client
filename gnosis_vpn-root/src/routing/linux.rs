//! Linux-specific routing setup.

use std::path::PathBuf;

use gnosis_vpn_lib::shell_command_ext::Logs;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::manager::{RouteCmd, RouteEvent, RouteManager};
use super::route_ops_linux::NetlinkRouteOps;
use super::Error;
use crate::wg_tooling;

/// Creates the route manager for Linux using the rtnetlink kernel interface.
pub(crate) fn create_manager(
    state_home: PathBuf,
) -> Result<(CancellationToken, mpsc::Sender<RouteCmd>, mpsc::Receiver<RouteEvent>, RouteManager), Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let route_ops = NetlinkRouteOps::new(handle);
    Ok(RouteManager::new(state_home, route_ops))
}

/// Clean up from any previous unclean shutdown.
pub async fn reset_on_startup(state_home: PathBuf) {
    let _ = wg_tooling::down(state_home, Logs::Suppress).await;
}
