use thiserror::Error;
use url::Url;

use std::path::PathBuf;

use gnosis_vpn_lib::network::Network;

use crate::cli::Cli;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Conflicting launch parameters")]
    ConflictingParams,
}

#[derive(Clone, Debug)]
pub struct HoprParams {
    pub identity_file: Option<PathBuf>,
    pub identity_pass: Option<String>,
    pub config_mode: ConfigMode,
}

#[derive(Clone, Debug)]
pub enum ConfigMode {
    Manual(PathBuf),
    Generated { rpc_provider: Url, network: Network },
}

impl TryFrom<Cli> for HoprParams {
    type Error = Error;

    fn try_from(cli: Cli) -> Result<Self, Self::Error> {
        let config_mode = match cli.hopr_config_path {
            Some(path) => ConfigMode::Manual(path),
            None => {
                let rpc_provider = cli.hopr_rpc_provider.ok_or(Error::ConflictingParams)?;
                let network = cli.hopr_network.ok_or(Error::ConflictingParams)?;
                ConfigMode::Generated { rpc_provider, network }
            }
        };

        Ok(HoprParams {
            config_mode,
            identity_file: cli.hopr_identity_file,
            identity_pass: cli.hopr_identity_pass,
        })
    }
}
