use edgli::hopr_lib::config::HoprLibConfig;
use serde_yaml;
use thiserror::Error;

use std::fs;
use std::path::Path;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Hopr edge client configuration file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    YamlDeserialization(#[from] serde_yaml::Error),
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
