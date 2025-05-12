use std::collections::HashMap;
use std::fs;
use std::path::Path;
use thiserror::Error;

use crate::connection::Destination;
use crate::entry_node::EntryNode;
use crate::wireguard::config::Config as WireGuardConfig;

mod v1;
mod v2;

pub const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";
pub const ENV_VAR: &str = "GNOSISVPN_CONFIG_PATH";

#[derive(Clone, Debug)]
pub enum Config {
    V1(v1::Config),
    V2(v2::Config),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] toml::de::Error),
    #[error("Unsupported config version: {0}")]
    VersionMismatch(u8),
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("hoprd_node is missing")]
    HoprdNodeMissing,
}

pub fn read(path: &Path) -> Result<Config, Error> {
    let content = fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;

    let res_v2 = toml::from_str::<v2::Config>(&content);
    match res_v2 {
        Ok(config) => {
            if config.version == 2 {
                return Ok(Config::V2(config));
            } else {
                return Err(Error::VersionMismatch(config.version));
            }
        }
        Err(err) => {
            let res_v1 = toml::from_str::<v1::Config>(&content);
            match res_v1 {
                Ok(config) => {
                    if config.version == 1 {
                        tracing::warn!("found v1 configuration file, please update to configuration file version 2");
                        return Ok(Config::V1(config));
                    } else {
                        return Err(Error::VersionMismatch(config.version));
                    }
                }
                Err(_err) => {
                    // return error from v2 config as this is the desired config file
                    return Err(Error::Deserialization(err));
                }
            }
        }
    }
}

impl Config {
    pub fn entry_node(&self) -> Result<EntryNode, ConfigError> {
        match self {
            Config::V1(config) => config.entry_node(),
            Config::V2(config) => config.entry_node(),
        }
    }

    pub fn destinations(&self) -> HashMap<String, Destination> {
        match self {
            Config::V1(config) => config.destinations(),
            Config::V2(config) => config.destinations(),
        }
    }

    pub fn wireguard(&self) -> WireGuardConfig {
        match self {
            Config::V1(config) => config.wireguard(),
            Config::V2(config) => config.wireguard(),
        }
    }
}
