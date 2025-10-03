use edgli::hopr_lib::exports::network::types::types::{IpOrHost, RoutingOptions, SealedHost};
use edgli::hopr_lib::{Address, GeneralError};
use thiserror::Error;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::connection::{destination::Destination, options::Options as ConnectionOptions};
use crate::wg_tooling::Config as WireGuardConfig;

mod v3;
mod v4;

pub const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";
pub const ENV_VAR: &str = "GNOSISVPN_CONFIG_PATH";

#[derive(Debug, PartialEq)]
pub struct Config {
    pub connection: ConnectionOptions,
    pub destinations: HashMap<Address, Destination>,
    pub wireguard: WireGuardConfig,
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
    TomlDeserialization(#[from] toml::de::Error),
    #[error("Unsupported config version: {0}")]
    VersionMismatch(u8),
    #[error("No destinations")]
    NoDestinations,
    #[error("Error in hopr-lib: {0}")]
    HoprGeneral(#[from] GeneralError),
}

impl Config {
    pub fn channel_targets(&self) -> Vec<Address> {
        self.destinations
            .values()
            .filter_map(|d| match d.routing.clone() {
                RoutingOptions::IntermediatePath(path) => path.into_iter().next(),
                _ => None,
            })
            .collect()
    }
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
        3 => {
            let res = toml::from_str::<v3::Config>(&content)?;
            let wrong_keys = v3::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            res.try_into()
        }
        4 => {
            let res = toml::from_str::<v4::Config>(&content)?;
            let wrong_keys = v4::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            res.try_into()
        }
        _ => Err(Error::VersionMismatch(version as u8)),
    }
}
