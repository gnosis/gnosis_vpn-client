//! Linux routing implementation for split-tunnel VPN behavior.
//!
//! Provides a [`StaticRouter`] that:
//! 1. Adds bypass routes for peer IPs and RFC1918 networks BEFORE bringing up WireGuard
//! 2. Runs `wg-quick up` with `Table = off` (no automatic routing)
//! 3. Adds VPN split routes (`0.0.0.0/1`, `128.0.0.0/1`) and VPN subnet (`10.128.0.0/9`) via wg0
//! 4. On teardown: removes VPN routes, brings down WireGuard, removes bypass routes
//!
//! ## Route Precedence
//! Route specificity handles all traffic without ip rules or extra routing tables:
//! `/32` (peer) > `/12`–`/16` (RFC1918) > `/9` (VPN subnet) > `/8` (RFC1918) > `/1` (VPN default) > `/0` (WAN)

use async_trait::async_trait;

use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::{event, wireguard};

use std::net::Ipv4Addr;
use std::path::PathBuf;

use super::route_ops::RouteOps;
use super::route_ops_linux::NetlinkRouteOps;
use super::wg_ops::{RealWgOps, WgOps};
use super::{Error, RFC1918_BYPASS_NETS, Routing, VPN_TUNNEL_SUBNET};

/// VPN split routes: two /1 halves cover all IPv4 space.
/// More specific than the WAN /0 default, routing all non-bypass internet traffic into the tunnel.
const VPN_SPLIT_ROUTES: &[(&str, u8)] = &[("0.0.0.0", 1), ("128.0.0.0", 1)];

/// Builds a static Linux router.
pub fn static_router(
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<impl Routing, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let route_ops = NetlinkRouteOps::new(handle);
    let wg = RealWgOps;
    Ok(StaticRouter {
        state_home: state_home.to_path_buf(),
        wg_data,
        peer_ips,
        route_ops,
        wg,
        wan_info: None,
        active_bypass_routes: Vec::new(),
    })
}

/// Linux static router using route operations via netlink.
///
/// Uses `Table = off` so wg-quick only creates the WireGuard interface.
/// All routing is owned explicitly by this struct via `RouteOps`:
/// - bypass routes (peer IPs + RFC1918) via WAN — added before wg-quick up
/// - VPN split routes (`0.0.0.0/1`, `128.0.0.0/1`) + VPN subnet via wg0 — static after setup
///
/// Generic over `R: RouteOps` and `W: WgOps` so tests can inject mock implementations.
pub struct StaticRouter<R: RouteOps, W: WgOps> {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: R,
    wg: W,
    /// WAN interface captured at setup time.
    wan_info: Option<(String, Option<String>)>,
    /// Bypass routes currently installed: (dest_cidr, wan_device).
    /// Tracked for explicit cleanup since the wg-quick config has no PreDown scripts.
    active_bypass_routes: Vec<(String, String)>,
}

impl<R: RouteOps, W: WgOps> StaticRouter<R, W> {
    async fn setup_vpn_routes(&self) -> Result<(), Error> {
        for (net, prefix) in VPN_SPLIT_ROUTES {
            let cidr = format!("{}/{}", net, prefix);
            let _ = self.route_ops.route_del(&cidr, wireguard::WG_INTERFACE).await;
            self.route_ops.route_add(&cidr, None, wireguard::WG_INTERFACE).await?;
        }
        let (net, prefix) = VPN_TUNNEL_SUBNET;
        let cidr = format!("{}/{}", net, prefix);
        let _ = self.route_ops.route_del(&cidr, wireguard::WG_INTERFACE).await;
        self.route_ops.route_add(&cidr, None, wireguard::WG_INTERFACE).await
    }

    async fn remove_vpn_routes(&self) {
        let vpn_routes = [("0.0.0.0", 1u8), ("128.0.0.0", 1u8), VPN_TUNNEL_SUBNET];
        for (net, prefix) in &vpn_routes {
            let cidr = format!("{}/{}", net, prefix);
            if let Err(e) = self.route_ops.route_del(&cidr, wireguard::WG_INTERFACE).await {
                tracing::warn!(%e, cidr = %cidr, "failed to remove VPN route");
            }
        }
    }

    async fn rollback_bypass_routes(&mut self) {
        for (dest, device) in self.active_bypass_routes.drain(..).collect::<Vec<_>>() {
            if let Err(e) = self.route_ops.route_del(&dest, &device).await {
                tracing::warn!(%e, dest = %dest, "failed to remove bypass route during rollback");
            }
        }
    }
}

#[async_trait]
impl<R: RouteOps + 'static, W: WgOps + 'static> Routing for StaticRouter<R, W> {
    /// Install split-tunnel routing.
    ///
    /// Phase 1 (before wg-quick up): add bypass routes via WAN
    ///   - Peer IP /32 routes (hard-fail: rollback all on error)
    ///   - RFC1918 bypass routes (soft-fail: warn and continue)
    ///
    /// Phase 2: wg-quick up with Table = off (no automatic routing)
    ///   - On failure: rollback Phase 1 bypass routes
    ///
    /// Phase 3 (after wg-quick up): add VPN routes via wg0
    ///   - `0.0.0.0/1` and `128.0.0.0/1` override the WAN default for all internet traffic
    ///   - `10.128.0.0/9` overrides the `10.0.0.0/8` RFC1918 bypass for VPN server traffic
    ///   - On failure: remove partial VPN routes, wg-quick down, rollback bypass routes
    async fn setup(&mut self) -> Result<String, Error> {
        let (device, gateway) = self.route_ops.get_default_interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface for bypass routes");

        // Phase 1: bypass routes before wg-quick up (avoids race with HOPR p2p connections)
        for ip in &self.peer_ips.clone() {
            let dest = ip.to_string();
            let _ = self.route_ops.route_del(&dest, &device).await;
            if let Err(e) = self.route_ops.route_add(&dest, gateway.as_deref(), &device).await {
                self.rollback_bypass_routes().await;
                return Err(e);
            }
            self.active_bypass_routes.push((dest, device.clone()));
        }
        for (net, prefix) in RFC1918_BYPASS_NETS {
            let cidr = format!("{}/{}", net, prefix);
            let _ = self.route_ops.route_del(&cidr, &device).await;
            match self.route_ops.route_add(&cidr, gateway.as_deref(), &device).await {
                Ok(_) => self.active_bypass_routes.push((cidr, device.clone())),
                Err(e) => tracing::warn!(%e, cidr = %cidr, "RFC1918 bypass route failed, continuing"),
            }
        }

        // Phase 2: wg-quick up with Table = off
        let wg_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            vec!["Table = off".to_string()],
        );
        let interface_name = match self.wg.wg_quick_up(self.state_home.clone(), wg_content).await {
            Ok(n) => n,
            Err(e) => {
                self.rollback_bypass_routes().await;
                return Err(e);
            }
        };
        tracing::debug!(%interface_name, "wg-quick up");

        // Phase 3: VPN routes via wg0 (split defaults + VPN subnet override)
        if let Err(e) = self.setup_vpn_routes().await {
            self.remove_vpn_routes().await;
            let _ = self.wg.wg_quick_down(self.state_home.clone(), Logs::Suppress).await;
            self.rollback_bypass_routes().await;
            return Err(e);
        }

        self.wan_info = Some((device, gateway));
        tracing::info!("routing is ready (linux static)");
        Ok(interface_name)
    }

    /// Teardown split-tunnel routing.
    ///
    /// 1. Remove VPN routes (wg0) — warn on error, continue
    /// 2. wg-quick down
    /// 3. Remove bypass routes (WAN) — warn on error, continue
    async fn teardown(&mut self, logs: Logs) {
        self.remove_vpn_routes().await;
        match self.wg.wg_quick_down(self.state_home.clone(), logs).await {
            Ok(_) => tracing::debug!("wg-quick down"),
            Err(error) => tracing::warn!(?error, "wg-quick down failed during teardown"),
        }
        for (dest, device) in self.active_bypass_routes.drain(..).collect::<Vec<_>>() {
            if let Err(e) = self.route_ops.route_del(&dest, &device).await {
                tracing::warn!(%e, dest = %dest, device = %device, "failed to remove bypass route");
            }
        }
        self.wan_info = None;
        tracing::info!("routing teardown complete");
    }

    async fn wan_changed(&mut self) -> Result<bool, Error> {
        let Some((captured_device, captured_gateway)) = &self.wan_info else {
            // no captured WAN means setup never completed — treat as changed
            return Ok(true);
        };
        // Check that the WAN interface used at setup still has a default route.
        // A new interface appearing with a lower metric (e.g. plugging in a cable
        // while WiFi is up) changes the "best" default but does not break the
        // existing bypass routes, which are explicit /32 routes via the old device.
        let still_viable = self
            .route_ops
            .has_default_route(captured_device, captured_gateway.as_deref())
            .await?;
        Ok(!still_viable)
    }
}
