use clap::{Parser, Subcommand};
use gnosis_vpn_lib::address::Address;
use gnosis_vpn_lib::command::Command as LibCommand;
use gnosis_vpn_lib::socket;
use std::path::PathBuf;

/// Gnosis VPN client control interface for Gnosis VPN service
#[derive(Debug, Parser)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Specify socket path
    #[arg(
        short,
        long,
        env = socket::ENV_VAR,
        default_value = socket::DEFAULT_PATH
    )]
    pub socket_path: PathBuf,

    /// Format output as json
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Query current service status
    #[command()]
    Status {},

    /// Connect to this exit location
    #[command()]
    Connect {
        /// Endpoint node address
        address: Address,
    },

    /// Disconnect from current exit location
    #[command()]
    Disconnect {},

    /// Query balance information
    #[command()]
    Balance {},

    /// Trigger a refresh of node information including balance
    #[command()]
    RefreshNode {},
}

impl From<Command> for LibCommand {
    fn from(val: Command) -> Self {
        match val {
            Command::Status {} => LibCommand::Status,
            Command::Connect { address } => LibCommand::Connect(address),
            Command::Disconnect {} => LibCommand::Disconnect,
            Command::Balance {} => LibCommand::Balance,
            Command::RefreshNode {} => LibCommand::RefreshNode,
        }
    }
}

pub fn parse() -> Cli {
    Cli::parse()
}
