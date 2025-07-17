use std::fmt::Debug;
use thiserror::Error;

pub mod config;
mod kernel;
mod tooling;
mod userspace;

#[derive(Error, Debug)]
pub enum Error {
    #[error("implementation pending: {0}")]
    NotYetImplemented(String),
    #[error("implementation not available")]
    NotAvailable,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("encoding error: {0}")]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("monitoring error: {0}")]
    Monitoring(String),
    #[error("wireguard error: {0}")]
    WgError(String),
    #[error("Unable to determine project directories")]
    ProjectDirs,
}

#[derive(Clone, Debug)]
pub struct ConnectSession {
    pub interface: InterfaceInfo,
    pub peer: PeerInfo,
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub key_pair: KeyPair,
    pub address: String,
    pub allowed_ips: Option<String>,
    pub listen_port: Option<u16>,
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub public_key: String,
    pub endpoint: String,
}

#[derive(Clone, Debug)]
pub struct KeyPair {
    priv_key: String,
    pub_key: String,
}

pub fn best_flavor() -> Result<Box<dyn WireGuard>, Error> {
    if kernel::available().is_ok() {
        return Ok(Box::new(kernel::Kernel::new()));
    }
    if userspace::available().is_ok() {
        return Ok(Box::new(userspace::UserSpace::new()));
    }
    if tooling::available().is_ok() {
        return Ok(Box::new(tooling::Tooling::new()));
    }
    Err(Error::NotAvailable)
}

pub trait WireGuard: Debug + WireGuardClone + Send {
    fn generate_key(&self) -> Result<String, Error>;
    fn connect_session(&self, session: &ConnectSession) -> Result<(), Error>;
    fn public_key(&self, priv_key: &str) -> Result<String, Error>;
    fn close_session(&self) -> Result<(), Error>;
}

pub trait WireGuardClone {
    fn clone_box(&self) -> Box<dyn WireGuard>;
}

impl<T> WireGuardClone for T
where
    T: 'static + WireGuard + Clone,
{
    fn clone_box(&self) -> Box<dyn WireGuard> {
        Box::new(self.clone())
    }
}

impl Clone for Box<dyn WireGuard> {
    fn clone(&self) -> Box<dyn WireGuard> {
        self.clone_box()
    }
}

impl ConnectSession {
    pub fn new(interface: &InterfaceInfo, peer: &PeerInfo) -> Self {
        ConnectSession {
            interface: interface.clone(),
            peer: peer.clone(),
        }
    }

    pub fn to_file_string(&self) -> String {
        let allowed_ips = match &self.interface.allowed_ips {
            Some(allowed_ips) => allowed_ips.clone(),
            None => {
                self.interface
                    .address
                    .split('.')
                    .take(2)
                    .collect::<Vec<&str>>()
                    .join(".")
                    + ".0.0/9"
            }
        };
        let listen_port_line = self
            .interface
            .listen_port
            .map(|port| format!("ListenPort = {port}\n"))
            .unwrap_or_default();

        format!(
            "[Interface]
PrivateKey = {private_key}
Address = {address}
{listen_port_line}

[Peer]
PublicKey = {public_key}
Endpoint = {endpoint}
AllowedIPs = {allowed_ips}
PersistentKeepalive = 30
",
            private_key = self.interface.key_pair.priv_key,
            address = self.interface.address,
            public_key = self.peer.public_key,
            endpoint = self.peer.endpoint,
            allowed_ips = allowed_ips,
            listen_port_line = listen_port_line,
        )
    }
}
