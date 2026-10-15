use edgli::hopr_lib::api::types::primitive::errors::GeneralError;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::path::{Path, PathBuf};
use tokio::fs;

use crate::connection::{
    destination::{DefaultTargets, Destinations},
    options::Options as ConnectionOptions,
};
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::pix_config::PixConfig;
use crate::hopr::strategy_config::StrategyConfig;
use crate::wireguard::Config as WireGuardConfig;

mod v3;
mod v4;
mod v5;
mod v6;
mod v7;

pub const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";
pub const ENV_VAR: &str = "GNOSISVPN_CONFIG_PATH";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub connection: ConnectionOptions,
    pub destinations: Destinations,
    /// Needed past load time: a destination that discovery stops reporting falls back to these.
    pub default_targets: DefaultTargets,
    pub wireguard: WireGuardConfig,
    pub blokli: BlokliConfig,
    pub strategy: StrategyConfig,
    pub pix_strategy: PixConfig,
    /// The country database the installer ships; a v7 file names it, older files get the platform default.
    pub geoip_db: PathBuf,
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
    #[error("geoip_db is required from config version 7 on")]
    GeoIpDbUnset,
    #[error("geoip_db is not a file: {}", .0.display())]
    GeoIpDbNotFound(PathBuf),
    #[error("ping and main sessions must both have surb_balancing enabled or both disabled")]
    SurbBalancingMismatch,
    #[error("surb_balancing.ramp interval and duration must both be greater than zero")]
    SurbRampZero,
    #[error("Error in hopr-lib: {0}")]
    HoprGeneral(#[from] GeneralError),
    #[error(
        "destinations {first} and {second} are both {address} at {hops} hops - a destination is its exit and its path, so one of them must go"
    )]
    DuplicateDestination {
        first: String,
        second: String,
        address: String,
        hops: usize,
    },
}

pub async fn read(path: &Path) -> Result<Config, Error> {
    let content = fs::read_to_string(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    let config = parse(&content)?;
    validate_geoip_db(&config.geoip_db)?;
    Ok(config)
}

/// Path only, failing at load: the worker must open the file anyway, a reader cannot ride in the startup params.
fn validate_geoip_db(path: &Path) -> Result<(), Error> {
    if path.is_file() {
        return Ok(());
    }
    Err(Error::GeoIpDbNotFound(path.to_path_buf()))
}

pub fn parse(content: &str) -> Result<Config, Error> {
    let table = content.parse::<toml::Table>()?;
    let version = table
        .get("version")
        .and_then(|v| v.as_integer())
        .ok_or(Error::VersionNotFound)?;

    match version {
        3 => {
            let res = toml::from_str::<v3::Config>(content)?;
            let wrong_keys = v3::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            let res: v7::Config = res.try_into()?;
            res.try_into()
        }
        4 => {
            let res = toml::from_str::<v4::Config>(content)?;
            let wrong_keys = v4::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            let res: v7::Config = res.try_into()?;
            res.try_into()
        }
        5 => {
            let res = toml::from_str::<v5::Config>(content)?;
            let wrong_keys = v5::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            let res: v7::Config = res.try_into()?;
            res.try_into()
        }
        6 => {
            let res = toml::from_str::<v6::Config>(content)?;
            let wrong_keys = v6::wrong_keys(&table);
            for key in wrong_keys.iter() {
                tracing::warn!(%key, "ignoring unsupported key in configuration file");
            }
            let res: v7::Config = res.try_into()?;
            res.try_into()
        }
        7 => {
            let res = toml::from_str::<v7::Config>(content)?;
            let wrong_keys = v7::wrong_keys(&table);
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

    fn read_config(contents: &str) -> Config {
        parse(contents).expect("valid config")
    }

    async fn read_file(contents: &str) -> Result<Config, Error> {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("config.toml");
        fs::write(&path, contents).await.expect("write config");
        read(&path).await
    }

    #[tokio::test]
    async fn a_v7_file_names_an_existing_geoip_database() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geoip-country.mmdb");
        let config = read_file(&format!("version = 7\ngeoip_db = {fixture:?}\n"))
            .await
            .expect("valid config");

        assert_eq!(fixture, config.geoip_db);
    }

    #[tokio::test]
    async fn a_v7_file_without_geoip_db_is_rejected() {
        let result = read_file("version = 7\n").await;

        assert!(matches!(result, Err(Error::GeoIpDbUnset)), "{result:?}");
    }

    #[tokio::test]
    async fn a_geoip_db_path_that_is_not_a_file_is_rejected() {
        let result = read_file("version = 7\ngeoip_db = \"/nonexistent/geoip.mmdb\"\n").await;

        assert!(matches!(result, Err(Error::GeoIpDbNotFound(_))), "{result:?}");
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
        let config = read_config(&with_blokli_section("[blokli]\nrequest_timeout = \"45s\""));

        assert_eq!(config.blokli.request_timeout, Duration::from_secs(45));
    }

    #[tokio::test]
    async fn an_absent_blokli_section_yields_the_default_request_timeout() {
        let config = read_config(&with_blokli_section(""));

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
        );

        assert_eq!(config.blokli.request_timeout, Duration::from_secs(45));
    }
}
