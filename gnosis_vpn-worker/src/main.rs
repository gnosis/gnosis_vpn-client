use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use std::env;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
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

enum SignalMessage {
    Shutdown,
    RotateLogs,
}

async fn signal_channel() -> Result<mpsc::Receiver<SignalMessage>, exitcode::ExitCode> {
    let (sender, receiver) = mpsc::channel(32);
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

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    // Set up logging
    let reload_handle = match &args.log_file {
        Some(log_path) => Some(logging::setup_log_file(log_path.clone()).map_err(|err| {
            eprintln!("Failed to open log file {}: {}", log_path.display(), err);
            exitcode::IOERR
        })?),
        None => {
            logging::setup_stdout();
            None
        }
    };
    let log_path = args.log_file.clone();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    // set up signal handler
    let mut signal_receiver = signal_channel().await?;

    tracing::debug!("accessing unix socket from fd");
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

    let child_socket = unsafe { StdUnixStream::from_raw_fd(fd) };
    child_socket.set_nonblocking(true).map_err(|err| {
        tracing::error!(error = %err, "failed to set non-blocking mode on worker socket");
        exitcode::IOERR
    })?;
    let child_stream = UnixStream::from_std(child_socket).map_err(|err| {
        tracing::error!(error = %err, "failed to create unix stream from socket");
        exitcode::IOERR
    })?;

    tracing::debug!("splitting unix stream into reader and writer halves");
    let (reader_half, writer_half) = io::split(child_stream);
    let reader = BufReader::new(reader_half);
    let mut lines_reader = reader.lines();
    let mut writer = BufWriter::new(writer_half);

    let (mut incoming_event_sender, incoming_event_receiver) = mpsc::channel(32);
    let (outgoing_event_sender, mut outgoing_event_receiver) = mpsc::channel(32);

    // enter main loop
    let mut shutdown_ongoing = false;
    let mut core_task = JoinSet::new();
    let mut init_opt = Some(init::Init::new());
    let mut incoming_event_receiver_opt = Some(incoming_event_receiver);
    let mut outoing_event_sender_opt = Some(outgoing_event_sender);
    tracing::info!("enter listening mode");
    loop {
        tokio::select! {
            Some(signal) = signal_receiver.recv() => match signal {
                SignalMessage::Shutdown => {
                if shutdown_ongoing {
                    tracing::info!("force shutdown immediately");
                    return Ok(());
                }
                tracing::info!("initiate shutdown");
                if core_task.is_empty() {
                    tracing::debug!("core not yet started");
                    return Ok(());
                }
                shutdown_ongoing = true;
                incoming_event_sender.send(WorkerToCore::Shutdown).await.map_err(|_| {
                    tracing::error!("command receiver already closed");
                    exitcode::IOERR
                })?;
                }
                SignalMessage::RotateLogs => {
                    // Recreate the file layer and swap it in the reload handle so that logging continues to the new file after rotation
                    // Note: we rely on newsyslog to have already rotated the file (renamed it and created a new one) before sending SIGHUP, so make_file_fmt_layer should open the new file rather than the rotated one
                    if let (Some(handle), Some(path)) = (&reload_handle, &log_path) {
                        let res =  logging::make_file_fmt_layer(&path.to_string_lossy())
                            .map(|new_layer| handle.reload(new_layer));
                        match res {
                            Ok(_) => {
                                tracing::info!("successfully reloaded logging layer with new log file after SIGHUP");
                            }
                            Err(e) => {
                                eprintln!("failed to reopen log file {:?}: {}", path, e);
                                return Err(exitcode::IOERR);
                            }
                        }
                    } else {
                        tracing::debug!("no log file configured, skipping log reload on SIGHUP");
                    }
                },
            },
            Ok(Some(line)) = lines_reader.next_line() => {
                tracing::debug!(line = %line, "incoming from root service");
                let wcmd = parse_incoming_worker(line)?;
                if let Some(init) = init_opt.take() {
                    let next = init.incoming_cmd(wcmd);
                    send_outgoing(WorkerToRoot::Ack, &mut writer).await?;
                    if let Some((config, worker_params)) = next.ready() {
                        if let (Some(mut incoming_event_receiver), Some(outgoing_event_sender)) = (incoming_event_receiver_opt.take(), outoing_event_sender_opt.take()) {
                            let core = Core::init(config, worker_params, outgoing_event_sender).await.map_err(|err| {
                                tracing::error!(error = ?err, "failed to initialize core logic");
                                exitcode::OSERR
                            })?;
                            core_task.spawn(async move { core.start(&mut incoming_event_receiver).await });
                        }
                    } else {
                        init_opt = Some(next);
                    }
                } else {
                    let resp = incoming_cmd(wcmd, &mut incoming_event_sender).await?;
                    send_outgoing(resp, &mut writer).await?;
                }
            },
            outgoing = outgoing_event_receiver.recv() => {
                match outgoing {
                    Some(event) => {
                        tracing::debug!(?event, "outgoing event from core");
                        match event {
                            CoreToWorker::RequestToRoot(req) =>
                                send_outgoing(WorkerToRoot::RequestToRoot(req), &mut writer).await?,
                        }
                    }
                    None => {
                        if !shutdown_ongoing {
                            tracing::error!("outgoing event channel closed unexpectedly");
                            return Err(exitcode::IOERR);
                        }
                    }
                }
            }
            Some(_) = core_task.join_next() => {
                tracing::info!("shutting down worker daemon");
                return Ok(());
            }
            else => {
                tracing::error!("unexpected channel closure");
                return Err(exitcode::IOERR);
            }
        }
    }
}

fn parse_incoming_worker(line: String) -> Result<RootToWorker, exitcode::ExitCode> {
    let cmd: RootToWorker = serde_json::from_str(&line).map_err(|err| {
        tracing::error!(error = %err, "failed parsing incoming worker command");
        exitcode::DATAERR
    })?;
    Ok(cmd)
}

async fn send_outgoing(
    resp: WorkerToRoot,
    writer: &mut BufWriter<WriteHalf<UnixStream>>,
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

async fn incoming_cmd(
    cmd: RootToWorker,
    event_sender: &mut mpsc::Sender<WorkerToCore>,
) -> Result<WorkerToRoot, exitcode::ExitCode> {
    match cmd {
        RootToWorker::Command { cmd, id } => {
            let (sender, recv) = oneshot::channel();
            event_sender
                .send(WorkerToCore::Command { cmd, resp: sender })
                .await
                .map_err(|_| {
                    tracing::error!("command receiver already closed");
                    exitcode::IOERR
                })?;
            let resp = recv.await.map_err(|_| {
                tracing::error!("command responder already closed");
                exitcode::IOERR
            })?;
            Ok(WorkerToRoot::Response { id, resp })
        }
        RootToWorker::WorkerParams { .. } => {
            tracing::warn!("received hopr params after init - ignoring");
            Ok(WorkerToRoot::OutOfSync)
        }
        RootToWorker::Config { .. } => {
            tracing::warn!("received config after init - ignoring");
            Ok(WorkerToRoot::OutOfSync)
        }
        RootToWorker::ResponseFromRoot(res) => {
            event_sender
                .send(WorkerToCore::ResponseFromRoot(res))
                .await
                .map_err(|_| {
                    tracing::error!("event receiver already closed");
                    exitcode::IOERR
                })?;
            Ok(WorkerToRoot::Ack)
        }
    }
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
