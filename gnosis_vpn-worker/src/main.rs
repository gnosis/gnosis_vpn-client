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
use gnosis_vpn_lib::event::{CoreToWorker, ResponseFromRoot, RootToWorker, WorkerToCore, WorkerToRoot};
use gnosis_vpn_lib::hopr::hopr_lib;
use gnosis_vpn_lib::{command, config, logging, socket, worker_params};

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
}

struct LoggingHandle {
    reload_handle: logging::LogReloadHandle,
    log_path: std::path::PathBuf,
}

struct State {
    core_handle: Option<CoreHandle>,
    log_handle: Option<LoggingHandle>,
}

enum IncomingResolution {
    Shutdown(exitcode::ExitCode),
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
    pub async fn incoming_command(
        &mut self,
        cmd: RootToWorker,
        sender_from_core: &mpsc::Sender<CoreToWorker>,
    ) -> IncomingResolution {
        match cmd {
            RootToWorker::Shutdown => {
                return self.cmd_shutdown().await;
            }
            RootToWorker::RotateLogs => {
                return self.cmd_rotate_logs().await;
            }
            RootToWorker::StartupParams { config, worker_params } => {
                return self.cmd_startup_params(config, worker_params, sender_from_core).await;
            }
            RootToWorker::WorkerCommand { cmd, id } => {
                return self.cmd_worker_command(cmd, id).await;
            }
            RootToWorker::ResponseFromRoot(response) => {
                return self.cmd_response_from_root(response).await;
            }
        }
    }

    async fn cmd_shutdown(&mut self) -> IncomingResolution {
        tracing::info!("received shutdown command from root");
        if let Some(core) = &mut self.core_handle {
            let _ = core.sender_to_core.send(WorkerToCore::Shutdown).await;
            return IncomingResolution::SustainLoop;
        }
        tracing::debug!("core not yet started");
        return IncomingResolution::Shutdown(exitcode::OK);
    }

    async fn cmd_rotate_logs(&mut self) -> IncomingResolution {
        let log_handle = match &self.log_handle {
            Some(handle) => handle,
            None => {
                tracing::warn!("received rotate logs command from root but no log file configured - ignoring");
                return IncomingResolution::SustainLoop;
            }
        };
        tracing::info!("received rotate logs command from root");
        let res = logging::use_file_fmt_layer(&log_handle.log_path.to_string_lossy())
            .map(|new_layer| log_handle.reload_handle.reload(new_layer));
        match res {
            Ok(_) => {
                tracing::info!("successfully reloaded logging layer with new log file after SIGHUP");
                return IncomingResolution::SustainLoop;
            }
            Err(e) => {
                eprintln!("failed to reopen log file {:?}: {}", log_handle.log_path, e);
                return IncomingResolution::Shutdown(exitcode::IOERR);
            }
        }
    }

    async fn cmd_startup_params(
        &mut self,
        config: config::Config,
        worker_params: worker_params::WorkerParams,
        sender_from_core: &mpsc::Sender<CoreToWorker>,
    ) -> IncomingResolution {
        tracing::debug!(?config, ?worker_params, "received startup params from root");
        if let Some(_) = &self.core_handle {
            tracing::warn!("core already initialized - ignoring startup params");
            return IncomingResolution::SustainLoop;
        }
        let res_core = Core::init(config, worker_params, sender_from_core.clone()).await;
        match res_core {
            Ok((core, worker_to_core_sender)) => {
                let mut task = JoinSet::new();
                task.spawn(async move { core.start().await });
                let ch = CoreHandle {
                    task,
                    sender_to_core: worker_to_core_sender,
                };
                self.core_handle = Some(ch);
                tracing::info!("core logic initialized and started");
                return IncomingResolution::SustainLoop;
            }
            Err(err) => {
                tracing::error!(error = ?err, "failed to initialize core logic");
                return IncomingResolution::Shutdown(exitcode::OSERR);
            }
        }
    }

    async fn cmd_worker_command(&mut self, cmd: command::WorkerCommand, id: u64) -> IncomingResolution {
        tracing::debug!(?cmd, id, "received command from root");
        let (sender, recv) = oneshot::channel();
        let core_handle = match self.core_handle {
            Some(ref handle) => handle,
            None => {
                tracing::warn!(
                    ?cmd,
                    id,
                    "received worker command from root but core not yet initialized - ignoring"
                );
                return IncomingResolution::SustainLoop;
            }
        };
        let _ = core_handle
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

    async fn cmd_response_from_root(&mut self, response: ResponseFromRoot) -> IncomingResolution {
        tracing::debug!(?response, "received command from root");
        let core_handle = match self.core_handle {
            Some(ref handle) => handle,
            None => {
                tracing::warn!(
                    ?response,
                    "received response from root but core not yet initialized - ignoring"
                );
                return IncomingResolution::SustainLoop;
            }
        };
        let _ = core_handle
            .sender_to_core
            .send(WorkerToCore::ResponseFromRoot(response))
            .await;
        return IncomingResolution::SustainLoop;
    }
}

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    // Set up logging
    let log_handle = setup_logging(&args.log_file)?;
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

    // enter main loop
    let mut state = State {
        core_handle: None,
        log_handle,
    };
    tracing::info!("enter listening mode");
    let (sender_from_core, mut receiver_from_core) = mpsc::channel::<CoreToWorker>(32);
    loop {
        tokio::select! {
            Some(cmd) = socket_receiver.recv() => match state.incoming_command(cmd, &sender_from_core).await {
                IncomingResolution::Shutdown(code) => {
                    tracing::info!(?code, "shutting down worker daemon before core loop initialization");
                    cancel_signal_swallower.cancel();
                    cancel_socket_reader.cancel();
                    return Err(code);
                }
                IncomingResolution::Response(resp) => {
                    tracing::debug!(?resp, "sending response to root");
                    send_to_root(resp, &mut writer).await?;
                }
                IncomingResolution::SustainLoop => {}
            },
            Some(event) = receiver_from_core.recv() => match event {
                CoreToWorker::RequestToRoot(req) => {
                    tracing::debug!(?req, "incoming request to root from core");
                    send_to_root(WorkerToRoot::RequestToRoot(req), &mut writer).await?;
                }
            },
            Some(_) = state.core_handle.as_mut().map(|h| h.task.join_next()).unwrap(), if state.core_handle.is_some() => {
                tracing::info!("shutting down worker daemon after core loop completion");
                return Ok(());
            },
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

fn setup_logging(log_file: &Option<std::path::PathBuf>) -> Result<Option<LoggingHandle>, exitcode::ExitCode> {
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
            let lh = LoggingHandle {
                reload_handle: handle,
                log_path: log_path.clone(),
            };
            Ok(Some(lh))
        }
        None => {
            logging::setup_stdout();
            Ok(None)
        }
    }
}
