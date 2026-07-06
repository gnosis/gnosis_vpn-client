//! macOS routing implementation for split-tunnel VPN behavior.
//!
//! Provides a [`StaticRouter`] that:
//! 1. Adds bypass routes for peer IPs and RFC1918 networks BEFORE creating the TUN
//!    (avoids a race for both HOPR traffic and LAN access)
//! 2. Creates a `utun` device and assigns its address + MTU (replaces `wg-quick up`)
//! 3. Adds VPN split routes (`0.0.0.0/1`, `128.0.0.0/1`) and VPN subnet (`10.128.0.0/9`)
//!    via the resolved utun interface, then blackholes IPv6 and pushes DNS
//! 4. On teardown: removes VPN routes + IPv6 blackhole, restores DNS, closes the
//!    TUN fd, removes bypass routes
//!
//! ## Route Precedence
//! Route specificity handles all traffic without extra routing tables:
//! `/32` (peer) > `/12`–`/16` (RFC1918) > `/9` (VPN subnet) > `/8` (RFC1918) > `/1` (VPN default) > `/0` (WAN)

use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::wireguard;

use std::net::Ipv4Addr;
use std::os::fd::RawFd;

use super::route_ops::{RouteOps, WanRoute};
use super::route_ops_macos::DarwinRouteOps;
use super::{Error, RFC1918_BYPASS_NETS, Routing, VPN_TUNNEL_SUBNET, dns, ipv6_blackhole, sweep, tun};

/// Public IP used to identify the WAN route and detect DHCP reassignments.
const PUBLIC_INTERNET_ADDRESS: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

/// VPN split routes: two /1 halves cover all IPv4 space.
/// More specific than the WAN /0 default, routing all non-bypass internet traffic into the tunnel.
const VPN_SPLIT_ROUTES: &[(&str, u8)] = &[("0.0.0.0", 1), ("128.0.0.0", 1)];

/// Builds a static macOS router.
pub fn static_router(
    interface_address: String,
    mtu: u32,
    dns: Option<String>,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<impl Routing, Error> {
    Ok(StaticRouter {
        interface_address,
        mtu,
        dns,
        dns_mechanism: None,
        peer_ips,
        route_ops: DarwinRouteOps,
        active_bypass_routes: Vec::new(),
        tun: None,
        wg_interface_name: None,
        wan_info: None,
    })
}

/// macOS static router using route operations via the `route` command.
///
/// All routing is owned explicitly by this struct via `RouteOps`:
/// - bypass routes (peer IPs + RFC1918) via WAN — added before the TUN is created
/// - VPN split routes (`0.0.0.0/1`, `128.0.0.0/1`) + VPN subnet via utun — static after setup
struct StaticRouter {
    /// The tunnel interface address (e.g. `10.128.0.5/32`) assigned to the utun.
    interface_address: String,
    /// Tunnel MTU (1420).
    mtu: u32,
    /// DNS servers to push (comma-separated), or `None` to leave DNS unmanaged.
    dns: Option<String>,
    /// Resolver mechanism that took effect at setup; teardown reverses exactly this one.
    dns_mechanism: Option<dns::Mechanism>,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: DarwinRouteOps,
    /// Bypass routes currently installed: (dest_cidr, wan_device).
    active_bypass_routes: Vec<(String, String)>,
    /// The created TUN device; holding it keeps root's fd (and the interface) alive.
    tun: Option<tun::Tun>,
    /// Resolved utun interface name (e.g. "utun8"); populated after the TUN is created.
    wg_interface_name: Option<String>,
    /// WAN route snapshot captured at setup time, for `wan_changed()`.
    wan_info: Option<WanRoute>,
}

impl StaticRouter {
    fn vpn_interface(&self) -> String {
        self.wg_interface_name
            .clone()
            .unwrap_or_else(|| wireguard::WG_INTERFACE.to_string())
    }

    async fn setup_vpn_routes(&self, iface: &str) -> Result<(), Error> {
        for (net, prefix) in VPN_SPLIT_ROUTES {
            let cidr = format!("{}/{}", net, prefix);
            let _ = self.route_ops.route_del(&cidr, iface).await;
            self.route_ops.route_add(&cidr, None, iface).await?;
        }
        let (net, prefix) = VPN_TUNNEL_SUBNET;
        let cidr = format!("{}/{}", net, prefix);
        let _ = self.route_ops.route_del(&cidr, iface).await;
        self.route_ops.route_add(&cidr, None, iface).await
    }

    async fn remove_vpn_routes(&self) {
        let iface = self.vpn_interface();
        let vpn_routes = [("0.0.0.0", 1u8), ("128.0.0.0", 1u8), VPN_TUNNEL_SUBNET];
        for (net, prefix) in &vpn_routes {
            let cidr = format!("{}/{}", net, prefix);
            if let Err(e) = self.route_ops.route_del(&cidr, &iface).await {
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

/// Assign the tunnel address + MTU to the utun and bring it up. utun is
/// point-to-point, so the /32 VPN address is used for both local and peer.
async fn configure_interface(iface: &str, interface_address: &str, mtu: u32) -> Result<(), Error> {
    let ip = interface_address.split('/').next().unwrap_or(interface_address);
    Command::new("ifconfig")
        .args([iface, "inet", ip, ip, "up"])
        .run(Logs::Print)
        .await?;
    Command::new("ifconfig")
        .args([iface, "mtu", &mtu.to_string()])
        .run(Logs::Print)
        .await?;
    Ok(())
}

#[async_trait]
impl Routing for StaticRouter {
    /// Install split-tunnel routing.
    ///
    /// Phase 1 (before the TUN): add bypass routes via WAN
    ///   - Peer IP /32 routes (hard-fail: rollback all on error)
    ///   - RFC1918 bypass routes (soft-fail: warn and continue)
    ///
    /// Phase 2: create the utun and assign its address + MTU
    ///   - On failure: rollback Phase 1 bypass routes
    ///
    /// Phase 3 (after the TUN): add VPN routes via the resolved utun interface
    ///   - On failure: remove partial VPN routes, drop the TUN, rollback bypass routes
    ///
    /// Phase 4: IPv6 blackhole + DNS (best-effort leak protection)
    async fn setup(&mut self) -> Result<String, Error> {
        let wan_route = self
            .route_ops
            .get_wan_route_for(PUBLIC_INTERNET_ADDRESS, wireguard::WG_INTERFACE)
            .await?
            .ok_or(Error::NoInterface)?;
        let device = wan_route.device.clone();
        let gateway = wan_route.gateway.clone();
        tracing::debug!(device = %device, gateway = ?gateway, src_ip = ?wan_route.src_ip, "WAN interface for bypass routes");

        // Phase 1: bypass routes before the TUN (avoids race with HOPR p2p connections)
        for ip in self.peer_ips.clone() {
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

        // Phase 2: create and configure the utun (replaces wg-quick up)
        let tun_device = match tun::Tun::create("utun") {
            Ok(t) => t,
            Err(e) => {
                self.rollback_bypass_routes().await;
                return Err(e);
            }
        };
        let interface_name = tun_device.name().to_string();
        if let Err(e) = configure_interface(&interface_name, &self.interface_address, self.mtu).await {
            self.rollback_bypass_routes().await;
            return Err(e);
        }
        self.wg_interface_name = Some(interface_name.clone());
        self.tun = Some(tun_device);
        tracing::debug!(%interface_name, "utun created and configured");

        // Phase 3: VPN routes via utun (split defaults + VPN subnet override)
        if let Err(e) = self.setup_vpn_routes(&interface_name).await {
            self.remove_vpn_routes().await;
            self.tun = None;
            self.rollback_bypass_routes().await;
            return Err(e);
        }

        // Phase 4: IPv6 blackhole + DNS (previously handled inside wg-quick)
        ipv6_blackhole::add().await;
        self.dns_mechanism = match self.dns.clone() {
            Some(servers) => dns::set(&interface_name, &servers).await,
            None => None,
        };
        // Record what was applied so a SIGKILLed root can be swept at next start.
        sweep::record(&sweep::TeardownState {
            interface_name: interface_name.clone(),
            dns_mechanism_applied: self.dns_mechanism,
            blackholes_added: true,
        });

        self.wan_info = Some(wan_route);
        tracing::info!("routing is ready (macOS static)");
        Ok(interface_name)
    }

    /// Teardown split-tunnel routing.
    ///
    /// 1. Remove VPN routes (utun) — warn on error, continue
    /// 2. Remove the IPv6 blackhole and restore DNS
    /// 3. Close the TUN fd (root's copy) — after routes are gone
    /// 4. Remove bypass routes (WAN) — warn on error, continue
    async fn teardown(&mut self, _logs: Logs) {
        self.remove_vpn_routes().await;
        ipv6_blackhole::remove().await;
        // DNS restore must target the recorded utunN name: the compile-time fallback
        // from vpn_interface() can never exist as a scutil service key on macOS.
        match (self.wg_interface_name.clone(), self.dns_mechanism.take()) {
            (Some(iface), Some(mechanism)) => dns::restore(&iface, mechanism).await,
            (None, Some(_)) => tracing::warn!("skipping DNS restore: utun interface name not recorded"),
            _ => {}
        }
        // Drop root's fd last so routes are removed before the interface can vanish.
        self.tun = None;
        for (dest, device) in self.active_bypass_routes.drain(..).collect::<Vec<_>>() {
            if let Err(e) = self.route_ops.route_del(&dest, &device).await {
                tracing::warn!(%e, dest = %dest, device = %device, "failed to remove bypass route");
            }
        }
        self.wg_interface_name = None;
        self.wan_info = None;
        sweep::clear();
        tracing::info!("routing teardown complete");
    }

    fn tun_fd(&self) -> Option<RawFd> {
        self.tun.as_ref().map(|t| t.as_raw_fd())
    }

    async fn wan_changed(&mut self) -> Result<bool, Error> {
        let Some(ref snapshot) = self.wan_info else {
            return Ok(true);
        };
        let current = self
            .route_ops
            .get_route_via_device(PUBLIC_INTERNET_ADDRESS, &snapshot.device)
            .await?;
        match current {
            None => Ok(true),
            Some(r) => Ok(r.src_ip != snapshot.src_ip || r.gateway != snapshot.gateway),
        }
    }

    async fn add_peer_bypass_route(&mut self, ip: Ipv4Addr) -> Result<(), Error> {
        let Some(ref wan) = self.wan_info else {
            return Ok(());
        };
        let device = wan.device.clone();
        let gateway = wan.gateway.clone();
        let dest = ip.to_string();
        let _ = self.route_ops.route_del(&dest, &device).await;
        self.route_ops.route_add(&dest, gateway.as_deref(), &device).await?;
        self.active_bypass_routes.push((dest, device));
        Ok(())
    }

    async fn remove_peer_bypass_route(&mut self, ip: Ipv4Addr) -> Result<(), Error> {
        let Some(ref wan) = self.wan_info else {
            return Ok(());
        };
        let dest = ip.to_string();
        let device = wan.device.clone();
        if let Err(e) = self.route_ops.route_del(&dest, &device).await {
            tracing::warn!(%e, %ip, "failed to remove dynamic peer bypass route");
        }
        self.active_bypass_routes.retain(|(d, _)| d != &dest);
        Ok(())
    }
}
