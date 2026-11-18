// The worker process keeps all descriptor ownership behind safe APIs.
#![deny(unsafe_code)]

use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::UnixStream as TokioUnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use std::os::unix::net::UnixStream;
use std::process;
use std::time::Duration;

use gnosis_vpn_ipc as ipc;
use gnosis_vpn_lib::core::{self, Core, startup_issue};
use gnosis_vpn_lib::event::{CoreToWorker, ResponseFromRoot, RootToWorker, WorkerToCore, WorkerToRoot};
use gnosis_vpn_lib::hopr::hopr_lib;
use gnosis_vpn_lib::internet_check::InternetCheck;
use gnosis_vpn_lib::{command, config, logging, socket, worker_params};

mod cli;
// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

struct State {
    core_task: JoinSet<()>,
    core_cancel: CancellationToken,
    root_socket_writer: BufWriter<WriteHalf<TokioUnixStream>>,
    pending_startup: Option<PendingStartup>,
    lookup_sender: mpsc::Sender<NetworkLookup>,
    lookup_receiver: mpsc::Receiver<NetworkLookup>,
}

/// Core init waits until Blokli names the network that keys the identity; status answers from here meanwhile.
struct PendingStartup {
    config: config::Config,
    /// Kept so a Disconnect can restart the lookup without the pin.
    spawn_params: worker_params::SpawnParams,
    target_dest_id: Option<String>,
    issue: Option<ipc::StartupIssue>,
    internet_check: InternetCheck,
    lookup: tokio::task::JoinHandle<()>,
}

enum NetworkLookup {
    Failed {
        kind: ipc::StartupIssueKind,
        message: String,
    },
    Found(Box<worker_params::WorkerParams>),
}

enum IncomingResolution {
    ResponseToCore(Box<ResponseFromRoot>),
    RoundtripViaCore(Box<(command::WorkerCommand, u64)>),
    ToRoot(Box<WorkerToRoot>),
    Shutdown(exitcode::ExitCode),
    ShutdownToCore,
    ForceReconnectToCore,
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
                    tracing::debug!("swallowed SIGHUP");
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

async fn incoming_socket(
    child_socket: UnixStream,
) -> Result<
    (
        CancellationToken,
        mpsc::Receiver<RootToWorker>,
        WriteHalf<TokioUnixStream>,
    ),
    exitcode::ExitCode,
> {
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
                read = lines_reader.next_line() => {
                    // Root gone: dropping the sender ends the daemon loop instead of retrying unsupervised.
                    let line = match read {
                        Ok(Some(line)) => line,
                        Ok(None) => {
                            tracing::warn!("root closed the control socket");
                            break;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "reading the control socket failed");
                            break;
                        }
                    };
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

/// How long to wait for root's TUN-fd socket bootstrap message. Root sends it
/// before spawning the worker, so it is already buffered on a healthy startup;
/// the timeout only turns a misbehaving parent into a clean exit instead of a hang.
const TUN_FD_SOCKET_TIMEOUT: Duration = Duration::from_secs(10);

/// Receive the dedicated TUN-fd passing socket from root and register it. The
/// connection runner pulls the TUN device fd from it once root reports the tunnel
/// is ready.
///
/// Root sends this socket's fd as the first message on the control socket (a
/// single `SCM_RIGHTS` datagram ahead of any JSON line), so this must run before
/// the control socket is handed to tokio: `recv_fd` consumes exactly the one-byte
/// fd message and leaves the JSON stream intact for the buffered reader. The
/// received socket stays in blocking mode on purpose: the runner receives TUN
/// device fds from a `spawn_blocking` task, and the underlying `recvmsg` is a
/// blocking call.
fn setup_tun_fd_socket(control_socket: &UnixStream) -> Result<(), exitcode::ExitCode> {
    control_socket
        .set_read_timeout(Some(TUN_FD_SOCKET_TIMEOUT))
        .map_err(|err| {
            tracing::error!(error = %err, "failed to set read timeout on control socket");
            exitcode::IOERR
        })?;
    let tun_fd_socket = socket::fd_passing::recv_fd(control_socket).map_err(|err| {
        tracing::error!(error = %err, "failed to receive TUN fd socket from root");
        exitcode::NOINPUT
    });
    control_socket.set_read_timeout(None).map_err(|err| {
        tracing::error!(error = %err, "failed to clear read timeout on control socket");
        exitcode::IOERR
    })?;
    let tun_fd_socket = tun_fd_socket?;
    socket::worker::set_tun_fd_socket(UnixStream::from(tun_fd_socket));
    tracing::info!("TUN fd passing socket set up");
    Ok(())
}

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    // Set up logging
    setup_logging(args.log_file.as_deref());
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    // set up signal swallower
    let cancel_signal_swallower = signal_swallower().await?;

    // claim the control socket root passed as stdin
    let control_socket = socket::worker::claim_stdin_socket().map_err(|err| {
        tracing::error!(error = %err, "failed to claim control socket from stdin");
        exitcode::NOINPUT
    })?;

    // receive the dedicated TUN-fd passing socket sent by root ahead of any
    // JSON traffic; must happen before the control socket is handed to tokio
    setup_tun_fd_socket(&control_socket)?;

    // setup socket communication with root process
    let (cancel_socket_reader, socket_receiver, writer_half) = incoming_socket(control_socket).await?;
    let writer = BufWriter::new(writer_half);

    // enter main loop
    let mut state = State::new(writer);
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
    match hopr_lib::prepare_tokio_runtime(None, None, None) {
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

    let _app_nap_token = gnosis_vpn_lib::app_nap::disable("Gnosis VPN session monitoring requires timely execution");

    match daemon(args).await {
        Ok(_) => (),
        Err(exitcode::OK) => (),
        Err(code) => {
            tracing::warn!(exit_code = code, "abnormal exit");
            process::exit(code);
        }
    }
}

/// Root passes `<logs>/worker.log`; an unwritable log dir degrades to stdout, which root relays.
fn setup_logging(log_file: Option<&std::path::Path>) {
    let Some(log_path) = log_file else {
        logging::setup_stdout();
        return;
    };
    logging::setup_file_or_stdout(log_path, logging::make_rolling_fmt_layer(log_path));
}

impl PendingStartup {
    /// Root forwards every socket command once the worker exists; only the config-level ones have an answer yet.
    fn answer(&self, cmd: command::WorkerCommand, id: u64) -> WorkerToRoot {
        let resp = match cmd {
            command::WorkerCommand::Status => {
                let mut run_mode = ipc::RunMode::Init {
                    issue: self.issue.clone(),
                };
                self.internet_check
                    .annotate(&mut run_mode, self.spawn_params.is_pinned());
                let status = self.config.offline_status(run_mode, self.target_dest_id.as_deref());
                ipc::Response::Status(Box::new(status))
            }
            command::WorkerCommand::Destinations => {
                let ids = self
                    .config
                    .destinations
                    .connect_ids()
                    .into_iter()
                    .map(Into::into)
                    .collect();
                ipc::Response::Destinations(ids)
            }
            _ => {
                return WorkerToRoot::Refused {
                    error: ipc::ServiceError::WorkerRestarting,
                    id,
                };
            }
        };
        WorkerToRoot::Response {
            resp: Box::new(resp),
            id,
        }
    }
}

impl State {
    pub fn new(root_socket_writer: BufWriter<WriteHalf<TokioUnixStream>>) -> Self {
        let (lookup_sender, lookup_receiver) = mpsc::channel(8);
        Self {
            core_task: JoinSet::new(),
            core_cancel: CancellationToken::new(),
            root_socket_writer,
            pending_startup: None,
            lookup_sender,
            lookup_receiver,
        }
    }

    pub async fn incoming_command(&mut self, cmd: RootToWorker) -> IncomingResolution {
        match cmd {
            RootToWorker::Shutdown => self.cmd_shutdown().await,
            RootToWorker::StartupParams {
                config,
                spawn_params,
                target_dest_id,
            } => self.cmd_startup_params(*config, *spawn_params, target_dest_id),
            RootToWorker::WorkerCommand { cmd, id } => {
                tracing::debug!(?cmd, id, "received command from root");
                let pending_disconnect =
                    self.pending_startup.is_some() && matches!(cmd, command::WorkerCommand::Disconnect);
                if pending_disconnect {
                    return self.disconnect_before_core(id);
                }
                match &self.pending_startup {
                    Some(pending) => IncomingResolution::ToRoot(Box::new(pending.answer(cmd, id))),
                    None => IncomingResolution::RoundtripViaCore(Box::new((cmd, id))),
                }
            }
            RootToWorker::ForceReconnect if self.pending_startup.is_some() => {
                tracing::debug!("received force reconnect from root before core init - nothing to reconnect");
                IncomingResolution::SustainLoop
            }
            RootToWorker::ForceReconnect => {
                tracing::debug!("received force reconnect from root");
                IncomingResolution::ForceReconnectToCore
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

    fn cmd_startup_params(
        &mut self,
        config: config::Config,
        spawn_params: worker_params::SpawnParams,
        target_dest_id: Option<String>,
    ) -> IncomingResolution {
        if !self.core_task.is_empty() || self.pending_startup.is_some() {
            tracing::warn!("core already initialized - ignoring startup params");
            return IncomingResolution::SustainLoop;
        }
        tracing::debug!(?config, ?spawn_params, "received startup params from root");
        let lookup = self.spawn_network_lookup(&config, spawn_params.clone());
        self.pending_startup = Some(PendingStartup {
            config,
            spawn_params,
            target_dest_id,
            issue: None,
            internet_check: InternetCheck::default(),
            lookup,
        });
        IncomingResolution::SustainLoop
    }

    fn spawn_network_lookup(
        &self,
        config: &config::Config,
        spawn_params: worker_params::SpawnParams,
    ) -> tokio::task::JoinHandle<()> {
        let sender = self.lookup_sender.clone();
        let (request_timeout, resolve_timeout) = (config.blokli.request_timeout, config.blokli.resolve_timeout);
        tokio::spawn(async move {
            let on_failure = |kind, message| {
                // Failures come seconds apart, so the queue never fills.
                let _ = sender.try_send(NetworkLookup::Failed { kind, message });
            };
            let params = spawn_params
                .query_network(request_timeout, resolve_timeout, on_failure)
                .await;
            let _ = sender.send(NetworkLookup::Found(Box::new(params))).await;
        })
    }

    /// Root already cleared its target and lifted the killswitch; the core must not resurrect either.
    fn disconnect_before_core(&mut self, id: u64) -> IncomingResolution {
        let Some(mut pending) = self.pending_startup.take() else {
            return IncomingResolution::SustainLoop;
        };
        pending.target_dest_id = None;
        if pending.spawn_params.is_pinned() {
            tracing::info!("killswitch lifted before core init - looking blokli up again");
            pending.lookup.abort();
            // A fresh channel drops anything the pinned lookup already queued.
            (self.lookup_sender, self.lookup_receiver) = mpsc::channel(8);
            pending.spawn_params = pending.spawn_params.without_pin();
            pending.lookup = self.spawn_network_lookup(&pending.config, pending.spawn_params.clone());
        }
        self.pending_startup = Some(pending);
        let resp = ipc::Response::Disconnect(ipc::DisconnectResponse::NotConnected);
        IncomingResolution::ToRoot(Box::new(WorkerToRoot::Response {
            resp: Box::new(resp),
            id,
        }))
    }

    async fn incoming_lookup(
        &mut self,
        lookup: NetworkLookup,
        worker_to_core_receiver_wrapper: &mut Option<mpsc::Receiver<WorkerToCore>>,
        core_to_worker_sender: mpsc::Sender<CoreToWorker>,
    ) -> IncomingResolution {
        match lookup {
            NetworkLookup::Failed { kind, message } => {
                if let Some(pending) = &mut self.pending_startup {
                    pending.issue = Some(startup_issue::continued(pending.issue.as_ref(), kind, message));
                }
                IncomingResolution::SustainLoop
            }
            NetworkLookup::Found(worker_params) => {
                let Some(pending) = self.pending_startup.take() else {
                    return IncomingResolution::SustainLoop;
                };
                tracing::info!(network = worker_params.network(), "blokli network");
                self.start_core(
                    pending.config,
                    *worker_params,
                    pending.target_dest_id,
                    worker_to_core_receiver_wrapper,
                    core_to_worker_sender,
                )
                .await
            }
        }
    }

    async fn start_core(
        &mut self,
        config: config::Config,
        worker_params: worker_params::WorkerParams,
        target_dest_id: Option<String>,
        worker_to_core_receiver_wrapper: &mut Option<mpsc::Receiver<WorkerToCore>>,
        core_to_worker_sender: mpsc::Sender<CoreToWorker>,
    ) -> IncomingResolution {
        // The per-network tree must exist before init writes the pass file into it.
        if let Err(err) = worker_params.ensure_dirs() {
            tracing::error!(error = %err, network = worker_params.network(), "failed to create per-user state directories");
            return IncomingResolution::Shutdown(exitcode::CANTCREAT);
        }
        let (sender, mut core_to_worker_receiver) = mpsc::channel(32);
        let res_core = Core::init(config, worker_params, target_dest_id, sender).await;
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
            // Identity trouble is an init failure root recovers from; anything else still takes it down.
            (Err(err @ core::Error::HoprParams(_)), _) => {
                tracing::error!(error = %err, "failed to load or generate the HOPR identity");
                IncomingResolution::Shutdown(exitcode::CANTCREAT)
            }
            (Err(err), _) => {
                tracing::error!(error = %err, "failed to initialize core logic");
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
                Some(lookup) = self.lookup_receiver.recv() => {
                    let resolution = self.incoming_lookup(lookup, &mut worker_to_core_receiver_wrapper, core_to_worker_sender.clone()).await;
                    if let IncomingResolution::Shutdown(code) = resolution {
                        return Err(code);
                    }
                }
                cmd = socket_receiver.recv() => {
                    let Some(cmd) = cmd else {
                        tracing::error!("control socket to root closed - exiting");
                        return Err(exitcode::IOERR);
                    };
                    match self.incoming_command(cmd).await {
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
                                send_to_root(
                                    Box::new(WorkerToRoot::Response {
                                        id,
                                        resp: Box::new(resp),
                                    }),
                                    &mut self.root_socket_writer,
                                )
                                .await?;
                            }
                            Err(err) => {
                                tracing::warn!(error = ?err, "core-to-worker receiver unexpectedly closed while awaiting response for command from root");
                            }
                        }
                    }
                    IncomingResolution::ToRoot(msg) => send_to_root(msg, &mut self.root_socket_writer).await?,
                    IncomingResolution::Shutdown(code) => {
                        tracing::info!(?code, "shutting down worker daemon before core loop initialization");
                        return Err(code);
                    }
                    IncomingResolution::ShutdownToCore => {
                        let _ = worker_to_core_sender.send(WorkerToCore::Shutdown).await;
                    }
                    IncomingResolution::ForceReconnectToCore => {
                        let _ = worker_to_core_sender.send(WorkerToCore::ForceReconnect).await;
                    }
                    IncomingResolution::SustainLoop => {}
                    }
                }
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
        if let Some(pending) = self.pending_startup.take() {
            pending.lookup.abort();
        }
        // should be already empty from main loop drainage
        self.core_task.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn pending(issue: Option<ipc::StartupIssue>) -> PendingStartup {
        PendingStartup {
            config: config::parse("version = 7").expect("minimal config"),
            spawn_params: worker_params::SpawnParams::new(
                worker_params::ConfigFileMode::Generated,
                worker_params::AllowFlags::default(),
                "https://blokli.invalid".parse().expect("valid url"),
                None,
                gnosis_vpn_lib::dirs::user_dirs(std::path::Path::new("/tmp")),
            ),
            target_dest_id: None,
            issue,
            internet_check: InternetCheck::default(),
            lookup: tokio::spawn(async {}),
        }
    }

    #[tokio::test]
    async fn status_before_core_init_reports_the_lookup_issue() {
        let issue = ipc::StartupIssue {
            kind: ipc::StartupIssueKind::ServiceUnavailable,
            message: "503 service unavailable".to_string(),
            since: std::time::SystemTime::UNIX_EPOCH,
        };

        let answer = pending(Some(issue.clone())).answer(command::WorkerCommand::Status, 7);

        let WorkerToRoot::Response { resp, id: 7 } = answer else {
            panic!("status must be answered");
        };
        let ipc::Response::Status(status) = *resp else {
            panic!("status must answer with a status");
        };
        assert!(matches!(status.run_mode, ipc::RunMode::Init { issue: Some(reported) } if reported == issue));
    }

    #[tokio::test]
    async fn disconnect_before_core_init_drops_the_target_and_the_pin() {
        let (root_end, _worker_end) = TokioUnixStream::pair().expect("socket pair");
        let (_, writer) = io::split(root_end);
        let mut state = State::new(BufWriter::new(writer));
        let mut startup = pending(None);
        startup.target_dest_id = Some("exit-1".to_string());
        startup.spawn_params = worker_params::SpawnParams::new(
            worker_params::ConfigFileMode::Generated,
            worker_params::AllowFlags::default(),
            "https://blokli.invalid".parse().expect("valid url"),
            Some(std::net::Ipv4Addr::new(203, 0, 113, 7)),
            gnosis_vpn_lib::dirs::user_dirs(std::path::Path::new("/tmp")),
        );
        state.pending_startup = Some(startup);

        let resolution = state.disconnect_before_core(7);

        let IncomingResolution::ToRoot(answer) = resolution else {
            panic!("disconnect must be answered");
        };
        assert!(matches!(
            *answer,
            WorkerToRoot::Response { ref resp, id: 7 }
                if matches!(**resp, ipc::Response::Disconnect(ipc::DisconnectResponse::NotConnected))
        ));
        let pending = state.pending_startup.take().expect("still pending");
        assert_eq!(pending.target_dest_id, None);
        assert!(!pending.spawn_params.is_pinned());
        pending.lookup.abort();
    }

    #[tokio::test]
    async fn commands_needing_the_core_are_refused_before_core_init() {
        let answer = pending(None).answer(command::WorkerCommand::Disconnect, 7);

        assert!(matches!(
            answer,
            WorkerToRoot::Refused {
                error: ipc::ServiceError::WorkerRestarting,
                id: 7
            }
        ));
    }

    #[test]
    fn tun_fd_socket_timeout_is_cleared_when_receiving_the_fd_fails() {
        let (mut root_socket, worker_socket) = UnixStream::pair().unwrap();
        worker_socket.set_read_timeout(Some(TUN_FD_SOCKET_TIMEOUT)).unwrap();
        assert_eq!(worker_socket.read_timeout().unwrap(), Some(TUN_FD_SOCKET_TIMEOUT));
        worker_socket.set_read_timeout(None).unwrap();
        root_socket.write_all(&[0]).unwrap();
        drop(root_socket);

        assert!(setup_tun_fd_socket(&worker_socket).is_err());
        assert_eq!(worker_socket.read_timeout().unwrap(), None);
    }
}
