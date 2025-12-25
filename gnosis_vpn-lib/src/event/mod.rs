//! This module holds event definitions for inter process communication between root <-> worker and worker <-> core.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use std::net::Ipv4Addr;
use std::time::Duration;

use crate::command::{Command, Response};
use crate::config::Config;
use crate::hopr_params::HoprParams;
use crate::ping;
use crate::wireguard::{self, WireGuard};

/// Messages sent from worker to core application logic
#[derive(Debug)]
pub enum WorkerToCore {
    /// Socket command avaiting response
    Command {
        cmd: Command,
        resp: oneshot::Sender<Response>,
    },
    Shutdown,
    /// Result of a request to root
    ResponseFromRoot(ResponseFromRoot),
}

/// Messages sent from core application logic to worker
#[derive(Debug)]
pub enum CoreToWorker {
    /// Requesting root execution
    RequestToRoot(RequestToRoot),
}

/// Messages sent from root to worker
#[derive(Debug, Serialize, Deserialize)]
pub enum RootToWorker {
    /// Startup parameters for hoprd
    HoprParams { hopr_params: HoprParams },
    /// Configuration file
    Config { config: Config },
    /// Socket command received by root
    Command(Command),
    /// Result of a request to root
    ResponseFromRoot(ResponseFromRoot),
}

/// Messages sent from worker to root
#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerToRoot {
    /// Response to a socket command
    Response(Response),
    /// Acknowledgement other incoming messages
    Ack,
    /// Request to root execution
    RequestToRoot(RequestToRoot),
    /// Received unexpected event from root
    OutOfSync,
}

/// Runner requesting root command and waiting for response
#[derive(Debug)]
pub enum RespondableRequestToRoot {
    DynamicWgRouting {
        wg_data: WireGuardData,
        resp: oneshot::Sender<Result<(), String>>,
    },
    StaticWgRouting {
        wg_data: WireGuardData,
        peer_ips: Vec<Ipv4Addr>,
        resp: oneshot::Sender<Result<(), String>>,
    },
    Ping {
        options: ping::Options,
        resp: oneshot::Sender<Result<Duration, String>>,
    },
}

/// Data required for WireGuard operations
#[derive(Debug, Serialize, Deserialize)]
pub struct WireGuardData {
    pub wg: WireGuard,
    pub interface_info: wireguard::InterfaceInfo,
    pub peer_info: wireguard::PeerInfo,
}

impl AsRef<RootToWorker> for RootToWorker {
    fn as_ref(&self) -> &RootToWorker {
        self
    }
}

/// Root execution request without unserializable response channel from runner.
/// Slimmed down **RespondableRequestToRoot** for inter-process communication.
#[derive(Debug, Serialize, Deserialize)]
pub enum RequestToRoot {
    DynamicWgRouting {
        wg_data: WireGuardData,
    },
    StaticWgRouting {
        wg_data: WireGuardData,
        peer_ips: Vec<Ipv4Addr>,
    },
    TearDownWg,
    Ping {
        options: ping::Options,
    },
}

/// Root execution response from root process.
/// Should be matched by worker to **RespondableRequestToRoot** request responses.
#[derive(Debug, Serialize, Deserialize)]
pub enum ResponseFromRoot {
    DynamicWgRouting { res: Result<(), String> },
    StaticWgRouting { res: Result<(), String> },
    Ping { res: Result<Duration, String> },
}
