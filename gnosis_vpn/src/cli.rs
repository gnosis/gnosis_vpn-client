use clap::Parser;
use url::Url;

use std::path::PathBuf;

use gnosis_vpn_lib::hopr_params::{self, HoprParams};
use gnosis_vpn_lib::network::Network;
use gnosis_vpn_lib::{config, hopr, socket};

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Cli {
    /// Socket path for communication with this servive
    #[arg(
        short,
        long,
        env = socket::ENV_VAR,
        default_value = socket::DEFAULT_PATH
    )]
    pub socket_path: PathBuf,

    /// General configuration file
    #[arg(
        short,
        long,
        env = config::ENV_VAR,
        default_value = config::DEFAULT_PATH,
        )]
    pub config_path: PathBuf,

    /// RPC provider URL needed for fat Hopr edge client
    #[arg(long, env = hopr::RPC_PROVIDER_ENV)]
    pub hopr_rpc_provider: Url,

    /// Hopr network
    #[arg(long, env = hopr::NETWORK_ENV, default_value = "dufour")]
    pub hopr_network: Network,

    /// Hopr edge client configuration path
    #[arg( long, env = hopr::CONFIG_ENV, default_value = None) ]
    pub hopr_config_path: Option<PathBuf>,

    /// Hopr edge client identity path
    #[arg( long, env = hopr::ID_FILE_ENV, default_value = None)]
    pub hopr_identity_file: Option<PathBuf>,

    /// Hopr edge client identity pass
    #[arg( long, env = hopr::ID_PASS_ENV, default_value = None)]
    pub hopr_identity_pass: Option<String>,
}

pub fn parse() -> Cli {
    Cli::parse()
}

impl From<Cli> for HoprParams {
    fn from(cli: Cli) -> Self {
        let network = cli.hopr_network;
        let rpc_provider = cli.hopr_rpc_provider;
        let config_mode = match cli.hopr_config_path {
            Some(path) => hopr_params::ConfigFileMode::Manual(path),
            None => hopr_params::ConfigFileMode::Generated,
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
