//! # Routing Modes
//!
//! This module provides split-tunnel VPN routing implementations for different platforms.
//!
//! ## Dynamic Routing (Linux only, default)
//! Uses rtnetlink + firewall rules for policy-based routing with firewall marks.
//! Most reliable but requires root and nftables availability.
//!
//! ## Static Routing (all platforms)
//! Uses route operations via platform-native APIs.
//! Simpler but may have reduced reliability during network changes.

use async_trait::async_trait;
use thiserror::Error;

use gnosis_vpn_lib::shell_command_ext::{self, Logs};
use gnosis_vpn_lib::{dirs, wireguard};

mod bypass;
pub(crate) mod route_ops;
pub(crate) mod wg_ops;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        pub(crate) mod netlink_ops;
        pub(crate) mod nftables_ops;
        pub(crate) mod route_ops_linux;
        mod linux;
    } else if #[cfg(target_os = "macos")] {
        pub(crate) mod route_ops_macos;
        mod macos;
    }
}

#[cfg(test)]
pub(crate) mod mocks;

pub(crate) use bypass::{BypassRouteManager, WanInterface};

// ============================================================================
// Shared Utilities
// ============================================================================

#[cfg(target_os = "linux")]
pub use linux::{
    cleanup_stale_fwmark_rules, dynamic_router,
    static_fallback_router as static_router,
};
#[cfg(target_os = "macos")]
pub use macos::{static_router, dynamic_router};

/// RFC1918 + link-local networks that should bypass VPN tunnel.
/// These are more specific than the VPN default routes (0.0.0.0/1, 128.0.0.0/1)
/// so they take precedence in the routing table.
///
/// **Limitation:** If your VPN server uses an IP in a non-standard 10.x range
/// (e.g., 10.1.0.0/16), traffic may be misrouted because 10.0.0.0/8 bypass
/// takes precedence over the more specific VPN server IP. The VPN_TUNNEL_SUBNET
/// (10.128.0.0/9) is designed to override this for the standard HOPR VPN range,
/// but custom VPN configurations may require adjustment.
pub(crate) const RFC1918_BYPASS_NETS: &[(&str, u8)] = &[
    ("10.0.0.0", 8),     // RFC1918 Class A private
    ("172.16.0.0", 12),  // RFC1918 Class B private
    ("192.168.0.0", 16), // RFC1918 Class C private
    ("169.254.0.0", 16), // Link-local (APIPA)
];

/// VPN internal subnet that must be routed through the tunnel.
/// This is more specific than the RFC1918 bypass (10.0.0.0/8),
/// so it takes precedence and ensures VPN server traffic uses the tunnel.
pub(crate) const VPN_TUNNEL_SUBNET: (&str, u8) = ("10.128.0.0", 9);

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    ShellCommand(#[from] shell_command_ext::Error),
    #[error("Unable to determine default interface")]
    NoInterface,
    #[error("Directories error: {0}")]
    Dirs(#[from] dirs::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("wg-quick error: {0}")]
    WgTooling(#[from] wireguard::Error),

    /// explicitly allowing dead_code here to avoid cumbersome cfg targets everywhere
    #[error("This functionality is not available on this platform")]
    #[allow(dead_code)]
    NotAvailable,

    #[error("General error: {0}")]
    General(String),

    #[cfg(target_os = "linux")]
    #[error("rtnetlink error: {0} ")]
    Rtnetlink(#[from] rtnetlink::Error),

    #[cfg(target_os = "linux")]
    #[error("nftables error: {0} ")]
    NfTables(String),
}

#[async_trait]
pub trait Routing {
    async fn setup(&mut self) -> Result<(), Error>;
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error>;
}
