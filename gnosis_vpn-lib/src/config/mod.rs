use edgli::hopr_lib::api::types::primitive::errors::GeneralError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::collections::HashMap;
use std::path::Path;
use tokio::fs;

use crate::connection::{destination::Destination, options::Options as ConnectionOptions};
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::strategy_config::StrategyConfig;
use crate::wireguard::Config as WireGuardConfig;

mod v3;
mod v4;
mod v5;
mod v6;

pub const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";
pub const ENV_VAR: &str = "GNOSISVPN_CONFIG_PATH";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub connection: ConnectionOptions,
    pub destinations: HashMap<String, Destination>,
    pub wireguard: WireGuardConfig,
    pub blokli: BlokliConfig,
    pub strategy: StrategyConfig,
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
    #[error("ping and main sessions must both have surb_balancing enabled or both disabled")]
    SurbBalancingMismatch,
    #[error("Error in hopr-lib: {0}")]
    HoprGeneral(#[from] GeneralError),
}

pub async fn read(path: &Path) -> Result<Config, Error> {
    let content = fs::read_to_string(path).await.map_err(|e| {
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
        5 => {
            let res = toml::from_str::<v5::Config>(&content)?;
            let wrong_keys = v5::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            res.try_into()
        }
        6 => {
            let res = toml::from_str::<v6::Config>(&content)?;
            let wrong_keys = v6::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            res.try_into()
        }
        _ => Err(Error::VersionMismatch(version as u8)),
    }
}

#[cfg(test)]
mod tests {
    // Keeps the shipped example config in sync with the schema: it must parse as the
    // current version without any ignored keys.
    #[test]
    fn documented_config_matches_current_schema() {
        let content = include_str!("../../../documented-config.toml");
        let table = content.parse::<toml::Table>().expect("valid TOML");
        assert_eq!(super::v6::wrong_keys(&table), Vec::<String>::new());
        let cfg = toml::from_str::<super::v6::Config>(content).expect("deserializes as v6");
        let _: super::Config = cfg.try_into().expect("converts to runtime config");
    }
}
