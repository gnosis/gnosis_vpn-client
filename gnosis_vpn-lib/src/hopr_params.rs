use edgli::blokli::SafelessInteractor;
use edgli::hopr_lib::HoprKeys;
use edgli::hopr_lib::config::HoprLibConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use url::Url;

use std::path::PathBuf;

use crate::compat::SafeModule;
use crate::hopr::{config, identity};

#[derive(Debug, Error)]
pub enum Error {
    #[error("HOPR identity error: {0}")]
    HoprIdentity(#[from] identity::Error),
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("HOPR config error: {0}")]
    Config(#[from] config::Error),
    #[error("URL parse error: {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("Blokli creation error: {0}")]
    BlokliCreation(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HoprParams {
    identity_file: Option<PathBuf>,
    identity_pass: Option<String>,
    config_mode: ConfigFileMode,
    allow_insecure: bool,
    blokli_url: Option<Url>,
    force_static_routing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConfigFileMode {
    Manual(PathBuf),
    Generated,
}

impl HoprParams {
    pub fn new(
        identity_file: Option<PathBuf>,
        identity_pass: Option<String>,
        config_mode: ConfigFileMode,
        allow_insecure: bool,
        blokli_url: Option<Url>,
        force_static_routing: bool,
    ) -> Self {
        Self {
            identity_file,
            identity_pass,
            config_mode,
            allow_insecure,
            blokli_url,
            force_static_routing,
        }
    }

    pub async fn persist_identity_generation(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => {
                tracing::info!(?path, "Using provided HOPR identity file");
                path.to_path_buf()
            }
            None => identity::file()?,
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => {
                tracing::info!("Using provided HOPR identity pass");
                pass.to_string()
            }
            None => {
                let path = identity::pass_file()?;
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
                        fs::write(&path, pw.as_bytes()).await?;
                        Ok(pw)
                    }
                    Err(e) => Err(e),
                }?
            }
        };

        identity::from_path(identity_file.as_path(), identity_pass.clone()).map_err(Error::from)
    }

    pub async fn calc_keys(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => path.to_path_buf(),
            None => identity::file()?,
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => pass.to_string(),
            None => {
                let path = identity::pass_file()?;
                fs::read_to_string(&path).await?
            }
        };

        identity::from_path(identity_file.as_path(), identity_pass.clone()).map_err(Error::from)
    }

    pub async fn to_config(&self, safe_module: &SafeModule) -> Result<HoprLibConfig, Error> {
        match self.config_mode.clone() {
            // use user provided configuration path
            ConfigFileMode::Manual(path) => config::from_path(path.as_ref()).await.map_err(Error::from),
            // check status of config generation
            ConfigFileMode::Generated => config::generate(safe_module).await.map_err(Error::from),
        }
    }

    /// Create safeless blokli instance
    pub async fn create_safeless_interactor(&self) -> Result<SafelessInteractor, Error> {
        let keys = self.calc_keys().await?;
        let private_key = keys.chain_key;
        let url = self.blokli_url();
        edgli::blokli::SafelessInteractor::new(url, &private_key)
            .await
            .map_err(|e| Error::BlokliCreation(e.to_string()))
    }

    pub fn allow_insecure(&self) -> bool {
        self.allow_insecure
    }

    pub fn force_static_routing(&self) -> bool {
        self.force_static_routing
    }

    pub fn blokli_url(&self) -> Option<Url> {
        self.blokli_url.clone()
    }
}
