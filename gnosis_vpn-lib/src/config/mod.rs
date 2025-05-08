use std::fs;
use std::path::PathBuf;
use thiserror::Error;

pub mod v1;
mod v2;

const CONFIG_VERSION: u8 = 2;
const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";

pub type Config = v2::Config;

#[cfg(target_family = "unix")]
pub fn path() -> PathBuf {
    match std::env::var("GNOSISVPN_CONFIG_PATH") {
        Ok(path) => PathBuf::from(path),
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

pub fn read() -> Result<v2::Config, Error> {
    let content = fs::read_to_string(path()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;

    let res_v2 = toml::from_str::<v2::Config>(&content);
    match res_v2 {
        Ok(config) => {
            if config.version == CONFIG_VERSION {
                return Ok(config);
            } else {
                return Err(Error::VersionMismatch(config.version));
            }
        }
        Err(err) => {
            let res_v1 = toml::from_str::<v1::Config>(&content);
            if res_v1.is_ok() {
                return Err(Error::VersionMismatch(1));
            } else {
                return Err(Error::Deserialization(err));
            }
        }
    }
}
