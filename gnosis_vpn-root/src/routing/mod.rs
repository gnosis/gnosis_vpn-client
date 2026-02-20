use async_trait::async_trait;
use thiserror::Error;

use gnosis_vpn_lib::shell_command_ext::{self, Logs};
use gnosis_vpn_lib::{dirs, wireguard};

mod bypass;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

pub(crate) use bypass::{BypassRouteManager, WanInterface};

// ============================================================================
// Shared Utilities
// ============================================================================

/// Parses key-value pairs from command output to extract device and gateway.
///
/// This utility works for both Linux (`ip route show default`) and macOS
/// (`route -n get 0.0.0.0`) command outputs by parameterizing the key names.
///
/// # Arguments
/// * `output` - The command output to parse
/// * `device_key` - Key for device name (e.g., "dev" on Linux, "interface:" on macOS)
/// * `gateway_key` - Key for gateway IP (e.g., "via" on Linux, "gateway:" on macOS)
/// * `filter_suffix` - Optional suffix to filter out (e.g., Some(":") for macOS
///   to handle "gateway: index: 28" cases)
///
/// # Returns
/// A tuple of (device_name, Option<gateway_ip>)
pub(crate) fn parse_key_value_output(
    output: &str,
    device_key: &str,
    gateway_key: &str,
    filter_suffix: Option<&str>,
) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();

    let device_index = parts.iter().position(|&x| x == device_key);
    let gateway_index = parts.iter().position(|&x| x == gateway_key);

    let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
        Some(dev) => dev.to_string(),
        None => {
            tracing::error!(%output, "Unable to determine default interface");
            return Err(Error::NoInterface);
        }
    };

    let gateway = gateway_index
        .and_then(|idx| parts.get(idx + 1))
        .filter(|gw| {
            // Filter out values matching the suffix (e.g., "index:" on macOS)
            filter_suffix.is_none_or(|suffix| !gw.ends_with(suffix))
        })
        .map(|gw| gw.to_string());

    Ok((device, gateway))
}

#[cfg(target_os = "linux")]
pub use linux::{
    FwmarkInfrastructure, WanInfo, dynamic_router, setup_fwmark_infrastructure,
    static_fallback_router as static_router, teardown_fwmark_infrastructure,
};
#[cfg(target_os = "macos")]
pub use macos::{WanInfo, dynamic_router, static_router};

#[cfg(target_os = "linux")]
pub type RouterHandle = rtnetlink::Handle;
#[cfg(not(target_os = "linux"))]
pub type RouterHandle = ();

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
}

#[async_trait]
pub trait Routing {
    async fn setup(&mut self) -> Result<(), Error>;
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error>;
}
