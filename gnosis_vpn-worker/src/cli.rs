use std::path::PathBuf;

use clap::Parser;
use gnosis_vpn_lib::logging;

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Cli {
    #[arg(
        long,
        env = logging::ENV_VAR_LOG_FILE,
    )]
    pub log_file: Option<PathBuf>,
}

pub fn parse() -> Cli {
    Cli::parse()
}
