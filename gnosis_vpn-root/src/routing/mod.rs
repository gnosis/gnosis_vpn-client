use thiserror::Error;

use gnosis_vpn_lib::{dirs, shell_command_ext, worker};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    ShellCommand(#[from] shell_command_ext::Error),
    #[error("Unable to determine default interface")]
    NoInterface,
    #[error(transparent)]
    Dirs(#[from] dirs::Error),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    linux::setup(worker).await
}

#[cfg(target_os = "macos")]
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    macos::setup(worker).await
}
