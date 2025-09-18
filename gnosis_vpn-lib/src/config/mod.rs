use std::collections::HashMap;
use std::fs;
use std::path::Path;

use edgli::hopr_lib::Address;
use thiserror::Error;

use crate::connection::{destination::Destination, options::Options};
use crate::wg_tooling::Config as WireGuardConfig;

mod v1;

pub const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";
pub const ENV_VAR: &str = "GNOSISVPN_CONFIG_PATH";

#[derive(Clone, Debug)]
pub enum Config {
    V1(v1::Config),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Configuration file not found")]
    NoFile,
    #[error("Unable to determine configuration version")]
    VersionNotFound,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] toml::de::Error),
    #[error("Unsupported config version: {0}")]
    VersionMismatch(u8),
}

pub fn read(path: &Path) -> Result<Config, Error> {
    let content = fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;

    let table = content.parse::<toml::Table>()?;
    let version = table
        .get("version")
        .and_then(|v| v.as_integer())
        .ok_or(Error::VersionNotFound)?;

    match version {
        1 => {
            tracing::warn!("found v1 configuration file, please update configuration file");
            let res_v1 = toml::from_str::<v1::Config>(&content)?;
            Ok(Config::V1(res_v1))
        }
        _ => Err(Error::VersionMismatch(version as u8)),
    }
}

impl Config {
    pub fn destinations(&self) -> HashMap<Address, Destination> {
        match self {
            Config::V1(config) => config.destinations(),
        }
    }

    pub fn connection(&self) -> Options {
        match self {
            Config::V1(config) => config.connection(),
        }
    }

    pub fn wireguard(&self) -> WireGuardConfig {
        match self {
            Config::V1(config) => config.wireguard(),
        }
    }
}
