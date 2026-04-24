//! Linux routing implementation for split-tunnel VPN behavior.
//!
//! Provides the [`FallbackRouter`] for static route-based split-tunnel routing:
//! 1. Embeds PostUp/PreDown hooks in the wg-quick config to add peer IP bypass routes
//! 2. Runs `wg-quick up` with `Table = off` to prevent automatic routing
//! 3. On teardown, brings down WireGuard (which triggers PreDown cleanup)

use async_trait::async_trait;

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::shell_command_ext::Logs;

use std::net::Ipv4Addr;
use std::path::PathBuf;

use super::route_ops::RouteOps;
use super::route_ops_linux::NetlinkRouteOps;
use super::wg_ops::{RealWgOps, WgOps};
use super::{Error, Routing};

// ============================================================================
// Factory Function
// ============================================================================

/// Creates a static router using route operations via netlink.
pub fn static_fallback_router(
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<impl Routing, Error> {
    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::task::spawn(conn);
    let route_ops = NetlinkRouteOps::new(handle);
    let wg = RealWgOps;
    Ok(FallbackRouter {
        state_home: state_home.to_path_buf(),
        wg_data,
        peer_ips,
        route_ops,
        wg,
    })
}

// ============================================================================
// Struct
// ============================================================================

/// Static router using wg-quick PostUp/PreDown hooks for peer IP bypass routes.
pub struct FallbackRouter<R: RouteOps, W: WgOps> {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: R,
    wg: W,
}

// ============================================================================
// Routing Implementation
// ============================================================================

#[async_trait]
impl<R: RouteOps + 'static, W: WgOps + 'static> Routing for FallbackRouter<R, W> {
    /// Install split-tunnel routing.
    ///
    /// Embeds PostUp/PreDown hooks in the wg-quick config so peer IP bypass
    /// routes are added on `wg-quick up` and removed on `wg-quick down`.
    /// This avoids any race condition since the routes are managed atomically
    /// within the wg-quick lifecycle.
    async fn setup(&mut self) -> Result<(), Error> {
        let (device, gateway) = self.route_ops.get_default_interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");

        let mut extra = vec![];
        for ip in &self.peer_ips {
            extra.extend(post_up_routing(ip.to_string(), device.clone(), gateway.clone()));
        }
        for ip in &self.peer_ips {
            extra.push(pre_down_routing(ip.to_string(), device.clone(), gateway.clone()));
        }

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        self.wg.wg_quick_up(self.state_home.clone(), wg_quick_content).await?;
        tracing::debug!("wg-quick up");
        Ok(())
    }

    async fn teardown(&mut self, logs: Logs) {
        match self.wg.wg_quick_down(self.state_home.clone(), logs).await {
            Ok(_) => tracing::debug!("wg-quick down"),
            Err(error) => tracing::error!(?error, "wg-quick down failed during teardown"),
        }
    }
}

fn post_up_routing(route_addr: String, device: String, gateway: Option<String>) -> Vec<String> {
    match gateway {
        Some(gw) => vec![
            // make routing idempotent by deleting routes before adding them ignoring errors
            format!("PostUp = ip route del {route_addr} via {gw} dev {device} || true"),
            format!("PostUp = ip route add {route_addr} via {gw} dev {device}"),
        ],
        None => vec![
            // make routing idempotent by deleting routes before adding them ignoring errors
            format!("PostUp = ip route del {route_addr} dev {device} || true"),
            format!("PostUp = ip route add {route_addr} dev {device}"),
        ],
    }
}

fn pre_down_routing(route_addr: String, device: String, gateway: Option<String>) -> String {
    match gateway {
        // wg-quick stops execution on error, ignore errors to hit all commands
        Some(gw) => format!("PreDown = ip route del {route_addr} via {gw} dev {device} || true"),
        None => format!("PreDown = ip route del {route_addr} dev {device} || true"),
    }
}

/// Clean up from any previous unclean shutdown.
pub async fn reset_on_startup(state_home: PathBuf) {
    let wg = RealWgOps {};
    let _ = wg.wg_quick_down(state_home, Logs::Suppress).await;
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;
    use crate::routing::mocks::*;

    fn test_wg_data() -> event::WireGuardData {
        use gnosis_vpn_lib::wireguard;
        event::WireGuardData {
            wg: wireguard::WireGuard::new(
                wireguard::Config {
                    listen_port: Some(51820),
                    allowed_ips: Some("0.0.0.0/0".into()),
                    force_private_key: None,
                    dns: None,
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

    fn make_fallback_router(route_ops: MockRouteOps, wg: MockWgOps) -> FallbackRouter<MockRouteOps, MockWgOps> {
        FallbackRouter {
            state_home: PathBuf::from("/tmp/test"),
            wg_data: test_wg_data(),
            peer_ips: vec![Ipv4Addr::new(1, 2, 3, 4), Ipv4Addr::new(5, 6, 7, 8)],
            route_ops,
            wg,
        }
    }

    #[tokio::test]
    async fn fallback_wg_failure_returns_error() -> anyhow::Result<()> {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
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

        let mut router = make_fallback_router(route_ops, wg);
        let result = router.setup().await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn fallback_teardown_brings_wg_down() -> anyhow::Result<()> {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops, wg.clone());
        router.setup().await?;
        router.teardown(Logs::Suppress).await;

        let wg_state = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
        assert!(!wg_state.wg_up);
        Ok(())
    }

    #[tokio::test]
    async fn fallback_teardown_continues_if_wg_down_fails() -> anyhow::Result<()> {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            default_iface: Some(("eth0".into(), Some("192.168.1.1".into()))),
            ..Default::default()
        });
        let wg = MockWgOps::new();

        let mut router = make_fallback_router(route_ops, wg.clone());
        router.setup().await?;

        {
            let mut s = wg.state.lock().map_err(|_| anyhow::anyhow!("Mutex poisoned"))?;
            s.fail_on
                .insert("wg_quick_down".into(), "simulated wg down failure".into());
        }

        // Should not panic even if wg_quick_down fails
        router.teardown(Logs::Suppress).await;
        Ok(())
    }
}
