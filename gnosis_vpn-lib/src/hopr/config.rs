use serde_saphyr;
use thiserror::Error;
use tokio::fs;

use std::path::PathBuf;
use std::time::Duration;

use crate::compat::SafeModule;
use crate::dirs;

pub use edgli::hopr_lib::config::HoprLibConfig;

const SAFE_FILE: &str = "gnosisvpn-hopr.safe";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Hopr edge client configuration file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Output error: {0}")]
    Output(String),
    #[error("Project directory error: {0}")]
    Dirs(#[from] crate::dirs::Error),
}

impl From<serde_saphyr::Error> for Error {
    fn from(e: serde_saphyr::Error) -> Self {
        Error::Output(e.to_string())
    }
}

impl From<serde_saphyr::ser::Error> for Error {
    fn from(e: serde_saphyr::ser::Error) -> Self {
        Error::Output(e.to_string())
    }
}

pub async fn from_path(path: PathBuf) -> Result<HoprLibConfig, Error> {
    let content = fs::read_to_string(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;

    serde_saphyr::from_str::<HoprLibConfig>(&content).map_err(Into::into)
}

pub async fn store_safe(state_home: PathBuf, safe_module: &SafeModule) -> Result<(), Error> {
    let safe_file = safe_file(state_home);
    let content = serde_saphyr::to_string(&safe_module)?;
    fs::write(&safe_file, &content).await.map_err(Error::IO)
}

pub async fn read_safe(state_home: PathBuf) -> Result<SafeModule, Error> {
    let content = fs::read_to_string(safe_file(state_home)).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    serde_saphyr::from_str::<SafeModule>(&content).map_err(Into::into)
}

pub async fn generate(safe_module: &SafeModule, path_planner_min_ack_rate: f64) -> Result<HoprLibConfig, Error> {
    let mut cfg = HoprLibConfig::default();
    cfg.safe_module.safe_address = safe_module
        .safe_address
        .parse()
        .map_err(|e| Error::Output(format!("invalid safe address: {e}")))?;
    cfg.safe_module.module_address = safe_module
        .module_address
        .parse()
        .map_err(|e| Error::Output(format!("invalid module address: {e}")))?;
    // Edge client: probe aggressively at startup so relay observations are populated
    // before the first health check fires. recheck_threshold matches interval so every
    // relay is re-probed on every round during warm-up.
    cfg.protocol.probe.timeout = Duration::from_secs(3);
    cfg.protocol.probe.interval = Duration::from_secs(3);
    cfg.protocol.probe.recheck_threshold = Duration::from_secs(3);
    cfg.protocol.path_planner = edgli::latency_path_planner_config(path_planner_min_ack_rate);
    Ok(cfg)
}

pub fn safe_file(state_home: PathBuf) -> PathBuf {
    dirs::config_dir(state_home, SAFE_FILE)
}
