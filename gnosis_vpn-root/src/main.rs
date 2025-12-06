use tokio::fs;
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, WriteHalf};
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::IntoRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::process::{self};

use gnosis_vpn_lib::command::{Command as cmdCmd, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::event::{IncomingWorker, OutgoingWorker, WireGuardCommand};
use gnosis_vpn_lib::hopr_params::HoprParams;
use gnosis_vpn_lib::{socket, worker};

mod cli;
mod routing;
mod wg_tooling;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

async fn ctrlc_channel() -> Result<mpsc::Receiver<()>, exitcode::ExitCode> {
    let (sender, receiver) = mpsc::channel(32);
    let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
        tracing::error!(error = ?e, "error setting up SIGINT handler");
        exitcode::IOERR
    })?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
        tracing::error!(error = ?e, "error setting up SIGTERM handler");
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

async fn socket_stream(socket_path: &Path) -> Result<UnixStream, exitcode::ExitCode> {
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

    let stream = UnixStream::connect(socket_path).await.map_err(|e| {
        tracing::error!(error = ?e, "error connecting to socket");
        exitcode::IOERR
    })?;

    // update permissions to allow unprivileged access
    fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o666))
        .await
        .map_err(|e| {
            tracing::error!(error = ?e, "error setting socket permissions");
            exitcode::NOPERM
        })?;

    Ok(stream)
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
    let socket = socket_stream(&args.socket_path).await?;

    // set up routing for mix node - ensure clean state by calling teardown first
    let _ = routing::teardown(&worker_user).await;
    routing::setup(&worker_user).await.map_err(|err| {
        tracing::error!(error = ?err, "error setting up routing");
        exitcode::OSERR
    })?;

    let res = loop_daemon(&mut ctrlc_receiver, socket, &worker_user, config, hopr_params).await;

    let _ = routing::teardown(&worker_user).await.map_err(|err| {
        tracing::error!(error = ?err, "error tearing down routing");
    });
    let _ = fs::remove_file(&socket_path).await.map_err(|err| {
        tracing::error!(error = ?err, "failed removing socket");
    });
    res
}

async fn loop_daemon(
    ctrlc_receiver: &mut mpsc::Receiver<()>,
    socket: UnixStream,
    worker_user: &worker::Worker,
    config: Config,
    hopr_params: HoprParams,
) -> Result<(), exitcode::ExitCode> {
    let (parent_socket, child_socket) = StdUnixStream::pair().map_err(|err| {
        tracing::error!(error = ?err, "unable to create socket pair for worker communication");
        exitcode::IOERR
    })?;

    let mut worker_child = Command::new(worker_user.binary.clone())
        .env(socket::worker::ENV_VAR, format!("{}", child_socket.into_raw_fd()))
        .uid(worker_user.uid)
        .gid(worker_user.gid)
        .spawn()
        .map_err(|err| {
            tracing::error!(error = ?err, ?worker_user, "unable to spawn worker process");
            exitcode::IOERR
        })?;

    let parent_stream = UnixStream::from_std(parent_socket).map_err(|err| {
        tracing::error!(error = ?err, "unable to create unix stream from socket");
        exitcode::IOERR
    })?;

    // root <-> worker communication setup
    let (reader_half, writer_half) = io::split(parent_stream);
    let reader = BufReader::new(reader_half);
    let mut lines_reader = reader.lines();
    let mut writer = BufWriter::new(writer_half);

    // root <-> system socket communication setup (UI app)
    let (socket_reader_half, socket_writer_half) = io::split(socket);
    let socket_reader = BufReader::new(socket_reader_half);
    let mut socket_lines_reader = socket_reader.lines();
    let mut socket_writer = BufWriter::new(socket_writer_half);

    // provide initial resources to worker
    send_to_worker(&IncomingWorker::HoprParams { hopr_params }, &mut writer).await?;
    send_to_worker(&IncomingWorker::Config { config }, &mut writer).await?;

    // enter main loop
    let mut shutdown_ongoing = false;
    tracing::info!("entering main daemon loop");
    loop {
        tokio::select! {
            Some(_) = ctrlc_receiver.recv() => {
                if shutdown_ongoing {
                    tracing::info!("force shutdown immediately");
                    return Err(exitcode::OK);
                } else {
                    shutdown_ongoing = true;
                    tracing::info!("initiate shutdown");
                    send_to_worker(&IncomingWorker::Shutdown, &mut writer).await?;
                }
            },
            Ok(Some(line)) = lines_reader.next_line() => {
                let cmd = parse_outgoing_worker(line)?;
                match cmd {
        OutgoingWorker::Ack => {
            tracing::debug!("received worker ack");
        }
        OutgoingWorker::OutOfSync => {
            tracing::error!("worker out of sync with root - exiting");
            return Err(exitcode::UNAVAILABLE);
        }
        OutgoingWorker::Response { resp } => {
            tracing::debug!(?resp, "received worker response");
            send_to_socket(&resp, &mut socket_writer).await?;
        }
        OutgoingWorker::WireGuard(wg_cmd) => {
            tracing::debug!(?wg_cmd, "received worker wireguard command");
            match wg_cmd {
                WireGuardCommand::WgUp( config_content ) => {
                    // ensure down before up even if redundant
                    let _ = wg_tooling::down().await;
                    let res = wg_tooling::up(config_content).await.map_err(|e| e.to_string());
                    send_to_worker(&IncomingWorker::WgUpResult { res }, &mut writer).await?;
                },
                WireGuardCommand::WgDown => {
                    // result does not matter here
                    let _ = wg_tooling::down().await;
        }
                }
            },
                }
            },
            Ok(Some(line)) = socket_lines_reader.next_line() => {
                let cmd: cmdCmd = parse_command(line)?;
                tracing::debug!(command = ?cmd, "received socket command");
                send_to_worker(&IncomingWorker::Command { cmd }, &mut writer).await?;
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

fn parse_outgoing_worker(line: String) -> Result<OutgoingWorker, exitcode::ExitCode> {
    let cmd: OutgoingWorker = serde_json::from_str(&line).map_err(|err| {
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
    msg: &IncomingWorker,
    writer: &mut BufWriter<WriteHalf<UnixStream>>,
) -> Result<(), exitcode::ExitCode> {
    let serialized = serde_json::to_string(msg).map_err(|err| {
        tracing::error!(error = ?err, "failed to serialize message");
        exitcode::DATAERR
    })?;
    writer.write_all(serialized.as_bytes()).await.map_err(|err| {
        tracing::error!(error = ?err, "error writing to UnixStream pair write half");
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

async fn send_to_socket(
    msg: &Response,
    writer: &mut BufWriter<WriteHalf<UnixStream>>,
) -> Result<(), exitcode::ExitCode> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use gnosis_vpn_lib::command::Response;
    use notify::event::{self, Event, EventKind};
    use std::path::Path;
    use std::time::Duration;
    use tempfile::tempdir;
    use tokio::io::AsyncReadExt;
    use tokio::net::UnixStream;
    use tokio::sync::mpsc;
    use tokio::time::timeout;

    fn build_event(kind: EventKind) -> Event {
        Event {
            kind,
            paths: Vec::new(),
            attrs: event::EventAttributes::default(),
        }
    }

    #[tokio::test]
    async fn config_channel_succeeds_when_file_exists() -> anyhow::Result<()> {
        let dir = tempdir().expect("temp dir");
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "log-level = \"info\"").expect("config");

        let result = config_channel(config_path.as_path()).await;
        result.expect("watcher available");
        Ok(())
    }

    #[tokio::test]
    async fn config_channel_fails_when_file_missing() -> anyhow::Result<()> {
        let dir = tempdir().expect("temp dir");
        let config_path = dir.path().join("missing.toml");

        let err = config_channel(config_path.as_path()).await.expect_err("missing config");
        assert_eq!(err, exitcode::NOINPUT);
        Ok(())
    }

    #[tokio::test]
    async fn socket_channel_accepts_new_connections() -> anyhow::Result<()> {
        let dir = tempdir().expect("temp dir");
        let socket_path = dir.path().join("daemon.sock");

        let mut receiver = socket_channel(socket_path.as_path()).await.expect("socket");
        let _client = UnixStream::connect(socket_path.as_path()).await.expect("connects");

        let incoming = timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("waiting for connection");
        assert!(incoming.is_some());
        Ok(())
    }

    #[test]
    fn incoming_config_fs_event_when_file_changes() -> anyhow::Result<()> {
        let event = build_event(EventKind::Create(event::CreateKind::File));
        assert!(incoming_config_fs_event(event, Path::new("config.toml")));

        let event = build_event(EventKind::Modify(event::ModifyKind::Data(event::DataChange::Size)));
        assert!(incoming_config_fs_event(event, Path::new("config.toml")));

        let event = build_event(EventKind::Remove(event::RemoveKind::File));
        assert!(incoming_config_fs_event(event, Path::new("config.toml")));
        Ok(())
    }

    #[test]
    fn incoming_config_fs_event_skips_irrelevant_events() -> anyhow::Result<()> {
        let event = build_event(EventKind::Other);
        assert!(!incoming_config_fs_event(event, Path::new("config.toml")));
        Ok(())
    }
}
