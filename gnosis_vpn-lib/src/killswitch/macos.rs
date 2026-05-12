use std::net::IpAddr;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Unsupported(String),
}

/// Killswitch firewall stub for macOS — no-op until a native implementation is added.
pub struct Firewall;

impl Firewall {
    pub fn new() -> Self {
        Firewall
    }

    pub fn apply_policy(&mut self, _allowed_ips: &[IpAddr]) -> Result<(), Error> {
        Ok(())
    }

    pub fn reset_policy(&mut self) -> Result<(), Error> {
        Ok(())
    }
}
