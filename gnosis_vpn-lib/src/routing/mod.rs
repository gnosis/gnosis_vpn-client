use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ip rule setup failed")]
    IpRuleSetup,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;
