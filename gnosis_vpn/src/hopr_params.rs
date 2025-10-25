use edgli::hopr_lib::{Balance, HoprKeys, WxHOPR};
use thiserror::Error;
use url::Url;

use std::fs;
use std::path::PathBuf;

use gnosis_vpn_lib::hopr::{Hopr, HoprError, api::HoprTelemetry, config as hopr_config, identity};
use gnosis_vpn_lib::network::Network;

use crate::cli::Cli;

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

impl From<Cli> for HoprParams {
    fn from(cli: Cli) -> Self {
        let network = cli.hopr_network;
        let rpc_provider = cli.hopr_rpc_provider;
        let config_mode = match cli.hopr_config_path {
            Some(path) => ConfigFileMode::Manual(path),
            None => ConfigFileMode::Generated,
        };

        HoprParams {
            config_mode,
            identity_file: cli.hopr_identity_file,
            identity_pass: cli.hopr_identity_pass,
            network,
            rpc_provider,
        }
    }
}

impl HoprParams {
    pub fn calc_keys(&self) -> Result<HoprKeys, Error> {
        let identity_file = match &self.identity_file {
            Some(path) => path.to_path_buf(),
            None => {
                let path = identity::file()?;
                tracing::info!(?path, "No HOPR identity file path provided - using default");
                path
            }
        };

        let identity_pass = match &self.identity_pass {
            Some(pass) => pass.to_string(),
            None => {
                let path = identity::pass_file()?;
                match fs::read_to_string(&path) {
                    Ok(p) => {
                        tracing::warn!(?path, "No HOPR identity pass provided - read from file instead");
                        Ok(p)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        tracing::warn!(
                            ?path,
                            "No HOPR identity pass provided - generating new one and storing alongside identity file"
                        );
                        let pw = identity::generate_pass();
                        fs::write(&path, pw.as_bytes())?;
                        Ok(pw)
                    }
                    Err(e) => Err(e),
                }?
            }
        };

        identity::from_path(identity_file.as_path(), identity_pass.clone()).map_err(Error::from)
    }
}
