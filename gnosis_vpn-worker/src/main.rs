use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinSet;

use std::env;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::process;

use gnosis_vpn_lib::core::Core;
use gnosis_vpn_lib::event::{IncomingCore, IncomingWorker, OutgoingCore, OutgoingWorker, WireGuardCommand};
use gnosis_vpn_lib::hopr::hopr_lib;
use gnosis_vpn_lib::socket;

mod cli;
mod init;
// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

async fn daemon() -> Result<(), exitcode::ExitCode> {
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

    let mut core_task = JoinSet::new();
    let mut init_opt = Some(init::Init::new());
    let mut incoming_event_receiver_opt = Some(incoming_event_receiver);
    let mut outoing_event_sender_opt = Some(outgoing_event_sender);
    tracing::info!("enter listening mode");
    loop {
        tokio::select! {
            Ok(Some(line)) = lines_reader.next_line() => {
                let wcmd = parse_incoming_worker(line)?;
                if let Some(init) = init_opt.take() {
                    let next = init.incoming_cmd(wcmd);
                    send_outgoing(OutgoingWorker::Ack, &mut writer).await?;
                    if next.is_shutdown() {
                        tracing::info!("shutting down worker daemon");
                        return Ok(());
                    }
                    if let Some((config, hopr_params)) = next.ready() {
                        if let (Some(mut incoming_event_receiver), Some(outgoing_event_sender)) = (incoming_event_receiver_opt.take(), outoing_event_sender_opt.take()) {
                            let core = Core::init(config, hopr_params, outgoing_event_sender).await.map_err(|err| {
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
                        match event {
                            OutgoingCore::WgUp(content) =>
                                send_outgoing(OutgoingWorker::WireGuard(WireGuardCommand::WgUp(content)), &mut writer).await?,
                                OutgoingCore::WgDown =>
                                send_outgoing(OutgoingWorker::WireGuard(WireGuardCommand::WgDown), &mut writer).await?,
                        }
                    }
                    None => {
                        tracing::error!("outgoing event channel closed unexpectedly");
                        return Err(exitcode::IOERR);
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

fn parse_incoming_worker(line: String) -> Result<IncomingWorker, exitcode::ExitCode> {
    let cmd: IncomingWorker = serde_json::from_str(&line).map_err(|err| {
        tracing::error!(error = %err, "failed parsing incoming worker command");
        exitcode::DATAERR
    })?;
    Ok(cmd)
}

async fn send_outgoing(
    resp: OutgoingWorker,
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
    cmd: IncomingWorker,
    event_sender: &mut mpsc::Sender<IncomingCore>,
) -> Result<OutgoingWorker, exitcode::ExitCode> {
    match cmd {
        IncomingWorker::Shutdown => {
            tracing::info!("initiate shutdown");
            event_sender.send(IncomingCore::Shutdown).await.map_err(|_| {
                tracing::error!("event receiver already closed");
                exitcode::IOERR
            })?;
            Ok(OutgoingWorker::Ack)
        }
        IncomingWorker::Command { cmd } => {
            let (sender, recv) = oneshot::channel();
            event_sender
                .send(IncomingCore::Command { cmd, resp: sender })
                .await
                .map_err(|_| {
                    tracing::error!("command receiver already closed");
                    exitcode::IOERR
                })?;
            let resp = recv.await.map_err(|_| {
                tracing::error!("command responder already closed");
                exitcode::IOERR
            })?;
            Ok(OutgoingWorker::Response { resp: Box::new(resp) })
        }
        IncomingWorker::HoprParams { .. } => {
            tracing::warn!("received hopr params after init - ignoring");
            Ok(OutgoingWorker::OutOfSync)
        }
        IncomingWorker::Config { .. } => {
            tracing::warn!("received config after init - ignoring");
            Ok(OutgoingWorker::OutOfSync)
        }
        IncomingWorker::WgUpResult { res } => {
            event_sender.send(IncomingCore::WgUpResult { res }).await.map_err(|_| {
                tracing::error!("event receiver already closed");
                exitcode::IOERR
            })?;
            Ok(OutgoingWorker::Ack)
        }
    }
}

fn main() {
    match hopr_lib::prepare_tokio_runtime() {
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
    let _args = cli::parse();

    // install global collector configured based on RUST_LOG env var.
    tracing_subscriber::fmt::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    match daemon().await {
        Ok(_) => (),
        Err(exitcode::OK) => (),
        Err(code) => {
            tracing::warn!("abnormal exit");
            process::exit(code);
        }
    }
}
