use thiserror::Error;

use gnosis_vpn_lib::{dirs, event, shell_command_ext, wireguard, worker};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "linux")]
use linux::{setup, teardown};
#[cfg(target_os = "macos")]
use macos::{setup, teardown};

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
}

pub struct Routing {
    worker: worker::Worker,
    wg_data: event::WgData,
}

impl Routing {
    pub fn new(worker: worker::Worker, wg_data: event::WgData) -> Self {
        Self { worker, wg_data }
    }

    pub async fn setup(&self) -> Result<(), Error> {
        setup(&self.worker, &self.wg_data).await
    }

    pub async fn teardown(&self) -> Result<(), Error> {
        teardown(&self.worker, &self.wg_data).await
    }
}
