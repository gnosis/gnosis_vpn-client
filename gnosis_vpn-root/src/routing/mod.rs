use thiserror::Error;

use gnosis_vpn_lib::{dirs, hopr::hopr_lib::async_trait, shell_command_ext, wireguard};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub use linux::{build_userspace_router as build_router, static_fallback_router};
#[cfg(target_os = "macos")]
pub use macos::{build_firewall_router as build_router, static_fallback_router};

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
    #[error("Not yet implemented")]
    NotImplemented,

    #[cfg(target_os = "macos")]
    #[error("firewall error: {0}")]
    PfCtl(#[from] pfctl::Error),

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[error("General error")]
    General(String),

    #[cfg(target_os = "linux")]
    #[error("rtnetlink error: {0} ")]
    RtnetlinkError(#[from] rtnetlink::Error),

    #[cfg(target_os = "linux")]
    #[error("iptables error: {0} ")]
    IpTablesError(String),
}

impl Error {
    #[cfg(target_os = "linux")]
    pub fn iptables(e: impl Into<Box<dyn std::error::Error>>) -> Self {
        Self::IpTablesError(e.into().to_string())
    }
}

#[async_trait]
pub trait Routing {
    async fn setup(&self) -> Result<(), Error>;

    async fn teardown(&self) -> Result<(), Error>;
}
