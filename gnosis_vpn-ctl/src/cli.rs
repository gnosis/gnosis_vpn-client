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

    /// Query some nerd stats for connecting/connected destination
    #[command()]
    NerdStats {},

    /// Display service information, such as versions and file locations
    #[command()]
    Info {},

    /// Start worker process that runs main connection loop
    /// Needs a keep alive timeout to determine how long to wait for commands before stopping
    /// worker and returning to idle mode
    /// This timeout will be reset on every worker command.
    /// Commands not resetting the timeout are: Info, StopClient, Ping
    #[command()]
    StartClient {
        /// Keep alive timeout - stops worker when expired
        keep_alive: humantime::Duration,
    },

    /// Stop worker process to return to idle mode
    #[command()]
    StopClient {},

    /// Fetch and display the latest available version from the update manifest
    #[command()]
    CheckUpdate {},
}

impl From<Command> for LibCommand {
    fn from(val: Command) -> Self {
        match val {
            Command::Status {} => LibCommand::Status,
            Command::Connect { id } => LibCommand::Connect(id),
            Command::Disconnect {} => LibCommand::Disconnect,
            Command::Balance {} => LibCommand::Balance,
            Command::RefreshNode {} => LibCommand::RefreshNode,
            Command::FundingTool { secret } => LibCommand::FundingTool(secret),
            Command::Ping {} => LibCommand::Ping,
            Command::Telemetry {} => LibCommand::Telemetry,
            Command::NerdStats {} => LibCommand::NerdStats,
            Command::Info {} => LibCommand::Info,
            Command::StartClient { keep_alive } => LibCommand::StartClient(keep_alive.into()),
            Command::StopClient {} => LibCommand::StopClient,
            Command::CheckUpdate {} => unreachable!("CheckUpdate is handled before socket dispatch"),
        }
    }
}

pub fn parse() -> Cli {
    Cli::parse()
}
