use gnosis_vpn_lib::logging::LogReloadHandle;
use tokio::fs;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener, UnixStream as TokioUnixStream};
use tokio::process::Command as TokioCommand;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{self};
use std::time::Duration;

use gnosis_vpn_lib::command::{Command as LibCommand, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::event::{RequestToRoot, ResponseFromRoot, RootToWorker, WorkerToRoot};
use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::worker_params::WorkerParams;
use gnosis_vpn_lib::{logging, ping, socket, worker};

mod cli;
mod routing;
mod wg_tooling;

use routing::Routing;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub const ENV_VAR_PID_FILE: &str = "GNOSISVPN_PID_FILE";

struct DaemonSetup {
    args: cli::Cli,
    worker_user: worker::Worker,
    config: Config,
    worker_params: WorkerParams,
    reload_handle: Option<LogReloadHandle>,
    log_path: Option<PathBuf>,
}

enum SignalMessage {
    Shutdown,
    RotateLogs,
}

struct SocketCmd {
    cmd: LibCommand,
    resp: oneshot::Sender<Response>,
}

struct WorkerChild {
    child: tokio::process::Child,
    pid: i32,
    parent_stream: TokioUnixStream,
}

async fn signal_channel() -> Result<mpsc::Receiver<SignalMessage>, exitcode::ExitCode> {
    let (sender, receiver) = mpsc::channel(32);
    let mut sigint = signal(SignalKind::interrupt()).map_err(|error| {
        tracing::error!(?error, "error setting up SIGINT handler");
        exitcode::IOERR
    })?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(|error| {
        tracing::error!(?error, "error setting up SIGTERM handler");
        exitcode::IOERR
    })?;
    let mut sighup = signal(SignalKind::hangup()).map_err(|error| {
        tracing::error!(?error, "error setting up SIGHUP handler");
        exitcode::IOERR
    })?;

    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(_) = sigint.recv() => {
                    tracing::debug!("received SIGINT");
                    if sender.send(SignalMessage::Shutdown).await.is_err() {
                        tracing::warn!("SIGINT: receiver closed");
                        break;
                    }
                },
                Some(_) = sigterm.recv() => {
                    tracing::debug!("received SIGTERM");
                    if sender.send(SignalMessage::Shutdown).await.is_err() {
                        tracing::warn!("SIGTERM: receiver closed");
                        break;
                    }
                },
                Some(_) = sighup.recv() => {
                    tracing::debug!("received SIGHUP");
                    if sender.send(SignalMessage::RotateLogs).await.is_err() {
                        tracing::warn!("SIGHUP: receiver closed");
                        break;
                    }
                }
                else => {
                    tracing::warn!("signal streams closed");
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
            match socket::root::process_cmd(socket_path, &LibCommand::Ping).await {
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
    let worker_params = WorkerParams::from(&args);

    // ensure worker user exists
    let input = worker::Input::new(
        args.worker_user.clone(),
        args.worker_binary.clone(),
        env!("CARGO_PKG_VERSION"),
        worker_params.state_home(),
    );
    let worker_user = worker::Worker::from_system(input).await.map_err(|err| {
        tracing::error!(error = ?err, "error determining worker user");
        exitcode::NOUSER
    })?;

    let reload_handle = setup_logging(&args.log_file, &worker_user)?;

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    // Write root pidfile for the newsyslog service to send signals to
    if let Some(ref pid_file) = args.pid_file {
        tracing::debug!(path = ?pid_file, "writing pidfile");
        let pid = process::id().to_string();
        fs::write(pid_file, pid).await.map_err(|e| {
            tracing::error!(error = ?e, "error writing pid file");
            exitcode::IOERR
        })?;
    }

    let mut signal_receiver = signal_channel().await?;

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

    // set up system socket
    let socket_path = args.socket_path.clone();
    let socket = socket_listener(&args.socket_path).await?;

    let mut maybe_router: Option<Box<dyn Routing>> = None;
    let log_path = args.log_file.clone();
    let setup = DaemonSetup {
        args,
        worker_user,
        config,
        worker_params,
        reload_handle,
        log_path,
    };
    let res = loop_daemon(setup, &mut signal_receiver, socket, &mut maybe_router).await;

    // restore routing if connected
    teardown_any_routing(&mut maybe_router, true).await;

    let _ = fs::remove_file(&socket_path).await.map_err(|err| {
        tracing::error!(error = ?err, "failed removing socket on shutdown");
    });

    res
}

async fn setup_worker(setup: &DaemonSetup) -> Result<WorkerChild, exitcode::ExitCode> {
    let (parent_socket, child_socket) = UnixStream::pair().map_err(|err| {
        tracing::error!(error = ?err, "unable to create socket pair for worker communication");
        exitcode::IOERR
    })?;

    // remove the "Close-On-Exec" flag to avoid premature socket closure by root
    let fd = child_socket.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
    }

    let mut worker_command = TokioCommand::new(setup.worker_user.binary.clone());
    if let Some(log_file) = &setup.args.log_file {
        worker_command
            .arg("--log-file")
            .arg(log_file.to_string_lossy().to_string());
    }
    let child = worker_command
        .current_dir(&setup.worker_user.home)
        .env(socket::worker::ENV_VAR, format!("{}", child_socket.into_raw_fd()))
        .env("HOPR_INTERNAL_MIXER_MINIMUM_DELAY_IN_MS", "0") // the client does not want to mix
        .env("HOPR_INTERNAL_MIXER_DELAY_RANGE_IN_MS", "1") // the mix range must be minimal to retain the QoS of the client
        .uid(setup.worker_user.uid)
        .gid(setup.worker_user.gid)
        .spawn()
        .map_err(|err| {
            tracing::error!(error = ?err, ?setup.worker_user, "unable to spawn worker process");
            exitcode::IOERR
        })?;

    let pid: i32 = child.id().map(|id| id as i32).ok_or_else(|| {
        tracing::error!("unable to get worker PID");
        exitcode::IOERR
    })?;

    parent_socket.set_nonblocking(true).map_err(|err| {
        tracing::error!(error = ?err, "unable to set non-blocking mode on parent socket");
        exitcode::IOERR
    })?;

    let parent_stream = TokioUnixStream::from_std(parent_socket).map_err(|err| {
        tracing::error!(error = ?err, "unable to create unix stream from socket");
        exitcode::IOERR
    })?;

    // root <-> worker communication setup
    tracing::debug!("splitting unix stream into reader and writer halves");

    Ok(WorkerChild {
        child,
        pid,
        parent_stream,
    })
}

async fn loop_daemon(
    setup: DaemonSetup,
    signal_receiver: &mut mpsc::Receiver<SignalMessage>,
    socket: UnixListener,
    maybe_router: &mut Option<Box<dyn Routing>>,
) -> Result<(), exitcode::ExitCode> {
    // safe state_home for usage later
    let state_home = setup.worker_params.state_home();

    /*
    let (reader_half, writer_half) = io::split(parent_stream);
    let reader = BufReader::new(reader_half);
    let mut socket_lines_reader = reader.lines();
    let mut socket_writer = BufWriter::new(writer_half);

    // provide initial resources to worker
    send_to_worker( RootToWorker::WorkerParams { worker_params: setup.worker_params, }, &mut socket_writer,) .await?;
    send_to_worker(RootToWorker::Config { config: setup.config }, &mut socket_writer).await?;
    */

    // External socket commands need an internal mapping:
    // root process will keep track of worker requests and map their responses
    // so that the requesting stream on the socket receives it's answer
    let mut pending_response_counter: u64 = 0;
    let mut pending_responses: HashMap<u64, oneshot::Sender<Response>> = HashMap::new();
    let mut ongoing_request_responses = JoinSet::new();

    // enter main loop
    let mut shutdown_ongoing = false;
    let cancel_token = CancellationToken::new();
    let (ping_sender, mut ping_receiver) = mpsc::channel(32);
    let (socket_cmd_sender, mut socket_cmd_receiver) = mpsc::channel(32);

    let mut worker_child: Option<WorkerChild> = None;

    tracing::info!("entering main daemon loop");

    loop {
        tokio::select! {
            Some(signal) = signal_receiver.recv() => match signal {
                SignalMessage::Shutdown => {
                    if shutdown_ongoing {
                        tracing::warn!("force shutdown immediately");
                        return Ok(());
                    }
                    tracing::info!("initiate shutdown");
                    shutdown_ongoing = true;
                    cancel_token.cancel();
                    teardown_any_routing(maybe_router, true).await;
                    ongoing_request_responses.shutdown().await;
                }
                SignalMessage::RotateLogs => {
                    // Recreate the file layer and swap it in the reload handle so that logging continues to the new file after rotation
                    // Note: we rely on newsyslog to have already rotated the file (renamed it and created a new one) before sending SIGHUP, so make_file_fmt_layer should open the new file rather than the rotated one
                    if let (Some(handle), Some(path)) = (&setup.reload_handle, &setup.log_path) {
                        let res = logging::make_file_fmt_layer(&path.to_string_lossy(), &setup.worker_user).map(|new_layer| handle.reload(new_layer));
                        match res {
                            Ok(_) => {
                                tracing::info!("successfully reloaded logging layer with new log file after SIGHUP");
                            },
                            Err(e) => {
                                eprintln!("failed to reopen log file {:?}: {}", path, e);
                                return Err(exitcode::IOERR);
                            }
                        };
                        if let Some(pid) = worker_child.as_ref().map(|child| child.pid) {
                        tracing::debug!("forwarding SIGHUP to worker process {}", pid);
                        unsafe {
                            libc::kill(pid, libc::SIGHUP);
                        }
                        }
                    } else {
                        tracing::debug!("no log file configured, skipping log reload on SIGHUP");
                    }
                }
            },

            Ok((stream, _addr)) = socket.accept() => {
                let cmd_sender = socket_cmd_sender.clone();
                ongoing_request_responses.spawn(async move {
                    if let Some(handle) = incoming_on_root_socket(stream, &cmd_sender).await {
                        handle.await.ok();
                    }
                });
            }
            Some(socket_cmd) = socket_cmd_receiver.recv() => {
                pending_response_counter += 1;
                pending_responses.insert(pending_response_counter, socket_cmd.resp);
                let msg = RootToWorker::Command { cmd: socket_cmd.cmd, id: pending_response_counter };
                send_to_worker(msg, &mut socket_writer).await?;
            }
            Some(res) = ping_receiver.recv() => {
                send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::Ping { res }), &mut socket_writer).await?;
            }
            Ok(Some(line)) = socket_lines_reader.next_line() => {
                let cmd = parse_outgoing_worker(line)?;
                match cmd {
                    WorkerToRoot::Ack => {
                        tracing::debug!("received worker ack");
                    }
                    WorkerToRoot::OutOfSync => {
                        tracing::error!("worker out of sync with root - exiting");
                        return Err(exitcode::UNAVAILABLE);
                    }
                    WorkerToRoot::Response { id, resp } => {
                        tracing::debug!(?resp, "received worker response");
                        if let Some(resp_sender) = pending_responses.remove(&id) {
                            if resp_sender.send(resp).is_err() {
                                tracing::error!(id, "unexpected channel closure");
                            }
                        } else {
                            tracing::warn!(id, "no pending response found for worker response");
                        }
                    }
                    WorkerToRoot::RequestToRoot(request) => {
                        tracing::debug!(?request, "received worker request to root");
                        match request {
                            RequestToRoot::DynamicWgRouting { wg_data  } => {
                                // ensure we run down before going up to ensure clean slate
                                teardown_any_routing(maybe_router, false).await;

                                let router_result = routing::dynamic_router(state_home.clone(), setup.worker_user.clone(), wg_data);

                                match router_result {
                                    Ok(mut router) => {
                                        let res = router.setup().await.map_err(|e| format!("routing setup error: {}", e));
                                        *maybe_router = Some(Box::new(router));
                                        send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::DynamicWgRouting { res }), &mut socket_writer).await?;
                                    },
                                    Err(error) => {
                                        if error.is_not_available() {
                                            tracing::debug!(?error, "dynamic routing not available on this platform");
                                        } else {
                                            tracing::error!(?error, "failed to build dynamic router");
                                        }
                                        let res = Err(error.to_string());
                                        send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::DynamicWgRouting { res }), &mut socket_writer).await?;
                                    }
                                }
                            },
                            RequestToRoot::StaticWgRouting { wg_data, peer_ips } => {
                                let mut new_routing = routing::static_router(state_home.clone(), wg_data, peer_ips);

                                // ensure we run down before going up to ensure clean slate
                                teardown_any_routing(maybe_router, true).await;
                                let _ = new_routing.teardown(Logs::Suppress).await;

                                // bring up new static routing
                                let res = new_routing.setup().await.map_err(|e| format!("routing setup error: {}", e));
                                *maybe_router = Some(Box::new(new_routing));
                                send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::StaticWgRouting { res }), &mut socket_writer).await?;
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

async fn incoming_on_root_socket(
    stream: TokioUnixStream,
    socket_cmd_sender: &mpsc::Sender<SocketCmd>,
) -> Option<JoinHandle<()>> {
    let (socket_reader_half, socket_writer_half) = stream.into_split();
    let socket_reader = BufReader::new(socket_reader_half);
    let res_line = socket_reader.lines().next_line().await;
    match res_line {
        Ok(Some(line)) => {
            let res_decode = serde_json::from_str::<LibCommand>(&line);
            match res_decode {
                Ok(cmd) => {
                    tracing::debug!(command = ?cmd, "received socket command");
                    let (resp_sender, resp_receiver) = oneshot::channel();
                    let socket_cmd = SocketCmd { cmd, resp: resp_sender };
                    if let Err(err) = socket_cmd_sender.send(socket_cmd).await {
                        tracing::error!(error = ?err, "failed to send socket command to main loop");
                        return None;
                    }
                    // wait for response and send back to socket
                    let handle = tokio::spawn(async move {
                        match resp_receiver.await {
                            Ok(resp) => {
                                let mut writer = BufWriter::new(socket_writer_half);
                                if let Err(err) = send_to_socket(&resp, &mut writer).await {
                                    tracing::error!(error = ?err, "failed to send response to socket");
                                }
                            }
                            Err(err) => {
                                tracing::error!(error = ?err, "socket command response channel closed");
                            }
                        }
                    });
                    return Some(handle);
                }
                Err(err) => {
                    tracing::error!(error = %err, "failed parsing incoming socket command");
                }
            }
        }
        Ok(None) => {
            tracing::warn!("socket connection closed by peer");
        }
        Err(err) => {
            tracing::error!(error = ?err, "error reading from socket");
        }
    };
    None
}

fn parse_outgoing_worker(line: String) -> Result<WorkerToRoot, exitcode::ExitCode> {
    let cmd: WorkerToRoot = serde_json::from_str(&line).map_err(|err| {
        tracing::error!(error = %err, "failed parsing outgoing worker command");
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

async fn teardown_any_routing(maybe_router: &mut Option<Box<dyn Routing>>, expected_up: bool) {
    if let Some(router) = maybe_router {
        let logs = if expected_up { Logs::Print } else { Logs::Suppress };
        match router.teardown(logs).await {
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

    match daemon(args).await {
        Ok(_) => (),
        Err(exitcode::OK) => (),
        Err(code) => {
            tracing::warn!("abnormal exit");
            process::exit(code);
        }
    }
}

fn setup_logging(
    log_file: &Option<std::path::PathBuf>,
    worker: &worker::Worker,
) -> Result<Option<logging::LogReloadHandle>, exitcode::ExitCode> {
    match log_file {
        Some(log_path) => {
            let fmt_layer = logging::make_file_fmt_layer(&log_path.to_string_lossy(), worker).map_err(|err| {
                eprintln!("Failed to create log layer for file {}: {}", log_path.display(), err);
                exitcode::IOERR
            })?;
            let handle = logging::setup_log_file(fmt_layer).map_err(|err| {
                eprintln!("Failed to open log file {}: {}", log_path.display(), err);
                exitcode::IOERR
            })?;
            Ok(Some(handle))
        }
        None => {
            logging::setup_stdout();
            Ok(None)
        }
    }
}
