use thiserror::Error;

use crate::worker;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Command error: {0}")]
    Command(#[from] crate::util::Error),
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
