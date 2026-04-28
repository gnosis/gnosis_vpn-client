//! Route manager — background actor.
//!
//! [`RouteManager`] owns all routing state for the daemon's lifetime.
//! Root constructs it via the platform-specific `create_manager()` factory,
//! which sets up the channel pairs and returns the endpoints the daemon loop needs.
//! The manager itself is handed to `tokio::spawn(manager.run())`.
//!
//! ## Channel protocol
//!
//! ```text
//! daemon                           RouteManager
//!   │── Sender<RouteCmd> ────────────────────►│  commands (fire-and-forget)
//!   │◄─────────────────── Sender<RouteEvent> ─│  async results / events
//! ```
//!
//! ## WiFi reactivity
//!
//! Every 5 seconds while connected, the manager polls `get_default_interface()`.
//! If the WAN device or gateway changes it tears down the bypass routes and
//! re-adds them through the new interface, then emits `RouteEvent::NetworkChanged`.
//! WireGuard is untouched — only bypass routes are rebuilt.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::shell_command_ext::Logs;
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use super::Error;
use super::bypass::{BypassRouteManager, WanInterface};

#[cfg(target_os = "linux")]
use super::route_ops_linux::NetlinkRouteOps;
#[cfg(target_os = "macos")]
use super::route_ops_macos::DarwinRouteOps;

use crate::wg_tooling;

const WAN_POLL_INTERVAL: Duration = Duration::from_secs(5);

// ============================================================================
// Protocol
// ============================================================================

/// Commands root sends to the route manager.
pub enum RouteCmd {
    /// Set up VPN routing for the given WireGuard config and peer IPs.
    /// Result comes back asynchronously as [`RouteEvent::Connected`].
    Connect {
        wg_data: Box<event::WireGuardData>,
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

    /// Bypass routes were rebuilt after the default WAN interface changed.
    /// Root uses this for logging; no further action needed.
    NetworkChanged,
}

// ============================================================================
// Internal state
// ============================================================================

/// State tracked while WireGuard is up.
struct WgConnection {
    interface: String,
    /// VPN routes added after wg-quick up (macOS only; empty on Linux).
    vpn_routes: Vec<String>,
    /// Peer IPs needed when rebuilding bypass routes after a WAN change.
    peer_ips: Vec<Ipv4Addr>,
}

// ============================================================================
// Actor
// ============================================================================

pub struct RouteManager {
    state_home: PathBuf,
    cancel: CancellationToken,
    cmd_receiver: mpsc::Receiver<RouteCmd>,
    event_sender: mpsc::Sender<RouteEvent>,
    #[cfg(target_os = "linux")]
    route_ops: NetlinkRouteOps,
    #[cfg(target_os = "macos")]
    route_ops: DarwinRouteOps,
    /// Active bypass route manager when VPN is up; `None` when disconnected.
    bypass: Option<BypassRouteManager>,
    /// WireGuard connection state when VPN is up; `None` when disconnected.
    wg_conn: Option<WgConnection>,
    /// WAN interface used when the current bypass was set up.
    current_wan: Option<WanInterface>,
}

impl RouteManager {
    #[cfg(target_os = "linux")]
    pub(crate) fn new(
        state_home: PathBuf,
        route_ops: NetlinkRouteOps,
    ) -> (
        CancellationToken,
        mpsc::Sender<RouteCmd>,
        mpsc::Receiver<RouteEvent>,
        Self,
    ) {
        let (cmd_sender, cmd_receiver) = mpsc::channel(32);
        let (event_sender, event_receiver) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        let manager = Self {
            state_home,
            cancel: cancel.clone(),
            cmd_receiver,
            event_sender,
            route_ops,
            bypass: None,
            wg_conn: None,
            current_wan: None,
        };
        (cancel, cmd_sender, event_receiver, manager)
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn new(
        state_home: PathBuf,
        route_ops: DarwinRouteOps,
    ) -> (
        CancellationToken,
        mpsc::Sender<RouteCmd>,
        mpsc::Receiver<RouteEvent>,
        Self,
    ) {
        let (cmd_sender, cmd_receiver) = mpsc::channel(32);
        let (event_sender, event_receiver) = mpsc::channel(32);
        let cancel = CancellationToken::new();
        let manager = Self {
            state_home,
            cancel: cancel.clone(),
            cmd_receiver,
            event_sender,
            route_ops,
            bypass: None,
            wg_conn: None,
            current_wan: None,
        };
        (cancel, cmd_sender, event_receiver, manager)
    }

    /// Drive the route manager loop. Pass this future to `tokio::spawn`.
    pub async fn run(mut self) {
        let mut poll = time::interval(WAN_POLL_INTERVAL);
        poll.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        // Consume the immediate first tick so we don't poll right at startup.
        poll.tick().await;

        loop {
            tokio::select! {
                _ = self.cancel.cancelled() => {
                    tracing::debug!("route manager: cancelled, tearing down");
                    self.disconnect(Logs::Suppress).await;
                    return;
                },

                cmd = self.cmd_receiver.recv() => match cmd {
                    Some(RouteCmd::Connect { wg_data, peer_ips }) => {
                        let res = self.connect(*wg_data, peer_ips).await;
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
                },

                _tick = poll.tick() => {
                    if self.wg_conn.is_some() {
                        self.on_poll_tick().await;
                    }
                }
            }
        }
    }

    async fn connect(&mut self, wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> Result<(), String> {
        self.disconnect(Logs::Suppress).await;

        // Phase 1: Bypass routes BEFORE WG up — peer IPs and RFC1918 go via WAN gateway.
        let (device, gateway) = self
            .route_ops
            .get_default_interface()
            .await
            .map_err(|e| e.to_string())?;
        let wan = WanInterface { device, gateway };
        let mut bypass = BypassRouteManager::new(wan.clone(), peer_ips.clone(), self.route_ops.clone());
        bypass.setup_peer_routes().await.map_err(|e| e.to_string())?;
        bypass.setup_rfc1918_routes().await.map_err(|e| e.to_string())?;

        // Phase 2: wg-quick up.
        let wg_config = build_wg_config(&wg_data);
        let interface = match wg_tooling::up(self.state_home.clone(), wg_config).await {
            Ok(iface) => iface,
            Err(e) => {
                bypass.rollback().await;
                return Err(e.to_string());
            }
        };
        tracing::debug!(interface = %interface, "wg-quick up");

        // Phase 3: VPN routes after WG up (macOS: from AllowedIPs; Linux: none needed).
        let vpn_routes = match self.setup_vpn_routes(&interface, &wg_data).await {
            Ok(routes) => routes,
            Err(e) => {
                let _ = wg_tooling::down(self.state_home.clone(), Logs::Suppress).await;
                bypass.rollback().await;
                return Err(e.to_string());
            }
        };

        self.bypass = Some(bypass);
        self.current_wan = Some(wan);
        self.wg_conn = Some(WgConnection {
            interface,
            vpn_routes,
            peer_ips,
        });
        tracing::info!("routing ready");
        Ok(())
    }

    async fn disconnect(&mut self, logs: Logs) {
        if let Some(conn) = self.wg_conn.take() {
            self.teardown_vpn_routes(&conn.interface, &conn.vpn_routes).await;
            match wg_tooling::down(self.state_home.clone(), logs).await {
                Ok(_) => tracing::debug!("wg-quick down"),
                Err(e) => tracing::warn!(%e, "wg-quick down failed, continuing"),
            }
        }
        if let Some(mut bypass) = self.bypass.take() {
            bypass.teardown().await;
        }
        self.current_wan = None;
    }

    /// Tear down and re-add bypass routes against the current WAN interface.
    ///
    /// Called on WAN change or when bypass routes disappear unexpectedly.
    /// WireGuard is untouched.
    async fn rebuild_bypass(&mut self) -> Result<(), Error> {
        let peer_ips = match &self.wg_conn {
            Some(conn) => conn.peer_ips.clone(),
            None => return Ok(()),
        };

        if let Some(mut old_bypass) = self.bypass.take() {
            old_bypass.teardown().await;
        }
        self.current_wan = None;

        let (device, gateway) = self.route_ops.get_default_interface().await?;
        let wan = WanInterface { device, gateway };
        let mut bypass = BypassRouteManager::new(wan.clone(), peer_ips, self.route_ops.clone());
        bypass.setup_peer_routes().await?;
        bypass.setup_rfc1918_routes().await?;

        self.bypass = Some(bypass);
        self.current_wan = Some(wan);
        tracing::info!("bypass routes rebuilt");
        Ok(())
    }

    async fn check_network_change(&mut self) {
        let (device, gateway) = match self.route_ops.get_default_interface().await {
            Ok(iface) => iface,
            Err(e) => {
                tracing::warn!(%e, "failed to get default interface during network change check");
                return;
            }
        };

        let changed = match &self.current_wan {
            None => true,
            Some(wan) => wan.device != device || wan.gateway != gateway,
        };

        if !changed {
            return;
        }

        tracing::info!(%device, gateway = ?gateway, "WAN changed, rebuilding bypass routes");
        match self.rebuild_bypass().await {
            Ok(_) => {
                let _ = self.event_sender.send(RouteEvent::NetworkChanged).await;
            }
            Err(e) => tracing::warn!(%e, "failed to rebuild bypass routes after WAN change"),
        }
    }

    async fn on_poll_tick(&mut self) {
        if self.bypass.is_none() {
            tracing::debug!("poll: bypass routes missing while VPN is up, rebuilding");
            match self.rebuild_bypass().await {
                Ok(_) => {
                    let _ = self.event_sender.send(RouteEvent::NetworkChanged).await;
                }
                Err(e) => tracing::warn!(%e, "failed to rebuild missing bypass routes"),
            }
        } else {
            self.check_network_change().await;
        }
    }

    #[cfg(target_os = "macos")]
    async fn setup_vpn_routes(&self, iface: &str, wg_data: &event::WireGuardData) -> Result<Vec<String>, Error> {
        let allowed_ips = wg_data.wg.config.allowed_ips.as_deref().unwrap_or("0.0.0.0/0");
        let routes = vpn_routes_for(allowed_ips);
        let mut added = Vec::new();
        for route in &routes {
            if let Err(e) = self.route_ops.route_add(route, None, iface).await {
                tracing::warn!(%e, route, "VPN route failed, rolling back");
                for r in added.iter().rev() {
                    let _ = self.route_ops.route_del(r, iface).await;
                }
                return Err(e);
            }
            added.push(route.clone());
        }
        tracing::debug!(routes = ?added, "VPN routes added");
        Ok(added)
    }

    #[cfg(target_os = "linux")]
    async fn setup_vpn_routes(&self, _iface: &str, _wg_data: &event::WireGuardData) -> Result<Vec<String>, Error> {
        Ok(vec![])
    }

    async fn teardown_vpn_routes(&self, iface: &str, vpn_routes: &[String]) {
        for route in vpn_routes {
            if let Err(e) = self.route_ops.route_del(route, iface).await {
                tracing::warn!(route, %e, "failed to remove VPN route");
            }
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Build the wg-quick config string for this connection.
///
/// macOS uses `Table = off` so wg-quick doesn't install routes automatically
/// (we add them ourselves). Linux uses wg-quick's default policy routing.
fn build_wg_config(wg_data: &event::WireGuardData) -> String {
    #[cfg(target_os = "macos")]
    let extra = vec!["Table = off".to_string()];
    #[cfg(target_os = "linux")]
    let extra = vec![];
    wg_data
        .wg
        .to_file_string(&wg_data.interface_info, &wg_data.peer_info, extra)
}

/// Compute the explicit VPN routes to add for a given AllowedIPs string.
///
/// `0.0.0.0/0` is split into `0.0.0.0/1` + `128.0.0.0/1` to avoid replacing
/// the default route that bypass routes depend on for their WAN gateway.
/// Any other IPv4 CIDR is normalised to its network address.
/// IPv6 CIDRs are skipped (not supported).
#[cfg(target_os = "macos")]
fn vpn_routes_for(allowed_ips: &str) -> Vec<String> {
    allowed_ips
        .split(',')
        .filter_map(|s| {
            let cidr = s.trim();
            if cidr.contains(':') {
                None // IPv6 — skip
            } else if cidr == "0.0.0.0/0" {
                // Split default route: preserve the existing default so bypass
                // routes can still reach their WAN gateway.
                Some(vec!["0.0.0.0/1".to_string(), "128.0.0.0/1".to_string()])
            } else {
                Some(vec![network_cidr(cidr)])
            }
        })
        .flatten()
        .collect()
}

/// Normalise a CIDR to its network address (zero out host bits).
///
/// `"10.128.0.1/9"` → `"10.128.0.0/9"`
#[cfg(target_os = "macos")]
fn network_cidr(cidr: &str) -> String {
    let Some((addr_str, prefix_str)) = cidr.split_once('/') else {
        return cidr.to_string();
    };
    let (Ok(addr), Ok(prefix)) = (addr_str.parse::<Ipv4Addr>(), prefix_str.parse::<u8>()) else {
        return cidr.to_string();
    };
    let mask: u32 = if prefix == 0 { 0 } else { !0u32 << (32 - prefix) };
    let network = Ipv4Addr::from(u32::from(addr) & mask);
    format!("{network}/{prefix}")
}
