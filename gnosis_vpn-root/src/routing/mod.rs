//! # Routing
//!
//! This module provides split-tunnel VPN routing implementations for different platforms.
//! Uses route operations via platform-native APIs to add peer IP bypass routes before
//! bringing up WireGuard, ensuring no interruption during VPN setup.
//!
//! ## Architecture
//!
//! The public API is [`RouteManager`] — a background actor that owns all routing state
//! and communicates with the root daemon loop via channel pairs:
//! - Root sends [`RouteCmd`] to the manager
//! - Manager sends [`RouteEvent`] back to root's `daemon_loop`
//!
//! Internal platform implementations ([`linux`] / [`macos`]) implement the [`Routing`]
//! trait and are owned by the manager.

use async_trait::async_trait;
use thiserror::Error;

use gnosis_vpn_lib::shell_command_ext::{self, Logs};
use gnosis_vpn_lib::{dirs, wireguard};

mod bypass;
pub mod manager;
pub(crate) mod route_ops;
pub(crate) mod wg_ops;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        pub(crate) mod route_ops_linux;
        mod linux;
    } else if #[cfg(target_os = "macos")] {
        pub(crate) mod route_ops_macos;
        mod macos;
    }
}

#[cfg(test)]
pub(crate) mod mocks;

pub use manager::{RouteCmd, RouteEvent, RouteManager};

// Platform-specific entry points used by the manager task (not public API).
#[cfg(target_os = "linux")]
pub(crate) use linux::{reset_on_startup, static_fallback_router as static_router};
#[cfg(target_os = "macos")]
pub(crate) use macos::{reset_on_startup, static_router};

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
}

#[async_trait]
pub trait Routing: Send {
    async fn setup(&mut self) -> Result<(), Error>;
    async fn teardown(&mut self, logs: Logs);
}
