use clap::Parser;

/// Gnosis VPN system service - client application for Gnosis VPN connections
#[derive(Clone, Debug, Parser)]
#[command(version)]
pub struct Cli {}

pub fn parse() -> Cli {
    Cli::parse()
}
