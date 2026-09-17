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

pub async fn generate(
    safe_module: &SafeModule,
    path_planner_min_ack_rate: f64,
    path_planner: crate::connection::options::PathPlannerOptions,
    pix_dimensions: crate::connection::options::PixDimensionOptions,
) -> Result<HoprLibConfig, Error> {
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
    // Layer user overrides on top of the latency preset; unset fields keep the preset value.
    path_planner.apply(&mut cfg.protocol.path_planner);
    // Apply PIX dimension overrides only when matching an Exit's advertised quota window.
    pix_dimensions.apply(&mut cfg.protocol.pix);
    Ok(cfg)
}

pub fn safe_file(state_home: PathBuf) -> PathBuf {
    dirs::config_dir(state_home, SAFE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::options::{PathPlannerOptions, PixDimensionOptions};

    fn safe_module() -> SafeModule {
        SafeModule {
            safe_address: "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739".to_string(),
            module_address: "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739".to_string(),
        }
    }

    async fn generated(
        path_planner: PathPlannerOptions,
        pix_dimensions: PixDimensionOptions,
    ) -> edgli::hopr_lib::config::HoprLibConfig {
        generate(&safe_module(), 0.1, path_planner, pix_dimensions)
            .await
            .expect("generate should succeed")
    }

    #[tokio::test]
    async fn default_keeps_the_preset_floor() {
        let cfg = generated(PathPlannerOptions::default(), PixDimensionOptions::default()).await;
        assert_eq!(cfg.protocol.path_planner.min_paths_anonymity_floor, 0);
    }

    #[tokio::test]
    async fn table_overrides_anonymity_floor() {
        let overrides = PathPlannerOptions {
            min_paths_anonymity_floor: Some(7),
            ..PathPlannerOptions::default()
        };
        let cfg = generated(overrides, PixDimensionOptions::default()).await;
        assert_eq!(cfg.protocol.path_planner.min_paths_anonymity_floor, 7);
    }

    #[tokio::test]
    async fn default_keeps_the_upstream_pix_dimensions() {
        let upstream = edgli::PixGlobalConfig::default();
        let cfg = generated(PathPlannerOptions::default(), PixDimensionOptions::default()).await;
        assert_eq!(cfg.protocol.pix.num_ssa_parts, upstream.num_ssa_parts);
        assert_eq!(cfg.protocol.pix.ssa_part_size, upstream.ssa_part_size);
        assert_eq!(cfg.protocol.pix.additional_shares, upstream.additional_shares);
    }

    #[tokio::test]
    async fn table_overrides_pix_dimensions() {
        // Match `hoprd-localcluster --enable-pix`'s demo geometry.
        let overrides = PixDimensionOptions {
            num_ssa_parts: Some(8),
            ssa_part_size: Some(2),
            additional_shares: Some(2),
        };
        let cfg = generated(PathPlannerOptions::default(), overrides).await;
        assert_eq!(cfg.protocol.pix.num_ssa_parts, 8);
        assert_eq!(cfg.protocol.pix.ssa_part_size, 2);
        assert_eq!(cfg.protocol.pix.additional_shares, Some(2));
    }

    #[tokio::test]
    async fn partial_pix_dimensions_leave_the_rest_upstream() {
        let upstream = edgli::PixGlobalConfig::default();
        let overrides = PixDimensionOptions {
            ssa_part_size: Some(4),
            ..PixDimensionOptions::default()
        };
        let cfg = generated(PathPlannerOptions::default(), overrides).await;
        assert_eq!(cfg.protocol.pix.ssa_part_size, 4);
        assert_eq!(cfg.protocol.pix.num_ssa_parts, upstream.num_ssa_parts);
        // Leave this unset so hopr-lib still derives it from `ssa_part_size`.
        assert_eq!(cfg.protocol.pix.additional_shares, upstream.additional_shares);
    }
}
