use serde::{Deserialize, Serialize};
use std::cmp::PartialEq;
use std::default::Default;
use std::fmt::Display;
use std::fs;
use std::path::PathBuf;
use std::vec::Vec;
use thiserror::Error;
use url::Url;

use crate::peer_id::PeerId;

const SUPPORTED_CONFIG_VERSIONS: [u8; 1] = [1];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub hoprd_node: Option<EntryNodeConfig>,
    pub connection: Option<SessionConfig>,
    pub wireguard: Option<WireGuardConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryNodeConfig {
    pub endpoint: Url,
    pub api_token: String,
    pub internal_connection_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub capabilities: Option<Vec<SessionCapabilitiesConfig>>,
    pub destination: PeerId,
    pub listen_host: Option<String>,
    pub path: Option<SessionPathConfig>,
    pub target: Option<SessionTargetConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WireGuardConfig {
    pub address: String,
    pub server_public_key: String,
    pub allowed_ips: Option<String>,
    pub preshared_key: Option<String>,
    pub private_key: Option<String>,
    pub listen_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionTargetConfig {
    pub type_: Option<SessionTargetType>,
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum SessionCapabilitiesConfig {
    #[default]
    #[serde(alias = "segmentation")]
    Segmentation,
    #[serde(alias = "retransmission")]
    Retransmission,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum SessionTargetType {
    #[default]
    Plain,
    Sealed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionPathConfig {
    #[serde(alias = "hop")]
    Hop(u8),
    #[serde(alias = "intermediates")]
    Intermediates(Vec<PeerId>),
}

const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";

#[cfg(target_family = "unix")]
pub fn path() -> PathBuf {
    match std::env::var("GNOSISVPN_CONFIG_PATH") {
        Ok(path) => {
            tracing::info!(?path, "using custom config path");
            PathBuf::from(path)
        }
        Err(std::env::VarError::NotPresent) => PathBuf::from(DEFAULT_PATH),
        Err(e) => {
            tracing::warn!(warn = ?e, "using default config path");
            PathBuf::from(DEFAULT_PATH)
        }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Config file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] toml::de::Error),
    #[error("Unsupported config version")]
    VersionMismatch(u8),
}

pub fn read() -> Result<Config, Error> {
    let content = fs::read_to_string(path()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    let config: Config = toml::from_str(&content).map_err(Error::Deserialization)?;
    if SUPPORTED_CONFIG_VERSIONS.contains(&config.version) {
        Ok(config)
    } else {
        Err(Error::VersionMismatch(config.version))
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            version: 1,
            hoprd_node: None,
            connection: None,
            wireguard: None,
        }
    }
}

impl Default for SessionPathConfig {
    fn default() -> Self {
        SessionPathConfig::Hop(1)
    }
}

impl Default for SessionTargetConfig {
    fn default() -> Self {
        SessionTargetConfig {
            type_: Some(SessionTargetType::Plain),
            host: Some(default_session_target_host()),
            port: Some(default_session_target_port()),
        }
    }
}

impl Display for SessionTargetType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SessionTargetType::Plain => write!(f, "Plain"),
            SessionTargetType::Sealed => write!(f, "Sealed"),
        }
    }
}

pub fn default_session_target_host() -> String {
    "172.17.0.1".to_string()
}

pub fn default_session_target_port() -> u16 {
    51820
}
