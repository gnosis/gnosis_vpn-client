//! Platform-agnostic route manipulation abstraction.
//!
//! Defines [`RouteOps`] trait for basic routing operations used by
//! [`BypassRouteManager`](super::BypassRouteManager) and [`FallbackRouter`](super::linux::FallbackRouter).
//!
//! **Limitation:** All operations are IPv4-only. IPv6 routing is not supported.
//!
//! Platform-specific implementations:
//! - Linux: [`NetlinkRouteOps`](super::route_ops_linux::NetlinkRouteOps) via rtnetlink
//! - macOS: [`DarwinRouteOps`](super::route_ops_macos::DarwinRouteOps) via route commands

use async_trait::async_trait;

use super::Error;

/// Abstraction over platform routing table operations.
///
/// Implementors must be cheaply cloneable (for sharing between
/// `FallbackRouter` and `BypassRouteManager`).
#[async_trait]
pub trait RouteOps: Send + Sync + Clone {
    /// Get the default WAN interface name and optional gateway.
    async fn get_default_interface(&self) -> Result<(String, Option<String>), Error>;

    /// Add a route: destination via optional gateway through device.
    async fn route_add(&self, dest: &str, gateway: Option<&str>, device: &str) -> Result<(), Error>;

    /// Delete a route by destination and device.
    async fn route_del(&self, dest: &str, device: &str) -> Result<(), Error>;

    /// Flush the kernel routing cache.
    /// No-op on modern Linux kernels (>= 3.6) and macOS.
    async fn flush_routing_cache(&self) -> Result<(), Error>;
}
