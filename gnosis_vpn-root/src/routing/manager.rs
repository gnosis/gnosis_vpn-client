//! Route manager — background actor.
//!
//! [`RouteManager`] owns all routing state for the daemon's lifetime.
//! Root constructs it via [`RouteManager::new`], which sets up the channel
//! pairs and returns the endpoints the daemon loop needs. The manager itself
//! is handed to `tokio::spawn(manager.run())`.
//!
//! ## Channel protocol
//!
//! ```text
//! daemon                           RouteManager
//!   │── Sender<RouteCmd> ────────────────────►│  commands (fire-and-forget)
//!   │◄─────────────────── Sender<RouteEvent> ─│  async results / events
//! ```
//!
//! ## Extension points
//!
//! - **Step 3 (WiFi reactivity)**: add a network monitor stream as a field,
//!   wire a new `select!` arm, emit `RouteEvent::NetworkChanged`.
//! - **Step 5 (lockdown)**: add `RouteCmd::EnableLockdown` and a
//!   `lockdown: bool` field; block traffic instead of leaving it open when
//!   the VPN drops.

use std::net::Ipv4Addr;
use std::path::PathBuf;

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::shell_command_ext::Logs;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::Routing;

// ============================================================================
// Protocol
// ============================================================================

/// Commands root sends to the route manager.
pub enum RouteCmd {
    /// Set up VPN routing for the given WireGuard config and peer IPs.
    /// Result comes back asynchronously as [`RouteEvent::Connected`].
    Connect {
        wg_data: event::WireGuardData,
        peer_ips: Vec<Ipv4Addr>,
    },
    /// Tear down all active routing. Fire-and-forget; no result event.
    Disconnect,
}

/// Events the route manager sends back to root's `daemon_loop`.
pub enum RouteEvent {
    /// Result of a [`RouteCmd::Connect`].
    /// Root forwards this to the worker as `ResponseFromRoot::StaticWgRouting`.
    Connected(Result<(), String>),

    // Step 3: NetworkChanged { device: String, gateway: Option<String> }
    //   Emitted when the default route changes (e.g. WiFi switch).
    //   Root re-issues Connect so bypass routes are re-applied to the new gateway.
}

// ============================================================================
// Actor
// ============================================================================

/// Background actor that owns all routing state.
///
/// Construct with [`RouteManager::new`], then hand to
/// `tokio::spawn(manager.run())`.
pub struct RouteManager {
    state_home: PathBuf,
    cancel: CancellationToken,
    cmd_receiver: mpsc::Receiver<RouteCmd>,
    event_sender: mpsc::Sender<RouteEvent>,
    /// Active router when connected; `None` when disconnected.
    router: Option<Box<dyn Routing>>,
}

impl RouteManager {
    /// Set up the route manager and return the daemon's channel endpoints.
    ///
    /// - `CancellationToken` — cancel to trigger graceful teardown
    /// - `Sender<RouteCmd>` — root sends routing commands here
    /// - `Receiver<RouteEvent>` — root's `daemon_loop` selects on this
    /// - `RouteManager` — hand to `tokio::spawn(manager.run())`
    pub fn new(
        state_home: PathBuf,
    ) -> (CancellationToken, mpsc::Sender<RouteCmd>, mpsc::Receiver<RouteEvent>, Self) {
        let (cmd_sender, cmd_receiver) = mpsc::channel(32);
        let (event_sender, event_receiver) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        let owned_cancel = cancel.clone();

        let manager = Self {
            state_home,
            cancel: owned_cancel,
            cmd_receiver,
            event_sender,
            router: None,
        };

        (cancel, cmd_sender, event_receiver, manager)
    }

    /// Drive the route manager loop. Pass this future to `tokio::spawn`.
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::debug!("route manager: cancelled, tearing down");
                    self.disconnect(Logs::Suppress).await;
                    return;
                }

                cmd = self.cmd_receiver.recv() => match cmd {
                    Some(RouteCmd::Connect { wg_data, peer_ips }) => {
                        let res = self.connect(wg_data, peer_ips).await;
                        let _ = self.event_sender.send(RouteEvent::Connected(res)).await;
                    }
                    Some(RouteCmd::Disconnect) => {
                        self.disconnect(Logs::Print).await;
                    }
                    None => {
                        tracing::debug!("route manager: command channel closed, tearing down");
                        self.disconnect(Logs::Suppress).await;
                        return;
                    }
                }

                // Step 3: Some(event) = network_monitor.recv() => self.on_network_change(event).await,
            }
        }
    }

    async fn connect(&mut self, wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> Result<(), String> {
        self.disconnect(Logs::Suppress).await;

        let mut router = super::static_router(self.state_home.clone(), wg_data, peer_ips)
            .map_err(|e| e.to_string())?;

        router.setup().await.map_err(|e| e.to_string())?;
        self.router = Some(Box::new(router));
        Ok(())
    }

    async fn disconnect(&mut self, logs: Logs) {
        if let Some(mut router) = self.router.take() {
            router.teardown(logs).await;
        }
    }
}
