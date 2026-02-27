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
use gnosis_vpn_lib::{event, worker, wireguard};

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use super::route_ops::RouteOps;
use super::route_ops_macos::DarwinRouteOps;
use super::wg_ops::{RealWgOps, WgOps};
use super::{Error, Routing, VPN_TUNNEL_SUBNET};

const DEFAULT_VPN_ROUTES: &[&str] = &["0.0.0.0/1", "128.0.0.0/1"];

fn vpn_subnet_route() -> String {
    format!("{}/{}", VPN_TUNNEL_SUBNET.0, VPN_TUNNEL_SUBNET.1)
}


/// Dynamic routing not available on macOS.
pub async fn dynamic_router(
    _state_home: Arc<PathBuf>,
    _worker: worker::Worker,
    _wg_data: event::WireGuardData,
) -> Result<DynamicRouter, Error> {
    Err(Error::NotAvailable)
}

pub struct DynamicRouter {}

/// Builds a static macOS router.
pub fn static_router(
    state_home: Arc<PathBuf>,
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
    state_home: Arc<PathBuf>,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: R,
    wg: W,
    bypass_manager: Option<super::BypassRouteManager<R>>,
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
    async fn setup(&mut self) -> Result<(), Error> {
        if self.bypass_manager.is_some() {
            return Err(Error::General("invalid state: already set up".into()));
        }

        // Phase 1: Add peer IP bypass routes BEFORE wg-quick up
        let (device, gateway) = self.route_ops.get_default_interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");

        let mut bypass_manager = super::BypassRouteManager::new(
            super::WanInterface {
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

        let iface_name = match self.wg.wg_quick_up((*self.state_home).clone(), wg_quick_content).await {
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
                tracing::warn!(route = route_dest, %e, "VPN route failed, rolling back");
                // Rollback VPN routes added so far
                for added in self.vpn_routes_added.drain(..).rev() {
                    if let Err(del_err) = self.route_ops.route_del(&added, iface).await {
                        tracing::warn!(route = %added, %del_err, "failed to rollback VPN route");
                    }
                }
                // Bring down WireGuard
                if let Err(wg_err) = self.wg.wg_quick_down((*self.state_home).clone(), Logs::Suppress).await {
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
        Ok(())
    }

    /// Teardown split-tunnel routing for macOS StaticRouter.
    ///
    /// Teardown order:
    /// 1. Remove VPN routes (best-effort)
    /// 2. wg-quick down
    /// 3. Remove bypass routes
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        let iface = self.wg_interface_name.as_deref().unwrap_or(wireguard::WG_INTERFACE);

        // Remove VPN routes (best-effort, warn on failure)
        for route in self.vpn_routes_added.drain(..) {
            if let Err(e) = self.route_ops.route_del(&route, iface).await {
                tracing::warn!(route = %route, %e, "failed to remove VPN route during teardown");
            }
        }

        // wg-quick down
        let wg_result = self.wg.wg_quick_down((*self.state_home).clone(), logs).await;
        if let Err(ref e) = wg_result {
            tracing::warn!(%e, "wg-quick down failed, continuing with bypass route cleanup");
        } else {
            tracing::debug!("wg-quick down");
        }

        // Remove bypass routes (always, even if wg-quick down failed)
        if let Some(ref mut bypass_manager) = self.bypass_manager {
            bypass_manager.teardown().await;
        }
        self.bypass_manager = None;

        wg_result
    }
}

#[async_trait]
impl Routing for DynamicRouter {
    async fn setup(&mut self) -> Result<(), Error> {
        Err(Error::NotAvailable)
    }

    async fn teardown(&mut self, _logs: Logs) -> Result<(), Error> {
        Err(Error::NotAvailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::mocks::*;

    fn make_static_router(route_ops: MockRouteOps, wg: MockWgOps) -> StaticRouter<MockRouteOps, MockWgOps> {
        StaticRouter {
            state_home: Arc::new(PathBuf::from("/tmp/test")),
            wg_data: test_wg_data(),
            peer_ips: vec![Ipv4Addr::new(1, 2, 3, 4), Ipv4Addr::new(5, 6, 7, 8)],
            route_ops,
            wg,
            bypass_manager: None,
            vpn_routes_added: Vec::new(),
            wg_interface_name: None,
        }
    }

    fn test_wg_data() -> event::WireGuardData {
        use gnosis_vpn_lib::wireguard;
        event::WireGuardData {
            wg: wireguard::WireGuard::new(
                wireguard::Config {
                    listen_port: Some(51820),
                    allowed_ips: Some("0.0.0.0/0".into()),
                    force_private_key: None,
                },
                wireguard::KeyPair {
                    priv_key: "test_priv_key".into(),
                    public_key: "test_pub_key".into(),
                },
            ),
            interface_info: wireguard::InterfaceInfo {
                address: "10.128.0.5/32".into(),
            },
            peer_info: wireguard::PeerInfo {
                public_key: "test_peer_pub_key".into(),
                preshared_key: "test_psk".into(),
                endpoint: "1.2.3.4:51820".into(),
            },
        }
    }

    #[tokio::test]
    async fn static_router_setup_adds_bypass_routes_then_wg_up() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_static_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();

        let state = route_ops.state.lock().unwrap();

        // 2 peer IP + 4 RFC1918 bypass + 3 VPN routes = 9 total
        assert_eq!(state.added_routes.len(), 9);
        assert_eq!(state.added_routes[0].0, "1.2.3.4");
        assert_eq!(state.added_routes[1].0, "5.6.7.8");
        assert_eq!(state.added_routes[2].0, "10.0.0.0/8");

        // VPN routes (last 3)
        assert_eq!(state.added_routes[6].0, "0.0.0.0/1");
        assert_eq!(state.added_routes[7].0, "128.0.0.0/1");
        assert_eq!(state.added_routes[8].0, "10.128.0.0/9");

        // VPN routes go through the WG interface
        assert_eq!(state.added_routes[6].2, wireguard::WG_INTERFACE);
        assert_eq!(state.added_routes[7].2, wireguard::WG_INTERFACE);
        assert_eq!(state.added_routes[8].2, wireguard::WG_INTERFACE);

        // WG should be up
        let wg_state = wg.state.lock().unwrap();
        assert!(wg_state.wg_up);
    }

    #[tokio::test]
    async fn static_router_wg_failure_rolls_back_bypass_routes() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::with_state(WgState {
            fail_on: {
                let mut m = std::collections::HashMap::new();
                m.insert("wg_quick_up".into(), "simulated wg failure".into());
                m
            },
            ..Default::default()
        });

        let mut router = make_static_router(route_ops.clone(), wg);
        let result = router.setup().await;
        assert!(result.is_err());

        // Bypass routes should be rolled back
        let state = route_ops.state.lock().unwrap();
        assert!(state.added_routes.is_empty());
    }

    #[tokio::test]
    async fn static_router_teardown_wg_down_then_bypass_cleanup() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_static_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();
        router.teardown(Logs::Suppress).await.unwrap();

        let state = route_ops.state.lock().unwrap();
        assert!(state.added_routes.is_empty());

        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up);
    }

    #[tokio::test]
    async fn static_router_teardown_cleans_bypass_even_if_wg_down_fails() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_static_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();

        // Make wg_quick_down fail
        {
            let mut s = wg.state.lock().unwrap();
            s.fail_on
                .insert("wg_quick_down".into(), "simulated wg down failure".into());
        }

        let result = router.teardown(Logs::Suppress).await;
        assert!(result.is_err());

        // But bypass routes should still be cleaned up
        let state = route_ops.state.lock().unwrap();
        assert!(state.added_routes.is_empty());
    }

    #[tokio::test]
    async fn setup_wg_config_has_no_routing_postup() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_static_router(route_ops, wg.clone());
        router.setup().await.unwrap();

        let wg_state = wg.state.lock().unwrap();
        let config = wg_state.last_wg_config.as_ref().unwrap();
        // IPv6 blackhole PostUp for leak prevention is expected,
        // but routing-related PostUp hooks should not be present
        assert!(
            !config.contains("PostUp = route"),
            "wg config should not contain routing PostUp hooks, got:\n{config}"
        );
    }

    #[tokio::test]
    async fn setup_adds_vpn_routes_after_wg_up() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_static_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();

        let state = route_ops.state.lock().unwrap();

        // Check the VPN routes exist
        let vpn_routes: Vec<_> = state
            .added_routes
            .iter()
            .filter(|(_, _, dev)| dev == wireguard::WG_INTERFACE)
            .collect();
        assert_eq!(vpn_routes.len(), 3);
        assert_eq!(vpn_routes[0].0, "0.0.0.0/1");
        assert_eq!(vpn_routes[1].0, "128.0.0.0/1");
        assert_eq!(vpn_routes[2].0, "10.128.0.0/9");

        // No gateway for VPN routes
        assert!(vpn_routes[0].1.is_none());
        assert!(vpn_routes[1].1.is_none());
        assert!(vpn_routes[2].1.is_none());
    }

    #[tokio::test]
    async fn setup_rolls_back_vpn_routes_on_failure() {
        // Fail on the VPN subnet route (third VPN route)
        // Bypass routes and first two VPN routes should succeed
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            fail_on_route_dest: {
                let mut m = std::collections::HashMap::new();
                m.insert("10.128.0.0/9".into(), "simulated VPN subnet route failure".into());
                m
            },
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_static_router(route_ops.clone(), wg.clone());
        let result = router.setup().await;
        assert!(result.is_err(), "setup should fail when VPN subnet route fails");

        // WG should be brought back down (rollback)
        let wg_state = wg.state.lock().unwrap();
        assert!(!wg_state.wg_up, "WG should be down after rollback");

        // All routes should be cleaned up (VPN routes rolled back + bypass routes rolled back)
        let state = route_ops.state.lock().unwrap();
        assert!(
            state.added_routes.is_empty(),
            "all routes should be rolled back, got: {:?}",
            state.added_routes
        );
    }

    #[tokio::test]
    async fn teardown_removes_vpn_routes() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("en0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_static_router(route_ops.clone(), wg.clone());
        router.setup().await.unwrap();

        // Verify VPN routes exist before teardown
        {
            let state = route_ops.state.lock().unwrap();
            let vpn_count = state
                .added_routes
                .iter()
                .filter(|(_, _, dev)| dev == wireguard::WG_INTERFACE)
                .count();
            assert_eq!(vpn_count, 3, "VPN routes should exist before teardown");
        }

        router.teardown(Logs::Suppress).await.unwrap();

        // All routes should be cleaned up (VPN + bypass)
        let state = route_ops.state.lock().unwrap();
        assert!(
            state.added_routes.is_empty(),
            "all routes should be removed after teardown"
        );

        // VPN routes tracker should be empty
        assert!(router.vpn_routes_added.is_empty());
    }
}
