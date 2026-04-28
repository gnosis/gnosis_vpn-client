//! Bypass route management for split-tunnel VPN.
//!
//! Manages peer IP and RFC1918 bypass routes via WAN, tracking which routes
//! were successfully added to enable reliable rollback on partial failure.

use std::net::Ipv4Addr;

use super::Error;
use super::RFC1918_BYPASS_NETS;

#[cfg(target_os = "linux")]
use super::route_ops_linux::NetlinkRouteOps;
#[cfg(target_os = "macos")]
use super::route_ops_macos::DarwinRouteOps;

/// WAN interface information for bypass routing.
#[derive(Debug, Clone)]
pub struct WanInterface {
    pub device: String,
    pub gateway: Option<String>,
}

/// Manages bypass routes for peer IPs and RFC1918 networks.
///
/// Tracks which routes were successfully added to enable rollback
/// if setup fails partway through.
pub struct BypassRouteManager {
    wan: WanInterface,
    peer_ips: Vec<Ipv4Addr>,
    #[cfg(target_os = "linux")]
    route_ops: NetlinkRouteOps,
    #[cfg(target_os = "macos")]
    route_ops: DarwinRouteOps,
    /// Peer IPs for which routes were successfully added (for rollback).
    added_peer_routes: Vec<Ipv4Addr>,
    /// RFC1918 CIDRs for which routes were successfully added (for rollback).
    added_rfc1918_routes: Vec<String>,
}

impl BypassRouteManager {
    #[cfg(target_os = "linux")]
    pub fn new(wan: WanInterface, peer_ips: Vec<Ipv4Addr>, route_ops: NetlinkRouteOps) -> Self {
        Self { wan, peer_ips, route_ops, added_peer_routes: Vec::new(), added_rfc1918_routes: Vec::new() }
    }

    #[cfg(target_os = "macos")]
    pub fn new(wan: WanInterface, peer_ips: Vec<Ipv4Addr>, route_ops: DarwinRouteOps) -> Self {
        Self { wan, peer_ips, route_ops, added_peer_routes: Vec::new(), added_rfc1918_routes: Vec::new() }
    }

    /// Adds bypass routes for all peer IPs.
    ///
    /// On error, automatically rolls back any routes that were successfully added.
    pub async fn setup_peer_routes(&mut self) -> Result<(), Error> {
        for ip in &self.peer_ips.clone() {
            if let Err(e) = self.add_peer_route(ip).await {
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
    pub async fn setup_rfc1918_routes(&mut self) -> Result<(), Error> {
        for (net, prefix) in RFC1918_BYPASS_NETS {
            let cidr = format!("{}/{}", net, prefix);
            if let Err(e) = self.add_subnet_route(&cidr).await {
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
    /// Called automatically on setup failure. Silently ignores deletion failures.
    pub async fn rollback(&mut self) {
        for cidr in self.added_rfc1918_routes.drain(..).collect::<Vec<_>>() {
            let _ = self.delete_subnet_route(&cidr).await;
        }
        for ip in self.added_peer_routes.drain(..).collect::<Vec<_>>() {
            let _ = self.delete_peer_route(&ip).await;
        }
        tracing::debug!("bypass routes rolled back");
    }

    /// Tears down all bypass routes, warning on failures but continuing.
    pub async fn teardown(&mut self) {
        for ip in &self.added_peer_routes.clone() {
            if let Err(e) = self.delete_peer_route(ip).await {
                tracing::warn!(%e, peer_ip = %ip, "failed to delete bypass route, continuing anyway");
            }
        }
        tracing::debug!(count = self.added_peer_routes.len(), "peer IP bypass routes cleanup attempted");

        for cidr in &self.added_rfc1918_routes.clone() {
            if let Err(e) = self.delete_subnet_route(cidr).await {
                tracing::warn!(%e, cidr = %cidr, "failed to delete RFC1918 bypass route, continuing anyway");
            }
        }
        if !self.added_rfc1918_routes.is_empty() {
            tracing::debug!(count = self.added_rfc1918_routes.len(), "RFC1918 bypass routes cleanup attempted");
        }

        self.added_peer_routes.clear();
        self.added_rfc1918_routes.clear();
    }

    async fn add_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        let _ = self.delete_peer_route(peer_ip).await; // idempotent
        self.route_ops
            .route_add(&peer_ip.to_string(), self.wan.gateway.as_deref(), &self.wan.device)
            .await?;
        tracing::debug!(peer_ip = %peer_ip, device = %self.wan.device, gateway = ?self.wan.gateway, "added bypass route");
        Ok(())
    }

    async fn delete_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        self.route_ops.route_del(&peer_ip.to_string(), &self.wan.device).await?;
        tracing::debug!(peer_ip = %peer_ip, "deleted bypass route");
        Ok(())
    }

    async fn add_subnet_route(&self, cidr: &str) -> Result<(), Error> {
        let _ = self.delete_subnet_route(cidr).await; // idempotent
        self.route_ops
            .route_add(cidr, self.wan.gateway.as_deref(), &self.wan.device)
            .await?;
        tracing::debug!(cidr = %cidr, device = %self.wan.device, gateway = ?self.wan.gateway, "added RFC1918 bypass route");
        Ok(())
    }

    async fn delete_subnet_route(&self, cidr: &str) -> Result<(), Error> {
        self.route_ops.route_del(cidr, &self.wan.device).await?;
        tracing::debug!(cidr = %cidr, "deleted RFC1918 bypass route");
        Ok(())
    }
}
