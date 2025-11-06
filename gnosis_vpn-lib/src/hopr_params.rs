use edgli::hopr_lib::HoprKeys;
use thiserror::Error;
use tokio::fs;
use url::Url;

use std::path::PathBuf;

use crate::hopr::identity;
use crate::network::Network;

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprIdentity(#[from] identity::Error),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

#[derive(Clone, Debug)]
pub struct HoprParams {
    pub identity_file: Option<PathBuf>,
    pub identity_pass: Option<String>,
    pub config_mode: ConfigFileMode,
    pub network: Network,
    pub rpc_provider: Url,
}

#[derive(Clone, Debug)]
pub enum ConfigFileMode {
    Manual(PathBuf),
    Generated,
}

impl HoprParams {
    pub async fn calc_keys(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => path.to_path_buf(),
            None => {
                let path = identity::file()?;
                tracing::debug!(?path, "No HOPR identity file path provided - using default");
                path
            }
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => pass.to_string(),
            None => {
                let path = identity::pass_file()?;
                match fs::read_to_string(&path).await {
                    Ok(p) => {
                        tracing::debug!(?path, "No HOPR identity pass provided - read from file instead");
                        Ok(p)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tracing::info!(
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
}
