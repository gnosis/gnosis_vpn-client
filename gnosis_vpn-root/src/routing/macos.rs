//! macOS routing implementation for split-tunnel VPN behavior.
//!
//! Provides a [`StaticRouter`] that:
//! 1. Adds bypass routes for peer IPs BEFORE bringing up WireGuard (avoids race condition)
//! 2. Runs `wg-quick up` with `Table = off` to prevent automatic routing
//! 3. Uses PostUp hooks to add:
//!    - Default routes (0.0.0.0/1 and 128.0.0.0/1) through VPN
//!    - VPN subnet route (10.128.0.0/9) through VPN - overrides the 10.0.0.0/8 bypass
//!      so VPN server traffic (e.g. 10.128.0.1) uses the tunnel
//!    - RFC1918 bypass routes (10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16)
//!      through WAN gateway for LAN access
//! 4. On teardown, brings down WireGuard first, then cleans up peer IP bypass routes
//!
//! ## Route Precedence (most specific wins)
//!
//! - 10.128.0.0/9 → VPN interface (VPN server subnet)
//! - 10.0.0.0/8 → WAN gateway (other RFC1918 Class A)
//! - 0.0.0.0/1, 128.0.0.0/1 → VPN interface (catch-all)
//!
//! ## Race Condition Window
//!
//! Peer IP bypass routes are added before `wg-quick up`, eliminating the race condition
//! for HOPR traffic. However, RFC1918 routes are added via PostUp hooks (after interface
//! is up), so there is a brief window where RFC1918 traffic could be routed incorrectly
//! if the VPN captures those routes before PostUp runs. This is acceptable because:
//!
//! - RFC1918 traffic (LAN access) is less time-sensitive than peer traffic
//! - The window is very short (milliseconds during wg-quick startup)
//! - PostUp hooks cannot reference the WAN gateway captured before wg-quick up
//!   without adding complexity (the gateway may change when VPN becomes active)
//!
//! ## Platform Notes
//!
//! Dynamic routing (using rtnetlink) is not available on macOS.
use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::event;
use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

use std::net::Ipv4Addr;
use std::path::PathBuf;

use super::{Error, RFC1918_BYPASS_NETS, Routing, VPN_TUNNEL_SUBNET};

use crate::wg_tooling;

/// WAN interface information stub for macOS (never used since dynamic routing is not available).
#[derive(Debug, Clone)]
pub struct WanInfo;

/// Dynamic routing not available on macOS.
pub fn dynamic_router(
    _state_home: PathBuf,
    _wg_data: event::WireGuardData,
    _wan_info: WanInfo,
) -> Result<DynamicRouter, Error> {
    Err(Error::NotAvailable)
}

pub struct DynamicRouter {}

/// Builds a static macOS router without a worker handle.
pub fn static_router(state_home: PathBuf, wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> StaticRouter {
    StaticRouter {
        state_home,
        wg_data,
        peer_ips,
        bypass_manager: None,
    }
}

/// macOS routing implementation that programs host routes directly before wg-quick up.
pub struct StaticRouter {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
    bypass_manager: Option<super::BypassRouteManager>,
}

#[async_trait]
impl Routing for StaticRouter {
    /// Install split-tunnel routing for macOS StaticRouter.
    ///
    /// Uses a phased approach to avoid a race condition where HOPR p2p connections
    /// could briefly drop when the WireGuard interface comes up.
    ///
    /// Phase 1 (before wg-quick up):
    ///   1. Get WAN interface info
    ///   2. Add bypass routes for all peer IPs directly via WAN
    ///
    /// Phase 2:
    ///   3. Run wg-quick up (safe now - bypass routes are already in place)
    ///
    async fn setup(&mut self) -> Result<(), Error> {
        // Phase 1: Add peer IP bypass routes BEFORE wg-quick up
        let (device, gateway) = interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");

        let mut bypass_manager = super::BypassRouteManager::new(
            super::WanInterface {
                device: device.clone(),
                gateway: gateway.clone(),
            },
            self.peer_ips.clone(),
        );

        // Only add peer IP bypass routes (RFC1918 done via PostUp hooks)
        bypass_manager.setup_peer_routes().await?;

        // Phase 2: wg-quick up with PostUp hooks for VPN and RFC1918 routes
        // Table = off prevents wg-quick from managing routes automatically
        // PostUp hooks add routes AFTER interface is established (critical for persistence)
        let mut extra = vec![
            "Table = off".to_string(),
            // VPN default routes (catch-all via tunnel)
            "PostUp = route -n add -inet 0.0.0.0/1 -interface %i".to_string(),
            "PostUp = route -n add -inet 128.0.0.0/1 -interface %i".to_string(),
            // VPN internal subnet (more specific than 10.0.0.0/8 bypass below)
            format!(
                "PostUp = route -n add -inet {}/{} -interface %i",
                VPN_TUNNEL_SUBNET.0, VPN_TUNNEL_SUBNET.1
            ),
        ];

        // RFC1918 bypass routes via PostUp (using captured WAN gateway)
        // These are more specific than 0/1 and 128/1, so they take precedence
        for (net, prefix) in RFC1918_BYPASS_NETS {
            let cidr = format!("{}/{}", net, prefix);
            let route_cmd = if let Some(ref gw) = gateway {
                format!("PostUp = route -n add -inet {} {}", cidr, gw)
            } else {
                format!("PostUp = route -n add -inet {} -interface {}", cidr, device)
            };
            extra.push(route_cmd);
        }
        tracing::debug!(
            rfc1918_routes = RFC1918_BYPASS_NETS.len(),
            "RFC1918 bypass routes configured as PostUp commands"
        );

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        if let Err(e) = wg_tooling::up(self.state_home.clone(), wg_quick_content).await {
            tracing::warn!("wg-quick up failed, rolling back peer IP bypass routes");
            bypass_manager.rollback().await;
            return Err(e.into());
        }
        tracing::debug!("wg-quick up");

        self.bypass_manager = Some(bypass_manager);
        tracing::info!("routing is ready (macOS static)");
        Ok(())
    }

    /// Teardown split-tunnel routing for macOS StaticRouter.
    ///
    /// Teardown order is important: wg-quick down first, then remove peer IP bypass routes.
    /// This ensures HOPR traffic continues to flow via WAN while VPN is being torn down.
    /// RFC1918 routes are cleaned up automatically when the WireGuard interface goes down.
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        // wg-quick down removes the interface and its associated routes (including RFC1918 PostUp routes)
        wg_tooling::down(self.state_home.clone(), logs).await?;
        tracing::debug!("wg-quick down");

        // Remove peer IP bypass routes using the bypass manager
        if let Some(ref mut bypass_manager) = self.bypass_manager {
            bypass_manager.rollback().await; // Use rollback for silent cleanup
        }
        self.bypass_manager = None;
        tracing::debug!("Peer IP bypass routes cleanup attempted after wg-quick down");

        Ok(())
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

/// Gets the default WAN interface name and gateway by querying the routing table.
///
/// Returns `(device_name, Option<gateway_ip>)`.
async fn interface() -> Result<(String, Option<String>), Error> {
    let output = Command::new("route")
        .arg("-n")
        .arg("get")
        .arg("0.0.0.0")
        .run_stdout(Logs::Print)
        .await?;

    let res = parse_interface(&output)?;
    Ok(res)
}

/// Parses the output of `route -n get 0.0.0.0` to extract interface and gateway.
fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    // Use shared parser with macOS-specific keys and suffix filter
    // (filters out "index:" when gateway shows "gateway: index: 28")
    super::parse_key_value_output(output, "interface:", "gateway:", Some(":"))
}

#[cfg(test)]
mod tests {
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

        let (device, gateway) = super::parse_interface(output)?;

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

        let (device, gateway) = super::parse_interface(output)?;

        assert_eq!(device, "utun8");
        assert_eq!(gateway, None); // Should be None, not "index:"
        Ok(())
    }
}
