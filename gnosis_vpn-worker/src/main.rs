use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::UnixStream as TokioUnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use std::env;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream;
use std::process;

use gnosis_vpn_lib::core::Core;
use gnosis_vpn_lib::event::{CoreToWorker, RootToWorker, WorkerToCore, WorkerToRoot};
use gnosis_vpn_lib::hopr::hopr_lib;
use gnosis_vpn_lib::logging;
use gnosis_vpn_lib::socket;

mod cli;
mod init;
// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct CoreHandle {
    task: JoinSet<()>,
    sender_to_core: mpsc::Sender<WorkerToCore>,
    receiver_from_core: mpsc::Receiver<CoreToWorker>,
}

struct State {
    core_handle: Option<CoreHandle>,
    sender_to_core: Option<mpsc::Sender<WorkerToCore>>,
    receiver_from_core: Option<mpsc::Receiver<CoreToWorker>>,
    reload_handle: logging::LogReloadHandle,
    log_path: Option<std::path::PathBuf>,
}

enum IncomingResolution {
    Shutdown(exitcode::ExitCode),
    ShutdownWaitingForCore,
    SustainLoop,
    Response(WorkerToRoot),
}

async fn signal_swallower() -> Result<CancellationToken, exitcode::ExitCode> {
    let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
        tracing::error!(error = ?e, "error setting up SIGINT handler");
        exitcode::IOERR
    })?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
        tracing::error!(error = ?e, "error setting up SIGTERM handler");
        exitcode::IOERR
    })?;
    let mut sighup = signal(SignalKind::hangup()).map_err(|e| {
        tracing::error!(error = ?e, "error setting up SIGHUP handler");
        exitcode::IOERR
    })?;

    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(_) = sigint.recv() => {
                    tracing::debug!("swallowed SIGINT");
                },
                Some(_) = sigterm.recv() => {
                    tracing::debug!("swallowed SIGTERM");
                },
                Some(_) = sighup.recv() => {
                    tracing::debug!("swallowed SIGTERM");
                }
                _ = cancel.cancelled() => {
                    tracing::debug!("signal swallower received cancellation");
                    break;
                }
                else => {
                    tracing::warn!("signal swallower streams closed");
                    break;
                }
            }
        }
    });
    tracing::info!("signal handlers set up");
    Ok(owned_cancel)
}

async fn incoming_socket() -> Result<
    (
        CancellationToken,
        mpsc::Receiver<RootToWorker>,
        WriteHalf<TokioUnixStream>,
    ),
    exitcode::ExitCode,
> {
    // accessing unix socket from fd
    let fd: i32 = env::var(socket::worker::ENV_VAR)
        .map_err(|err| {
            tracing::error!(error = %err, env = %socket::worker::ENV_VAR, "missing worker env var");
            exitcode::NOINPUT
        })?
        .parse()
        .map_err(|err| {
            tracing::error!(error = %err, "invalid worker socket fd env var");
            exitcode::NOINPUT
        })?;

    let child_socket = unsafe { UnixStream::from_raw_fd(fd) };
    child_socket.set_nonblocking(true).map_err(|err| {
        tracing::error!(error = %err, "failed to set non-blocking mode on worker socket");
        exitcode::IOERR
    })?;
    let child_stream = TokioUnixStream::from_std(child_socket).map_err(|err| {
        tracing::error!(error = %err, "failed to create unix stream from socket");
        exitcode::IOERR
    })?;

    // splitting unix stream into reader and writer halves
    let (reader_half, writer_half) = io::split(child_stream);
    let reader = BufReader::new(reader_half);
    let mut lines_reader = reader.lines();

    let cancel = CancellationToken::new();
    let owned_cancel = cancel.clone();
    let (sender, receiver) = mpsc::channel(32);
    tokio::spawn(async move {
        loop {
            tokio::select! {
            Ok(Some(line)) = lines_reader.next_line() => {
                tracing::debug!(line = %line, "incoming from root service");
                let res_cmd = serde_json::from_str::<RootToWorker>(&line);
                match res_cmd {
                    Ok(cmd) => {
                        let _ = sender.send(cmd).await;
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "failed parsing incoming worker command - ignoring");
                    }
                }
            }
            _ = cancel.cancelled() => {
                tracing::debug!("socket reader received cancellation");
                break;
            }
            else => {
                tracing::warn!("socket reader stream closed");
                break;
            }
            }
        }
    });
    tracing::info!("socket reader set up");
    Ok((owned_cancel, receiver, writer_half))
}

impl State {
    async fn incoming_command(&mut self, cmd: RootToWorker) -> IncomingResolution {
        match cmd {
            RootToWorker::Shutdown => {
                tracing::info!("received shutdown command from root");
                if let Some(core) = &mut self.core_handle {
                    let _ = core.sender_to_core.send(WorkerToCore::Shutdown).await;
                    return IncomingResolution::ShutdownWaitingForCore;
                }
                tracing::debug!("core not yet started");
                return IncomingResolution::Shutdown(exitcode::OK);
            }
            RootToWorker::RotateLogs => {
                if let Some(log_path) = &self.log_path {
                    tracing::info!("received rotate logs command from root");
                    let res = logging::use_file_fmt_layer(&log_path.to_string_lossy())
                        .map(|new_layer| self.reload_handle.reload(new_layer));
                    match res {
                        Ok(_) => {
                            tracing::info!("successfully reloaded logging layer with new log file after SIGHUP");
                            return IncomingResolution::SustainLoop;
                        }
                        Err(e) => {
                            eprintln!("failed to reopen log file {:?}: {}", self.log_path, e);
                            return IncomingResolution::Shutdown(exitcode::IOERR);
                        }
                    }
                } else {
                    tracing::warn!("received rotate logs command from root but no log file configured - ignoring");
                    return IncomingResolution::SustainLoop;
                }
            }
            RootToWorker::StartupParams { config, worker_params } => {
                tracing::debug!(?config, ?worker_params, "received startup params from root");
                match &self.core_handle {
                    Some(core) => {}
                    None => {}
                }
                if !self.core_task.is_empty() {
                    tracing::warn!("core already initialized - ignoring startup params");
                    return IncomingResolution::SustainLoop;
                }
                let (core_to_worker_sender, core_to_worker_receiver) = mpsc::channel(32);
                let res_core = Core::init(config, worker_params, core_to_worker_sender).await;
                match res_core {
                    Ok((core, worker_to_core_sender)) => {
                        self.core_task.spawn(async move { core.start().await });
                        self.receiver_from_core = Some(core_to_worker_receiver);
                        self.sender_to_core = worker_to_core_sender;
                        tracing::info!("core logic initialized and started");
                        return IncomingResolution::SustainLoop;
                    }
                    Err(err) => {
                        tracing::error!(error = ?err, "failed to initialize core logic");
                        return IncomingResolution::Shutdown(exitcode::OSERR);
                    }
                }
            }
            RootToWorker::WorkerCommand { cmd, id } => {
                tracing::debug!(?cmd, id, "received command from root");
                let (sender, recv) = oneshot::channel();
                let _ = self
                    .sender_to_core
                    .send(WorkerToCore::WorkerCommand { cmd, resp: sender })
                    .await;
                let res_recv = recv.await;
                match res_recv {
                    Ok(resp) => {
                        return IncomingResolution::Response(WorkerToRoot::Response { id, resp });
                    }
                    Err(err) => {
                        tracing::warn!(error = ?err, "core-to-worker receiver unexepectedly closed");
                        return IncomingResolution::SustainLoop;
                    }
                }
            }
            RootToWorker::ResponseFromRoot(response) => {
                tracing::debug!(?response, "received response from root");
                self.sender_to_core.send(WorkerToCore::ResponseFromRoot(response)).await;
                return IncomingResolution::SustainLoop;
            }
        }
    }

    async fn incoming_event(&mut self, evt: CoreToWorker) {
        match evt {
            CoreToWorker::RequestToRoot(req) => {
                tracing::info!("core loop reported shutdown complete");
            }
            CoreToWorker::ResponseFromCore { response } => {
                tracing::debug!(?response, "received response from core to send to root");
                let res = send_to_root(
                    WorkerToRoot::Response {
                        id: response.id,
                        resp: response.resp,
                    },
                    &mut BufWriter::new(io::stdout()),
                )
                .await;
                if let Err(err) = res {
                    tracing::error!(error = ?err, "failed to send response to root");
                }
            }
        }
    }
}

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    // Set up logging
    let reload_handle = setup_logging(&args.log_file)?;
    let log_path = args.log_file;
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    // set up signal swallower
    let cancel_signal_swallower = signal_swallower().await?;

    // setup socket communication with root process
    let (cancel_socket_reader, mut socket_receiver, writer_half) = incoming_socket().await?;
    let mut writer = BufWriter::new(writer_half);

    let (mut incoming_event_sender, incoming_event_receiver) = mpsc::channel(32);

    // enter main loop
    let mut shutdown_ongoing = false;

    let state = State {
        core_task: JoinSet::new(),
        sender_to_core: incoming_event_sender,
        receiver_from_core: None,
        reload_handle,
        log_path,
    };
    tracing::info!("enter listening mode");
    loop {
        tokio::select! {
            Some(cmd) = socket_receiver.recv() => match state.incoming_command(cmd).await {
                IncomingResolution::Shutdown(code) => {
                    tracing::info!(?code, "shutting down worker daemon before core loop initialization");
                    cancel_signal_swallower.cancel();
                    cancel_socket_reader.cancel();
                    return Err(code);
                }
                IncomingResolution::ShutdownWaitingForCore => {
                    tracing::info!("waiting for core loop to finish before shutting down worker daemon");
                }
                IncomingResolution::Response(resp) => {
                    send_to_root(resp, &mut writer).await?;
                }
                IncomingResolution::SustainLoop => {}
            },
            Some(evt) = incoming_event_receiver.recv() => state.incoming_event(evt).await,
            Some(_) = state.core_task => {
                tracing::info!("shutting down worker daemon after core loop completion");
                return Ok(());
            }
            else => {
                tracing::error!("unexpected channel closure");
                return Err(exitcode::IOERR);
            }
        }
    }
}

async fn send_to_root(
    resp: WorkerToRoot,
    writer: &mut BufWriter<WriteHalf<TokioUnixStream>>,
) -> Result<(), exitcode::ExitCode> {
    let serialized = serde_json::to_string(&resp).map_err(|err| {
        tracing::error!(error = ?err, "failed to serialize response");
        exitcode::DATAERR
    })?;
    writer.write_all(serialized.as_bytes()).await.map_err(|err| {
        tracing::error!(error = ?err, "error writing to stdout");
        exitcode::IOERR
    })?;
    writer.write_all(b"\n").await.map_err(|err| {
        tracing::error!(error = ?err, "error appending newline to stdout");
        exitcode::IOERR
    })?;
    writer.flush().await.map_err(|err| {
        tracing::error!(error = ?err, "error flushing stdout");
        exitcode::IOERR
    })?;
    Ok(())
}

fn main() {
    match hopr_lib::prepare_tokio_runtime(None, None) {
        Ok(rt) => {
            rt.block_on(main_inner());
        }
        Err(e) => {
            eprintln!("error preparing tokio runtime: {}", e);
            process::exit(exitcode::IOERR);
        }
    }
}

async fn main_inner() {
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
) -> Result<Option<logging::LogReloadHandle>, exitcode::ExitCode> {
    match log_file {
        Some(log_path) => {
            let fmt_layer = logging::use_file_fmt_layer(&log_path.to_string_lossy()).map_err(|err| {
                eprintln!("Failed to open log layer for file {}: {}", log_path.display(), err);
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
