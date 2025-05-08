use std::fs;
use std::path::PathBuf;
use thiserror::Error;

pub mod v1;

const SUPPORTED_CONFIG_VERSIONS: [u8; 1] = [1];
const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";

#[cfg(target_family = "unix")]
pub fn path() -> PathBuf {
    match std::env::var("GNOSISVPN_CONFIG_PATH") {
        Ok(path) => {
            tracing::info!(?path, "using custom config path");
            PathBuf::from(path)
        }
        Err(std::env::VarError::NotPresent) => PathBuf::from(DEFAULT_PATH),
        Err(e) => {
            tracing::warn!(warn = ?e, "using default config path");
            PathBuf::from(DEFAULT_PATH)
        }
    }
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("Config file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] toml::de::Error),
    #[error("Unsupported config version")]
    VersionMismatch(u8),
}

pub fn read() -> Result<v1::Config, Error> {
    let content = fs::read_to_string(path()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    let config: v1::Config = v1::parse(&content).map_err(Error::Deserialization)?;
    if SUPPORTED_CONFIG_VERSIONS.contains(&config.version) {
        Ok(config)
    } else {
        Err(Error::VersionMismatch(config.version))
    }
}
