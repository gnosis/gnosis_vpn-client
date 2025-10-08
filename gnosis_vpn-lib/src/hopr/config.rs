pub use edgli::hopr_lib::config::{HoprLibConfig, SafeModule};

use edgli::hopr_lib::{Balance, WxHOPR};
use rand::Rng;
use serde_yaml;
use thiserror::Error;
use url::Url;

use std::fs;
use std::path::{Path, PathBuf};

use crate::balance;
use crate::chain::contracts::SafeModuleDeploymentResult;
use crate::dirs;
use crate::network::Network;

const CONFIG_FILE: &str = "gnosisvpn-hopr.yaml";
const DB_FILE: &str = "gnosisvpn-hopr.db";
const SAFE_FILE: &str = "gnosisvpn-hopr.safe";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Hopr edge client configuration file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    YamlDeserialization(#[from] serde_yaml::Error),
    #[error("Project directory error: {0}")]
    Dirs(#[from] crate::dirs::Error),
}

impl From<SafeModuleDeploymentResult> for SafeModule {
    fn from(result: SafeModuleDeploymentResult) -> Self {
        Self {
            safe_address: result.safe_address.into(),
            module_address: result.module_address.into(),
            ..Default::default()
        }
    }
}

pub fn from_path(path: &Path) -> Result<HoprLibConfig, Error> {
    let content = fs::read_to_string(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;

    serde_yaml::from_str::<HoprLibConfig>(&content).map_err(Error::YamlDeserialization)
}

pub fn store_safe(safe_module: &SafeModule) -> Result<(), Error> {
    let safe_file = safe_file()?;
    let content = serde_yaml::to_string(&safe_module)?;
    fs::write(&safe_file, &content).map_err(Error::IO)
}

pub fn write_cached(cfg: &HoprLibConfig) -> Result<(), Error> {
    let conf_file = config_file()?;
    let content = serde_yaml::to_string(&cfg)?;
    fs::write(&conf_file, &content).map_err(Error::IO)
}

pub fn generate(network: Network, rpc_provider: Url, ticket_value: Balance<WxHOPR>) -> Result<HoprLibConfig, Error> {
    let safe_module: SafeModule = match fs::read_to_string(safe_file()?) {
        Ok(content) => serde_yaml::from_str::<SafeModule>(&content)?,
        Err(e) => return Err(Error::IO(e)),
    };
    let content = format!(
        r#"
db:
    data: {db_file}
host:
    port: {port}
    address: !Domain edge.example.com
chain:
    network: {network}
    provider: {rpc_provider}
    announce: true
    enable_logs_snapshot: true
    logs_snapshot_url: {logs_snapshot_url}
safe_module:
    safe_address: {safe_address}
    module_address: {module_address}
strategy:
    on_fail_continue: true
    strategies:
        - !AutoFunding
          funding_amount: {funding_amount}
          min_stake_threshold: {min_stake_threshold}
        - !ClosureFinalizer
          max_closure_overdue: 300
"#,
        db_file = db_file()?.to_string_lossy(),
        port = rand::rng().random_range(20000..65000),
        network = network,
        rpc_provider = rpc_provider,
        logs_snapshot_url = snapshot_url(network.clone()),
        safe_address = safe_module.safe_address,
        module_address = safe_module.module_address,
        funding_amount = balance::funding_amount(ticket_value),
        min_stake_threshold = balance::min_stake_threshold(ticket_value),
    );
    serde_yaml::from_str::<HoprLibConfig>(&content).map_err(Error::YamlDeserialization)
}

pub fn config_file() -> Result<PathBuf, Error> {
    dirs::cache_dir(CONFIG_FILE).map_err(Error::Dirs)
}

fn db_file() -> Result<PathBuf, Error> {
    dirs::config_dir(DB_FILE).map_err(Error::Dirs)
}

fn safe_file() -> Result<PathBuf, Error> {
    dirs::config_dir(SAFE_FILE).map_err(Error::Dirs)
}

fn snapshot_url(network: Network) -> &'static str {
    match network {
        Network::Dufour => "https://logs-snapshots.hoprnet.org/dufour-v3.0-latest.tar.xz",
        Network::Rotsee => "https://logs-snapshots-rotsee.hoprnet.org/rotsee-v3.0-latest.tar.xz",
    }
}
