use edgli::blokli::{IncentiveOperations, make_incentive_operations};
use edgli::hopr_lib::HoprKeys;
use edgli::hopr_lib::config::HoprLibConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use url::Url;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;

use crate::compat::SafeModule;
use crate::hopr::blokli_config::BlokliConfig;
use crate::hopr::{config, identity};

#[derive(Debug, Error)]
pub enum Error {
    #[error("HOPR identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error accessing {path}: {source}")]
    IOFile { path: PathBuf, source: std::io::Error },
    #[error("HOPR config error: {0}")]
    Config(#[from] config::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("Blokli creation error: {0}")]
    BlokliCreation(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkerParams {
    identity_file: Option<PathBuf>,
    identity_pass: Option<String>,
    config_mode: ConfigFileMode,
    allow_insecure: bool,
    allow_experimental: bool,
    blokli_url: Option<Url>,
    state_home: PathBuf,
    cached_blokli_ips: Vec<Ipv4Addr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConfigFileMode {
    Manual(PathBuf),
    Generated,
}

impl WorkerParams {
    pub fn new(
        identity_file: Option<PathBuf>,
        identity_pass: Option<String>,
        config_mode: ConfigFileMode,
        allow_insecure: bool,
        allow_experimental: bool,
        blokli_url: Option<Url>,
        state_home: PathBuf,
    ) -> Self {
        Self {
            identity_file,
            identity_pass,
            config_mode,
            allow_insecure,
            allow_experimental,
            blokli_url,
            state_home,
            cached_blokli_ips: Vec::new(),
        }
    }

    pub fn set_cached_blokli_ips(&mut self, ips: Vec<Ipv4Addr>) {
        self.cached_blokli_ips = ips;
    }

    pub fn cached_blokli_ips(&self) -> &[Ipv4Addr] {
        &self.cached_blokli_ips
    }

    pub async fn persist_identity_generation(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => {
                tracing::info!(?path, "Using provided HOPR identity file");
                path.to_path_buf()
            }
            None => identity::file(self.state_home()),
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => {
                tracing::info!("Using provided HOPR identity pass");
                pass.to_string()
            }
            None => {
                let path = identity::pass_file(self.state_home());
                match fs::read_to_string(&path).await {
                    Ok(p) => {
                        tracing::debug!(?path, "No HOPR identity pass provided - read from file instead");
                        Ok(p)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tracing::debug!(
                            ?path,
                            "No HOPR identity pass provided - generating new one and storing alongside identity file"
                        );
                        let pw = identity::generate_pass();
                        let mut file = fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(&path)
                            .await
                            .map_err(|e| {
                                tracing::error!(error = %e, ?path, "failed to create HOPR identity pass file");
                                Error::IOFile {
                                    path: path.clone(),
                                    source: e,
                                }
                            })?;
                        file.write_all(pw.as_bytes()).await.map_err(|e| {
                            tracing::error!(error = %e, ?path, "failed to write generated HOPR identity pass file");
                            Error::IOFile {
                                path: path.clone(),
                                source: e,
                            }
                        })?;
                        Ok(pw)
                    }
                    Err(e) => {
                        tracing::error!(error = %e, ?path, "failed to read HOPR identity pass file");
                        if e.kind() == std::io::ErrorKind::PermissionDenied {
                            log_path_diagnostics(&path);
                        }
                        Err(Error::IOFile { path, source: e })
                    }
                }?
            }
        };

        identity::from_path(identity_file, identity_pass.clone()).map_err(Error::from)
    }

    pub async fn calc_keys(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => path.to_path_buf(),
            None => identity::file(self.state_home()),
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => pass.to_string(),
            None => {
                let path = identity::pass_file(self.state_home());
                fs::read_to_string(&path).await.map_err(|e| {
                    tracing::error!(error = %e, ?path, "failed to read HOPR identity pass file");
                    Error::IOFile {
                        path: path.clone(),
                        source: e,
                    }
                })?
            }
        };

        identity::from_path(identity_file, identity_pass.clone()).map_err(Error::from)
    }

    pub async fn to_config(
        &self,
        safe_module: &SafeModule,
        path_planner_min_ack_rate: f64,
    ) -> Result<HoprLibConfig, Error> {
        match self.config_mode.clone() {
            ConfigFileMode::Manual(path) => config::from_path(path).await.map_err(Error::from),
            ConfigFileMode::Generated => config::generate(safe_module, path_planner_min_ack_rate)
                .await
                .map_err(Error::from),
        }
    }

    /// Create an [`IncentiveOperations`] handle for pre-Safe on-chain interactions.
    pub async fn create_incentive_operations(
        &self,
        config: BlokliConfig,
    ) -> Result<Arc<dyn IncentiveOperations>, Error> {
        let keys = self.calc_keys().await?;
        let private_key = keys.chain_key;
        let url = self.blokli_url();
        let ops = make_incentive_operations(url, &private_key, Some(config.into()))
            .await
            .map_err(|e| Error::BlokliCreation(e.to_string()))?;
        Ok(Arc::from(ops))
    }

    pub fn allow_insecure(&self) -> bool {
        self.allow_insecure
    }

    pub fn allow_experimental(&self) -> bool {
        self.allow_experimental
    }

    pub fn blokli_url(&self) -> Option<Url> {
        self.blokli_url.clone()
    }

    pub fn state_home(&self) -> PathBuf {
        self.state_home.clone()
    }
}

fn log_path_diagnostics(path: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    match std::fs::metadata(path) {
        Ok(meta) => tracing::error!(
            uid = meta.uid(),
            gid = meta.gid(),
            mode = format!("{:o}", meta.mode() & 0o777),
            ?path,
            "pass file metadata"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            tracing::error!(?path, "pass file does not exist")
        }
        Err(e) => tracing::error!(error = %e, ?path, "pass file metadata error"),
    }
    if let Some(parent) = path.parent()
        && let Ok(meta) = std::fs::metadata(parent)
    {
        tracing::error!(
            uid = meta.uid(),
            gid = meta.gid(),
            mode = format!("{:o}", meta.mode() & 0o777),
            path = ?parent,
            "pass file parent directory metadata"
        );
    }
}
