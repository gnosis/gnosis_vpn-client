//! # Routing
//!
//! This module provides split-tunnel VPN routing for different platforms.
//! Uses platform-native APIs to add peer IP bypass routes before bringing up
//! WireGuard, ensuring no interruption during VPN setup.
//!
//! ## Architecture
//!
//! The public API is [`RouteManager`] — a background actor that owns all routing
//! state and communicates with the root daemon loop via channel pairs:
//! - Root sends [`RouteCmd`] to the manager
//! - Manager sends [`RouteEvent`] back to root's `daemon_loop`

use thiserror::Error;

use gnosis_vpn_lib::shell_command_ext;
use gnosis_vpn_lib::{dirs, wireguard};

mod bypass;
pub mod manager;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        pub(crate) mod route_ops_linux;
        mod linux;
    } else if #[cfg(target_os = "macos")] {
        pub(crate) mod route_ops_macos;
        mod macos;
    }
}

pub use manager::{RouteCmd, RouteEvent};

#[cfg(target_os = "linux")]
pub(crate) use linux::{create_manager, reset_on_startup};
#[cfg(target_os = "macos")]
pub(crate) use macos::{create_manager, reset_on_startup};

/// RFC1918 + link-local networks that should bypass VPN tunnel.
/// These are more specific than the VPN default routes (0.0.0.0/1, 128.0.0.0/1)
/// so they take precedence in the routing table.
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

    #[error("General error: {0}")]
    General(String),

    #[cfg(target_os = "linux")]
    #[error("rtnetlink error: {0} ")]
    Rtnetlink(#[from] rtnetlink::Error),
}
