use clap::{Parser, Subcommand};
use gnosis_vpn_lib::{peer_id::PeerId, socket, Command as LibCommand};
use std::path::PathBuf;

/// Gnosis VPN client - WireGuard client for GnosisVPN connections
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
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Query current service status
    #[command()]
    Status {
        /// Format output as json
        #[arg(long)]
        json: bool,
    },

    /// Connect to this exit location
    #[command()]
    Connect {
        /// Endpoint peer id
        peer_id: PeerId,
        /// Format output as json
        #[arg(long)]
        json: bool,
    },

    /// Disconnect from current exit location
    #[command()]
    Disconnect {
        /// Format output as json
        #[arg(long)]
        json: bool,
    },
}

impl Into<LibCommand> for Command {
    fn into(self) -> LibCommand {
        match self {
            Command::Status { .. } => LibCommand::Status,
            Command::Connect { peer_id, .. } => LibCommand::Connect { peer_id },
            Command::Disconnect { .. } => LibCommand::Disconnect,
        }
    }
}

pub fn parse() -> Cli {
    Cli::parse()
}
