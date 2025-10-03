pub use edgli::hopr_lib::config::{HoprLibConfig, SafeModule};
use rand::Rng;
use serde_yaml;
use thiserror::Error;
use url::Url;

use std::fs;
use std::path::{Path, PathBuf};

use crate::chain::contracts::SafeModuleDeploymentResult;
use crate::dirs;

const CONFIG_FILE: &str = "gnosisvpn-hopr.yaml";
const DB_FILE: &str = "gnosisvpn-hopr.db";
pub const DEFAULT_NETWORK: &str = "dufour";

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

pub fn write_default(cfg: &HoprLibConfig) -> Result<(), Error> {
    let conf_file = config_file()?;
    let content = serde_yaml::to_string(&cfg)?;
    fs::write(&conf_file, &content).map_err(Error::IO)
}

pub fn generate(network: String, rpc_provider: Url, safe_module: SafeModule) -> Result<HoprLibConfig, Error> {
    // TODO use typed HoprLibConfig
    // TODO use channel funding amounts dependend on network
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
    announce: false
safe_module:
    safe_address: {safe_address}
    module_address: {module_address}
strategy:
    on_fail_continue: true
    strategies:
        - !AutoFunding
          funding_amount: "10 wxHOPR"
          min_stake_threshold: "1 wxHOPR"
        - !ClosureFinalizer
          max_closure_overdue: 300
"#,
        db_file = db_file()?.to_string_lossy(),
        port = rand::rng().random_range(20000..65000),
        network = network,
        rpc_provider = rpc_provider,
        safe_address = safe_module.safe_address,
        module_address = safe_module.module_address,
    );
    serde_yaml::from_str::<HoprLibConfig>(&content).map_err(Error::YamlDeserialization)
}

pub fn config_file() -> Result<PathBuf, Error> {
    dirs::config_dir(CONFIG_FILE).map_err(Error::Dirs)
}

fn db_file() -> Result<PathBuf, Error> {
    dirs::config_dir(DB_FILE).map_err(Error::Dirs)
}
