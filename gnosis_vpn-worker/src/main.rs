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
// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

struct LoggingHandle {
    reload_handle: logging::LogReloadHandle,
    log_path: std::path::PathBuf,
}

struct State {
    log_handle: Option<LoggingHandle>,
    core_task: JoinSet<()>,
    core_cancel: CancellationToken,
    root_socket_writer: BufWriter<WriteHalf<TokioUnixStream>>,
}

enum IncomingResolution {
    ResponseToCore(Box<ResponseFromRoot>),
    RoundtripViaCore(Box<(command::WorkerCommand, u64)>),
    Shutdown(exitcode::ExitCode),
    ShutdownToCore,
    SustainLoop,
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
    let (cancel_socket_reader, socket_receiver, writer_half) = incoming_socket().await?;
    let writer = BufWriter::new(writer_half);

    // enter main loop
    let mut state = State::new(log_handle, writer);
    let res = state.daemon_loop(socket_receiver).await;

    // cancel running tasks and run teardown logic
    state.teardown().await;
    cancel_signal_swallower.cancel();
    cancel_socket_reader.cancel();

    res
}

async fn send_to_root(
    resp: Box<WorkerToRoot>,
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

impl State {
    pub fn new(log_handle: Option<LoggingHandle>, root_socket_writer: BufWriter<WriteHalf<TokioUnixStream>>) -> Self {
        Self {
            log_handle,
            core_task: JoinSet::new(),
            core_cancel: CancellationToken::new(),
            root_socket_writer,
        }
    }

    pub async fn incoming_command(
        &mut self,
        cmd: RootToWorker,
        worker_to_core_receiver_wrapper: &mut Option<mpsc::Receiver<WorkerToCore>>,
        core_to_worker_sender: mpsc::Sender<CoreToWorker>,
    ) -> IncomingResolution {
        match cmd {
            RootToWorker::Shutdown => self.cmd_shutdown().await,
            RootToWorker::RotateLogs => self.cmd_rotate_logs().await,
            RootToWorker::StartupParams {
                config,
                worker_params,
                target_dest_id,
            } => {
                self.cmd_startup_params(
                    config,
                    worker_params,
                    worker_to_core_receiver_wrapper,
                    core_to_worker_sender,
                )
                .await
            }
            RootToWorker::WorkerCommand { cmd, id } => {
                tracing::debug!(?cmd, id, "received command from root");
                IncomingResolution::RoundtripViaCore(Box::new((cmd, id)))
            }
            RootToWorker::ResponseFromRoot(response) => {
                tracing::debug!(?response, "received response from root");
                IncomingResolution::ResponseToCore(Box::new(response))
            }
        }
    }

    async fn cmd_shutdown(&self) -> IncomingResolution {
        if self.core_task.is_empty() {
            tracing::info!("received shutdown command from root but core loop not yet initialized");
            IncomingResolution::Shutdown(exitcode::OK)
        } else {
            tracing::info!("received shutdown command from root - shutting down core loop");
            IncomingResolution::ShutdownToCore
        }
    }

    async fn cmd_rotate_logs(&self) -> IncomingResolution {
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
                IncomingResolution::SustainLoop
            }
            Err(e) => {
                eprintln!("failed to reopen log file {:?}: {}", log_handle.log_path, e);
                IncomingResolution::Shutdown(exitcode::IOERR)
            }
        }
    }

    async fn cmd_startup_params(
        &mut self,
        config: config::Config,
        worker_params: worker_params::WorkerParams,
        worker_to_core_receiver_wrapper: &mut Option<mpsc::Receiver<WorkerToCore>>,
        core_to_worker_sender: mpsc::Sender<CoreToWorker>,
    ) -> IncomingResolution {
        if !self.core_task.is_empty() {
            tracing::warn!("core already initialized - ignoring startup params");
            return IncomingResolution::SustainLoop;
        }
        tracing::debug!(?config, ?worker_params, "received startup params from root");
        let (sender, mut core_to_worker_receiver) = mpsc::channel(32);
        let res_core = Core::init(config, worker_params, sender).await;
        match (res_core, worker_to_core_receiver_wrapper.take()) {
            (Ok((core, worker_to_core_sender)), Some(mut worker_to_core_receiver)) => {
                self.core_task.spawn(async move { core.start().await });
                let owned_cancel = self.core_cancel.clone();
                // set up message forwarding to work around lifetime ownership of receivers
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            Some(cmd) = worker_to_core_receiver.recv() => {
                                let _ = worker_to_core_sender.send(cmd).await;
                            },
                            Some(cmd) = core_to_worker_receiver.recv() => {
                                let _ = core_to_worker_sender.send(cmd).await;
                            },
                            _ = owned_cancel.cancelled() => {
                                tracing::debug!("worker-core channel forwarding task received cancellation");
                                break;
                            },
                            else => {
                                tracing::warn!("worker-core channel forwarding task closed");
                                break;
                            }
                        }
                    }
                });
                tracing::info!("core logic initialized and started");
                IncomingResolution::SustainLoop
            }
            (Ok(_), None) => {
                tracing::error!("failed to initialize core logic - exhausted worker-to-core channel");
                IncomingResolution::Shutdown(exitcode::SOFTWARE)
            }
            (Err(err), _) => {
                tracing::error!(error = ?err, "failed to initialize core logic");
                IncomingResolution::Shutdown(exitcode::OSERR)
            }
        }
    }

    async fn daemon_loop(
        &mut self,
        mut socket_receiver: mpsc::Receiver<RootToWorker>,
    ) -> Result<(), exitcode::ExitCode> {
        tracing::info!("entering worker main loop");
        let (worker_to_core_sender, worker_to_core_receiver) = mpsc::channel::<WorkerToCore>(32);
        let (core_to_worker_sender, mut core_to_worker_receiver) = mpsc::channel::<CoreToWorker>(32);
        let mut worker_to_core_receiver_wrapper = Some(worker_to_core_receiver);
        loop {
            tokio::select! {
                Some(cmd) = socket_receiver.recv() => match self.incoming_command(cmd, &mut worker_to_core_receiver_wrapper, core_to_worker_sender.clone()).await {
                    IncomingResolution::ResponseToCore(resp) => {
                        let _ = worker_to_core_sender.send(WorkerToCore::ResponseFromRoot(*resp)).await;
                    }
                    IncomingResolution::RoundtripViaCore(roundtrip) => {
                        let (cmd, id) = *roundtrip;
                        let (resp_sender, resp_recv) = oneshot::channel();
                        let _ = worker_to_core_sender.send(WorkerToCore::WorkerCommand { cmd, resp: resp_sender }).await;
                        let res_recv = resp_recv.await;
                        match res_recv {
                            Ok(resp) => {
                                send_to_root(Box::new(WorkerToRoot::Response { id, resp }), &mut self.root_socket_writer).await?;
                            }
                            Err(err) => {
                                tracing::warn!(error = ?err, "core-to-worker receiver unexepectedly closed while awaiting response for command from root");
                            }
                        }
                    }
                    IncomingResolution::Shutdown(code) => {
                        tracing::info!(?code, "shutting down worker daemon before core loop initialization");
                        return Err(code);
                    }
                    IncomingResolution::ShutdownToCore => {
                        let _ = worker_to_core_sender.send(WorkerToCore::Shutdown).await;
                    }
                    IncomingResolution::SustainLoop => {}
                },
                Some(event) = core_to_worker_receiver.recv() => match event {
                    CoreToWorker::RequestToRoot(req) => {
                        tracing::debug!(?req, "incoming request to root from core");
                        send_to_root(Box::new(WorkerToRoot::RequestToRoot(req)), &mut self.root_socket_writer).await?;
                    }
                },
                Some(_) = self.core_task.join_next() => {
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

    async fn teardown(&mut self) {
        // should be already empty from main loop drainage
        self.core_task.shutdown().await;
    }
}
