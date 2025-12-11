use thiserror::Error;

use gnosis_vpn_lib::{dirs, event, shell_command_ext, worker};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    ShellCommand(#[from] shell_command_ext::Error),
    #[cfg(target_os = "macos")]
    #[error("Unable to determine default interface")]
    NoInterface,
    #[error(transparent)]
    Dirs(#[from] dirs::Error),
    #[error(transparent)]
    IO(#[from] std::io::Error),
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
        #[cfg(target_os = "linux")]
        linux::setup(&self.worker, &self.wg_data).await?;

        #[cfg(target_os = "macos")]
        macos::setup(&self.worker, &self.wg_data).await?;

        Ok(())
    }

    pub async fn teardown(&self) -> Result<(), Error> {
        #[cfg(target_os = "linux")]
        linux::teardown(&self.worker, &self.wg_data).await?;

        #[cfg(target_os = "macos")]
        macos::teardown(&self.worker, &self.wg_data).await?;

        Ok(())
    }
}
