//! macOS routing implementation for split-tunnel VPN behavior.
//!
//! Provides a [`StaticRouter`] that:
//! 1. Adds bypass routes for peer IPs and RFC1918 networks BEFORE bringing up WireGuard
//!    (avoids race condition for both HOPR traffic and LAN access)
//! 2. Runs `wg-quick up` with `Table = off` to prevent automatic routing
//! 3. Adds VPN-specific routes programmatically after wg-quick up:
//!    - Default routes (0.0.0.0/1 and 128.0.0.0/1) through VPN
//!    - VPN subnet route (10.128.0.0/9) through VPN - overrides the 10.0.0.0/8 bypass
//!      so VPN server traffic (e.g. 10.128.0.1) uses the tunnel
//! 4. On teardown, removes VPN routes, brings down WireGuard, then cleans up bypass routes
//!
//! ## Route Precedence (most specific wins)
//!
//! - 10.128.0.0/9 → VPN interface (VPN server subnet)
//! - 10.0.0.0/8 → WAN gateway (other RFC1918 Class A)
//! - 0.0.0.0/1, 128.0.0.0/1 → VPN interface (catch-all)
//!
//! ## Platform Notes
//!
//! Dynamic routing (using rtnetlink) is not available on macOS.

use async_trait::async_trait;

use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::{event, wireguard};

use std::net::Ipv4Addr;
use std::path::PathBuf;

use super::bypass;
use super::route_ops::RouteOps;
use super::route_ops_macos::DarwinRouteOps;
use super::wg_ops::{RealWgOps, WgOps};
use super::{Error, Routing, VPN_TUNNEL_SUBNET};

const DEFAULT_VPN_ROUTES: &[&str] = &["0.0.0.0/1", "128.0.0.0/1"];

fn vpn_subnet_route() -> String {
    format!("{}/{}", VPN_TUNNEL_SUBNET.0, VPN_TUNNEL_SUBNET.1)
}

/// Builds a static macOS router.
pub fn static_router(
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<impl Routing, Error> {
    Ok(StaticRouter {
        state_home,
        wg_data,
        peer_ips,
        route_ops: DarwinRouteOps,
        wg: RealWgOps,
        bypass_manager: None,
        vpn_routes_added: Vec::new(),
        wg_interface_name: None,
    })
}

/// macOS routing implementation that programs host routes directly before wg-quick up.
///
/// Generic over `R: RouteOps` and `W: WgOps` so tests can inject mock implementations.
pub struct StaticRouter<R: RouteOps, W: WgOps> {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: R,
    wg: W,
    bypass_manager: Option<bypass::BypassRouteManager<R>>,
    /// VPN routes successfully added after wg-quick up (for rollback/teardown).
    vpn_routes_added: Vec<String>,
    /// Resolved WireGuard interface name (e.g. "utun8" on macOS, "wg0_gnosisvpn" on Linux).
    /// Populated after wg_quick_up; None before setup.
    wg_interface_name: Option<String>,
}

#[async_trait]
impl<R: RouteOps + 'static, W: WgOps + 'static> Routing for StaticRouter<R, W> {
    /// Install split-tunnel routing for macOS StaticRouter.
    ///
    /// Uses a phased approach to avoid a race condition where HOPR p2p connections
    /// could briefly drop when the WireGuard interface comes up.
    ///
    /// Phase 1 (before wg-quick up):
    ///   1. Get WAN interface info
    ///   2. Add bypass routes for all peer IPs directly via WAN
    ///   3. Add RFC1918 bypass routes (10.0.0.0/8, etc.) via WAN for LAN access
    ///
    /// Phase 2:
    ///   4. Run wg-quick up with Table = off (no automatic routing)
    ///
    /// Phase 3 (after wg-quick up):
    ///   5. Add VPN routes (default + subnet) programmatically via route_ops
    ///
    async fn setup(&mut self) -> Result<String, Error> {
        if self.bypass_manager.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // Phase 1: Add peer IP bypass routes BEFORE wg-quick up
        let (device, gateway) = self.route_ops.get_default_interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");

        let mut bypass_manager = bypass::BypassRouteManager::new(
            bypass::WanInterface {
                device: device.clone(),
                gateway: gateway.clone(),
            },
            self.peer_ips.clone(),
            self.route_ops.clone(),
        );

        // Add peer IP and RFC1918 bypass routes (auto-rollback on failure)
        bypass_manager.setup_peer_routes().await?;
        bypass_manager.setup_rfc1918_routes().await?;

        // Phase 2: wg-quick up with Table = off only (no PostUp hooks)
        let extra = vec!["Table = off".to_string()];

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        let iface_name = match self.wg.wg_quick_up(self.state_home.clone(), wg_quick_content).await {
            Ok(name) => name,
            Err(e) => {
                tracing::warn!("wg-quick up failed, rolling back peer IP bypass routes");
                bypass_manager.rollback().await;
                return Err(e);
            }
        };
        self.wg_interface_name = Some(iface_name);
        let iface = self.wg_interface_name.as_deref().unwrap_or(wireguard::WG_INTERFACE);
        tracing::debug!(interface = %iface, "wg-quick up");

        // Phase 3: Add VPN routes programmatically
        let vpn_subnet_route = vpn_subnet_route();
        for route_dest in DEFAULT_VPN_ROUTES
            .iter()
            .copied()
            .chain(std::iter::once(vpn_subnet_route.as_str()))
        {
            if let Err(e) = self.route_ops.route_add(route_dest, None, iface).await {
                tracing::warn!(%e, route = route_dest, "VPN route failed, rolling back");
                // Rollback VPN routes added so far
                for added in self.vpn_routes_added.drain(..).rev() {
                    if let Err(del_err) = self.route_ops.route_del(&added, iface).await {
                        tracing::warn!(%del_err, route = %added, "failed to rollback VPN route");
                    }
                }
                // Bring down WireGuard
                if let Err(wg_err) = self.wg.wg_quick_down(self.state_home.clone(), Logs::Suppress).await {
                    tracing::warn!(%wg_err, "rollback failed: could not bring down WireGuard");
                }
                // Rollback bypass routes
                bypass_manager.rollback().await;
                return Err(e);
            }
            self.vpn_routes_added.push((*route_dest).to_string());
        }
        tracing::debug!(routes = ?self.vpn_routes_added, "VPN routes added");

        self.bypass_manager = Some(bypass_manager);
        tracing::info!("routing is ready (macOS static)");
        Ok(iface.to_string())
    }

    /// Teardown split-tunnel routing for macOS StaticRouter.
    ///
    /// Teardown order:
    /// 1. Remove VPN routes (best-effort)
    /// 2. wg-quick down
    /// 3. Remove bypass routes
    ///
    async fn teardown(&mut self, logs: Logs) {
        let iface = self.wg_interface_name.as_deref().unwrap_or(wireguard::WG_INTERFACE);

        // Remove VPN routes (best-effort, warn on failure)
        for route in self.vpn_routes_added.drain(..) {
            if let Err(e) = self.route_ops.route_del(&route, iface).await {
                tracing::warn!(route = %route, %e, "failed to remove VPN route during teardown");
            }
        }

        // wg-quick down
        match self.wg.wg_quick_down(self.state_home.clone(), logs).await {
            Ok(_) => tracing::debug!("wg-quick down"),
            Err(error) => tracing::warn!(?error, "wg-quick down failed, continuing with bypass route cleanup"),
        }

        // Remove bypass routes (always, even if wg-quick down failed)
        if let Some(ref mut bypass_manager) = self.bypass_manager {
            bypass_manager.teardown().await;
        }
        self.bypass_manager = None;
    }
}
