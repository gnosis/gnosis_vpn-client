//! Fake service that walks a user interface through the sync screen: deploying safe, warmup, then running without exits.
//! Run as root after stopping the real service: `sudo ./target/debug/examples/fake_service [step_seconds]`.
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gnosis_vpn_ipc::*;

const WARMUP: &[WarmupStage] = &[
    WarmupStage::Initializing,
    WarmupStage::ValidatingConfig,
    WarmupStage::IdentifyingNode,
    WarmupStage::ConnectingBlockchain,
    WarmupStage::CreatingNode,
    WarmupStage::StartingNode,
    WarmupStage::ValidatingNetworkConfig,
    WarmupStage::CheckingOnchainAddress,
    WarmupStage::AnnouncingNode,
    WarmupStage::AwaitingKeyBinding,
    WarmupStage::InitializingServices,
];

fn main() {
    let step_secs: u64 = std::env::args().nth(1).map_or(3, |s| s.parse().expect("step seconds"));
    let step = Duration::from_secs(step_secs);
    let path = std::env::var_os(ENV_VAR).map_or_else(|| PathBuf::from(DEFAULT_PATH), PathBuf::from);

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).expect("create socket dir");
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("bind socket");
    // The app runs unprivileged while this usually runs as root.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).expect("chmod socket");
    println!("listening on {}, one step every {step_secs}s", path.display());

    let started = Instant::now();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve(stream, started.elapsed(), step),
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
}

fn serve(stream: UnixStream, elapsed: Duration, step: Duration) {
    let mut line = String::new();
    if let Err(e) = BufReader::new(&stream).read_line(&mut line) {
        eprintln!("read failed: {e}");
        return;
    }
    let run_mode = run_mode_at(elapsed, step);
    let reply = match decode_request(&line) {
        Ok(request) => {
            let shown = serde_json::to_string(&run_mode).expect("run mode serializes");
            println!("{request:?} -> {shown}");
            Reply::Ok {
                response: respond(request, run_mode),
            }
        }
        Err(error) => {
            println!("refused: {error}");
            Reply::Err { error }
        }
    };
    if let Err(e) = (&stream).write_all(encode_reply(&reply).as_bytes()) {
        eprintln!("write failed: {e}");
    }
}

fn run_mode_at(elapsed: Duration, step: Duration) -> RunMode {
    let index = (elapsed.as_secs_f64() / step.as_secs_f64()) as usize;
    if index == 0 {
        return RunMode::DeployingSafe {
            node_address: Address::new([0x42; 20]),
        };
    }
    if let Some(stage) = WARMUP.get(index - 1) {
        return RunMode::Warmup {
            stage: *stage,
            last_error: None,
        };
    }
    // No exits keeps the app on "Looking for exits" until its own watchdog moves on.
    RunMode::Running {
        hopr_status: Some(HoprStatus::Running),
        funding_status: None,
        channel_maintenance: ChannelMaintenance::Ok,
    }
}

fn respond(request: Request, run_mode: RunMode) -> Response {
    match request {
        Request::Status => Response::Status(Box::new(Status {
            run_mode,
            destinations: vec![],
            target: None,
            connection: Connection::Disconnected,
            disconnecting: vec![],
            probe: None,
        })),
        Request::NerdStats => Response::NerdStats(Box::new(NerdStats::NoInfo {
            tickets: TicketStatsStatus::Waiting,
        })),
        Request::Connect { .. } => Response::Connect(ConnectResponse::DestinationNotFound),
        Request::Disconnect => Response::Disconnect(DisconnectResponse::NotConnected),
        Request::Balance => Response::Balance(BalanceResponse::Unavailable {
            reason: "fake service".to_string(),
        }),
        Request::FundingTool { .. } => Response::FundingTool(FundingToolResponse::WrongPhase),
        Request::Telemetry => Response::Telemetry(None),
        Request::Ping => Response::Pong,
        Request::Info => Response::Info(InfoResponse {
            version: semver::Version::new(0, 0, 0),
            package_version: Some("fake".to_string()),
            log_file: None,
            worker_log_dir: None,
        }),
        Request::StartClient { .. } => Response::StartClient(StartClientResponse::Started),
        Request::StopClient => Response::StopClient(StopClientResponse::Stopped),
        Request::Destinations => Response::Destinations(vec![]),
        Request::Probe { .. } => Response::Probe(ProbeResponse::DestinationNotFound),
        Request::Unprobe => Response::Unprobe(UnprobeResponse::NotProbing),
        Request::QuickProbe { .. } => Response::QuickProbe(QuickProbeResponse::DestinationNotFound),
    }
}
