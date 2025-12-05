use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("dependency not available: {0}")]
    NotAvailable(String),
    #[error("dependency {0} not executable: {1}")]
    NotExecutable(String, String),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Encoding error: {0}")]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
    #[error("TOML error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("Monitoring error: {0}")]
    Monitoring(String),
    #[error("WG generate key error [status: {0}]: {1}")]
    WgGenKey(i32, String),
    #[error("WG quick error [status: {0}]: {1}")]
    WgQuick(i32, String),
    #[error("Project directory error: {0}")]
    Dirs(#[from] crate::dirs::Error),
}

#[derive(Clone, Debug)]
pub struct WireGuard {
    pub config: Config,
    pub key_pair: KeyPair,
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub address: String,
    #[allow(dead_code)]
    pub mtu: usize,
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub public_key: String,
    pub endpoint: String,
}

#[derive(Clone, Debug)]
pub struct KeyPair {
    pub priv_key: String,
    pub public_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub listen_port: Option<u16>,
    pub force_private_key: Option<String>,
    pub allowed_ips: Option<String>,
}

impl Config {
    pub(crate) fn new<L, M, S>(listen_port: Option<L>, allowed_ips: Option<M>, force_private_key: Option<S>) -> Self
    where
        L: Into<u16>,
        M: Into<String>,
        S: Into<String>,
    {
        Config {
            listen_port: listen_port.map(Into::into),
            allowed_ips: allowed_ips.map(Into::into),
            force_private_key: force_private_key.map(Into::into),
        }
    }
}

const WG_CONFIG_FILE: &str = "wg0_gnosisvpn.conf";

impl WireGuard {
    fn to_file_string(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> String {
        let allowed_ips = match &self.config.allowed_ips {
            Some(allowed_ips) => allowed_ips.clone(),
            None => interface.address.split('.').take(2).collect::<Vec<&str>>().join(".") + ".0.0/9",
        };
        let listen_port_line = self
            .config
            .listen_port
            .map(|port| format!("ListenPort = {port}\n"))
            .unwrap_or_default();

        // WireGuard has differently sized packets not exactly adhering to MTU
        // so we postpone optimizing on this level for now
        // MTU = {mtu}
        format!(
            "[Interface]
PrivateKey = {private_key}
Address = {address}
{listen_port_line}

[Peer]
PublicKey = {public_key}
Endpoint = {endpoint}
AllowedIPs = {allowed_ips}
",
            private_key = self.key_pair.priv_key,
            address = interface.address,
            public_key = peer.public_key,
            endpoint = peer.endpoint,
            allowed_ips = allowed_ips,
            listen_port_line = listen_port_line,
            // WireGuard has differnently sized packets not exactly adhering to MTU
            // so we postpone optimizing on this level for now
            // mtu = interface.mtu,
        )
    }
}
