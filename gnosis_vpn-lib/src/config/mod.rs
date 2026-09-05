use edgli::hopr_lib::api::types::primitive::errors::GeneralError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::collections::HashMap;
use std::path::Path;
use tokio::fs;

use crate::connection::{destination::Destination, options::Options as ConnectionOptions};
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::pix_config::PixConfig;
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
    pub pix_strategy: PixConfig,
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
    use super::*;
    use std::time::Duration;

    async fn read_config(contents: &str) -> Config {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        fs::write(&path, contents).await.expect("write config");
        read(&path).await.expect("valid config")
    }

    fn with_blokli_section(blokli: &str) -> String {
        format!(
            r#"version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

{blokli}
"#
        )
    }

    #[tokio::test]
    async fn blokli_request_timeout_is_read_from_the_config_file() {
        let config = read_config(&with_blokli_section("[blokli]\nrequest_timeout = \"45s\"")).await;

        assert_eq!(config.blokli.request_timeout, Duration::from_secs(45));
    }

    #[tokio::test]
    async fn an_absent_blokli_section_yields_the_default_request_timeout() {
        let config = read_config(&with_blokli_section("")).await;

        assert_eq!(config.blokli.request_timeout, BlokliConfig::default().request_timeout);
    }

    /// The `[blokli]` table is shared by every config version, so an older file picks the key
    /// up without a version bump.
    #[tokio::test]
    async fn blokli_request_timeout_is_read_from_a_v5_config_file() {
        let config = read_config(
            r#"version = 5

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

[blokli]
request_timeout = "45s"
"#,
        )
        .await;

        assert_eq!(config.blokli.request_timeout, Duration::from_secs(45));
    }
}
