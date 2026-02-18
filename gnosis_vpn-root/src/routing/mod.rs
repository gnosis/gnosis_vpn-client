use async_trait::async_trait;
use thiserror::Error;

use gnosis_vpn_lib::shell_command_ext::{self, Logs};
use gnosis_vpn_lib::{dirs, wireguard};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::{
    FwmarkInfrastructure, WanInfo, dynamic_router, setup_fwmark_infrastructure,
    static_fallback_router as static_router, teardown_fwmark_infrastructure,
};
#[cfg(target_os = "macos")]
pub use macos::{WanInfo, dynamic_router, static_router};

/// RFC1918 + link-local networks that should bypass VPN tunnel.
/// These are more specific than the VPN routes (0.0.0.0/1, 128.0.0.0/1)
/// so they take precedence in the routing table.
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

    #[cfg(target_os = "linux")]
    #[error("General error: {0}")]
    General(String),

    #[cfg(target_os = "linux")]
    #[error("rtnetlink error: {0} ")]
    Rtnetlink(#[from] rtnetlink::Error),

    #[cfg(target_os = "linux")]
    #[error("iptables error: {0} ")]
    IpTables(String),
}

impl Error {
    #[cfg(target_os = "linux")]
    pub fn iptables(e: impl Into<Box<dyn std::error::Error>>) -> Self {
        Self::IpTables(e.into().to_string())
    }

    #[allow(dead_code)] // May be used for error handling in the future
    pub fn is_not_available(&self) -> bool {
        matches!(self, Self::NotAvailable)
    }
}

#[async_trait]
pub trait Routing {
    async fn setup(&mut self) -> Result<(), Error>;
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error>;
}
