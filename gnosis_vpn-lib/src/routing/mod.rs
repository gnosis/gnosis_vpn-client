use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Command error: {0}")]
    Command(#[from] crate::util::Error),
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;
