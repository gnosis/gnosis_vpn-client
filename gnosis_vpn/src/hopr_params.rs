use url::Url;

use std::path::PathBuf;

use gnosis_vpn_lib::network::Network;

use crate::cli::Cli;

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
