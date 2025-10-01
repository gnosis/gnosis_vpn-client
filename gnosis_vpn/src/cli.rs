use clap::Parser;
use url::Url;

use std::path::PathBuf;

use gnosis_vpn_lib::{config, hopr, socket};

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Debug, Parser)]
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
    #[arg(long, env = "RPC_PROVIDER")]
    pub rpc_provider: Url,

    /// Hopr edge client configuration path
    #[arg(
        long,
        env = hopr::CONFIG_ENV,
        default_value = None,
    )]
    pub hopr_config_path: Option<PathBuf>,

    /// Hopr edge client identity path
    #[arg(
        long,
        env = hopr::ID_FILE_ENV,
        default_value = None,
    )]
    pub hopr_identity_file: Option<PathBuf>,

    /// Hopr edge client identity pass
    #[arg(
        long,
        env = hopr::ID_PASS_ENV,
        default_value = None,
    )]
    pub hopr_identity_pass: Option<String>,
}

pub fn parse() -> Cli {
    Cli::parse()
}
