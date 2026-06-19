//! This module holds event definitions for inter process communication between root <-> worker and worker <-> core.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use std::net::Ipv4Addr;
use std::time::Duration;

use crate::command::{Response, WorkerCommand};
use crate::config::Config;
use crate::ping;
use crate::wireguard::{self, WireGuard};
use crate::worker_params::WorkerParams;

/// Messages sent from worker to core application logic
#[derive(Debug)]
pub enum WorkerToCore {
    /// Socket command avaiting response
    WorkerCommand {
        cmd: WorkerCommand,
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
/// Allowing large variant as this is sent between processes
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize)]
pub enum RootToWorker {
    /// Wrap up and tear down any resources before worker process exits
    Shutdown,
    /// Rotate logs
    RotateLogs,
    /// Startup parameters
    StartupParams {
        config: Config,
        worker_params: WorkerParams,
        target_dest_id: Option<String>,
    },
    /// Socket command received by root
    WorkerCommand { cmd: WorkerCommand, id: u64 },
    /// Result of a request to root
    ResponseFromRoot(ResponseFromRoot),
}

/// Messages sent from worker to root
#[derive(Debug, Serialize, Deserialize)]
pub enum WorkerToRoot {
    /// Response to a socket command
    Response { resp: Response, id: u64 },
    /// Request to root execution
    RequestToRoot(RequestToRoot),
}

/// Runner requesting root command and usually waiting for response
#[derive(Debug)]
pub(crate) enum RunnerToRoot {
    KillswitchLockdown {
        peer_ips: Vec<Ipv4Addr>,
        interface: String,
        resp: oneshot::Sender<Result<(), String>>,
    },
    StaticWgRouting {
        wg_data: WireGuardData,
        peer_ips: Vec<Ipv4Addr>,
        resp: oneshot::Sender<Result<String, String>>,
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
    KillswitchLockdown {
        request_id: u64,
        peer_ips: Vec<Ipv4Addr>,
        interface: String,
    },
    StaticWgRouting {
        request_id: u64,
        wg_data: WireGuardData,
        peer_ips: Vec<Ipv4Addr>,
    },
    TearDownWg,
    Ping {
        request_id: u64,
        options: ping::Options,
    },
    /// Fire-and-forget: ask root to hold resolved IPs so they survive a worker restart.
    CacheBlokliIps {
        ips: Vec<Ipv4Addr>,
    },
    /// Fire-and-forget: refresh the peer-IP allowlist used by the killswitch and routing bypass.
    UpdatePeerIps {
        peer_ips: Vec<Ipv4Addr>,
    },
}

/// Root execution response from root process.
/// Should be matched by worker to **RespondableRequestToRoot** request responses.
#[derive(Debug, Serialize, Deserialize)]
pub enum ResponseFromRoot {
    KillswitchLockdown {
        request_id: u64,
        res: Result<(), String>,
    },
    /// On success, the String is the resolved WireGuard interface name.
    StaticWgRouting {
        request_id: u64,
        res: Result<String, String>,
    },
    Ping {
        request_id: u64,
        res: Result<Duration, String>,
    },
}
