//! Bypass route management for split-tunnel VPN.
//!
//! This module provides a unified abstraction for managing bypass routes
//! that allow certain traffic (peer IPs and RFC1918 networks) to bypass
//! the VPN tunnel and route directly through the WAN interface.
//!
//! The manager tracks which routes were successfully added, enabling
//! reliable rollback on partial failure.

use std::net::Ipv4Addr;

use super::Error;

use super::RFC1918_BYPASS_NETS;
use super::route_ops::RouteOps;

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
///
/// Generic over `R: RouteOps` so tests can inject mock route operations.
pub struct BypassRouteManager<R: RouteOps> {
    wan: WanInterface,
    peer_ips: Vec<Ipv4Addr>,
    route_ops: R,
    /// Peer IPs for which routes were successfully added (for rollback)
    added_peer_routes: Vec<Ipv4Addr>,
    /// RFC1918 CIDRs for which routes were successfully added (for rollback)
    added_rfc1918_routes: Vec<String>,
}

impl<R: RouteOps> BypassRouteManager<R> {
    /// Creates a new bypass route manager.
    ///
    /// The manager starts with no routes added. Call `setup_peer_routes()`
    /// and optionally `setup_rfc1918_routes()` to add routes.
    pub fn new(wan: WanInterface, peer_ips: Vec<Ipv4Addr>, route_ops: R) -> Self {
        Self {
            wan,
            peer_ips,
            route_ops,
            added_peer_routes: Vec::new(),
            added_rfc1918_routes: Vec::new(),
        }
    }

    /// Adds bypass routes for all peer IPs.
    ///
    /// On error, automatically rolls back any routes that were successfully added.
    pub async fn setup_peer_routes(&mut self) -> Result<(), Error> {
        for ip in &self.peer_ips.clone() {
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
    // Route operations via RouteOps trait
    // ========================================================================

    async fn add_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        // Delete any existing route first (make idempotent)
        let _ = self.delete_peer_route(peer_ip).await;

        self.route_ops
            .route_add(&peer_ip.to_string(), self.wan.gateway.as_deref(), &self.wan.device)
            .await?;
        tracing::debug!(peer_ip = %peer_ip, device = %self.wan.device, gateway = ?self.wan.gateway, "added bypass route");
        Ok(())
    }

    async fn delete_peer_route(&self, peer_ip: &Ipv4Addr) -> Result<(), Error> {
        // Omit gateway from deletion - the gateway may have changed since the route was added,
        // and route del can match by destination + device alone
        self.route_ops.route_del(&peer_ip.to_string(), &self.wan.device).await?;
        tracing::debug!(peer_ip = %peer_ip, "deleted bypass route");
        Ok(())
    }

    async fn add_subnet_route(&self, cidr: &str) -> Result<(), Error> {
        // Delete any existing route first (make idempotent)
        let _ = self.delete_subnet_route(cidr).await;

        self.route_ops
            .route_add(cidr, self.wan.gateway.as_deref(), &self.wan.device)
            .await?;
        tracing::debug!(cidr = %cidr, device = %self.wan.device, gateway = ?self.wan.gateway, "added RFC1918 bypass route");
        Ok(())
    }

    async fn delete_subnet_route(&self, cidr: &str) -> Result<(), Error> {
        // Omit gateway from deletion - the gateway may have changed since the route was added,
        // and route del can match by destination + device alone
        self.route_ops.route_del(cidr, &self.wan.device).await?;
        tracing::debug!(cidr = %cidr, "deleted RFC1918 bypass route");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::mocks::MockRouteOps;
    use crate::routing::mocks::RouteOpsState;

    fn make_route_ops() -> MockRouteOps {
        MockRouteOps::new()
    }

    #[test]
    fn test_bypass_manager_creation() {
        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("192.168.1.1".to_string()),
        };
        let peer_ips = vec!["1.2.3.4".parse().unwrap(), "5.6.7.8".parse().unwrap()];

        let manager = BypassRouteManager::new(wan.clone(), peer_ips.clone(), make_route_ops());

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
        let manager = BypassRouteManager::new(wan, vec![], make_route_ops());

        assert!(manager.peer_ips.is_empty());
        assert!(manager.added_peer_routes.is_empty());
    }

    #[tokio::test]
    async fn test_setup_peer_routes_adds_routes() {
        let route_ops = make_route_ops();
        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("192.168.1.1".to_string()),
        };
        let peer_ips: Vec<Ipv4Addr> = vec!["1.2.3.4".parse().unwrap(), "5.6.7.8".parse().unwrap()];

        let mut manager = BypassRouteManager::new(wan, peer_ips, route_ops.clone());
        manager.setup_peer_routes().await.unwrap();

        let state = route_ops.state.lock().unwrap();
        // Each peer gets a delete attempt (idempotent) then an add.
        // The delete fails silently (route doesn't exist), then add succeeds.
        assert_eq!(state.added_routes.len(), 2);
        assert_eq!(state.added_routes[0].0, "1.2.3.4");
        assert_eq!(state.added_routes[0].1, Some("192.168.1.1".into()));
        assert_eq!(state.added_routes[0].2, "eth0");
        assert_eq!(state.added_routes[1].0, "5.6.7.8");
    }

    #[tokio::test]
    async fn test_setup_rfc1918_routes_adds_routes() {
        let route_ops = make_route_ops();
        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("10.0.0.1".to_string()),
        };

        let mut manager = BypassRouteManager::new(wan, vec![], route_ops.clone());
        manager.setup_rfc1918_routes().await.unwrap();

        let state = route_ops.state.lock().unwrap();
        assert_eq!(state.added_routes.len(), 4); // 4 RFC1918 networks
        assert_eq!(state.added_routes[0].0, "10.0.0.0/8");
        assert_eq!(state.added_routes[1].0, "172.16.0.0/12");
        assert_eq!(state.added_routes[2].0, "192.168.0.0/16");
        assert_eq!(state.added_routes[3].0, "169.254.0.0/16");
    }

    #[tokio::test]
    async fn test_setup_peer_routes_rollback_on_failure() {
        let route_ops = MockRouteOps::with_state(RouteOpsState {
            fail_on: {
                let mut m = std::collections::HashMap::new();
                // Fail on route_add
                m.insert("route_add".into(), "simulated failure".into());
                m
            },
            ..Default::default()
        });

        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("192.168.1.1".to_string()),
        };
        let peer_ips = vec!["1.2.3.4".parse().unwrap()];

        let mut manager = BypassRouteManager::new(wan, peer_ips, route_ops.clone());
        let result = manager.setup_peer_routes().await;

        assert!(result.is_err());
        assert!(manager.added_peer_routes.is_empty());
    }

    #[tokio::test]
    async fn test_teardown_clears_routes() {
        let route_ops = make_route_ops();
        let wan = WanInterface {
            device: "eth0".to_string(),
            gateway: Some("192.168.1.1".to_string()),
        };
        let peer_ips: Vec<Ipv4Addr> = vec!["1.2.3.4".parse().unwrap()];

        let mut manager = BypassRouteManager::new(wan, peer_ips, route_ops.clone());
        manager.setup_peer_routes().await.unwrap();
        manager.setup_rfc1918_routes().await.unwrap();

        // Verify routes exist
        {
            let state = route_ops.state.lock().unwrap();
            assert_eq!(state.added_routes.len(), 5); // 1 peer + 4 RFC1918
        }

        manager.teardown().await;

        // Verify routes are cleaned up
        {
            let state = route_ops.state.lock().unwrap();
            assert!(state.added_routes.is_empty());
        }
        assert!(manager.added_peer_routes.is_empty());
        assert!(manager.added_rfc1918_routes.is_empty());
    }

    #[tokio::test]
    async fn test_bypass_without_gateway() {
        let route_ops = make_route_ops();
        let wan = WanInterface {
            device: "wlan0".to_string(),
            gateway: None,
        };
        let peer_ips: Vec<Ipv4Addr> = vec!["8.8.8.8".parse().unwrap()];

        let mut manager = BypassRouteManager::new(wan, peer_ips, route_ops.clone());
        manager.setup_peer_routes().await.unwrap();

        let state = route_ops.state.lock().unwrap();
        assert_eq!(state.added_routes.len(), 1);
        assert_eq!(state.added_routes[0].1, None); // no gateway
    }
}
