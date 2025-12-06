//! This module holds event definitions for inter process communication between root <-> worker and worker <-> core.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use crate::command::{Command, Response};
use crate::config::Config;
use crate::hopr_params::HoprParams;

/// Messages sent from worker to core application logic
#[derive(Debug)]
pub enum IncomingCore {
    /// Socket command avaiting response
    Command {
        cmd: Command,
        resp: oneshot::Sender<Response>,
    },
    Shutdown,
    /// Result of WireGuard tooling execution
    WireGuardResult {
        res: Result<(), String>,
    },
}

/// Messages sent from core application logic to worker
pub type OutgoingCore = WireGuardCommand;

/// Messages sent from root to workerq
#[derive(Debug, Serialize, Deserialize)]
pub enum IncomingWorker {
    /// Startup parameters for hoprd
    HoprParams {
        hopr_params: HoprParams,
    },
    /// Configuration file
    Config {
        config: Config,
    },
    /// Socket command received by root
    Command {
        cmd: Command,
    },
    /// Result of WireGuard tooling execution
    WireGuardResult {
        result: Result<(), String>,
    },
    Shutdown,
}

/// Messages sent from worker to root
#[derive(Debug, Serialize, Deserialize)]
pub enum OutoingWorker {
    /// Response to a socket command
    Response { resp: Box<Response> },
    /// Acknowledgement other incoming messages
    Ack,
    /// Instruct root to execute WireGuard tooling
    WireGuard(WireGuardCommand),
}

/// WireGuard tooling execution commands
#[derive(Debug, Serialize, Deserialize)]
pub enum WireGuardCommand {
    /// generated WireGuard configuration file
    WgUp(String),
    /// Tear down WireGuard interface
    WgDown,
}
