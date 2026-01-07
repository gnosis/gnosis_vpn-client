use tokio::fs;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::process::{self};
use std::time::Duration;

use gnosis_vpn_lib::command::{Command as cmdCmd, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::event::{RequestToRoot, ResponseFromRoot, RootToWorker, WorkerToRoot};
use gnosis_vpn_lib::hopr_params::HoprParams;
use gnosis_vpn_lib::{ping, socket, worker};

mod cli;
mod routing;
mod wg_tooling;

use crate::routing::Routing;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

async fn ctrlc_channel() -> Result<mpsc::Receiver<()>, exitcode::ExitCode> {
    let (sender, receiver) = mpsc::channel(32);
    let mut sigint = signal(SignalKind::interrupt()).map_err(|error| {
        tracing::error!(?error, "error setting up SIGINT handler");
        exitcode::IOERR
    })?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(|error| {
        tracing::error!(?error, "error setting up SIGTERM handler");
        exitcode::IOERR
    })?;

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(_) = sigint.recv() => {
                    tracing::debug!("received SIGINT");
                    if sender.send(()).await.is_err() {
                        tracing::warn!("sigint: receiver closed");
                        break;
                    }
                },
                Some(_) = sigterm.recv() => {
                    tracing::debug!("received SIGTERM");
                    if sender.send(()).await.is_err() {
                        tracing::warn!("sigterm: receiver closed");
                        break;
                    }
                },
                else => {
                    tracing::warn!("sigint and sigterm streams closed");
                    break;
                }
            }
        }
    });

    Ok(receiver)
}

async fn socket_listener(socket_path: &Path) -> Result<UnixListener, exitcode::ExitCode> {
    match socket_path.try_exists() {
        Ok(true) => {
            tracing::info!("probing for running instance");
            match socket::root::process_cmd(socket_path, &cmdCmd::Ping).await {
                Ok(_) => {
                    tracing::error!("system service is already running - cannot start another instance");
                    return Err(exitcode::TEMPFAIL);
                }
                Err(e) => {
                    tracing::debug!(warn = ?e, "done probing for running instance");
                }
            };
            fs::remove_file(socket_path).await.map_err(|e| {
                tracing::error!(error = ?e, "error removing stale socket file");
                exitcode::IOERR
            })?;
        }
        Ok(false) => (),
        Err(e) => {
            tracing::error!(error = ?e, "error checking socket path");
            return Err(exitcode::IOERR);
        }
    };

    let socket_dir = socket_path.parent().ok_or_else(|| {
        tracing::error!("socket path has no parent");
        exitcode::UNAVAILABLE
    })?;
    fs::create_dir_all(socket_dir).await.map_err(|e| {
        tracing::error!(error = %e, "error creating socket directory");
        exitcode::IOERR
    })?;

    let listener = UnixListener::bind(socket_path).map_err(|e| {
        tracing::error!(error = ?e, "error binding socket");
        exitcode::OSFILE
    })?;

    // update permissions to allow unprivileged access
    fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o666))
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "error setting socket permissions");
            exitcode::NOPERM
        })?;

    Ok(listener)
}

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    // set up signal handler
    let mut ctrlc_receiver = ctrlc_channel().await?;

    // ensure worker user exists
    let input = worker::Input::new(
        args.worker_user.clone(),
        args.worker_binary.clone(),
        env!("CARGO_PKG_VERSION"),
    );
    let worker_user = worker::Worker::from_system(input).await.map_err(|err| {
        tracing::error!(error = ?err, "error determining worker user");
        exitcode::NOUSER
    })?;

    // check wireguard tooling
    wg_tooling::available()
        .await
        .and(wg_tooling::executable().await)
        .map_err(|err| {
            tracing::error!(error = ?err, "error checking WireGuard tools");
            exitcode::UNAVAILABLE
        })?;

    // prepare worker resources
    let config_path = match args.config_path.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(error = %e, "error canonicalizing config path");
            return Err(exitcode::IOERR);
        }
    };
    let config = config::read(config_path.as_path()).await.map_err(|err| {
        tracing::error!(error = ?err, "unable to read initial configuration file");
        exitcode::NOINPUT
    })?;
    let hopr_params = HoprParams::from(&args);

    // set up system socket
    let socket_path = args.socket_path.clone();
    let socket = socket_listener(&args.socket_path).await?;

    let mut maybe_router: Option<Box<dyn Routing>> = None;
    let res = loop_daemon(
        &mut ctrlc_receiver,
        socket,
        &worker_user,
        config,
        hopr_params,
        &mut maybe_router,
    )
    .await;

    // restore routing if connected
    teardown_any_routing(&maybe_router, true).await;

    let _ = fs::remove_file(&socket_path).await.map_err(|err| {
        tracing::error!(error = ?err, "failed removing socket on shutdown");
    });

    res
}

async fn loop_daemon(
    ctrlc_receiver: &mut mpsc::Receiver<()>,
    socket: UnixListener,
    worker_user: &worker::Worker,
    config: Config,
    hopr_params: HoprParams,
    maybe_router: &mut Option<Box<dyn Routing>>,
) -> Result<(), exitcode::ExitCode> {
    let (parent_socket, child_socket) = StdUnixStream::pair().map_err(|err| {
        tracing::error!(error = ?err, "unable to create socket pair for worker communication");
        exitcode::IOERR
    })?;

    // remove the "Close-On-Exec" flag to avoid premature socket closure by root
    let fd = child_socket.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
    }

    let mut worker_child = Command::new(worker_user.binary.clone())
        .current_dir(&worker_user.home)
        .env(socket::worker::ENV_VAR, format!("{}", child_socket.into_raw_fd()))
        .env("HOME", &worker_user.home)
        .uid(worker_user.uid)
        .gid(worker_user.gid)
        .spawn()
        .map_err(|err| {
            tracing::error!(error = ?err, ?worker_user, "unable to spawn worker process");
            exitcode::IOERR
        })?;

    parent_socket.set_nonblocking(true).map_err(|err| {
        tracing::error!(error = ?err, "unable to set non-blocking mode on parent socket");
        exitcode::IOERR
    })?;

    let parent_stream = UnixStream::from_std(parent_socket).map_err(|err| {
        tracing::error!(error = ?err, "unable to create unix stream from socket");
        exitcode::IOERR
    })?;

    // root <-> worker communication setup
    tracing::debug!("splitting unix stream into reader and writer halves");
    let (reader_half, writer_half) = io::split(parent_stream);
    let reader = BufReader::new(reader_half);
    let mut lines_reader = reader.lines();
    let mut writer = BufWriter::new(writer_half);

    // provide initial resources to worker
    send_to_worker(RootToWorker::HoprParams { hopr_params }, &mut writer).await?;
    send_to_worker(RootToWorker::Config { config }, &mut writer).await?;

    // enter main loop
    let mut shutdown_ongoing = false;
    // root <-> system socket communication setup (UI app)
    let mut socket_lines_reader: Option<io::Lines<BufReader<OwnedReadHalf>>> = None;
    let mut socket_writer: Option<BufWriter<OwnedWriteHalf>> = None;
    let cancel_token = CancellationToken::new();
    let (ping_sender, mut ping_receiver) = mpsc::channel(32);

    tracing::info!("entering main daemon loop");

    loop {
        tokio::select! {
            Some(_) = ctrlc_receiver.recv() => {
                if shutdown_ongoing {
                    tracing::info!("force shutdown immediately");
                    return Ok(());
                }
                // child will receive event as well - waiting for it to shutdown
                tracing::info!("initiate shutdown");
                shutdown_ongoing = true;
                cancel_token.cancel();
            },
            Ok((stream, _addr)) = socket.accept() , if socket_lines_reader.is_none() => {
                let (socket_reader_half, socket_writer_half) = stream.into_split();
                let socket_reader = BufReader::new(socket_reader_half);
                socket_lines_reader = Some(socket_reader.lines());
                socket_writer = Some(BufWriter::new(socket_writer_half));
            }
            Some(res) = ping_receiver.recv() => {
                send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::Ping { res }), &mut writer).await?;
            }
            Ok(Some(line)) = lines_reader.next_line() => {
                let cmd = parse_outgoing_worker(line)?;
                match cmd {
                    WorkerToRoot::Ack => {
                        tracing::debug!("received worker ack");
                    }
                    WorkerToRoot::OutOfSync => {
                        tracing::error!("worker out of sync with root - exiting");
                        return Err(exitcode::UNAVAILABLE);
                    }
                    WorkerToRoot::Response ( resp ) => {
                        tracing::debug!(?resp, "received worker response");
                        if let Some(mut writer) = socket_writer.take() {
                        send_to_socket(&resp, &mut writer).await?;
                        } else {
                            tracing::error!(?resp, "failed to send response to socket - no socket connection");
                        }
                    }
                    WorkerToRoot::RequestToRoot(request) => {
                        tracing::debug!(?request, "received worker request to root");
                        match request {
                            RequestToRoot::DynamicWgRouting { wg_data } => {
                                // ensure we run down before going up to ensure clean slate
                                teardown_any_routing(maybe_router, false).await;

                                match routing::build_router(worker_user.clone(), wg_data) {
                                    Ok(router) => {
                                        let res = router.setup().await.map_err(|e| format!("routing setup error: {}", e));
                                        *maybe_router = Some(Box::new(router));
                                        send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::DynamicWgRouting { res }), &mut writer).await?;
                                    },
                                    Err(error) => {
                                        tracing::error!(?error, "failed to build router");
                                        let res = Err(error.to_string());
                                        send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::DynamicWgRouting { res }), &mut writer).await?;
                                    }
                                }
                            },
                            RequestToRoot::StaticWgRouting { wg_data, peer_ips } => {
                                // ensure we run down before going up to ensure clean slate
                                teardown_any_routing(maybe_router, false).await;

                                let new_routing = routing::static_fallback_router(wg_data, peer_ips);
                                let res = new_routing.setup().await.map_err(|e| format!("routing setup error: {}", e));
                                *maybe_router = Some(Box::new(new_routing));
                                send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::StaticWgRouting { res }), &mut writer).await?;
                            }
                            RequestToRoot::TearDownWg => {
                                teardown_any_routing(maybe_router, true).await;
                            }
                            RequestToRoot::Ping { options } => {
                                spawn_ping(&options, &ping_sender, &cancel_token);
                            }
                        }
                    },
                }
            },
            Ok(Some(line)) = async { socket_lines_reader.take().unwrap().next_line().await }, if socket_lines_reader.is_some() => {
                let cmd: cmdCmd = parse_command(line)?;
                tracing::debug!(command = ?cmd, "received socket command");
                send_to_worker(RootToWorker::Command ( cmd ), &mut writer).await?;
            }
            Ok(status) = worker_child.wait() => {
                if shutdown_ongoing {
                    if status.success() {
                        tracing::info!("worker exited cleanly");
                    } else {
                        tracing::warn!(status = ?status.code(), "worker exited with error during shutdown");
                    }
                    return Ok(());
                } else {
                    tracing::error!(status = ?status.code(), "worker process exited unexpectedly");
                    return Err(exitcode::IOERR);
                }
            }
            else => {
                tracing::error!("unexpected channel closure");
                return Err(exitcode::IOERR);
            }
        }
    }
}

fn parse_outgoing_worker(line: String) -> Result<WorkerToRoot, exitcode::ExitCode> {
    let cmd: WorkerToRoot = serde_json::from_str(&line).map_err(|err| {
        tracing::error!(error = %err, "failed parsing outgoing worker command");
        exitcode::DATAERR
    })?;
    Ok(cmd)
}

fn parse_command(line: String) -> Result<cmdCmd, exitcode::ExitCode> {
    let cmd: cmdCmd = serde_json::from_str(&line).map_err(|err| {
        tracing::error!(error = %err, "failed parsing incoming socket command");
        exitcode::DATAERR
    })?;
    Ok(cmd)
}

async fn send_to_worker(
    msg: RootToWorker,
    writer: &mut BufWriter<WriteHalf<UnixStream>>,
) -> Result<(), exitcode::ExitCode> {
    let serialized = serde_json::to_string(&msg).map_err(|err| {
        tracing::error!(msg = ?msg, error = ?err, "failed to serialize message");
        exitcode::DATAERR
    })?;
    writer.write_all(serialized.as_bytes()).await.map_err(|err| {
        tracing::error!(serialized = ?serialized, error = ?err, "error writing to UnixStream pair write half");
        exitcode::IOERR
    })?;
    writer.write_all(b"\n").await.map_err(|err| {
        tracing::error!(error = ?err, "error appending newline to UnixStream pair write half");
        exitcode::IOERR
    })?;
    writer.flush().await.map_err(|err| {
        tracing::error!(error = ?err, "error flushing UnixStream pair write half");
        exitcode::IOERR
    })?;
    Ok(())
}

async fn send_to_socket(msg: &Response, writer: &mut BufWriter<OwnedWriteHalf>) -> Result<(), exitcode::ExitCode> {
    let serialized = serde_json::to_string(msg).map_err(|err| {
        tracing::error!(error = ?err, "failed to serialize response");
        exitcode::DATAERR
    })?;
    writer.write_all(serialized.as_bytes()).await.map_err(|err| {
        tracing::error!(error = ?err, "error writing to system socket");
        exitcode::IOERR
    })?;
    writer.write_all(b"\n").await.map_err(|err| {
        tracing::error!(error = ?err, "error appending newline to system socket");
        exitcode::IOERR
    })?;
    writer.flush().await.map_err(|err| {
        tracing::error!(error = ?err, "error flushing system socket");
        exitcode::IOERR
    })?;
    Ok(())
}

async fn teardown_any_routing(maybe_router: &Option<Box<dyn Routing>>, expected_up: bool) {
    if let Some(router) = maybe_router {
        match router.teardown().await {
            Ok(_) => {
                if !expected_up {
                    tracing::warn!("cleaned up unexpected existing routing");
                }
            }
            Err(err) => {
                if expected_up {
                    tracing::error!(error = ?err, "error tearing down routing");
                } else {
                    tracing::debug!(error = ?err, "expected error during non existing routing teardown");
                }
            }
        }
    }
}

fn spawn_ping(
    options: &ping::Options,
    sender: &mpsc::Sender<Result<Duration, String>>,
    cancel_token: &CancellationToken,
) {
    let cancel = cancel_token.clone();
    let sender = sender.clone();
    let options = options.clone();
    tokio::spawn(async move {
        cancel
            .run_until_cancelled(async move {
                // delay ping by one sec to increase success rate
                time::sleep(Duration::from_secs(1)).await;
                let res = ping::ping(&options).await.map_err(|e| {
                    tracing::debug!(error = ?e, "ping error");
                    e.to_string()
                });
                let _ = sender.send(res).await.map_err(|_| {
                    tracing::error!("ping receiver closed unexpectedly");
                });
            })
            .await;
    });
}

/// limit root service to two threads
/// one for the socket to be responsive
/// one for handling worker task orchestration
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args = cli::parse();

    // install global collector configured based on RUST_LOG env var.
    tracing_subscriber::fmt::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    match daemon(args).await {
        Ok(_) => (),
        Err(exitcode::OK) => (),
        Err(code) => {
            tracing::warn!("abnormal exit");
            process::exit(code);
        }
    }
}
