use edgli::hopr_lib::api::types::primitive::errors::GeneralError;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::connection::{
    destination::{DefaultTargets, Destinations},
    options::Options as ConnectionOptions,
};
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::pix_config::PixConfig;
use crate::hopr::strategy_config::StrategyConfig;
use crate::network_probing::ProbingPolicy;
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
    pub probing: ProbingPolicy,
    /// The country database, at the installer's location unless a v7 file points elsewhere.
    pub geoip_db: PathBuf,
    /// The Curvy proving files, at the installer's location unless a v7 file points elsewhere.
    pub curvy_zk_dir: PathBuf,
    /// The fixed node set of bootstrap mode; none means the mode is not available.
    pub bootstrap: Option<Bootstrap>,
    /// The mode this configuration runs in; only [`Config::in_mode`] leaves normal mode.
    pub mode: gnosis_vpn_ipc::Mode,
}

/// The exits bootstrap mode connects to and the relayers it opens channels to, neither discovered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bootstrap {
    pub relayers: HashSet<Address>,
    pub exits: Destinations,
}

impl Config {
    /// The configuration a worker runs with in `mode`: bootstrap mode swaps in the bootstrap exits
    /// and keeps channels to the bootstrap relayers; without a `[bootstrap]` section it changes nothing.
    pub fn in_mode(mut self, mode: gnosis_vpn_ipc::Mode) -> Config {
        if let (gnosis_vpn_ipc::Mode::Bootstrap, Some(bootstrap)) = (mode, &self.bootstrap) {
            self.destinations = bootstrap.exits.clone();
            self.strategy.channel_allowlist = Some(bootstrap.relayers.clone());
            self.mode = mode;
        }
        self
    }

    /// Status from the config file alone, while no core runs to answer it.
    pub fn offline_status(&self, run_mode: gnosis_vpn_ipc::RunMode, target: Option<&str>) -> gnosis_vpn_ipc::Status {
        let mut vals: Vec<_> = self.destinations.values().collect();
        vals.sort_unstable_by(|a, b| a.connect_id.cmp(&b.connect_id));
        let destinations = vals
            .into_iter()
            .map(|dest| gnosis_vpn_ipc::DestinationStatus {
                destination: dest.into(),
                route_health: None,
            })
            .collect();
        gnosis_vpn_ipc::Status {
            run_mode,
            destinations,
            target: target.map(Into::into),
            connection: gnosis_vpn_ipc::Connection::Disconnected,
            disconnecting: vec![],
            probe: None,
            pix_chunk_bytes: None,
            pix_price_per_byte: crate::wire::wxhopr(self.pix_strategy.price_per_byte),
            probing: None,
            recent: vec![],
            mode: self.mode,
            bootstrap_available: self.bootstrap.is_some(),
        }
    }
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
    #[error("geoip_db is not a file: {}", .0.display())]
    GeoIpDbNotFound(PathBuf),
    #[error("curvy_zk_dir is not a directory: {}", .0.display())]
    CurvyZkDirNotFound(PathBuf),
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
    #[error("[bootstrap] needs at least one entry in {0}")]
    EmptyBootstrap(&'static str),
}

pub async fn read(path: &Path) -> Result<Config, Error> {
    let content = fs::read_to_string(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    let mut config = parse(&content)?;
    // The worker runs in the owner's home, so a relative path would resolve elsewhere there.
    config.geoip_db = std::path::absolute(&config.geoip_db)?;
    config.curvy_zk_dir = std::path::absolute(&config.curvy_zk_dir)?;
    validate_geoip_db(&config.geoip_db)?;
    validate_curvy_zk_dir(&config.curvy_zk_dir)?;
    Ok(config)
}

/// Path only, failing at load: the worker must open the file anyway, a reader cannot ride in the startup params.
fn validate_geoip_db(path: &Path) -> Result<(), Error> {
    if path.is_file() {
        return Ok(());
    }
    Err(Error::GeoIpDbNotFound(path.to_path_buf()))
}

/// Directory only: curvy-witnesscalc checks each file's digest when it proves.
fn validate_curvy_zk_dir(path: &Path) -> Result<(), Error> {
    if path.is_dir() {
        return Ok(());
    }
    Err(Error::CurvyZkDirNotFound(path.to_path_buf()))
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
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let fixture = fixtures.join("geoip-country.mmdb");
        let config = read_file(&format!(
            "version = 7\ngeoip_db = {fixture:?}\ncurvy_zk_dir = {fixtures:?}\n"
        ))
        .await
        .expect("valid config");

        assert_eq!(fixture, config.geoip_db);
        assert_eq!(fixtures, config.curvy_zk_dir);
    }

    #[tokio::test]
    async fn relative_paths_resolve_against_the_reading_process() {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let config = read_file(
            "version = 7\ngeoip_db = \"tests/fixtures/geoip-country.mmdb\"\ncurvy_zk_dir = \"tests/fixtures\"\n",
        )
        .await
        .expect("valid config");

        assert_eq!(fixtures.join("geoip-country.mmdb"), config.geoip_db);
        assert_eq!(fixtures, config.curvy_zk_dir);
    }

    #[test]
    fn a_v7_file_without_geoip_db_falls_back_to_the_installer_path() {
        let config = parse("version = 7\n").expect("valid config");

        assert_eq!(PathBuf::from(crate::geoip::DEFAULT_DB_PATH), config.geoip_db);
    }

    #[tokio::test]
    async fn a_geoip_db_path_that_is_not_a_file_is_rejected() {
        let result = read_file("version = 7\ngeoip_db = \"/nonexistent/geoip.mmdb\"\n").await;

        assert!(matches!(result, Err(Error::GeoIpDbNotFound(_))), "{result:?}");
    }

    #[test]
    fn a_v7_file_without_curvy_zk_dir_falls_back_to_the_installer_path() {
        let config = parse("version = 7\n").expect("valid config");

        assert_eq!(
            PathBuf::from(crate::hopr::pix_config::DEFAULT_CURVY_ZK_DIR),
            config.curvy_zk_dir
        );
    }

    #[tokio::test]
    async fn a_curvy_zk_dir_that_is_not_a_directory_is_rejected() {
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geoip-country.mmdb");
        let result = read_file(&format!(
            "version = 7\ngeoip_db = {fixture:?}\ncurvy_zk_dir = {fixture:?}\n"
        ))
        .await;

        assert!(matches!(result, Err(Error::CurvyZkDirNotFound(_))), "{result:?}");
    }

    const AMSTERDAM: &str = "0xc9a0ab0f96608b4da489321003070fda0d45f4ac";
    const IOWA: &str = "0xd61cb773673dcfe622872f1c7d9965639d6f8f45";
    const RELAY_A: &str = "0x2598c0773e3fa07437de329737d3e1a18f272e8f";
    const RELAY_B: &str = "0x6cc978516dfe8f007bfaf1736b33fb9e6c7f7e2f";
    const GERMANY: &str = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739";

    /// A configured destination beside a bootstrap set of two exits and two relays.
    fn with_bootstrap(bootstrap: &str) -> String {
        format!(
            r#"version = 7

[destinations.germany]
address = "{GERMANY}"

{bootstrap}
"#
        )
    }

    fn bootstrap_section() -> String {
        format!(
            r#"[bootstrap]
relayers = ["{RELAY_A}", "{RELAY_B}"]

[bootstrap.exits.amsterdam]
address = "{AMSTERDAM}"
country = "NL"
meta = {{ name = "Amsterdam" }}

[bootstrap.exits.iowa]
address = "{IOWA}"
path = {{ hops = 2 }}
wireguard_server = "10.0.0.1:51820"
"#
        )
    }

    fn addr(s: &str) -> Address {
        s.parse().expect("valid address")
    }

    #[test]
    fn a_bootstrap_section_carries_its_relayers_and_exits() {
        let config = read_config(&with_bootstrap(&bootstrap_section()));
        let bootstrap = config.bootstrap.expect("bootstrap section present");

        assert_eq!(
            std::collections::HashSet::from([addr(RELAY_A), addr(RELAY_B)]),
            bootstrap.relayers
        );
        assert_eq!(2, bootstrap.exits.len());
        let amsterdam = bootstrap.exits.by_connect_id("amsterdam").expect("amsterdam exit");
        assert_eq!(addr(AMSTERDAM), amsterdam.address);
        assert_eq!(Some("Amsterdam"), amsterdam.meta.name.as_deref());
        assert_eq!(
            Some("NL".to_string()),
            amsterdam.overrides.configured_country.as_ref().map(|c| c.to_string())
        );
        // Shown before the first announced-peers fetch, which refreshes the geo of all exits.
        assert_eq!(
            Some(("NL".to_string(), crate::geoip::GeoSource::Configured)),
            amsterdam.geo.as_ref().map(|g| (g.country.to_string(), g.source))
        );
        assert_eq!(config.default_targets.wireguard_server, amsterdam.wireguard_server);
        let iowa = bootstrap.exits.by_connect_id("iowa").expect("iowa exit");
        assert_eq!(2, iowa.routing.hop_count());
        assert_eq!(None, iowa.overrides.configured_country);
        assert_eq!(
            "10.0.0.1:51820".parse::<std::net::SocketAddr>().unwrap(),
            iowa.wireguard_server
        );
        // The bootstrap exits stay out of the normal destinations.
        assert_eq!(1, config.destinations.len());
    }

    #[test]
    fn a_config_without_a_bootstrap_section_has_no_bootstrap() {
        assert_eq!(None, read_config(&with_bootstrap("")).bootstrap);
    }

    #[test]
    fn a_bootstrap_exit_with_an_invalid_country_is_rejected() {
        let section = bootstrap_section().replace(r#"country = "NL""#, r#"country = "NLD""#);

        assert!(parse(&with_bootstrap(&section)).is_err());
    }

    #[test]
    fn a_bootstrap_relayer_that_is_not_an_address_is_rejected() {
        let section = bootstrap_section().replace(RELAY_B, "0x1234");

        assert!(parse(&with_bootstrap(&section)).is_err());
    }

    #[test]
    fn a_bootstrap_section_without_relayers_or_exits_is_rejected() {
        let no_relayers =
            format!("[bootstrap]\nrelayers = []\n\n[bootstrap.exits.amsterdam]\naddress = \"{AMSTERDAM}\"\n");
        let no_exits = format!("[bootstrap]\nrelayers = [\"{RELAY_A}\"]\n");

        assert!(matches!(
            parse(&with_bootstrap(&no_relayers)),
            Err(Error::EmptyBootstrap("relayers"))
        ));
        assert!(matches!(
            parse(&with_bootstrap(&no_exits)),
            Err(Error::EmptyBootstrap("exits"))
        ));
    }

    #[test]
    fn bootstrap_mode_runs_only_the_bootstrap_exits_and_relayers() {
        let config = read_config(&with_bootstrap(&bootstrap_section()));
        let bootstrap = config.bootstrap.clone().expect("bootstrap section present");

        let effective = config.in_mode(gnosis_vpn_ipc::Mode::Bootstrap);

        assert_eq!(bootstrap.exits, effective.destinations);
        assert_eq!(Some(bootstrap.relayers), effective.strategy.channel_allowlist);
        assert_eq!(gnosis_vpn_ipc::Mode::Bootstrap, effective.mode);
    }

    #[test]
    fn normal_mode_and_a_config_without_bootstrap_run_as_configured() {
        let with = read_config(&with_bootstrap(&bootstrap_section()));
        let without = read_config(&with_bootstrap(""));

        assert_eq!(with.clone(), with.clone().in_mode(gnosis_vpn_ipc::Mode::Normal));
        assert_eq!(
            without.clone(),
            without.clone().in_mode(gnosis_vpn_ipc::Mode::Bootstrap)
        );
    }

    #[test]
    fn offline_status_reports_the_mode_and_whether_bootstrap_is_available() {
        let with = read_config(&with_bootstrap(&bootstrap_section()));
        let without = read_config(&with_bootstrap(""));

        let status = with
            .in_mode(gnosis_vpn_ipc::Mode::Bootstrap)
            .offline_status(gnosis_vpn_ipc::RunMode::NotRunning, None);
        assert_eq!(gnosis_vpn_ipc::Mode::Bootstrap, status.mode);
        assert!(status.bootstrap_available);

        // Without a node set, bootstrap mode is refused and the config stays in normal mode.
        let status = without
            .in_mode(gnosis_vpn_ipc::Mode::Bootstrap)
            .offline_status(gnosis_vpn_ipc::RunMode::NotRunning, None);
        assert_eq!(gnosis_vpn_ipc::Mode::Normal, status.mode);
        assert!(!status.bootstrap_available);
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
