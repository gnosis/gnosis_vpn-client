use clap::{Parser, Subcommand};
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
        env = socket::root::ENV_VAR,
        default_value = socket::root::DEFAULT_PATH
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

    /// Start edge client to enable mixnet connections
    #[command()]
    Start {},

    /// Stop edge client
    #[command()]
    Stop {},

    /// Connect to this exit location
    #[command()]
    Connect {
        /// Endpoint node address
        id: String,
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

    /// Trigger a funding tool run to claim funds for your account during onboarding
    #[command()]
    FundingTool {
        /// Your secret hash
        secret: String,
    },

    /// Solicit a ping response ("pong") from the service and it's worker process to check if it is alive
    #[command()]
    Ping {},

    /// Trigger telemetry gathering from underlying edge client
    #[command()]
    Telemetry {},
}

impl From<Command> for LibCommand {
    fn from(val: Command) -> Self {
        match val {
            Command::Status {} => LibCommand::Status,
            Command::Start {} => LibCommand::Start,
            Command::Stop {} => LibCommand::Stop,
            Command::Connect { id } => LibCommand::Connect(id),
            Command::Disconnect {} => LibCommand::Disconnect,
            Command::Balance {} => LibCommand::Balance,
            Command::RefreshNode {} => LibCommand::RefreshNode,
            Command::FundingTool { secret } => LibCommand::FundingTool(secret),
            Command::Ping {} => LibCommand::Ping,
            Command::Telemetry {} => LibCommand::Telemetry,
        }
    }
}

pub fn parse() -> Cli {
    Cli::parse()
}
