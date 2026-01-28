use async_trait::async_trait;
use thiserror::Error;

use gnosis_vpn_lib::{dirs, wireguard};
use gnosis_vpn_lib::shell_command_ext::{self, Logs};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::{dynamic_router, static_fallback_router as static_router};
#[cfg(target_os = "macos")]
pub use macos::{dynamic_router, static_router};

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

    pub fn is_not_available(&self) -> bool {
        matches!(self, Self::NotAvailable)
    }
}

#[async_trait]
pub trait Routing {
    async fn setup(&mut self) -> Result<(), Error>;
    async fn teardown(&mut self, logs: Logs) -> Result<(), Error>;
}
