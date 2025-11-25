use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::{mpsc, oneshot};

use std::process;

use gnosis_vpn_lib::command;
use gnosis_vpn_lib::config::Config;
use gnosis_vpn_lib::hopr::hopr_lib;
use gnosis_vpn_lib::hopr_params::HoprParams;
use gnosis_vpn_lib::worker_command::WorkerCommand;
use gnosis_vpn_lib::{core, external_event};

mod cli;
// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

async fn gather_resources() -> Result<(Config, HoprParams), exitcode::ExitCode> {
    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();

    let mut hopr_params: Option<HoprParams> = None;
    let mut config: Option<Config> = None;

    // first gather necessary resources to initialize worker core loop
    while let Some(line) = lines.next_line().await.map_err(|err| {
        tracing::error!(error = %err, "failed reading stdin");
        exitcode::IOERR
    })? {
        let cmd: WorkerCommand = serde_json::from_str(&line).map_err(|err| {
            tracing::error!(error = %err, "failed parsing worker command");
            exitcode::DATAERR
        })?;
        tracing::debug!(?cmd, "received worker command");
        match cmd {
            WorkerCommand::HoprParams { hopr_params: params } => {
                hopr_params = Some(params);
                if config.is_some() {
                    return Ok((config.unwrap(), hopr_params.unwrap()));
                }
            }
            WorkerCommand::Config { config: cfg } => {
                config = Some(cfg);
                if hopr_params.is_some() {
                    return Ok((config.unwrap(), hopr_params.unwrap()));
                }
            }
            WorkerCommand::Shutdown => {
                tracing::info!("received shutdown command before initialization");
                return Err(exitcode::OK);
            }
            WorkerCommand::Command { cmd } => {
                tracing::warn!(?cmd, "received command before initialization, ignoring");
            }
        }
    }
    Err(exitcode::NOINPUT)
}

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    let (config, hopr_params) = gather_resources().await?;

    let stdin = BufReader::new(io::stdin());
    let mut lines = stdin.lines();
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout);

    let core = core::Core::init(config, hopr_params).await.map_err(|err| {
        tracing::error!(error = ?err, "failed to initialize core logic");
        exitcode::OSERR
    })?;

    let (mut event_sender, mut event_receiver) = mpsc::channel(32);
    tokio::spawn(async move { core.start(&mut event_receiver).await });

    let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
    // keep sender in an Option so we can take() it exactly once
    let mut shutdown_sender_opt: Option<oneshot::Sender<()>> = Some(shutdown_sender);

    tracing::info!("enter listening mode");
    loop {
        tokio::select! {
            res = lines.next_line() => {
                match res {
                    Ok(None) => {
                        tracing::error!("stdin closed unexpectedly");
                        return Err(exitcode::IOERR);
                    },
                    Err(err) => {
                        tracing::error!(error = %err, "failed reading stdin");
                        return Err(exitcode::IOERR);
                    },
                    Ok(Some(line)) => {
                        let cmd: WorkerCommand = serde_json::from_str(&line).map_err(|err| {
                            tracing::error!(error = %err, "failed parsing worker command");
                            exitcode::DATAERR
                        })?;
                        tracing::debug!(?cmd, "received worker command");
                        let resp = handle_command(cmd, &mut event_sender, &mut shutdown_sender_opt).await?;
                        if let Some(r)  = resp {
                            let str = serde_json::to_string(&r).map_err(|err| {
                                tracing::error!(error = ?err, "failed to serialize response");
                                exitcode::DATAERR
                            })?;
                            writer.write_all(str.as_bytes()).await.map_err(|err| {
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
                        }
                    }
                }
            },
            Ok(_) = &mut shutdown_receiver => {
                tracing::info!("shutdown complete");
                return Ok(());
            }
            else => {
                tracing::error!("unexpected channel closure");
                return Err(exitcode::IOERR);
            }
        }
    }
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
