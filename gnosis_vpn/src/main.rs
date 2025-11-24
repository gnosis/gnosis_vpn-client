use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, sleep};
use tokio_util::sync::CancellationToken;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process;

use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::hopr::hopr_lib;
use gnosis_vpn_lib::{core, external_event, hopr_params, socket};

mod cli;
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

async fn config_channel(
    param_config_path: &Path,
) -> Result<(RecommendedWatcher, mpsc::Receiver<notify::Event>), exitcode::ExitCode> {
    match param_config_path.try_exists() {
        Ok(true) => (),
        Ok(false) => {
            tracing::error!(config_file = %param_config_path.display(), "cannot find configuration file");
            return Err(exitcode::NOINPUT);
        }
        Err(e) => {
            tracing::error!(error = ?e, "error checking configuration file path");
            return Err(exitcode::IOERR);
        }
    };

    let config_path = match fs::canonicalize(param_config_path).await {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(error = ?e, "error canonicalizing config path");
            return Err(exitcode::IOERR);
        }
    };

    let parent = match config_path.parent() {
        Some(p) => p,
        None => {
            tracing::error!("config path has no parent");
            return Err(exitcode::UNAVAILABLE);
        }
    };

    let (sender, receiver) = mpsc::channel(32);
    let mut watcher = match notify::recommended_watcher(move |res| match res {
        Ok(event) => {
            let _ = sender.blocking_send(event).map_err(|e| {
                tracing::error!(error = ?e, "error sending config watch event");
            });
        }
        Err(e) => tracing::error!(error = ?e, "config watch error"),
    }) {
        Ok(watcher) => watcher,
        Err(e) => {
            tracing::error!(error = ?e, "error creating config watcher");
            return Err(exitcode::IOERR);
        }
    };

    if let Err(e) = watcher.watch(parent, RecursiveMode::NonRecursive) {
        tracing::error!(error = ?e, "error watching config directory");
        return Err(exitcode::IOERR);
    }

    Ok((watcher, receiver))
}

async fn socket_channel(socket_path: &Path) -> Result<mpsc::Receiver<UnixStream>, exitcode::ExitCode> {
    match socket_path.try_exists() {
        Ok(true) => {
            tracing::info!("probing for running instance");
            match socket::process_cmd(socket_path, &Command::Ping).await {
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

    let listener = UnixListener::bind(socket_path).map_err(|e| {
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

    let (sender, receiver) = mpsc::channel(32);

    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    if let Err(e) = sender.send(stream).await {
                        tracing::error!(error = ?e, "sending incoming data");
                    }
                }
                Err(e) => {
                    tracing::error!(error = ?e, "waiting for incoming message");
                }
            }
        }
    });

    Ok(receiver)
}

async fn incoming_stream(stream: &mut UnixStream, event_sender: &mut mpsc::Sender<external_event::Event>) {
    let mut msg = String::new();
    if let Err(e) = stream.read_to_string(&mut msg).await {
        tracing::error!(error = ?e, "error reading message");
        return;
    }

    let cmd = match msg.parse::<Command>() {
        Ok(cmd) => cmd,
        Err(e) => {
            tracing::error!(error = ?e, %msg, "error parsing command");
            return;
        }
    };

    tracing::debug!(command = %cmd, "incoming command");

    let (resp_sender, resp_receiver) = oneshot::channel();
    match event_sender.send(external_event::command(cmd, resp_sender)).await {
        Ok(()) => (),
        Err(e) => {
            tracing::error!(error = ?e, "error handling command");
            return;
        }
    };

    let resp = match resp_receiver.await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = ?e, "error receiving command response");
            return;
        }
    };
    let str_resp = match serde_json::to_string(&resp) {
        Ok(res) => res,
        Err(e) => {
            tracing::error!(error = ?e, "error serializing response");
            return;
        }
    };

    if let Err(e) = stream.write_all(str_resp.as_bytes()).await {
        tracing::error!(error = ?e, "error writing response");
        return;
    }

    if let Err(e) = stream.flush().await {
        tracing::error!(error = ?e, "error flushing stream");
    }
}

// handling fs config events with a grace period to avoid duplicate reads without delay
const CONFIG_GRACE_PERIOD: Duration = Duration::from_millis(333);

fn incoming_config_fs_event(event: notify::Event, config_path: &Path) -> bool {
    tracing::debug!(?event, ?config_path, "incoming config event");
    match event {
        notify::Event {
            kind: kind @ notify::event::EventKind::Create(notify::event::CreateKind::File),
            paths: _,
            attrs: _,
        }
        | notify::Event {
            kind: kind @ notify::event::EventKind::Remove(notify::event::RemoveKind::File),
            paths: _,
            attrs: _,
        }
        | notify::Event {
            kind: kind @ notify::event::EventKind::Modify(notify::event::ModifyKind::Data(_)),
            paths: _,
            attrs: _,
        } => {
            tracing::debug!(?kind, "config file change detected");
            true
        }
        _ => false,
    }
}

async fn daemon(args: cli::Cli) -> Result<(), exitcode::ExitCode> {
    let config_path = match args.config_path.canonicalize() {
        Ok(path) => path,
        Err(e) => {
            tracing::error!(error = %e, "error canonicalizing config path");
            return Err(exitcode::IOERR);
        }
    };

    let mut ctrlc_receiver = ctrlc_channel().await?;

    // keep config watcher in scope so it does not get dropped
    let (_config_watcher, mut config_receiver) = config_channel(&config_path).await?;

    let socket_path = args.socket_path.clone();
    let mut socket_receiver = socket_channel(&args.socket_path).await?;

    let exit_code = loop_daemon(&mut ctrlc_receiver, &mut config_receiver, &mut socket_receiver, args).await;
    match fs::remove_file(&socket_path).await {
        Ok(_) => (),
        Err(e) => {
            tracing::error!(error = %e, "failed removing socket");
        }
    }
    Err(exit_code)
}

async fn loop_daemon(
    ctrlc_receiver: &mut mpsc::Receiver<()>,
    config_receiver: &mut mpsc::Receiver<notify::Event>,
    socket_receiver: &mut mpsc::Receiver<UnixStream>,
    args: cli::Cli,
) -> exitcode::ExitCode {
    let hopr_params = hopr_params::HoprParams::from(args.clone());
    let config_path = args.config_path.clone();
    let (mut event_sender, mut event_receiver) = mpsc::channel(32);
    let core = match core::Core::init(&config_path, hopr_params).await {
        Ok(core) => core,
        Err(e) => {
            tracing::error!(error = ?e, "failed to initialize core logic");
            return exitcode::OSERR;
        }
    };

    tracing::info!("enter listening mode");
    tokio::spawn(async move { core.start(&mut event_receiver).await });
    let mut reload_cancel = CancellationToken::new();
    let mut ctrc_already_triggered = false;
    let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
    // keep sender in an Option so we can take() it exactly once
    let mut shutdown_sender_opt: Option<oneshot::Sender<()>> = Some(shutdown_sender);

    loop {
        tokio::select! {
            Some(_) = ctrlc_receiver.recv() => {
                if ctrc_already_triggered {
                    tracing::info!("force shutdown immediately");
                    return exitcode::OK;
                } else {
                    ctrc_already_triggered = true;
                    tracing::info!("initiate shutdown");
                    match shutdown_sender_opt.take() {
                        Some(sender) => {
                            if event_sender.send(external_event::shutdown(sender)).await.is_err() {
                                tracing::warn!("event receiver already closed");
                            }
                        }
                        None => {
                            tracing::error!("shutdown sender already taken");
                            return exitcode::IOERR;
                        }
                    }
                }
            },
            Ok(_) = &mut shutdown_receiver => {
                tracing::info!("shutdown complete");
                return exitcode::OK;
            }
            Some(mut stream) = socket_receiver.recv() => {
                incoming_stream(&mut stream, &mut event_sender).await;
            },
            Some(evt) = config_receiver.recv() => {
                if incoming_config_fs_event(evt, &config_path) {
                    reload_cancel.cancel();
                    reload_cancel = CancellationToken::new();
                    let cancel_token = reload_cancel.clone();
                    let evt_sender = event_sender.clone();
                    let path = config_path.clone();
                    tokio::spawn(async move {
                        cancel_token.run_until_cancelled(async move {
                            sleep(CONFIG_GRACE_PERIOD).await;
                            if evt_sender.send(external_event::config_reload(path)).await.is_err() {
                                tracing::warn!("event receiver already closed");
                            }
                        }).await;
                    });
                }
            },
            else => {
                tracing::error!("unexpected channel closure");
                return exitcode::IOERR;
            }
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
