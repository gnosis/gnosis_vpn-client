//! Platform-agnostic route manipulation abstraction.
//!
//! Defines the [`RouteOps`] trait for basic routing operations used by:
//! - the Linux fallback router (type `FallbackRouter` in module `routing::linux`)
//! - the bypass route manager (`bypass::BypassRouteManager`)
//! - the macOS router (module `routing::macos`)
//!
//! **Limitation:** All operations are IPv4-only. IPv6 routing is not supported.
//!
//! Platform-specific implementations:
//! - Linux: type `NetlinkRouteOps` in module `routing::route_ops_linux` (via rtnetlink)
//! - macOS: type `DarwinRouteOps` in module `routing::route_ops_macos`

use async_trait::async_trait;

use super::Error;

/// Abstraction over platform routing table operations.
#[async_trait]
pub trait RouteOps: Send + Sync + Clone {
    /// Get the default WAN interface name and optional gateway.
    async fn get_default_interface(&self) -> Result<(String, Option<String>), Error>;

    /// Get the WAN default route (`/0` entry) even while VPN split routes are up.
    ///
    /// On Linux `get_default_interface` already reads the main-table `/0` route,
    /// so the default implementation just delegates. macOS overrides this because
    /// `route get 0.0.0.0` does longest-prefix matching and would return the VPN
    /// interface while the `0.0.0.0/1` split route is installed.
    async fn get_wan_default(&self) -> Result<(String, Option<String>), Error> {
        self.get_default_interface().await
    }

    /// Add a route: destination via optional gateway through device.
    #[allow(dead_code)]
    async fn route_add(&self, dest: &str, gateway: Option<&str>, device: &str) -> Result<(), Error>;

    /// Delete a route by destination and device.
    #[allow(dead_code)]
    async fn route_del(&self, dest: &str, device: &str) -> Result<(), Error>;
}
