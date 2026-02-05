use rand::Rng;
use serde_yaml;
use thiserror::Error;
use tokio::fs;

use std::path::{Path, PathBuf};

use crate::compat::SafeModule;
use crate::dirs;

pub use edgli::hopr_lib::config::HoprLibConfig;

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

pub async fn from_path(path: &Path) -> Result<HoprLibConfig, Error> {
    let content = fs::read_to_string(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;

    serde_yaml::from_str::<HoprLibConfig>(&content).map_err(Error::YamlDeserialization)
}

pub async fn store_safe(safe_module: &SafeModule) -> Result<(), Error> {
    let safe_file = safe_file()?;
    let content = serde_yaml::to_string(&safe_module)?;
    fs::write(&safe_file, &content).await.map_err(Error::IO)
}

pub async fn read_safe() -> Result<SafeModule, Error> {
    let content = fs::read_to_string(safe_file()?).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    serde_yaml::from_str::<SafeModule>(&content).map_err(Error::YamlDeserialization)
}

pub async fn generate(safe_module: &SafeModule) -> Result<HoprLibConfig, Error> {
    let content = format!(
        r##"
host:
    port: {port}
    address: !Domain edge.example.com
safe_module:
    safe_address: {safe_address}
    module_address: {module_address}
"##,
        port = rand::rng().random_range(20000..65000),
        safe_address = safe_module.safe_address,
        module_address = safe_module.module_address,
    );
    serde_yaml::from_str::<HoprLibConfig>(&content).map_err(Error::YamlDeserialization)
}

pub fn safe_file() -> Result<PathBuf, Error> {
    let file = dirs::config_dir(SAFE_FILE).map_err(Error::Dirs)?;
    debug!("Using safe file at {:?}", file);
    Ok(file)
}

pub(crate) fn db_file() -> Result<PathBuf, Error> {
    dirs::config_dir(DB_FILE).map_err(Error::Dirs)
}
