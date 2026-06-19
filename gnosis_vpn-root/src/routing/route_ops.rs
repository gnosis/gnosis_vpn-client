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
use std::net::Ipv4Addr;

use super::Error;

/// A snapshot of the WAN route used to reach a public destination.
#[derive(Debug, Clone, PartialEq)]
pub struct WanRoute {
    /// Outbound interface name (e.g. "en0", "wlan0").
    pub device: String,
    /// Next-hop gateway IP, if any.
    pub gateway: Option<String>,
    /// Preferred source address (local IP the kernel would stamp on outbound packets).
    /// `None` when the platform does not expose this (e.g. macOS with no inet address).
    pub src_ip: Option<Ipv4Addr>,
}

/// Abstraction over platform routing table operations.
#[async_trait]
pub trait RouteOps: Send + Sync + Clone {
    /// Find the best WAN-layer route for `dest`: the most specific match in the
    /// main routing table that does NOT go through `exclude_iface` (the VPN tunnel).
    ///
    /// Returns `None` when WAN connectivity is gone entirely.
    /// The returned [`WanRoute`] includes the preferred source address (local IP),
    /// which lets `wan_changed()` detect DHCP reassignments on the same interface.
    async fn get_wan_route_for(&self, dest: Ipv4Addr, exclude_iface: &str) -> Result<Option<WanRoute>, Error>;

    /// Check whether `device` still has a route to `dest` and, if so, return its
    /// current gateway and preferred source address.
    ///
    /// Returns `None` when the interface no longer routes `dest` (interface removed,
    /// brought down, or its route was deleted). Used by `wan_changed()` to detect
    /// DHCP reassignments on the original WAN device without triggering a false
    /// reconnect when a second network interface is added.
    async fn get_route_via_device(&self, dest: Ipv4Addr, device: &str) -> Result<Option<WanRoute>, Error>;

    /// Add a route: destination via optional gateway through device.
    #[allow(dead_code)]
    async fn route_add(&self, dest: &str, gateway: Option<&str>, device: &str) -> Result<(), Error>;

    /// Delete a route by destination and device.
    #[allow(dead_code)]
    async fn route_del(&self, dest: &str, device: &str) -> Result<(), Error>;
}
