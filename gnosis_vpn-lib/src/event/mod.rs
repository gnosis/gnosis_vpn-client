//! This module holds event definitions for inter process communication between root <-> worker and worker <-> core.

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use std::net::Ipv4Addr;
use std::time::Duration;

use crate::command::{Response, WorkerCommand};
use crate::config::Config;
use crate::ping;
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
/// Allowing large variant as this is sent between processes
#[allow(clippy::large_enum_variant)]
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
    /// Ask root to provision the TUN device and split-tunnel routing for the
    /// NepTUN data plane. No key material crosses the process boundary: the
    /// WireGuard keys stay in the worker where the `WgTunnel` lives.
    SetupTunnel {
        interface_address: String,
        mtu: u32,
        dns: Option<String>,
        peer_ips: Vec<Ipv4Addr>,
        resp: oneshot::Sender<Result<String, String>>,
    },
    Ping {
        options: ping::Options,
        resp: oneshot::Sender<Result<Duration, String>>,
    },
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
    SetupTunnel {
        request_id: u64,
        interface_address: String,
        mtu: u32,
        dns: Option<String>,
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
    /// On success, the String is the resolved TUN interface name.
    TunnelReady {
        request_id: u64,
        res: Result<String, String>,
    },
    Ping {
        request_id: u64,
        res: Result<Duration, String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_tunnel_request_survives_serde_round_trip() {
        let request = RequestToRoot::SetupTunnel {
            request_id: 7,
            interface_address: "10.128.0.5/9".to_string(),
            mtu: 1420,
            dns: Some("1.1.1.1,8.8.8.8".to_string()),
            peer_ips: vec![Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(198, 51, 100, 42)],
        };
        let json = serde_json::to_string(&request).expect("serialize");
        let decoded: RequestToRoot = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            RequestToRoot::SetupTunnel {
                request_id,
                interface_address,
                mtu,
                dns,
                peer_ips,
            } => {
                assert_eq!(request_id, 7);
                assert_eq!(interface_address, "10.128.0.5/9");
                assert_eq!(mtu, 1420);
                assert_eq!(dns.as_deref(), Some("1.1.1.1,8.8.8.8"));
                assert_eq!(
                    peer_ips,
                    vec![Ipv4Addr::new(192, 0, 2, 1), Ipv4Addr::new(198, 51, 100, 42)]
                );
            }
            other => panic!("expected SetupTunnel, got {other:?}"),
        }
    }

    #[test]
    fn tunnel_ready_success_survives_serde_round_trip() {
        let response = ResponseFromRoot::TunnelReady {
            request_id: 7,
            res: Ok("utun7".to_string()),
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let decoded: ResponseFromRoot = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ResponseFromRoot::TunnelReady { request_id, res } => {
                assert_eq!(request_id, 7);
                assert_eq!(res.as_deref(), Ok("utun7"));
            }
            other => panic!("expected TunnelReady, got {other:?}"),
        }
    }

    #[test]
    fn tunnel_ready_failure_survives_serde_round_trip() {
        let response = ResponseFromRoot::TunnelReady {
            request_id: 8,
            res: Err("routing setup failed".to_string()),
        };
        let json = serde_json::to_string(&response).expect("serialize");
        let decoded: ResponseFromRoot = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            ResponseFromRoot::TunnelReady { request_id, res } => {
                assert_eq!(request_id, 8);
                assert_eq!(res, Err("routing setup failed".to_string()));
            }
            other => panic!("expected TunnelReady, got {other:?}"),
        }
    }
}
