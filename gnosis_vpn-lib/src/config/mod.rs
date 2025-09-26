use edgli::hopr_lib::Address;
use thiserror::Error;
use url::Url;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::connection::{destination::Destination, options::Options};
use crate::wg_tooling::Config as WireGuardConfig;

mod v1;
mod v2;
mod v3;
mod v4;

pub const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";
pub const ENV_VAR: &str = "GNOSISVPN_CONFIG_PATH";

#[derive(Debug)]
pub struct Hopr {
    rpc_provider: Url,
    identity_pass: String,
    identity_file: Path,
}

#[derive(Clone, Debug)]
pub enum Config {
    V4(v4::Config),
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
    #[error("Outdated config version: {0}")]
    OutdatedVersion(u8),
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
            Err(Error::OutdatedVersion(1))
        }
        2 => {
            tracing::warn!("found v2 configuration file, please update configuration file");
            Err(Error::OutdatedVersion(2))
        }
        3 => {
            tracing::warn!("found v3 configuration file, please update configuration file");
            Err(Error::OutdatedVersion(3))
        }
        4 => {
            let res = toml::from_str::<v4::Config>(&content)?;
            let wrong_keys = v4::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            Ok(Config::V4(res))
        }
        _ => Err(Error::VersionMismatch(version as u8)),
    }
}

impl Config {
    pub fn destinations(&self) -> HashMap<Address, Destination> {
        match self {
            Config::V4(config) => config.destinations(),
        }
    }

    pub fn connection(&self) -> Options {
        match self {
            Config::V4(config) => config.connection(),
        }
    }

    pub fn wireguard(&self) -> WireGuardConfig {
        match self {
            Config::V4(config) => config.wireguard(),
        }
    }

    pub fn hopr(&self) -> Hopr {
        match self {
            Config::V4(config) => config.hopr(),
        }
    }
}
