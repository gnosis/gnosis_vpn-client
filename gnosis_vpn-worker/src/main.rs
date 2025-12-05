use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter, ReadHalf, WriteHalf};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

use std::env;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::process;

use gnosis_vpn_lib::command;
use gnosis_vpn_lib::config::Config;
use gnosis_vpn_lib::core::Core;
use gnosis_vpn_lib::external_event::Event;
use gnosis_vpn_lib::hopr::hopr_lib;
use gnosis_vpn_lib::hopr_params::HoprParams;
use gnosis_vpn_lib::worker_command::{WorkerCommand, WorkerResponse};
use gnosis_vpn_lib::{external_event, socket};

mod cli;
mod init;
// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

async fn daemon() -> Result<(), exitcode::ExitCode> {
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
    let child_stream = UnixStream::from_std(child_socket).map_err(|err| {
        tracing::error!(error = %err, "failed to create unix stream from socket");
        exitcode::IOERR
    })?;

    let (reader_half, writer_half) = io::split(child_stream);
    let reader = BufReader::new(reader_half);
    let mut lines = reader.lines();
    let mut writer = BufWriter::new(writer_half);

    let (mut incoming_event_sender, mut incoming_event_receiver) = mpsc::channel(32);
    let (mut outgoing_event_sender, mut outgoing_event_receiver) = mpsc::channel(32);

    /*
    let core = core::Core::init(config, hopr_params).await.map_err(|err| {
        tracing::error!(error = ?err, "failed to initialize core logic");
        exitcode::OSERR
    })?;

    tokio::spawn(async move { core.start(&mut event_receiver).await });
    */

    // let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
    // keep sender in an Option so we can take() it exactly once
    // let mut shutdown_sender_opt: Option<oneshot::Sender<()>> = Some(shutdown_sender);

    let mut init = init::Init::new();
    tracing::info!("enter listening mode");
    loop {
        tokio::select! {
            res = lines.next_line() => {
                let wcmd = parse_worker_command(res)?;
                init.incoming_cmd(wcmd);
                send_response(WorkerResponse::Ack, &mut writer).await?;
                if !keep_going {
                    tracing::info!("shutting down worker daemon");
                    return Ok(());
                }
            },
            outgoing = outgoing_event_receiver.recv() => handle_outgoing_event(outgoing),
            else => {
                tracing::error!("unexpected channel closure");
                return Err(exitcode::IOERR);
            }
        }
    }
}

fn parse_worker_command(res: io::Result<Option<String>>) -> Result<WorkerCommand, exitcode::ExitCode> {
    match res {
        Ok(None) => {
            tracing::error!("incoming socket closed unexpectedly");
            Err(exitcode::IOERR)
        }
        Err(err) => {
            tracing::error!(error = %err, "failed reading incoming socket");
            Err(exitcode::IOERR)
        }
        Ok(Some(line)) => {
            let cmd: WorkerCommand = serde_json::from_str(&line).map_err(|err| {
                tracing::error!(error = %err, "failed parsing worker command");
                exitcode::DATAERR
            })?;
            Ok(cmd)
        }
    }
}

async fn incoming_for_ready(mut init_state: InitState, cmd: WorkerCommand) -> Result<bool, exitcode::ExitCode> {
    match (init_state, cmd) {
        (_, WorkerCommand::Shutdown) => Ok(false),
        (InitState::AwaitingResources, WorkerCommand::HoprParams { hopr_params }) => {
            init_state = InitState::AwaitingConfig(hopr_params);
            Ok(true)
        }
        (InitState::AwaitingResources, WorkerCommand::Config { config }) => {
            init_state = InitState::AwaitingHoprParams(config);
            Ok(true)
        }
        (InitState::AwaitingHoprParams(config), WorkerCommand::HoprParams { hopr_params }) => {
            init_state = InitState::Ready(config, hopr_params);
            Ok(true)
        }
        (InitState::AwaitingConfig(hopr_params), WorkerCommand::Config { config }) => {
            init_state = InitState::Ready(config, hopr_params);
            Ok(true)
        }
        (state, cmd) => {
            tracing::warn!(?state, ?cmd, "received command before init complete - ignoring");
            Ok(true)
        }
    }
}

/*
        (InitState::Running(_), WorkerCommand::Shutdown) => {
            let (sender, recv) = oneshot::channel();
            incoming_event_sender
                .send(Event::Shutdown { resp: sender })
                .await
                .map_err(|_| {
                    tracing::warn!("event receiver already closed");
                    exitcode::IOERR
                })?;
            _ = recv.await.map_err(|_| {
                tracing::error!("event responder already closed");
                exitcode::IOERR
            })?;
            Ok(WorkerResponse::Ack)
        }




        tracing::info!("initiate shutdown");
        match shutdown_sender_opt.take() {
            Some(sender) => {
                event_sender.send(external_event::shutdown(sender)).await.map_err(|_| {
                    tracing::warn!("event receiver already closed");
                    exitcode::IOERR
                })?;
                Ok(None)
            }
            None => {
                tracing::error!("shutdown sender already taken");
                Err(exitcode::IOERR)
            }
        }
    }
    WorkerCommand::Command { cmd } => {
        let (sender, recv) = oneshot::channel();
        event_sender
            .send(external_event::Event::Command { cmd, resp: sender })
            .await
            .map_err(|_| {
                tracing::error!("command receiver already closed");
                exitcode::IOERR
            })?;
        let resp = recv.await.map_err(|_| {
            tracing::error!("command responder already closed");
            exitcode::IOERR
        })?;
        Ok(Some(resp))
    }
    WorkerCommand::HoprParams { .. } => {
        tracing::warn!("received hopr params after init - ignoring");
        Ok(None)
    }
    WorkerCommand::Config { .. } => {
        tracing::warn!("received hopr params after init - ignoring");
        Ok(None)
    }
}
*/

async fn send_response(
    resp: WorkerResponse,
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

async fn handle_command(
    cmd: WorkerCommand,
    event_sender: &mut mpsc::Sender<external_event::Event>,
    shutdown_sender_opt: &mut Option<oneshot::Sender<()>>,
) -> Result<Option<command::Response>, exitcode::ExitCode> {
    match cmd {
        WorkerCommand::Shutdown => {
            tracing::info!("initiate shutdown");
            match shutdown_sender_opt.take() {
                Some(sender) => {
                    event_sender.send(external_event::shutdown(sender)).await.map_err(|_| {
                        tracing::warn!("event receiver already closed");
                        exitcode::IOERR
                    })?;
                    Ok(None)
                }
                None => {
                    tracing::error!("shutdown sender already taken");
                    Err(exitcode::IOERR)
                }
            }
        }
        WorkerCommand::Command { cmd } => {
            let (sender, recv) = oneshot::channel();
            event_sender
                .send(external_event::Event::Command { cmd, resp: sender })
                .await
                .map_err(|_| {
                    tracing::error!("command receiver already closed");
                    exitcode::IOERR
                })?;
            let resp = recv.await.map_err(|_| {
                tracing::error!("command responder already closed");
                exitcode::IOERR
            })?;
            Ok(Some(resp))
        }
        WorkerCommand::HoprParams { .. } => {
            tracing::warn!("received hopr params after init - ignoring");
            Ok(None)
        }
        WorkerCommand::Config { .. } => {
            tracing::warn!("received hopr params after init - ignoring");
            Ok(None)
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

    #[tokio::test]
    async fn incoming_stream_processes_valid_command() -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        let (mut server, mut client) = UnixStream::pair().expect("pair");
        let (mut sender, mut receiver) = mpsc::channel(1);
        let payload = serde_json::to_string(&Command::Ping).expect("serialize");
        client.write_all(payload.as_bytes()).await.expect("write");
        client.shutdown().await.expect("shutdown write");

        let response_handle = tokio::spawn(async move {
            if let Some(external_event::Event::Command { cmd, resp }) = receiver.recv().await {
                assert!(matches!(cmd, Command::Ping));
                resp.send(Response::Pong).expect("response sent");
            } else {
                panic!("expected command event");
            }
        });

        incoming_stream(&mut server, &mut sender).await;
        server.shutdown().await.expect("shutdown response stream");
        response_handle.await.expect("response task");

        let mut buf = String::new();
        client.read_to_string(&mut buf).await.expect("read response");

        let expected = serde_json::to_string(&Response::Pong).expect("response");
        assert_eq!(buf, expected);
        Ok(())
    }

    #[tokio::test]
    async fn incoming_stream_ignores_invalid_payloads() -> anyhow::Result<()> {
        use tokio::io::AsyncWriteExt;

        let (mut server, mut client) = UnixStream::pair().expect("pair");
        let (mut sender, mut receiver) = mpsc::channel(1);

        client.write_all(b"not-a-command").await.expect("write");
        client.shutdown().await.expect("shutdown write");

        incoming_stream(&mut server, &mut sender).await;
        drop(server);

        assert!(receiver.try_recv().is_err(), "no command dispatched");

        let mut buf = String::new();
        client.read_to_string(&mut buf).await.expect("read");
        assert!(buf.is_empty(), "invalid payload should not yield response");
        Ok(())
    }
}
