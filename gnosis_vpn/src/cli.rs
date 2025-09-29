use clap::Parser;
use gnosis_vpn_lib::{config, hopr, socket};
use std::path::PathBuf;

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

    /// Hopr edge client configuration path
    #[arg(
        short,
        long,
        env = hopr::CONFIG_ENV,
    )]
    pub hopr_config_path: PathBuf,

    /// Hopr edge client identity path
    #[arg(
        short,
        long,
        env = hopr::ID_FILE_ENV,
    )]
    pub hopr_identity_file: PathBuf,

    /// Hopr edge client identity pass
    #[arg(
        short,
        long,
        env = hopr::ID_PASS_ENV,
    )]
    pub hopr_identity_pass: String,

    /// Specify hopr edge client db path
    #[arg(
        short,
        long,
        env = hopr::DB_ENV,
    )]
    pub hopr_db_path: PathBuf,
}

pub fn parse() -> Cli {
    Cli::parse()
}
