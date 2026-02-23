//! Bypass route management for split-tunnel VPN.
//!
//! This module provides a unified abstraction for managing bypass routes
//! that allow certain traffic (peer IPs and RFC1918 networks) to bypass
//! the VPN tunnel and route directly through the WAN interface.
//!
//! The manager tracks which routes were successfully added, enabling
//! reliable rollback on partial failure.

use std::net::Ipv4Addr;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::{Logs, ShellCommandExt};

use super::Error;

#[cfg(target_os = "linux")]
use super::RFC1918_BYPASS_NETS;

/// WAN interface information for bypass routing.
#[derive(Debug, Clone)]
pub struct WanInterface {
    pub device: String,
    pub gateway: Option<String>,
}

/// Manages bypass routes for peer IPs and RFC1918 networks.
///
/// Tracks which routes were successfully added to enable proper rollback
/// if setup fails partway through.
pub struct BypassRouteManager {
    wan: WanInterface,
    peer_ips: Vec<Ipv4Addr>,
    /// Peer IPs for which routes were successfully added (for rollback)
    added_peer_routes: Vec<Ipv4Addr>,
    /// RFC1918 CIDRs for which routes were successfully added (for rollback)
    added_rfc1918_routes: Vec<String>,
}

impl BypassRouteManager {
    /// Creates a new bypass route manager.
    ///
    /// The manager starts with no routes added. Call `setup_peer_routes()`
    /// and optionally `setup_rfc1918_routes()` to add routes.
    pub fn new(wan: WanInterface, peer_ips: Vec<Ipv4Addr>) -> Self {
        Self {
            wan,
            peer_ips,
            added_peer_routes: Vec::new(),
            added_rfc1918_routes: Vec::new(),
        }
    }

    /// Adds bypass routes for all peer IPs.
    ///
    /// On error, automatically rolls back any routes that were successfully added.
    pub async fn setup_peer_routes(&mut self) -> Result<(), Error> {
        for ip in &self.peer_ips {
            if let Err(e) = self.add_peer_route(ip).await {
                // Rollback what we added so far
                self.rollback().await;
                return Err(e);
            }
            self.added_peer_routes.push(*ip);
        }
        tracing::debug!(count = self.added_peer_routes.len(), "peer IP bypass routes added");
        Ok(())
    }

    /// Adds bypass routes for RFC1918 networks.
    ///
    /// On error, automatically rolls back any routes that were successfully added
    /// (both RFC1918 and peer IP routes).
    #[cfg(target_os = "linux")]
    pub async fn setup_rfc1918_routes(&mut self) -> Result<(), Error> {
        for (net, prefix) in RFC1918_BYPASS_NETS {
            let cidr = format!("{}/{}", net, prefix);
            if let Err(e) = self.add_subnet_route(&cidr).await {
                // Rollback all routes (both RFC1918 and peer IPs)
                self.rollback().await;
                return Err(e);
            }
            self.added_rfc1918_routes.push(cidr);
        }
        tracing::debug!(count = self.added_rfc1918_routes.len(), "RFC1918 bypass routes added");
        Ok(())
    }

    /// Rolls back all routes that were successfully added.
    ///
    /// This is called automatically on setup failure, but can also be called
    /// explicitly if needed. Silently ignores deletion failures.
    pub async fn rollback(&mut self) {
        // Rollback RFC1918 routes first (reverse order of setup)
        for cidr in self.added_rfc1918_routes.drain(..).collect::<Vec<_>>() {
            let _ = self.delete_subnet_route(&cidr).await;
        }
        // Then rollback peer routes
        for ip in self.added_peer_routes.drain(..).collect::<Vec<_>>() {
            let _ = self.delete_peer_route(&ip).await;
        }
        tracing::debug!("bypass routes rolled back");
    }

    /// Tears down all bypass routes, warning on failures but continuing.
    ///
    /// Unlike rollback, this logs warnings for any failures and is intended
    /// for normal shutdown rather than error recovery.
    #[cfg(target_os = "linux")]
    pub async fn teardown(&mut self) {
        // Remove peer IP bypass routes
        for ip in &self.added_peer_routes.clone() {
            if let Err(e) = self.delete_peer_route(ip).await {
                tracing::warn!(%e, peer_ip = %ip, "failed to delete bypass route, continuing anyway");
            }
        }
        tracing::debug!(
            count = self.added_peer_routes.len(),
            "peer IP bypass routes cleanup attempted"
        );

        // Remove RFC1918 bypass routes
        for cidr in &self.added_rfc1918_routes.clone() {
            if let Err(e) = self.delete_subnet_route(cidr).await {
                tracing::warn!(%e, cidr = %cidr, "failed to delete RFC1918 bypass route, continuing anyway");
            }
        }
        if !self.added_rfc1918_routes.is_empty() {
            tracing::debug!(
                count = self.added_rfc1918_routes.len(),
                "RFC1918 bypass routes cleanup attempted"
            );
        }

        self.added_peer_routes.clear();
        self.added_rfc1918_routes.clear();
    }

    // ========================================================================
    // Platform-specific route commands
    // ========================================================================

    #[cfg(target_os = "linux")]
    async fn add_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        // Delete any existing route first (make idempotent)
        let _ = self.delete_peer_route(peer_ip).await;

        let mut cmd = Command::new("ip");
        cmd.arg("route").arg("add").arg(peer_ip.to_string());
        if let Some(ref gw) = self.wan.gateway {
            cmd.arg("via").arg(gw);
        }
        cmd.arg("dev").arg(&self.wan.device);
        cmd.run_stdout(Logs::Print).await?;
        tracing::debug!(peer_ip = %peer_ip, device = %self.wan.device, gateway = ?self.wan.gateway, "added bypass route");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn delete_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        // Omit gateway from deletion - the gateway may have changed since the route was added,
        // and ip route del can match by destination + device alone
        Command::new("ip")
            .arg("route")
            .arg("del")
            .arg(peer_ip.to_string())
            .arg("dev")
            .arg(&self.wan.device)
            .run_stdout(Logs::Suppress)
            .await?;
        tracing::debug!(peer_ip = %peer_ip, "deleted bypass route");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn add_subnet_route(&self, cidr: &str) -> Result<(), Error> {
        // Delete any existing route first (make idempotent)
        let _ = self.delete_subnet_route(cidr).await;

        let mut cmd = Command::new("ip");
        cmd.arg("route").arg("add").arg(cidr);
        if let Some(ref gw) = self.wan.gateway {
            cmd.arg("via").arg(gw);
        }
        cmd.arg("dev").arg(&self.wan.device);
        cmd.run_stdout(Logs::Print).await?;
        tracing::debug!(cidr = %cidr, device = %self.wan.device, gateway = ?self.wan.gateway, "added RFC1918 bypass route");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn delete_subnet_route(&self, cidr: &str) -> Result<(), Error> {
        // Omit gateway from deletion - the gateway may have changed since the route was added,
        // and ip route del can match by destination + device alone
        Command::new("ip")
            .arg("route")
            .arg("del")
            .arg(cidr)
            .arg("dev")
            .arg(&self.wan.device)
            .run_stdout(Logs::Suppress)
            .await?;
        tracing::debug!(cidr = %cidr, "deleted RFC1918 bypass route");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn add_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        // Delete any existing route first (make idempotent)
        let _ = self.delete_peer_route(peer_ip).await;

        let mut cmd = Command::new("route");
        cmd.arg("-n").arg("add").arg("-host").arg(peer_ip.to_string());
        if let Some(ref gw) = self.wan.gateway {
            cmd.arg(gw);
        } else {
            cmd.arg("-interface").arg(&self.wan.device);
        }
        cmd.run_stdout(Logs::Print).await?;
        tracing::debug!(peer_ip = %peer_ip, device = %self.wan.device, gateway = ?self.wan.gateway, "added bypass route");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn delete_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        Command::new("route")
            .arg("-n")
            .arg("delete")
            .arg("-host")
            .arg(peer_ip.to_string())
            .run_stdout(Logs::Suppress)
            .await?;
        tracing::debug!(peer_ip = %peer_ip, "deleted bypass route");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn delete_subnet_route(&self, cidr: &str) -> Result<(), Error> {
        Command::new("route")
            .arg("-n")
            .arg("delete")
            .arg("-inet")
            .arg(cidr)
            .run_stdout(Logs::Suppress)
            .await?;
        tracing::debug!(cidr = %cidr, "deleted RFC1918 bypass route");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bypass_manager_creation() {
        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("192.168.1.1".to_string()),
        };
        let peer_ips = vec!["1.2.3.4".parse().unwrap(), "5.6.7.8".parse().unwrap()];

        let manager = BypassRouteManager::new(wan.clone(), peer_ips.clone());

        assert_eq!(manager.wan.device, "eth0");
        assert_eq!(manager.wan.gateway, Some("192.168.1.1".to_string()));
        assert_eq!(manager.peer_ips.len(), 2);
        assert!(manager.added_peer_routes.is_empty());
        assert!(manager.added_rfc1918_routes.is_empty());
    }

    #[test]
    fn test_wan_interface_without_gateway() {
        let wan = WanInterface {
            device: "wlan0".to_string(),
            gateway: None,
        };

        assert!(wan.gateway.is_none());
        assert_eq!(wan.device, "wlan0");
    }

    #[test]
    fn test_bypass_manager_empty_peer_ips() {
        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("10.0.0.1".to_string()),
        };
        let manager = BypassRouteManager::new(wan, vec![]);

        assert!(manager.peer_ips.is_empty());
        assert!(manager.added_peer_routes.is_empty());
    }

    #[test]
    fn test_bypass_manager_state_tracking() {
        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("10.0.0.1".to_string()),
        };
        let mut manager = BypassRouteManager::new(wan, vec![]);

        // Simulate adding routes by directly modifying
        manager.added_peer_routes.push("1.1.1.1".parse().unwrap());
        manager.added_peer_routes.push("8.8.8.8".parse().unwrap());
        manager.added_rfc1918_routes.push("10.0.0.0/8".to_string());
        manager.added_rfc1918_routes.push("192.168.0.0/16".to_string());

        assert_eq!(manager.added_peer_routes.len(), 2);
        assert_eq!(manager.added_rfc1918_routes.len(), 2);

        // Verify specific entries
        assert!(manager.added_peer_routes.contains(&"1.1.1.1".parse().unwrap()));
        assert!(manager.added_rfc1918_routes.contains(&"10.0.0.0/8".to_string()));
    }
}
