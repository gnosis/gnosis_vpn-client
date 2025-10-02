use edgli::hopr_lib::Address;
use edgli::hopr_lib::config::HoprLibConfig;
use serde_yaml;
use thiserror::Error;

use std::fs;
use std::path::{Path, PathBuf};

use crate::chain::contracts::SafeModuleDeploymentResult;
use crate::dirs;

const CONFIG_FILE: &str = "gnosisvpn-hopr.yaml";

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

#[derive(Clone, Debug)]
pub struct SafeModule {
    pub safe_address: Address,
    pub module_address: Address,
}

impl From<SafeModuleDeploymentResult> for SafeModule {
    fn from(result: SafeModuleDeploymentResult) -> Self {
        Self {
            safe_address: result.safe_address.into(),
            module_address: result.module_address.into(),
        }
    }
}

// db:
//   data: default
// chain:
//   network: default
//   provider:  https://gnosis-rpc.publicnode.com - from parameter
//   announce: true - static
// host:
//   port: 23334 - random until working one found
//   address: !Domain edge.example.com - static
// safe_module:
//   safe_address: from safecreation
//   module_address: from safecreation

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

pub fn config_file() -> Result<PathBuf, Error> {
    dirs::config_dir(CONFIG_FILE).map_err(Error::Dirs)
}
