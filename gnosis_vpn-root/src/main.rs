use gnosis_vpn_lib::logging::LogReloadHandle;
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::fs;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
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

use gnosis_vpn_lib::command::{self, Command as LibCommand, Response, WorkerCommand};
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
    worker_user: worker::Worker,
    config: Config,
    config_path: PathBuf,
    log_file: Option<PathBuf>,
    worker_params: WorkerParams,
    reload_handle: Option<LogReloadHandle>,
    router: Option<Box<dyn Routing>>,
    shutdown_ongoing: Shutdown,
    // keep track of the current target for restore/restart/reload logic
    target_dest_id: Option<String>,
    // used to forward messages incoming on unix socket to worker process
    incoming_worker_channel: (mpsc::Sender<String>, mpsc::Receiver<String>),
    // optional worker paramters set after construction
    worker_child: Option<WorkerChild>,
    // status code channel for when the worker process exits
    worker_exit_channel: (mpsc::Sender<process::ExitStatus>, mpsc::Receiver<process::ExitStatus>),
    // keep track of longer running root tasks
    ping_tasks: JoinSet<Result<Duration, String>>,
    // External socket commands need an internal mapping:
    // root process will keep track of worker requests and map their responses
    // so that the requesting stream on the socket receives it's answer
    pending_response_counter: u64,
    pending_responses: HashMap<u64, oneshot::Sender<Response>>,
    // keepalive instructions from service to timer loop
    keep_alive_instruction_sender: mpsc::Sender<KeepAliveInstruction>,
}

#[derive(Debug, Clone, Copy)]
enum Shutdown {
    Worker,
    RestartWorker,
    Service,
    None,
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
    socket_writer: BufWriter<WriteHalf<TokioUnixStream>>,
    cancel: CancellationToken,
}

#[derive(Debug)]
enum KeepAliveInstruction {
    Reset,
    Ignite(Duration),
    Stop,
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
    socket_cmd_sender: mpsc::Sender<SocketCmd>,
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

    let mut ongoing = JoinSet::new();
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let (sender, receiver) = mpsc::channel(32);
    tokio::spawn(async move {
        loop {
            let cloned_sender = sender.clone();
            tokio::select! {
                Ok((stream, _addr)) = listener.accept() => {
                    ongoing.spawn(async move {
                        if let Some(handle) = incoming_on_root_socket(stream, cloned_sender).await {
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

pub async fn config_watcher(
    config_path: PathBuf,
) -> Result<(CancellationToken, mpsc::Receiver<()>), exitcode::ExitCode> {
    let parent = match config_path.parent() {
        Some(parent) => parent,
        None => {
            tracing::error!("config path has no parent directory");
            return Err(exitcode::IOERR);
        }
    };

    let config_file = config_path.clone();
    // Bridge from sync OS thread to async Tokio task
    let (notify_tx, mut notify_rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(event) = res
            && event.paths.iter().any(|path| path == &config_file)
        {
            match event.kind {
                EventKind::Modify(_data) => {
                    let _ = notify_tx.send(());
                }
                EventKind::Create(_data) => {
                    let _ = notify_tx.send(());
                }
                _ => {}
            }
        }
    })
    .map_err(|e| {
        tracing::error!(error = ?e, "error setting up config file watcher");
        exitcode::IOERR
    })?;

    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    watcher.watch(parent, RecursiveMode::NonRecursive).map_err(|e| {
        tracing::error!(error = ?e, "error watching config file");
        exitcode::IOERR
    })?;
    tracing::info!(config_path = %config_path.display(), "watching config file for changes");

    let (sender, receiver) = mpsc::channel(1);
    let debounce_duration = Duration::from_millis(250);
    tokio::spawn(async move {
        // keep watcher alive
        let _watcher = watcher;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!("config watcher received cancellation.");
                    return;
                }
                Some(_) = notify_rx.recv() => {
                    // create second debounce loop
                    let debounce_timeout = time::sleep(debounce_duration);
                    tokio::pin!(debounce_timeout);
                    loop {
                        tokio::select! {
                            _ = cancel.cancelled() => {
                                tracing::debug!("config watcher received cancellation during debounce.");
                                return;
                            }
                            _ = debounce_timeout.as_mut() => {
                                let _ = sender.send(()).await;
                                break;
                            }
                            Some(_) = notify_rx.recv() => {
                                debounce_timeout.as_mut().reset(time::Instant::now() + debounce_duration);
                            }
                        }
                    }
                }
            }
        }
    });

    Ok((owned_cancel, receiver))
}

async fn keep_alive_timer(
    mut keep_alive_instruction_receiver: mpsc::Receiver<KeepAliveInstruction>,
) -> Result<(CancellationToken, mpsc::Receiver<()>), exitcode::ExitCode> {
    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let (sender, receiver) = mpsc::channel(1);
    tokio::spawn(async move {
        let mut active = false;
        let mut dur = Duration::ZERO;
        let keepalive = time::sleep(dur);
        tokio::pin!(keepalive);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    tracing::debug!("keep alive timer received cancellation.");
                    return;
                }
                Some(msg) = keep_alive_instruction_receiver.recv() => {
                    match msg {
                        KeepAliveInstruction::Reset => {
                            keepalive.as_mut().reset(time::Instant::now() + dur);
                        }
                        KeepAliveInstruction::Ignite(duration) => {
                            tracing::debug!(?duration, "ignite keep alive timer");
                            active = true;
                            dur = duration;
                            keepalive.as_mut().reset(time::Instant::now() + dur);
                        }
                        KeepAliveInstruction::Stop => {
                            tracing::debug!("stop keep alive timer");
                            active = false;
                            keepalive.as_mut().reset(time::Instant::now())
                        }
                    }
                }
                _ = keepalive.as_mut(), if active => {
                    active = false;
                    let _ = sender.send(()).await;
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

    // set up config file watcher
    let (cancel_config_watcher, config_receiver) = config_watcher(config_path.clone()).await?;

    // set up keepalive timer
    let (keep_alive_instruction_sender, keep_alive_instruction_receiver) = mpsc::channel(32);
    let (cancel_keep_alive_timer, keep_alive_expired) = keep_alive_timer(keep_alive_instruction_receiver).await?;

    // Clean up any stale fwmark infrastructure if it exists (Linux only, dynamic routing only)
    routing::reset_on_startup(worker_params.state_home()).await;

    let mut state = DaemonState::new(
        worker_user,
        config,
        config_path,
        worker_params,
        reload_handle,
        args.log_file,
        keep_alive_instruction_sender,
    );
    if let Some(keepalive) = args.client_autostart {
        tracing::debug!(?keepalive, "autostarting worker process");
        state.setup_worker().await?;
        let _ = state
            .keep_alive_instruction_sender
            .send(KeepAliveInstruction::Ignite(keepalive))
            .await;
    }
    let res = state
        .daemon_loop(signal_receiver, socket_listener, config_receiver, keep_alive_expired)
        .await;

    // cancel running tasks and run teardown logic
    state.teardown().await;
    cancel_socket_listener.cancel();
    cancel_signal_handlers.cancel();
    cancel_config_watcher.cancel();
    cancel_keep_alive_timer.cancel();

    // remove socket file
    let _ = fs::remove_file(&socket_path).await.map_err(|err| {
        tracing::error!(error = ?err, "failed removing socket on shutdown");
    });

    res
}

async fn send_to_worker(
    msg: RootToWorker,
    writer: &mut BufWriter<WriteHalf<TokioUnixStream>>,
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

async fn spawn_ping(options: ping::Options) -> Result<Duration, String> {
    // delay ping by one sec to increase success rate
    time::sleep(Duration::from_secs(1)).await;
    ping::ping(&options).await.map_err(|e| {
        tracing::debug!(error = ?e, "ping error");
        e.to_string()
    })
}

fn setup_logging(
    log_file: &Option<std::path::PathBuf>,
    worker: &worker::Worker,
) -> Result<Option<logging::LogReloadHandle>, exitcode::ExitCode> {
    match log_file {
        Some(log_path) => {
            if let Some(parent) = log_path.parent() {
                dirs::ensure_dir(parent.to_path_buf(), 0o755, worker.uid, worker.gid).map_err(|err| {
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

impl DaemonState {
    fn new(
        worker_user: worker::Worker,
        config: Config,
        config_path: PathBuf,
        worker_params: WorkerParams,
        reload_handle: Option<LogReloadHandle>,
        log_file: Option<PathBuf>,
        keep_alive_instruction_sender: mpsc::Sender<KeepAliveInstruction>,
    ) -> Self {
        Self {
            config,
            config_path,
            incoming_worker_channel: mpsc::channel(32),
            log_file,
            pending_response_counter: 0,
            pending_responses: HashMap::new(),
            ping_tasks: JoinSet::new(),
            reload_handle,
            router: None,
            shutdown_ongoing: Shutdown::None,
            target_dest_id: None,
            worker_child: None,
            worker_exit_channel: mpsc::channel(1),
            worker_params,
            worker_user,
            keep_alive_instruction_sender,
        }
    }

    async fn daemon_loop(
        &mut self,
        mut signal_receiver: mpsc::Receiver<SignalMessage>,
        mut socket_listener: mpsc::Receiver<SocketCmd>,
        mut config_receiver: mpsc::Receiver<()>,
        mut keep_alive_expired: mpsc::Receiver<()>,
    ) -> Result<(), exitcode::ExitCode> {
        tracing::info!("entering root main loop");
        loop {
            tokio::select! {
                Some(signal) = signal_receiver.recv() => self.incoming_signal(signal).await?,
                Some(cmd) = socket_listener.recv() => self.incoming_socket_command(cmd).await?,
                Some(_) = config_receiver.recv() => self.incoming_config_change().await?,
                Some(res) = self.ping_tasks.join_next() =>  match res {
                        Ok(res) => self.outgoing_response_from_root(ResponseFromRoot::Ping { res }).await?,
                        Err(err) => tracing::error!(error = ?err, "ping task join error"),
                },
                Some(line) = self.incoming_worker_channel.1.recv() => self.incoming_worker_line(line).await?,
                Some(res) = self.worker_exit_channel.1.recv() => self.incoming_worker_exit(res).await?,
                Some(_) = keep_alive_expired.recv() => self.keep_alive_expired().await?,
                else => {
                    tracing::error!("unexpected channel closure");
                    return Err(exitcode::IOERR);
                }
            }
        }
    }

    async fn incoming_signal(&mut self, signal: SignalMessage) -> Result<(), exitcode::ExitCode> {
        match signal {
            SignalMessage::Shutdown => match self.shutdown_ongoing {
                Shutdown::None => {
                    tracing::info!("received shutdown signal - initiating shutdown");
                    self.shutdown_ongoing = Shutdown::Service;
                    if let Some(ref mut child) = self.worker_child {
                        tracing::debug!("sending shutdown signal to worker process");
                        send_to_worker(RootToWorker::Shutdown, &mut child.socket_writer).await?;
                        self.cleanup_worker_resources().await;
                        Ok(())
                    } else {
                        tracing::debug!("no worker process active - shutdown immediately");
                        Err(exitcode::OK)
                    }
                }
                Shutdown::Worker => {
                    tracing::info!(
                        "received shutdown signal but worker already shutting down - escalate to service shutdown"
                    );
                    self.shutdown_ongoing = Shutdown::Service;
                    Ok(())
                }
                Shutdown::Service => {
                    tracing::warn!(
                        "received shutdown signal but service shutdown already ongoing - forcing immediate exit"
                    );
                    Err(exitcode::OK)
                }
                Shutdown::RestartWorker => {
                    tracing::info!(
                        "received shutdown signal but worker restart already ongoing - escalate to service shutdown"
                    );
                    self.shutdown_ongoing = Shutdown::Service;
                    Ok(())
                }
            },
            SignalMessage::RotateLogs => {
                if let (Some(handle), Some(path)) = (&self.reload_handle, &self.log_file) {
                    let res = logging::make_file_fmt_layer(&path.to_string_lossy(), &self.worker_user)
                        .map(|new_layer| handle.reload(new_layer));
                    match res {
                        Ok(_) => {
                            tracing::info!("successfully reloaded logging layer with new log file after SIGHUP");
                            if matches!(self.shutdown_ongoing, Shutdown::None)
                                && let Some(ref mut child) = self.worker_child
                            {
                                tracing::debug!("sending rotate logs to worker process");
                                send_to_worker(RootToWorker::RotateLogs, &mut child.socket_writer).await?;
                            }
                            Ok(())
                        }
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
        let SocketCmd { cmd, resp } = socket_cmd;
        match WorkerCommand::try_from(cmd.clone()) {
            Ok(w_cmd) => {
                self.handle_hybrid_cmd(&w_cmd);
                if matches!(self.shutdown_ongoing, Shutdown::None)
                    && let Some(ref mut child) = self.worker_child
                {
                    self.pending_response_counter += 1;
                    self.pending_responses.insert(self.pending_response_counter, resp);
                    let msg = RootToWorker::WorkerCommand {
                        cmd: w_cmd,
                        id: self.pending_response_counter,
                    };
                    send_to_worker(msg, &mut child.socket_writer).await?;
                    let _ = self
                        .keep_alive_instruction_sender
                        .send(KeepAliveInstruction::Reset)
                        .await;
                    Ok(())
                } else {
                    let _ = resp.send(Response::WorkerOffline).map_err(|error| {
                        tracing::error!(?error, "socket command response channel closed");
                    });
                    Ok(())
                }
            }
            Err(_) => {
                let response = self.incoming_root_command(cmd).await?;
                let _ = resp.send(response).map_err(|error| {
                    tracing::error!(?error, "socket command response channel closed");
                });
                Ok(())
            }
        }
    }

    async fn incoming_config_change(&mut self) -> Result<(), exitcode::ExitCode> {
        tracing::info!("configuration file change detected - reloading configuration");

        match config::read(self.config_path.as_path()).await {
            Ok(new_config) => {
                self.config = new_config;
                if matches!(self.shutdown_ongoing, Shutdown::None)
                    && let Some(ref mut child) = self.worker_child
                {
                    tracing::debug!("sending shutdown signal to worker process due to config reload");
                    self.shutdown_ongoing = Shutdown::RestartWorker;
                    send_to_worker(RootToWorker::Shutdown, &mut child.socket_writer).await?;
                    self.cleanup_worker_resources().await;
                }
            }
            Err(err) => {
                tracing::error!(error = ?err, "unable to read updated configuration file - ignoring change");
            }
        }
        Ok(())
    }

    async fn outgoing_response_from_root(&mut self, resp: ResponseFromRoot) -> Result<(), exitcode::ExitCode> {
        if matches!(self.shutdown_ongoing, Shutdown::None)
            && let Some(ref mut child) = self.worker_child
        {
            let msg = RootToWorker::ResponseFromRoot(resp);
            send_to_worker(msg, &mut child.socket_writer).await
        } else {
            tracing::warn!(
                ?resp,
                "received response from root but no active worker process - ignoring"
            );
            Ok(())
        }
    }

    async fn incoming_root_command(&mut self, cmd: LibCommand) -> Result<Response, exitcode::ExitCode> {
        match cmd {
            LibCommand::Status
            | LibCommand::NerdStats
            | LibCommand::Connect(_)
            | LibCommand::Disconnect
            | LibCommand::Balance
            | LibCommand::FundingTool(_)
            | LibCommand::Telemetry
            | LibCommand::RefreshNode => Ok(Response::WorkerOffline),
            LibCommand::Ping => Ok(Response::Pong),
            LibCommand::Info => {
                let info = command::InfoResponse {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    log_file: self.log_file.clone(),
                };
                Ok(Response::Info(info))
            }

            LibCommand::StartClient(keepalive) => match (self.shutdown_ongoing, &self.worker_child) {
                (Shutdown::None, Some(_)) => {
                    let _ = self
                        .keep_alive_instruction_sender
                        .send(KeepAliveInstruction::Ignite(keepalive))
                        .await;
                    Ok(Response::StartClient(command::StartClientResponse::AlreadyRunning))
                }
                (Shutdown::None, None) => {
                    self.setup_worker().await?;
                    let _ = self
                        .keep_alive_instruction_sender
                        .send(KeepAliveInstruction::Ignite(keepalive))
                        .await;
                    Ok(Response::StartClient(command::StartClientResponse::Started))
                }
                (Shutdown::Worker, _) => {
                    // escalate to restart
                    tracing::debug!("received start client command during worker shutdown - escalating to restart");
                    self.shutdown_ongoing = Shutdown::RestartWorker;
                    let _ = self
                        .keep_alive_instruction_sender
                        .send(KeepAliveInstruction::Ignite(keepalive))
                        .await;
                    Ok(Response::StartClient(command::StartClientResponse::Started))
                }
                (Shutdown::RestartWorker, _) => {
                    // ignore already restarting
                    tracing::debug!("received start client command during worker restart - ignoring");
                    let _ = self
                        .keep_alive_instruction_sender
                        .send(KeepAliveInstruction::Ignite(keepalive))
                        .await;
                    Ok(Response::StartClient(command::StartClientResponse::Started))
                }
                (Shutdown::Service, _) => {
                    tracing::warn!("received start client command during service shutdown - cannot start client");
                    Err(exitcode::TEMPFAIL)
                }
            },

            LibCommand::StopClient => match (self.shutdown_ongoing, &mut self.worker_child) {
                (Shutdown::None, None) => Ok(Response::StopClient(command::StopClientResponse::NotRunning)),
                (Shutdown::None, Some(child)) => {
                    tracing::debug!("sending shutdown signal to worker process due to stop client command");
                    self.shutdown_ongoing = Shutdown::Worker;
                    send_to_worker(RootToWorker::Shutdown, &mut child.socket_writer).await?;
                    self.cleanup_worker_resources().await;
                    self.target_dest_id = None;
                    Ok(Response::StopClient(command::StopClientResponse::Stopped))
                }
                (Shutdown::Worker, _) => {
                    // ignore already stopping
                    tracing::debug!("received stop client command during worker shutdown - ignoring");
                    Ok(Response::StopClient(command::StopClientResponse::Stopped))
                }
                (Shutdown::RestartWorker, _) => {
                    // cancel worker restart and keep it stopped
                    tracing::debug!("received stop client command during worker restart - cancelling restart");
                    self.shutdown_ongoing = Shutdown::Worker;
                    self.target_dest_id = None;
                    Ok(Response::StopClient(command::StopClientResponse::Stopped))
                }
                (Shutdown::Service, _) => {
                    tracing::warn!("received stop client command during service shutdown - cannot stop client");
                    Err(exitcode::TEMPFAIL)
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
            WorkerToRoot::Response { id, resp } => self.incoming_worker_response(id, resp).await,
            WorkerToRoot::RequestToRoot(request) => self.incoming_worker_request(request).await,
        }
    }

    async fn incoming_worker_response(&mut self, id: u64, resp: Response) -> Result<(), exitcode::ExitCode> {
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

    async fn incoming_worker_request(&mut self, request: RequestToRoot) -> Result<(), exitcode::ExitCode> {
        tracing::debug!(?request, "received worker request to root");
        match request {
            RequestToRoot::DynamicWgRouting { wg_data } => {
                let res = self.setup_dynamic_routing(wg_data).await;
                if matches!(self.shutdown_ongoing, Shutdown::None)
                    && let Some(ref mut child) = self.worker_child
                {
                    send_to_worker(
                        RootToWorker::ResponseFromRoot(ResponseFromRoot::DynamicWgRouting { res }),
                        &mut child.socket_writer,
                    )
                    .await?;
                }
                Ok(())
            }
            RequestToRoot::StaticWgRouting { wg_data, peer_ips } => {
                let res = self.setup_static_routing(wg_data, peer_ips).await;
                if matches!(self.shutdown_ongoing, Shutdown::None)
                    && let Some(ref mut child) = self.worker_child
                {
                    send_to_worker(
                        RootToWorker::ResponseFromRoot(ResponseFromRoot::StaticWgRouting { res }),
                        &mut child.socket_writer,
                    )
                    .await?;
                }
                Ok(())
            }
            RequestToRoot::TearDownWg => {
                self.teardown_any_routing().await;
                Ok(())
            }
            RequestToRoot::Ping { options } => {
                self.ping_tasks.spawn(async move { spawn_ping(options).await });
                Ok(())
            }
        }
    }

    async fn incoming_worker_exit(&mut self, status: process::ExitStatus) -> Result<(), exitcode::ExitCode> {
        self.worker_child = None;
        match self.shutdown_ongoing {
            Shutdown::None => {
                if status.success() {
                    tracing::warn!("worker process exited cleanly without shutdown signal - restarting");
                    self.setup_worker().await?;
                    let _ = self
                        .keep_alive_instruction_sender
                        .send(KeepAliveInstruction::Reset)
                        .await;
                    Ok(())
                } else {
                    tracing::error!(status = ?status.code(), "worker process exited unexpectedly");
                    Err(exitcode::TEMPFAIL)
                }
            }
            Shutdown::Worker => {
                if status.success() {
                    tracing::info!("worker exited cleanly as requested");
                } else {
                    tracing::warn!(status = ?status.code(), "worker exited with error during requested shutdown");
                }
                self.shutdown_ongoing = Shutdown::None;
                Ok(())
            }
            Shutdown::Service => {
                if status.success() {
                    tracing::info!("worker exited cleanly on service shutdown");
                    Err(exitcode::OK)
                } else {
                    tracing::error!(status = ?status.code(), "worker exited with error during service shutdown");
                    Err(exitcode::IOERR)
                }
            }
            Shutdown::RestartWorker => {
                if status.success() {
                    tracing::info!("worker exited cleanly before restart");
                } else {
                    tracing::warn!(status = ?status.code(), "worker exited with error before restart");
                }
                self.shutdown_ongoing = Shutdown::None;
                self.setup_worker().await?;
                let _ = self
                    .keep_alive_instruction_sender
                    .send(KeepAliveInstruction::Reset)
                    .await;
                Ok(())
            }
        }
    }

    async fn keep_alive_expired(&mut self) -> Result<(), exitcode::ExitCode> {
        tracing::info!("keepalive timer expired - shutting down worker process");
        if matches!(self.shutdown_ongoing, Shutdown::None)
            && let Some(ref mut child) = self.worker_child
        {
            self.shutdown_ongoing = Shutdown::Worker;
            send_to_worker(RootToWorker::Shutdown, &mut child.socket_writer).await?;
            self.cleanup_worker_resources().await;
            self.target_dest_id = None;
        }
        Ok(())
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

        let mut worker_command = TokioCommand::new(self.worker_user.binary.clone());

        worker_command
            .current_dir(self.worker_user.home.clone())
            .env(socket::worker::ENV_VAR, format!("{}", child_socket.into_raw_fd()))
            .env("HOPR_INTERNAL_MIXER_MINIMUM_DELAY_IN_MS", "0") // the client does not want to mix
            .env("HOPR_INTERNAL_MIXER_DELAY_RANGE_IN_MS", "1") // the mix range must be minimal to retain the QoS of the client
            .uid(self.worker_user.uid)
            .gid(self.worker_user.gid);

        if let Some(ref log_file) = self.log_file {
            worker_command.env(logging::ENV_VAR_LOG_FILE, log_file.to_string_lossy().to_string());
        }
        let mut child = worker_command.spawn().map_err(|err| {
            tracing::error!(error = ?err, ?self.worker_user, "unable to spawn worker process");
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
        send_to_worker(
            RootToWorker::StartupParams {
                config: self.config.clone(),
                worker_params: self.worker_params.clone(),
                target_dest_id: self.target_dest_id.clone(),
            },
            &mut socket_writer,
        )
        .await?;

        let cancel = CancellationToken::new();
        let owned_cancel = cancel.clone();
        let lines_sender = self.incoming_worker_channel.0.clone();
        let exit_sender = self.worker_exit_channel.0.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Ok(Some(line)) = socket_lines_reader.next_line() => {
                        let _ = lines_sender.send(line.clone()).await.map_err(|err| {
                            tracing::error!(error = ?err, "worker channel receiver dropped");
                        });
                    },
                    Ok(status) = child.wait() => {
                        let _ = exit_sender.send(status).await.map_err(|err| {
                            tracing::error!(error = ?err, "worker exit channel receiver dropped");
                        });
                        break;
                    },
                    _ = owned_cancel.cancelled() => {
                        tracing::debug!("worker command listener received cancellation");
                        break;
                    }
                    else => {
                        tracing::warn!("worker streams closed");
                        break;
                    }
                }
            }
        });

        self.worker_child = Some(WorkerChild { cancel, socket_writer });
        Ok(())
    }

    async fn teardown_any_routing(&mut self) {
        if let Some(ref mut router) = self.router {
            router.teardown(Logs::Print).await;
        }
        self.router = None;
    }

    /// Remove routing and stop ping tasks
    async fn teardown(&mut self) {
        self.cleanup_worker_resources().await;
        if let Some(ref mut child) = self.worker_child {
            child.cancel.cancel();
        }
    }

    async fn cleanup_worker_resources(&mut self) {
        self.ping_tasks.shutdown().await;
        self.teardown_any_routing().await;
        self.pending_responses.clear();
        let _ = self
            .keep_alive_instruction_sender
            .send(KeepAliveInstruction::Stop)
            .await;
    }

    async fn setup_dynamic_routing(&mut self, wg_data: event::WireGuardData) -> Result<(), String> {
        // ensure clean slate
        self.teardown_any_routing().await;

        let state_home = self.worker_params.state_home();
        let worker_user = self.worker_user.clone();
        let res_router = routing::dynamic_router(state_home, worker_user, wg_data).await;
        match res_router {
            Ok(mut router) => {
                let res_setup = router.setup().await;
                self.router = Some(Box::new(router));
                match res_setup {
                    Ok(_) => {
                        tracing::info!("dynamic routing setup successfully");
                        Ok(())
                    }
                    Err(error) => {
                        tracing::error!(?error, "dynamic routing setup error");
                        self.teardown_any_routing().await;
                        Err(error.to_string())
                    }
                }
            }
            Err(error) => {
                tracing::error!(?error, "failed to build dynamic router");
                Err(error.to_string())
            }
        }
    }

    async fn setup_static_routing(
        &mut self,
        wg_data: event::WireGuardData,
        peer_ips: Vec<Ipv4Addr>,
    ) -> Result<(), String> {
        // ensure clean slate
        self.teardown_any_routing().await;

        let state_home = self.worker_params.state_home();
        let res_router = routing::static_router(state_home, wg_data, peer_ips);
        match res_router {
            Ok(mut router) => {
                let res_setup = router.setup().await;
                self.router = Some(Box::new(router));
                match res_setup {
                    Ok(_) => {
                        tracing::info!("static routing setup successfully");
                        Ok(())
                    }
                    Err(error) => {
                        tracing::error!(?error, "static routing setup error");
                        self.teardown_any_routing().await;
                        Err(error.to_string())
                    }
                }
            }
            Err(error) => {
                tracing::error!(?error, "failed to build static router");
                Err(error.to_string())
            }
        }
    }

    fn handle_hybrid_cmd(&mut self, cmd: &WorkerCommand) {
        match cmd {
            WorkerCommand::Connect(id) => {
                tracing::debug!(?id, "remembering target destination from connect command");
                self.target_dest_id = Some(id.clone());
            }
            WorkerCommand::Disconnect => {
                tracing::debug!("clearing target destination from disconnect command");
                self.target_dest_id = None;
            }
            _ => (),
        }
    }
}
