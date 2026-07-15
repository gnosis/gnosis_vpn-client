//! Linux routing implementation for split-tunnel VPN behavior.
//!
//! Provides a [`StaticRouter`] that:
//! 1. Adds bypass routes for peer IPs and RFC1918 networks BEFORE creating the TUN
//! 2. Creates the `wg0_gnosisvpn` TUN and assigns its address + MTU via netlink
//!    (replaces `wg-quick up`)
//! 3. Adds VPN split routes (`0.0.0.0/1`, `128.0.0.0/1`) and VPN subnet
//!    (`10.128.0.0/9`) via the TUN, then blackholes IPv6 and pushes DNS
//! 4. On teardown: removes VPN routes + IPv6 blackhole, restores DNS, closes the
//!    TUN fd, removes bypass routes
//!
//! ## Route Precedence
//! Route specificity handles all traffic without ip rules or extra routing tables:
//! `/32` (peer) > `/12`–`/16` (RFC1918) > `/9` (VPN subnet) > `/8` (RFC1918) > `/1` (VPN default) > `/0` (WAN)

use async_trait::async_trait;
use futures::TryStreamExt;
use rtnetlink::{LinkMessageBuilder, LinkUnspec};

use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::wireguard;

use std::net::{IpAddr, Ipv4Addr};
use std::os::fd::{AsFd, BorrowedFd};

use super::route_ops::{RouteOps, WanRoute};
use super::route_ops_linux::NetlinkRouteOps;
use super::{Error, RFC1918_BYPASS_NETS, Routing, VPN_TUNNEL_SUBNET, dns, ipv6_blackhole, sweep, tun};

/// Public IP used to identify the WAN route and detect DHCP reassignments.
const PUBLIC_INTERNET_ADDRESS: Ipv4Addr = Ipv4Addr::new(1, 1, 1, 1);

/// VPN split routes: two /1 halves cover all IPv4 space.
/// More specific than the WAN /0 default, routing all non-bypass internet traffic into the tunnel.
const VPN_SPLIT_ROUTES: &[(&str, u8)] = &[("0.0.0.0", 1), ("128.0.0.0", 1)];

/// Builds a static Linux router.
pub fn static_router(
    interface_address: String,
    mtu: u32,
    dns: Option<String>,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<impl Routing, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let route_ops = NetlinkRouteOps::new(handle.clone());
    Ok(StaticRouter {
        interface_address,
        mtu,
        dns,
        dns_mechanism: None,
        peer_ips,
        handle,
        route_ops,
        wan_info: None,
        active_bypass_routes: Vec::new(),
        tun: None,
    })
}

/// Linux static router using route operations via netlink.
///
/// All routing is owned explicitly by this struct via `RouteOps`:
/// - bypass routes (peer IPs + RFC1918) via WAN — added before the TUN is created
/// - VPN split routes (`0.0.0.0/1`, `128.0.0.0/1`) + VPN subnet via wg0 — static after setup
struct StaticRouter {
    /// The tunnel interface address (e.g. `10.128.0.5/32`) assigned to the TUN.
    interface_address: String,
    /// Tunnel MTU (1420).
    mtu: u32,
    /// DNS servers to push (comma-separated), or `None` to leave DNS unmanaged.
    dns: Option<String>,
    /// Resolver mechanism that took effect at setup; teardown reverses exactly this one.
    dns_mechanism: Option<dns::Mechanism>,
    peer_ips: Vec<Ipv4Addr>,
    /// Netlink handle used for address + link-state assignment on the TUN.
    handle: rtnetlink::Handle,
    route_ops: NetlinkRouteOps,
    /// WAN route snapshot captured at setup time, for `wan_changed()`.
    wan_info: Option<WanRoute>,
    /// Bypass routes currently installed: (dest_cidr, wan_device).
    active_bypass_routes: Vec<(String, String)>,
    /// The created TUN device; holding it keeps root's fd (and the interface) alive.
    tun: Option<tun::Tun>,
}

impl StaticRouter {
    /// Snapshot the current side effects into the crash-recovery state file. Called
    /// after setup and after every bypass-route change so the persisted set - which a
    /// SIGKILLed root is swept against at next start - always reflects reality.
    fn persist_teardown_state(&self) {
        sweep::record(&sweep::TeardownState {
            interface_name: wireguard::WG_INTERFACE.to_string(),
            dns_mechanism_applied: self.dns_mechanism,
            blackholes_added: true,
            bypass_routes: self.active_bypass_routes.clone(),
        });
    }

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

    /// Assign the tunnel address + MTU to the TUN and bring it up, via netlink.
    async fn configure_interface(&self) -> Result<(), Error> {
        let links: Vec<_> = self
            .handle
            .link()
            .get()
            .match_name(wireguard::WG_INTERFACE.to_string())
            .execute()
            .try_collect()
            .await
            .map_err(|e| Error::General(format!("failed to resolve TUN index: {e}")))?;
        let index = links
            .first()
            .map(|l| l.header.index)
            .ok_or_else(|| Error::General(format!("TUN interface '{}' not found", wireguard::WG_INTERFACE)))?;

        let (ip, prefix) = parse_cidr(&self.interface_address)?;
        // `.replace()` (NLM_F_REPLACE) keeps address assignment idempotent, matching
        // the del-before-add pattern used for routes in this file. Without it the add
        // uses NLM_F_EXCL and fails with EEXIST if the address is still present - e.g.
        // on a reconnect that reattaches to a `wg0_gnosisvpn` a lingering worker fd
        // kept alive.
        self.handle
            .address()
            .add(index, ip, prefix)
            .replace()
            .execute()
            .await
            .map_err(|e| Error::General(format!("failed to assign address {}: {e}", self.interface_address)))?;

        self.handle
            .link()
            .change(
                LinkMessageBuilder::<LinkUnspec>::new()
                    .index(index)
                    .mtu(self.mtu)
                    .up()
                    .build(),
            )
            .execute()
            .await
            .map_err(|e| Error::General(format!("failed to bring up TUN: {e}")))?;
        Ok(())
    }
}

/// Parse an IP network, treating bare IPv4 addresses as /32 and bare IPv6 addresses as /128.
fn parse_cidr(s: &str) -> Result<(IpAddr, u8), Error> {
    match s.split_once('/') {
        Some((addr, prefix)) => {
            let ip: IpAddr = addr
                .parse()
                .map_err(|e| Error::General(format!("invalid address '{addr}': {e}")))?;
            let prefix: u8 = prefix
                .parse()
                .map_err(|e| Error::General(format!("invalid prefix '{prefix}': {e}")))?;
            Ok((ip, prefix))
        }
        None => {
            let ip: IpAddr = s
                .parse()
                .map_err(|e| Error::General(format!("invalid address '{s}': {e}")))?;
            let prefix = match ip {
                IpAddr::V4(_) => 32,
                IpAddr::V6(_) => 128,
            };
            Ok((ip, prefix))
        }
    }
}

#[async_trait]
impl Routing for StaticRouter {
    /// Install split-tunnel routing (see the macOS impl for the phase rationale; the
    /// only Linux difference is netlink address assignment and the fixed interface
    /// name `wg0_gnosisvpn`).
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

        // Phase 2: create and configure the TUN (replaces wg-quick up)
        let tun_device = match tun::Tun::create(wireguard::WG_INTERFACE) {
            Ok(t) => t,
            Err(e) => {
                self.rollback_bypass_routes().await;
                return Err(e);
            }
        };
        let interface_name = tun_device.name().to_string();
        self.tun = Some(tun_device);
        if let Err(e) = self.configure_interface().await {
            self.tun = None;
            self.rollback_bypass_routes().await;
            return Err(e);
        }
        tracing::debug!(%interface_name, "TUN created and configured");

        // Phase 3: VPN routes via wg0 (split defaults + VPN subnet override)
        if let Err(e) = self.setup_vpn_routes().await {
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
        self.persist_teardown_state();

        self.wan_info = Some(wan_route);
        tracing::info!("routing is ready (linux static)");
        Ok(interface_name)
    }

    async fn teardown(&mut self, _logs: Logs) {
        self.remove_vpn_routes().await;
        ipv6_blackhole::remove().await;
        if let Some(mechanism) = self.dns_mechanism.take() {
            dns::restore(wireguard::WG_INTERFACE, mechanism).await;
        }
        // Drop root's fd last so routes are removed before the interface can vanish.
        self.tun = None;
        for (dest, device) in self.active_bypass_routes.drain(..).collect::<Vec<_>>() {
            if let Err(e) = self.route_ops.route_del(&dest, &device).await {
                tracing::warn!(%e, dest = %dest, device = %device, "failed to remove bypass route");
            }
        }
        self.wan_info = None;
        sweep::clear();
        tracing::info!("routing teardown complete");
    }

    fn tun_fd(&self) -> Option<BorrowedFd<'_>> {
        self.tun.as_ref().map(AsFd::as_fd)
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
        self.persist_teardown_state();
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
        self.persist_teardown_state();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_ipv4_address_defaults_to_host_prefix() {
        assert_eq!(parse_cidr("192.0.2.25").unwrap(), ("192.0.2.25".parse().unwrap(), 32));
    }

    #[test]
    fn bare_ipv6_address_defaults_to_host_prefix() {
        assert_eq!(
            parse_cidr("2001:db8::25").unwrap(),
            ("2001:db8::25".parse().unwrap(), 128)
        );
    }

    #[test]
    fn explicit_prefixes_are_unchanged() {
        assert_eq!(parse_cidr("10.128.0.5/9").unwrap(), ("10.128.0.5".parse().unwrap(), 9));
        assert_eq!(
            parse_cidr("2001:db8::25/64").unwrap(),
            ("2001:db8::25".parse().unwrap(), 64)
        );
    }
}
