use gnosis_vpn_lib::logging::LogReloadHandle;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::{UnixListener as TokioUnixListener, UnixStream as TokioUnixStream};
use tokio::process::Command as TokioCommand;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{AsRawFd, IntoRawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{self};
use std::time::Duration;

use gnosis_vpn_lib::command::{Command as LibCommand, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::event::{self, RequestToRoot, ResponseFromRoot, RootToWorker, WorkerToRoot};
use gnosis_vpn_lib::shell_command_ext::Logs;
use gnosis_vpn_lib::worker_params::WorkerParams;
use gnosis_vpn_lib::{dirs, logging, ping, socket, worker};

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

struct DaemonState {
    args: cli::Cli,
    worker_user: worker::Worker,
    config: Config,
    worker_params: WorkerParams,
    reload_handle: Option<LogReloadHandle>,
    router: Option<Box<dyn Routing>>,
    worker_child: Option<WorkerChild>,
    shutdown_ongoing: bool,
    // External socket commands need an internal mapping:
    // root process will keep track of worker requests and map their responses
    // so that the requesting stream on the socket receives it's answer
    pending_response_counter: u64,
    pending_responses: HashMap<u64, oneshot::Sender<Response>>,
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
    socket_writer:  BufWriter<WriteHalf<UnixStream>>,
    cancel: CancellationToken,
}

async fn signal_channel() -> Result<(CancellationToken, mpsc::Receiver<SignalMessage>), exitcode::ExitCode> {
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

    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(_) = sigint.recv() => {
                    tracing::debug!("received SIGINT");
                    let _ =  sender.send(SignalMessage::Shutdown).await;
                },
                Some(_) = sigterm.recv() => {
                    tracing::debug!("received SIGTERM");
                    let _ =  sender.send(SignalMessage::Shutdown).await;
                },
                Some(_) = sighup.recv() => {
                    tracing::debug!("received SIGHUP");
                    let _ =  sender.send(SignalMessage::RotateLogs).await;
                }
                _ = cancel.cancelled() => {
                    tracing::debug!("signal channel received cancellation");
                    break;
                }
                else => {
                    tracing::warn!("signal channel streams closed");
                    break;
                }
            }
        }
    });

    tracing::info!("signal handlers set up for SIGINT, SIGTERM and SIGHUP");
    Ok((owned_cancel, receiver))
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

async fn socket_listener(
    socket_path: &Path,
) -> Result<(CancellationToken, mpsc::Receiver<SocketCmd>), exitcode::ExitCode> {
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

    if let Some(socket_dir) = socket_path.parent() {
        fs::create_dir_all(socket_dir).await.map_err(|e| {
            tracing::error!(error = %e, "error creating socket directory");
            exitcode::IOERR
        })?;
    }

    let listener = TokioUnixListener::bind(socket_path).map_err(|e| {
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

    let ongoing = JoinSet::new();
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let (sender, receiver) = mpsc::channel(32);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok((stream, addr)) = listener.accept() => {
                    ongoing.spawn(async move {
                        if let Some(handle) = incoming_on_root_socket(stream, &sender).await {
                            handle.await.ok();
                        }
                    });
                },
                _ = cancel.cancelled() => {
                    tracing::debug!("socket listener received cancellation");
                    ongoing.shutdown().await;
                    break;
                }
                else => {
                    tracing::warn!("socket listener streams closed");
                    break;
                }

            }
        }
    });

    Ok((owned_cancel, receiver))
}

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    // ensure worker user exists
    let worker_params = WorkerParams::from(&args);
    let input = worker::Input::new(
        args.worker_user.clone(),
        args.worker_binary.clone(),
        env!("CARGO_PKG_VERSION"),
        worker_params.state_home(),
    );
    let worker_user = worker::Worker::from_system(input).await.map_err(|error| {
        eprintln!("error determining worker user: {:?}", error);
        exitcode::NOUSER
    })?;

    // setup logging
    let reload_handle = setup_logging(&args.log_file, &worker_user)?;

    // introduce ourself in the logs
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        state_home = %worker_params.state_home().display(),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    // Write root pidfile for the newsyslog service to send signals to
    write_pidfile(&args.pid_file).await?;

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

    // set up signal handlers
    let (cancel_signal_handlers, signal_receiver) = signal_channel().await?;

    // set up system socket
    let socket_path = args.socket_path.clone();
    let (cancel_socket_listener, socket_listener) = socket_listener(&args.socket_path).await?;

    // Clean up any stale fwmark infrastructure if it exists (Linux only, dynamic routing only)
    routing::reset_on_startup(&worker_params.state_home()).await;

    let state = DaemonState::new( args, worker_user, config, worker_params, reload_handle,);
    let res = state.loop(signal_receiver, socket_listener).await;

    // restore routing if connected
    state.teardown_any_routing(None).await;

    // cancel running tasks
    cancel_socket_listener.cancel();
    cancel_signal_handlers.cancel();

    // remove socket file
    let _ = fs::remove_file(&socket_path).await.map_err(|err| {
        tracing::error!(error = ?err, "failed removing socket on shutdown");
    });

    res
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
        router.teardown(logs).await;
    }
}

async fn setup_dynamic_routing(
    maybe_router: &mut Option<Box<dyn Routing>>,
    state_home: PathBuf,
    worker_user: worker::Worker,
    wg_data: event::WireGuardData,
) -> Result<(), String> {
    // ensure we run down before going up to ensure clean slate
    teardown_any_routing(maybe_router, false).await;

    let res_router = routing::dynamic_router(state_home, worker_user, wg_data).await;
    match res_router {
        Ok(mut router) => {
            // router creation successfull, try setup
            let res_setup = router.setup().await;
            *maybe_router = Some(Box::new(router));
            match res_setup {
                Ok(_) => {
                    tracing::info!("dynamic routing setup successfully");
                    Ok(())
                }
                Err(error) => {
                    // setup failed, call cleanup and return error
                    tracing::error!(?error, "dynamic routing setup error");
                    teardown_any_routing(maybe_router, true).await;
                    Err(error.to_string())
                }
            }
        }
        Err(error) => {
            // router creation failed, return error
            tracing::error!(?error, "failed to build dynamic router");
            Err(error.to_string())
        }
    }
}

async fn setup_static_routing(
    maybe_router: &mut Option<Box<dyn Routing>>,
    state_home: PathBuf,
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
) -> Result<(), String> {
    // ensure we run down before going up to ensure clean slate
    teardown_any_routing(maybe_router, false).await;

    let res_router = routing::static_router(state_home, wg_data, peer_ips);
    match res_router {
        Ok(mut router) => {
            // router creation successfull, try setup
            let res_setup = router.setup().await;
            *maybe_router = Some(Box::new(router));
            match res_setup {
                Ok(_) => {
                    tracing::info!("static routing setup successfully");
                    Ok(())
                }
                Err(error) => {
                    // setup failed, call cleanup and return error
                    tracing::error!(?error, "static routing setup error");
                    teardown_any_routing(maybe_router, true).await;
                    Err(error.to_string())
                }
            }
        }
        Err(error) => {
            // router creation failed, return error
            tracing::error!(?error, "failed to build static router");
            Err(error.to_string())
        }
    }
}

fn spawn_ping(options: ping::Options) -> (CancellationToken, oneshot::Receiver<Result<Duration, String>>) {
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let (sender, receiver) = oneshot::channel();
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
    (owned_cancel, receiver)
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
            if let Some(parent) = log_path.parent() {
                dirs::ensure_dir(&parent.to_path_buf(), 0o755, worker.uid, worker.gid).map_err(|err| {
                    eprintln!("Failed to create log directory {}: {}", parent.display(), err);
                    exitcode::IOERR
                })?;
            }
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

// On macOS newsyslog service needs the pid accessible via pidfile
// Launchctl will not create that pidfile for us
async fn write_pidfile(pid_file: &Option<PathBuf>) -> Result<(), exitcode::ExitCode> {
    if let Some(pid_file) = pid_file {
        if let Some(pid_dir) = pid_file.parent() {
            fs::create_dir_all(pid_dir).await.map_err(|e| {
                tracing::error!(error = %e, "error creating pid_file directory");
                exitcode::IOERR
            })?;
        }

        tracing::debug!(path = ?pid_file, "writing pidfile");
        let pid = process::id().to_string();
        fs::write(pid_file, pid).await.map_err(|e| {
            tracing::error!(error = ?e, "error writing pid file");
            exitcode::IOERR
        })?;
    }
    Ok(())
}

impl DaemonState {
    fn new(
    args: cli::Cli,
    worker_user: worker::Worker,
    config: Config,
    worker_params: WorkerParams,
    reload_handle: Option<LogReloadHandle>,
    ) -> Self{
        Self {
            args,
            worker_user,
            config,
            worker_params,
            reload_handle,
            router: None,
            worker_child: None,
            shutdown_ongoing: false,
            pending_response_counter: 0,
            pending_responses: HashMap::new(),
        }
    }


    async fn teardown_any_routing(&mut self, overwrite_expected_up: Option<bool>) {
        if let Some(ref mut router) = self.router {
            let logs = overwrite_expected_up.map(|expected_up| if expected_up { Logs::Print } else { Logs::Suppress })
                .unwrap_or( Logs::Print);
            router.teardown(logs).await;
        }
    }

    async fn loop(&mut self, signal_receiver: mpsc::Receiver<SignalMessage>, socket_listener: mpsc::Receiver<LibCommand>) -> Result<(), exitcode::ExitCode> {

    loop {
        tokio::select! {
            Some(signal) = signal_receiver.recv() => self.incoming_signal(signal).await?,
            Some(cmd) = socket_listener.recv() => self.incoming_socket_command(cmd).await?,
            Ok(status) = self.worker_child.as_mut().map(|c| c.child.wait()) if self.worker_child.is_some() => self.incoming_worker_exit(status).await?,
            else => {
                tracing::error!("unexpected channel closure");
                return Err(exitcode::IOERR);
            }
        }
    }
    }

    async fn incoming_signal(&mut self, signal:SignalMessage) -> Result<(), exitcode::ExitCode> {
        match signal {
            SignalMessage::Shutdown => {
                if self.shutdown_ongoing {
                    tracing::warn!("received shutdown signal but shutdown is already ongoing - forcing immediate exit");
                    Err(exitcode::OK)
                } else {
                tracing::info!("received shutdown signal - initiating shutdown");
                self.shutdown_ongoing = true;
                if let Some(child) = self.worker_child.as_ref() {
                    tracing::debug!("sending shutdown signal to worker process");
                    send_to_worker(RootToWorker::Shutdown, &mut child.socket_writer).await?;
                    Ok(())
                } else {
                    tracing::debug!("no worker process active - shutdown immediately");
                    Err(exitcode::OK)
                }
                }
            },
            SignalMessage::RotateLogs => {
                if let (Some(handle), Some(path)) = (&self.reload_handle, &self.args.log_file) {
                    let res = logging::make_file_fmt_layer(&path.to_string_lossy(), &self.worker_user).map(|new_layer| handle.reload(new_layer));
                    match res {
                        Ok(_) => {
                            tracing::info!("successfully reloaded logging layer with new log file after SIGHUP");
                            if let Some(child) = self.worker_child.as_ref() {
                                tracing::debug!("sending rotate logs to worker process");
                                send_to_worker(RootToWorker::RotateLogs, &mut child.socket_writer).await?;
                            }
                            OK(())
                        },
                        Err(e) => {
                            eprintln!("failed to reopen log file {:?}: {}", path, e);
                            Err(exitcode::IOERR)
                        }
                    }
                } else {
                    tracing::debug!("no log file configured, skipping log reload on SIGHUP");
                    Ok(())
                }
            }
        }
    }

    async fn incoming_socket_command(&mut self, socket_cmd: SocketCmd) -> Result<(), exitcode::ExitCode> {
        match WorkerCommand::try_from(socket_cmd.cmd) {
            Ok(w_cmd) => {
                match self.worker_child {
                    Some(child) => {
                        self.pending_response_counter += 1;
                        self.pending_responses.insert(self.pending_response_counter, socket_cmd.resp);
                        let msg = RootToWorker::Command { cmd: socket_cmd.cmd, id: self.pending_response_counter };
                        send_to_worker(msg, &mut child.socket_writer).await?;
                    }
                    None => {
                        let _ = socket_cmd.resp.send(Response::WorkerOffline).map_err(|error| {
                            tracing::error!(?error, "socket command response channel closed");
                        });
                    }
                }
            }
            Err(_) => {
                let resp = self.incoming_root_command(socket_cmd.cmd);
                let _ = socket_cmd.resp.send(resp).map_err(|error| {
                            tracing::error!(?error, "socket command response channel closed");
                });
            }
        }
    }

    async fn incoming_root_command(&self, cmd: LibCommand) -> Result<Response, exitcode::ExitCode> {
        match cmd {
    LibCommand::Status | LibCommand::NerdStats | LibCommand::Connect(_) | LibCommand::Disconnect | LibCommand::Balance | LibCommand::FundingTool(_) | LibCommand::Telemetry | LibCommand::RefreshNode => {
        Ok(Response::WorkerOffline)
    },
        LibCommand::Ping => Ok(Response::Pong),
            LibCommand::Info => Ok(Response::Info(InfoResponse { version: env!("CARGO_PKG_VERSION").to_string() })),
            LibCommand::StartClient => {
                match self.worker_child {
                    Some(_) => {
                    Ok(Response::StartClient(StartClientResponse::AlreadyRunning))
                    },
                    None => {
                        self.setup_worker().await?;
                        Ok(Response::StartClient(StartClientResponse::Started))
                    }
                }
            },
            LibCommand::StopClient => {
                match self.worker_child {
                    Some(child) => {
                        tracing::debug!("sending shutdown signal to worker process due to StopClient command");
                        send_to_worker(RootToWorker::Shutdown, &mut child.socket_writer).await?;
                        Ok(Response::StopClient(StopClientResponse::Stopped))
                    },
                    None => {
                        Ok(Response::StopClient(StopClientResponse::NotRunning))
                    }
                }
            },
        }
    }

    async fn incoming_worker_line(&mut self, line: String) -> Result<(), exitcode::ExitCode> {
    let cmd: WorkerToRoot = serde_json::from_str(&line).map_err(|err| {
        tracing::error!(error = %err, "failed parsing incoming worker command");
        exitcode::DATAERR
    })?;
    match cmd {
        WorkerToRoot::Response {id, resp} => self.incoming_worker_response(id, resp).await,
        WorkerToRoot::RequestToRoot(request) => self.incoming_worker_request(request).await
    }
    }

    fn incoming_worker_response(&mut self, id: u64, resp: Response) -> Result<(), exitcode::ExitCode> {
        tracing::debug!(?resp, "received worker response");
        if let Some(resp_sender) = self.pending_responses.remove(&id) {
            if resp_sender.send(resp).is_err() {
                tracing::error!(id, "unexpected channel closure");
            }
        } else {
            tracing::warn!(id, "no pending response found for worker response");
        }
        Ok(())
    }

    async fn incoming_worker_request(&mut self, request: RequestToRoot) {
        tracing::debug!(?request, "received worker request to root");
        match request {
            RequestToRoot::DynamicWgRouting { wg_data } => {
                let res = self.setup_dynamic_routing(wg_data);
                                send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::DynamicWgRouting { res }), &mut socket_writer).await?;
            },
            RequestToRoot::StaticWgRouting { wg_data, peer_ips } => {
                let res = self.setup_static_routing(wg_data);
                let state_home = self.worker_params.state_home();
                                send_to_worker(RootToWorker::ResponseFromRoot(ResponseFromRoot::StaticWgRouting { res }), &mut socket_writer).await?;
            },
            RequestToRoot::TearDownWg => {
                self.teardown_any_routing(None).await;
            },
            RequestToRoot::Ping { options } => {
                let (cancel, receiver) = spawn_ping(options).await;
                Ok(())
            }
        }
    }

    fn incoming_worker_exit(&mut self, status: std::process::ExitStatus) -> exitcode::ExitCode {
                if self.shutdown_ongoing {
                    if status.success() {
                        tracing::info!("worker exited cleanly");
                        exitcode::OK
                    } else {
                        tracing::warn!(status = ?status.code(), "worker exited with error during shutdown");
                        exitcode::IOERR
                    }
                } else {
                    tracing::error!(status = ?status.code(), "worker process exited unexpectedly");
                    exitcode::IOERR
                }
    }

async fn setup_worker(&mut self) -> Result<(), exitcode::ExitCode> {
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

    let mut worker_command = TokioCommand::new(setup.worker_user.binary.clone())
        .current_dir(&setup.worker_user.home)
        .env(socket::worker::ENV_VAR, format!("{}", child_socket.into_raw_fd()))
        .env("HOPR_INTERNAL_MIXER_MINIMUM_DELAY_IN_MS", "0") // the client does not want to mix
        .env("HOPR_INTERNAL_MIXER_DELAY_RANGE_IN_MS", "1") // the mix range must be minimal to retain the QoS of the client
        .uid(setup.worker_user.uid)
        .gid(setup.worker_user.gid);

    if let Some(log_file) = self.args.log_file {
        worker_command = worker_command.env(logging::ENV_VAR_LOG_FILE, log_file.to_string_lossy().to_string());
    }
    let child = worker_command.spawn() .map_err(|err| {
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
    let (reader_half, writer_half) = io::split(parent_stream);
    let reader = BufReader::new(reader_half);
    let mut socket_lines_reader = reader.lines();
    let mut socket_writer = BufWriter::new(writer_half);

    // send initial configuration and resources to worker
    send_to_worker( RootToWorker::StartupParams { config: self.config.clone(), worker_params: self.worker_params.clone(), }, &mut socket_writer,).await?;

    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Ok(Some(line)) = socket_lines_reader.next_line() => self.incoming_worker_line(line).await,
                _ = cancel.cancelled() => {
                    tracing::debug!("worker received cancellation");
                    break;
                }
                else => {
                    tracing::warn!("worker streams closed");
                    break;
                }
            }
        }
    });

    self.worker_child = Some(WorkerChild {
        child,
        pid,
        cancel,
        socket_writer,
    });
}
}
