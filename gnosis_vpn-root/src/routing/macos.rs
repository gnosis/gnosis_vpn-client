//! macOS routing implementation using `wg-quick` hook commands.
//!
//! Builds a static router that installs per-peer host routes and
//! default-route overrides via `route` commands for split-tunnel behavior.
use async_trait::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};
use gnosis_vpn_lib::{event, worker};

use std::net::Ipv4Addr;
use std::path::PathBuf;

use super::{Error, Routing};
use crate::wg_tooling;

/// Dynamic routing not available on macOS.
pub fn dynamic_router(
    _state_home: PathBuf,
    _worker: worker::Worker,
    _wg_data: event::WireGuardData,
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
    }
}

/// macOS routing implementation that programs host routes directly before wg-quick up.
pub struct StaticRouter {
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
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
        // === PHASE 1: Add bypass routes BEFORE wg-quick up ===
        let (device, gateway) = interface().await?;
        tracing::debug!(device = %device, gateway = ?gateway, "WAN interface info for bypass routes");

        for ip in &self.peer_ips {
            add_bypass_route_macos(ip, &device, gateway.as_deref()).await?;
        }
        tracing::debug!("Bypass routes added before wg-quick up");

        // === PHASE 2: wg-quick up (without PreUp routing hooks) ===
        // Keep Table = off to manage routing ourselves
        // PostUp hooks add default routes through VPN interface
        let extra = vec![
            "Table = off".to_string(),
            "PostUp = route -n add -inet 0.0.0.0/1 -interface %i".to_string(),
            "PostUp = route -n add -inet 128.0.0.0/1 -interface %i".to_string(),
        ];

        let wg_quick_content =
            self.wg_data
                .wg
                .to_file_string(&self.wg_data.interface_info, &self.wg_data.peer_info, extra);

        if let Err(e) = wg_tooling::up(self.state_home.clone(), wg_quick_content).await {
            // Rollback bypass routes on failure
            tracing::warn!("wg-quick up failed, rolling back bypass routes");
            for ip in &self.peer_ips {
                let _ = delete_bypass_route_macos(ip).await;
            }
            return Err(e.into());
        }
        tracing::debug!("wg-quick up");

        tracing::info!("routing is ready (macOS static)");
        Ok(())
    }

    /// Teardown split-tunnel routing for macOS StaticRouter.
    ///
    /// Teardown order is important: wg-quick down FIRST, then remove bypass routes.
    /// This ensures HOPR traffic continues to flow via WAN while VPN is being torn down.
    ///
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error> {
        // === wg-quick down FIRST ===
        wg_tooling::down(self.state_home.clone(), logs).await?;
        tracing::debug!("wg-quick down");

        // === THEN remove bypass routes (ignore failures - routes may not exist) ===
        for ip in &self.peer_ips {
            let _ = delete_bypass_route_macos(ip).await;
        }
        tracing::debug!("Bypass routes cleanup attempted after wg-quick down");

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

/// Add a bypass route for a peer IP on macOS.
///
/// This ensures traffic to the peer IP goes directly via WAN, bypassing the VPN tunnel.
/// macOS `route` command is idempotent - it returns exit code 0 even if route already exists.
async fn add_bypass_route_macos(peer_ip: &Ipv4Addr, device: &str, gateway: Option<&str>) -> Result<(), Error> {
    let mut cmd = Command::new("route");
    cmd.arg("-n").arg("add").arg("-host").arg(peer_ip.to_string());

    if let Some(gw) = gateway {
        cmd.arg(gw);
    } else {
        cmd.arg("-interface").arg(device);
    }

    cmd.run_stdout(Logs::Print).await?;
    tracing::debug!(peer_ip = %peer_ip, device = %device, gateway = ?gateway, "Added bypass route");
    Ok(())
}

/// Delete a bypass route for a peer IP on macOS.
async fn delete_bypass_route_macos(peer_ip: &Ipv4Addr) -> Result<(), Error> {
    Command::new("route")
        .arg("-n")
        .arg("delete")
        .arg("-host")
        .arg(peer_ip.to_string())
        .run_stdout(Logs::Suppress)
        .await?;
    tracing::debug!(peer_ip = %peer_ip, "Deleted bypass route");
    Ok(())
}

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

fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();
    let device_index = parts.iter().position(|&x| x == "interface:");
    let via_index = parts.iter().position(|&x| x == "gateway:");
    let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
        Some(dev) => dev.to_string(),
        None => {
            tracing::error!(%output, "Unable to determine default interface");
            return Err(Error::NoInterface);
        }
    };

    let gateway = via_index.and_then(|idx| parts.get(idx + 1)).map(|gw| gw.to_string());
    Ok((device, gateway))
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
}
