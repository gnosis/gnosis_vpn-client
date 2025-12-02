use thiserror::Error;

use crate::{dirs, util, worker};

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Command(#[from] util::Error),
    #[error("Unable to determine default interface")]
    NoInterface,
    #[error(transparent)]
    Dirs(#[from] dirs::Error),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

#[cfg(target_os = "linux")]
mod linux;

mod macos;

#[cfg(target_os = "linux")]
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    linux::setup(worker).await
}

pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    macos::setup(worker).await
}
