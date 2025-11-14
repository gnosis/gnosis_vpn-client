use edgli::hopr_lib::config::HoprLibConfig;
use edgli::hopr_lib::{Balance, HoprKeys, WxHOPR};
use thiserror::Error;
use tokio::fs;
use url::Url;

use std::path::PathBuf;

use crate::hopr::{config, identity};
use crate::network::Network;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprIdentity(#[from] identity::Error),
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error(transparent)]
    Config(#[from] config::Error),
}

#[derive(Clone, Debug)]
pub struct HoprParams {
    identity_file: Option<PathBuf>,
    identity_pass: Option<String>,
    config_mode: ConfigFileMode,
    network: Network,
    rpc_provider: Url,
    allow_insecure: bool,
}

#[derive(Clone, Debug)]
pub enum ConfigFileMode {
    Manual(PathBuf),
    Generated,
}

impl HoprParams {
    pub fn new(
        identity_file: Option<PathBuf>,
        identity_pass: Option<String>,
        config_mode: ConfigFileMode,
        network: Network,
        rpc_provider: Url,
        allow_insecure: bool,
    ) -> Self {
        Self {
            identity_file,
            identity_pass,
            config_mode,
            network,
            rpc_provider,
            allow_insecure,
        }
    }

    pub async fn generate_id_if_absent(&self) -> Result<HoprKeys, Error> {
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

    pub async fn to_config(&self, ticket_value: Balance<WxHOPR>) -> Result<HoprLibConfig, Error> {
        match self.config_mode.clone() {
            // use user provided configuration path
            ConfigFileMode::Manual(path) => config::from_path(path.as_ref()).await.map_err(Error::from),
            // check status of config generation
            ConfigFileMode::Generated => config::generate(self.network(), self.rpc_provider(), ticket_value)
                .await
                .map_err(Error::from),
        }
    }

    pub fn rpc_provider(&self) -> Url {
        self.rpc_provider.clone()
    }

    pub fn network(&self) -> Network {
        self.network.clone()
    }

    pub fn allow_insecure(&self) -> bool {
        self.allow_insecure
    }
}
