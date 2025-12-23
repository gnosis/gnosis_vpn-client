use async_trait::async_trait;
use thiserror::Error;

use gnosis_vpn_lib::{dirs, shell_command_ext, wireguard};

#[async_trait]
pub trait Routing {
    async fn setup(&self) -> Result<(), Error>;
    async fn teardown(&self) -> Result<(), Error>;
}

mod dynamic;
mod static_;

pub use dynamic::Dynamic;
pub use static_::Static;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    ShellCommand(#[from] shell_command_ext::Error),
    #[cfg(target_os = "macos")]
    #[error("Unable to determine default interface")]
    NoInterface,
    #[error("Directories error: {0}")]
    Dirs(#[from] dirs::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("wg-quick error: {0}")]
    WgTooling(#[from] wireguard::Error),
    #[error("Not implemented")]
    NotImplemented,
}
