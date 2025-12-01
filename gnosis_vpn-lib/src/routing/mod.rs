use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ip rule setup error - status code {0}")]
    IpRuleSetup(u16),
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;
