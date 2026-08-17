//! Split-tunnel VPN routing implementations for Linux and macOS.
//! Uses route operations via platform-native APIs.

use async_trait::async_trait;
use thiserror::Error;

use gnosis_vpn_lib::dirs;
use gnosis_vpn_lib::shell_command_ext::{self, Logs};

use std::net::Ipv4Addr;
use std::os::fd::BorrowedFd;

pub(crate) mod dns;
pub(crate) mod ipv6_blackhole;
pub(crate) mod route_ops;
pub(crate) mod sweep;
pub(crate) mod tun;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        pub(crate) mod route_ops_linux;
        mod linux;
    } else if #[cfg(target_os = "macos")] {
        pub(crate) mod route_ops_macos;
        mod macos;
    }
}

// ============================================================================
// Shared Utilities
// ============================================================================

#[cfg(target_os = "linux")]
pub use linux::static_router;
#[cfg(target_os = "macos")]
pub use macos::static_router;

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
    #[error("TUN device error: {0}")]
    Tun(String),

    #[cfg(target_os = "linux")]
    #[error("General error: {0}")]
    General(String),

    #[cfg(target_os = "linux")]
    #[error("rtnetlink error: {0} ")]
    Rtnetlink(#[from] rtnetlink::Error),
}

#[async_trait]
pub trait Routing {
    /// Set up the VPN tunnel. Returns the resolved TUN interface name on success.
    async fn setup(&mut self) -> Result<String, Error>;
    async fn teardown(&mut self, logs: Logs);
    /// A borrowed fd for the TUN device root created, so it can be duplicated
    /// before being handed to the worker via `SCM_RIGHTS`. `None` before setup
    /// or after teardown.
    fn tun_fd(&self) -> Option<BorrowedFd<'_>>;
    /// Whether the WAN default route differs from the one captured during setup.
    ///
    /// Used to tell real network changes apart from route events caused by our
    /// own routing setup/teardown (which would otherwise feed back into an
    /// endless reconnect loop).
    async fn wan_changed(&mut self) -> Result<bool, Error>;

    /// Add a /32 bypass route for a dynamically-discovered peer IP.
    /// Called by the routing actor during periodic allowlist refresh.
    /// Should be a no-op (return Ok) if routing is not yet set up.
    async fn add_peer_bypass_route(&mut self, ip: Ipv4Addr) -> Result<(), Error>;

    /// Remove the /32 bypass route for a peer IP that is no longer alive.
    /// Should be a no-op (return Ok) if routing is not yet set up.
    async fn remove_peer_bypass_route(&mut self, ip: Ipv4Addr) -> Result<(), Error>;
}
