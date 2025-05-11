use std::fmt::Debug;
use thiserror::Error;

mod kernel;
mod tooling;
mod userspace;

#[derive(Error, Debug, Clone)]
pub enum Error {
    #[error("implementation pending: {0}")]
    NotYetImplemented(String),
    // cannot use IO error because it does not allow Clone or Copy
    #[error("IO error: {0}")]
    IO(String),
    #[error("encoding error: {0}")]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("monitoring error: {0}")]
    Monitoring(String),
    #[error("wireguard error: {0}")]
    WgError(String),
}

#[derive(Clone, Debug)]
pub struct ConnectSession {
    pub interface: InterfaceInfo,
    pub peer: PeerInfo,
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub private_key: String,
    pub address: String,
    pub allowed_ips: Option<String>,
    pub listen_port: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub public_key: String,
    pub endpoint: String,
}

pub fn best_flavor() -> Result<Box<dyn WireGuard>, Error> {
    if kernel::available().is_ok() {
        return Ok(Box::new(kernel::Kernel::new()));
    }
    if userspace::available().is_ok() {
        return Ok(Box::new(userspace::UserSpace::new()));
    }
    tooling::available().map(|_| Box::new(tooling::Tooling::new()) as Box<dyn WireGuard>)
}

pub trait WireGuard: Debug {
    fn generate_key(&self) -> Result<String, Error>;
    fn connect_session(&self, session: &ConnectSession) -> Result<(), Error>;
    fn public_key(&self, priv_key: &str) -> Result<String, Error>;
    fn close_session(&self) -> Result<(), Error>;
}

impl ConnectSession {
    pub fn new(interface: &InterfaceInfo, peer: &PeerInfo) -> Self {
        ConnectSession {
            interface: interface.clone(),
            peer: peer.clone(),
        }
    }
}
