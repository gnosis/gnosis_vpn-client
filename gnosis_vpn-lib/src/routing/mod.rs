use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("ip rule setup failed with status code {0}")]
    IpRuleSetup(i32),
}

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;
