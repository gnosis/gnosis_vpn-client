use std::fs;
use std::path::PathBuf;
use thiserror::Error;

use crate::entry_node::EntryNode;

pub mod v1;
mod v2;

const CONFIG_VERSION: u8 = 2;
const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";

pub type Config = v2::Config;

#[cfg(target_family = "unix")]
pub fn path() -> PathBuf {
    match std::env::var("GNOSISVPN_CONFIG_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(std::env::VarError::NotPresent) => PathBuf::from(DEFAULT_PATH),
        Err(e) => {
            tracing::warn!(warn = ?e, "using default config path");
            PathBuf::from(DEFAULT_PATH)
        }
    }
}

#[derive(Error, Debug)]
pub enum ReadError {
    #[error("Config file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] toml::de::Error),
    #[error("Unsupported config version")]
    VersionMismatch(u8),
}

#[derive(Error, Debug)]
pub enum ConfigIssue {
    #[error("[hoprd_node] entry missing")]
    HoprdNodeMissing,
}

pub fn read() -> Result<Config, ReadError> {
    let content = fs::read_to_string(path()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ReadError::NoFile
        } else {
            ReadError::IO(e)
        }
    })?;

    let res_v2 = toml::from_str::<v2::Config>(&content);
    match res_v2 {
        Ok(config) => {
            if config.version == CONFIG_VERSION {
                return Ok(config);
            } else {
                return Err(ReadError::VersionMismatch(config.version));
            }
        }
        Err(err) => {
            let res_v1 = toml::from_str::<v1::Config>(&content);
            if res_v1.is_ok() {
                return Err(ReadError::VersionMismatch(1));
            } else {
                return Err(ReadError::Deserialization(err));
            }
        }
    }
}

impl Config {
    pub fn entry_node(&self) -> Result<EntryNode, ConfigIssue> {
        let hoprd_node = self.hoprd_node.as_ref().ok_or(ConfigIssue::HoprdNodeMissing)?;
        let internal_connection_port = hoprd_node.internal_connection_port.map(|p| format!(":{}", p));
        let listen_host = self
            .connection
            .as_ref()
            .and_then(|c| c.listen_host.clone())
            .or(internal_connection_port);
        let en = EntryNode::new(hoprd_node.endpoint.clone(), hoprd_node.api_token.clone(), listen_host);
        Ok(en)
    }
}
