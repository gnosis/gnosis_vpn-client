//! macOS routing implementation for split-tunnel VPN behavior.
//!
//! Provides a [`StaticRouter`] that:
//! 1. Adds bypass routes for peer IPs and RFC1918 networks BEFORE bringing up WireGuard
//!    (avoids race condition for both HOPR traffic and LAN access)
//! 2. Runs `wg-quick up` with `Table = off` to prevent automatic routing
//! 3. Uses PostUp hooks to add VPN-specific routes:
//!    - Default routes (0.0.0.0/1 and 128.0.0.0/1) through VPN
//!    - VPN subnet route (10.128.0.0/9) through VPN - overrides the 10.0.0.0/8 bypass
//!      so VPN server traffic (e.g. 10.128.0.1) uses the tunnel
//! 4. On teardown, brings down WireGuard first, then cleans up all bypass routes
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

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::shell_command_ext::Logs;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use super::route_ops::RouteOps;
use super::route_ops_macos::DarwinRouteOps;
use super::wg_ops::{RealWgOps, WgOps};
use super::{Error, Routing, VPN_TUNNEL_SUBNET};

/// WAN interface information stub for macOS (never used since dynamic routing is not available).
#[derive(Debug, Clone)]
pub struct WanInfo;

/// Dynamic routing not available on macOS.
pub fn dynamic_router(
    _state_home: Arc<PathBuf>,
    _wg_data: event::WireGuardData,
    _wan_info: WanInfo,
) -> Result<DynamicRouter, Error> {
    Err(Error::NotAvailable)
}

pub struct DynamicRouter {}

/// Builds a static macOS router.
pub fn static_router(
    state_home: Arc<PathBuf>,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> StaticRouter<DarwinRouteOps, RealWgOps> {
    StaticRouter {
        state_home,
        wg_data,
        peer_ips,
        route_ops: DarwinRouteOps,
        wg: RealWgOps,
        bypass_manager: None,
    }
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
    ///   4. Run wg-quick up with PostUp hooks for VPN routes (default + subnet)
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

        // Phase 2: wg-quick up with PostUp hooks for VPN routes only
        // Table = off prevents wg-quick from managing routes automatically
        // PostUp hooks add VPN-specific routes AFTER interface is established
        let extra = vec![
            "Table = off".to_string(),
            // VPN default routes (catch-all via tunnel)
            "PostUp = route -n add -inet 0.0.0.0/1 -interface %i".to_string(),
            "PostUp = route -n add -inet 128.0.0.0/1 -interface %i".to_string(),
            // VPN internal subnet (more specific than 10.0.0.0/8 bypass)
            format!(
                "PostUp = route -n add -inet {}/{} -interface %i",
                VPN_TUNNEL_SUBNET.0, VPN_TUNNEL_SUBNET.1
            ),
        ];

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        if let Err(e) = self
            .wg
            .wg_quick_up((*self.state_home).clone(), wg_quick_content)
            .await
        {
            tracing::warn!("wg-quick up failed, rolling back peer IP bypass routes");
            bypass_manager.rollback().await;
            return Err(e);
        }
        tracing::debug!("wg-quick up");

        self.bypass_manager = Some(bypass_manager);
        tracing::info!("routing is ready (macOS static)");
        Ok(())
    }

    /// Teardown split-tunnel routing for macOS StaticRouter.
    ///
    /// Teardown order is important: wg-quick down first, then remove bypass routes.
    /// This ensures HOPR traffic continues to flow via WAN while VPN is being torn down.
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        // wg-quick down first
        let wg_result = self
            .wg
            .wg_quick_down((*self.state_home).clone(), logs)
            .await;
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

/// Dynamic routing not available on macOS.
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

    fn make_static_router(
        route_ops: MockRouteOps,
        wg: MockWgOps,
    ) -> StaticRouter<MockRouteOps, MockWgOps> {
        StaticRouter {
            state_home: Arc::new(PathBuf::from("/tmp/test")),
            wg_data: test_wg_data(),
            peer_ips: vec![
                Ipv4Addr::new(1, 2, 3, 4),
                Ipv4Addr::new(5, 6, 7, 8),
            ],
            route_ops,
            wg,
            bypass_manager: None,
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

        // Peer IP routes + RFC1918 routes
        assert_eq!(state.added_routes.len(), 6);
        assert_eq!(state.added_routes[0].0, "1.2.3.4");
        assert_eq!(state.added_routes[1].0, "5.6.7.8");
        assert_eq!(state.added_routes[2].0, "10.0.0.0/8");

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

    #[test]
    fn parses_interface_gateway() -> anyhow::Result<()> {
        let output = r#"
           route to: default
        destination: default
               mask: default
            gateway: 192.168.178.1
          interface: en1
              flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
         recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire
               0         0         0         0         0         0      1500         0
        "#;

        let (device, gateway) =
            super::super::parse_key_value_output(output, "interface:", "gateway:", Some(":"))?;

        assert_eq!(device, "en1");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }

    #[test]
    fn parses_interface_no_gateway_with_index() -> anyhow::Result<()> {
        // When VPN is active, gateway may show as "index: N" instead of an IP
        let output = r#"
           route to: default
        destination: default
               mask: default
            gateway: index: 28
          interface: utun8
              flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
        "#;

        let (device, gateway) =
            super::super::parse_key_value_output(output, "interface:", "gateway:", Some(":"))?;

        assert_eq!(device, "utun8");
        assert_eq!(gateway, None); // Should be None, not "index:"
        Ok(())
    }
}
