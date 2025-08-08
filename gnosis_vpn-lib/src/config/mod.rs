use std::collections::HashMap;
use std::fs;
use std::path::Path;

use thiserror::Error;

use crate::connection::Destination;
use crate::entry_node::EntryNode;
use crate::peer_id::PeerId;
use crate::wg_tooling::Config as WireGuardConfig;

mod v1;
mod v2;
mod v3;

pub const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";
pub const ENV_VAR: &str = "GNOSISVPN_CONFIG_PATH";

#[derive(Clone, Debug)]
pub enum Config {
    V1(v1::Config),
    V2(v2::Config),
    V3(v3::Config),
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
        2 => {
            tracing::warn!("found v2 configuration file, please update configuration file");
            let res_v2 = toml::from_str::<v2::Config>(&content)?;
            let wrong_keys = v2::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            Ok(Config::V2(res_v2))
        }
        3 => {
            let res_v3 = toml::from_str::<v3::Config>(&content)?;
            let wrong_keys = v3::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            Ok(Config::V3(res_v3))
        }
        _ => Err(Error::VersionMismatch(version as u8)),
    }
}

impl Config {
    pub fn entry_node(&self) -> EntryNode {
        match self {
            Config::V1(config) => config.entry_node(),
            Config::V2(config) => Into::<v3::Config>::into(config).entry_node(),
            Config::V3(config) => config.entry_node(),
        }
    }

    pub fn destinations(&self) -> HashMap<PeerId, Destination> {
        match self {
            Config::V1(config) => config.destinations(),
            Config::V2(config) => Into::<v3::Config>::into(config).destinations(),
            Config::V3(config) => config.destinations(),
        }
    }

    pub fn wireguard(&self) -> WireGuardConfig {
        match self {
            Config::V1(config) => config.wireguard(),
            Config::V2(config) => Into::<v3::Config>::into(config).wireguard(),
            Config::V3(config) => config.wireguard(),
        }
    }
}
