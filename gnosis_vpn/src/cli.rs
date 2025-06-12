use clap::Parser;
use gnosis_vpn_lib::config;
use gnosis_vpn_lib::socket;
use std::path::PathBuf;

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Debug, Parser)]
#[command(version)]
pub struct Cli {
    /// Specify socket path
    #[arg(
        short,
        long,
        env = socket::ENV_VAR,
        default_value = socket::DEFAULT_PATH
    )]
    pub socket_path: PathBuf,

    /// Specify configuration path
    #[arg(
        short,
        long,
        env = config::ENV_VAR,
        default_value = config::DEFAULT_PATH)
    ]
    pub config_path: PathBuf,
}

pub fn parse() -> Cli {
    Cli::parse()
}
