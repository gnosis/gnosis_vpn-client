use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process;

use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::socket;

mod cli;
mod core;
mod event;
mod hopr_params;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

async fn ctrlc_channel() -> Result<mpsc::Receiver<()>, exitcode::ExitCode> {
    let (sender, receiver) = mpsc::channel(1);
    let mut sigint = signal(SignalKind::interrupt()).map_err(|e| {
        tracing::error!(error = ?e, "error setting up SIGINT handler");
        exitcode::IOERR
    })?;
    let mut sigterm = signal(SignalKind::terminate()).map_err(|e| {
        tracing::error!(error = ?e, "error setting up SIGTERM handler");
        exitcode::IOERR
    })?;

    tokio::spawn(async move {
        tokio::select! {
            _ = sigint.recv() => {
                let _ = sender.send(()).await.map_err(|e| {
                    tracing::error!(error = ?e, "sending sigint");
                });
            }
            _ = sigterm.recv() => {
                let _ = sender.send(()).await.map_err(|e| {
                    tracing::error!(error = ?e, "sending sigterm");
                });
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

    let config_path = match fs::canonicalize(param_config_path) {
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
            sender.blocking_send(event).map_err(|e| {
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

async fn socket_channel(socket_path: &Path) -> Result<mpsc::Receiver<tokio::net::UnixStream>, exitcode::ExitCode> {
    match socket_path.try_exists() {
        Ok(true) => {
            tracing::info!("probing for running instance");
            match socket::process_cmd(socket_path, &Command::Ping) {
                Ok(_) => {
                    tracing::error!("system service is already running - cannot start another instance");
                    return Err(exitcode::TEMPFAIL);
                }
                Err(e) => {
                    tracing::debug!(warn = ?e, "done probing for running instance");
                }
            };
            fs::remove_file(socket_path).map_err(|e| {
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
    fs::create_dir_all(socket_dir).map_err(|e| {
        tracing::error!(error = %e, "error creating socket directory");
        exitcode::IOERR
    })?;

    let listener = UnixListener::bind(socket_path).map_err(|e| {
        tracing::error!(error = ?e, "error binding socket");
        exitcode::OSFILE
    })?;

    // update permissions to allow unprivileged access
    fs::set_permissions(socket_path, fs::Permissions::from_mode(0o666)).map_err(|e| {
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

async fn incoming_stream(core: &mut crate::core::Core, stream: &mut UnixStream) {
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

    let resp = match core.handle_cmd(&cmd) {
        Ok(res) => res,
        Err(e) => {
            tracing::error!(error = ?e, "error handling command");
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

fn incoming_event(core: &mut core::Core, res_event: Result<event::Event, crossbeam_channel::RecvError>) {
    let event: event::Event = match res_event {
        Ok(evt) => evt,
        Err(e) => {
            tracing::error!(error = ?e, "error receiving event");
            return;
        }
    };

    tracing::debug!(event = %event, "incoming event");

    match core.handle_event(event) {
        Ok(_) => (),
        Err(e) => {
            tracing::error!(error = ?e, "error handling event")
        }
    }
}

// handling fs config events with a grace period to avoid duplicate reads without delay
const CONFIG_GRACE_PERIOD: Duration = Duration::from_millis(333);

fn incoming_config_fs_event(event: notify::Event, config_path: &Path) -> bool {
    tracing::debug!(?event, ?config_path, "incoming config event");
    match event {
        notify::Event {
            kind: kind @ notify::event::EventKind::Create(notify::event::CreateKind::File),
            paths,
            attrs: _,
        }
        | notify::Event {
            kind: kind @ notify::event::EventKind::Remove(notify::event::RemoveKind::File),
            paths,
            attrs: _,
        }
        | notify::Event {
            kind: kind @ notify::event::EventKind::Modify(notify::event::ModifyKind::Data(_)),
            paths,
            attrs: _,
        } => {
            if paths.as_slice() == [config_path] {
                tracing::debug!(?kind, "config file change detected");
                true
            } else {
                false
            }
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

    let ctrlc_receiver = ctrlc_channel().await?;

    // keep config watcher in scope so it does not get dropped
    let (_config_watcher, config_receiver) = config_channel(&config_path).await?;

    let socket_path = args.socket_path.clone();
    let socket_receiver = socket_channel(&args.socket_path).await?;

    let exit_code = loop_daemon(&ctrlc_receiver, &config_receiver, &socket_receiver, args).await;
    match fs::remove_file(&socket_path) {
        Ok(_) => (),
        Err(e) => {
            tracing::error!(error = %e, "failed removing socket");
        }
    }
    Err(exit_code)
}

async fn loop_daemon(
    ctrlc_receiver: &mpsc::Receiver<()>,
    config_receiver: &mpsc::Receiver<notify::Event>,
    socket_receiver: &mpsc::Receiver<UnixStream>,
    args: cli::Cli,
) -> exitcode::ExitCode {
    let hopr_params = hopr_params::HoprParams::from(args.clone());
    let config_path = args.config_path.clone();
    let mut core = match core::Core::init(&config_path, hopr_params) {
        Ok(core) => core,
        Err(e) => {
            tracing::error!(error = ?e, "failed to initialize core logic");
            return exitcode::OSERR;
        }
    };

    // Channel to signal when to reload the config after the grace period
    let (mut reload_sender, mut reload_receiver) = mpsc::channel(1);
    // Channel to signal when shutdown is complete
    let (mut shutdown_sender, mut shutdown_receiver) = mpsc::channel(1);
    let mut ctrc_already_triggered = false;
    tracing::info!("enter listening mode");
    loop {
        tokio::select! {
            _ = ctrlc_receiver.recv() => {
                if ctrc_already_triggered {
                    tracing::info!("force shutdown immediately");
                    return exitcode::OK;
                } else {
                    ctrc_already_triggered = true;
                    tracing::info!("initiate shutdown");
                     shutdown_receiver = core.shutdown().await;
                }
            }
            _ = shutdown_receiver.recv() => {
                return exitcode::OK;
            }
            res = socket_receiver.recv() => match res {
                Some(stream) => incoming_stream(&mut core, &mut stream).await,
                None => {
                    tracing::error!("socket receiver closed unexpectedly");
                    return exitcode::IOERR;
                }
            },
            res = config_receiver.recv() => match res {
                Some(evt) => if incoming_config_fs_event(evt, &config_path) {
                        let (tx, mut rx) = mpsc::channel(1);
                        reload_sender = tx;

                        // Spawn a task to handle the grace period
                        tokio::spawn(async move {
                            sleep(CONFIG_GRACE_PERIOD).await;
                            // If we get here, no new events were received within the grace period
                            // Check if this is the most recent channel by trying to send a signal
                            if let Err(e) = reload_sender.send(()).await {
                                tracing::error!(error = ?e, "error sending reload signal");
                            }
                        });
                    }
                None => {
                    tracing::error!("config receiver closed unexpectedly");
                    return exitcode::IOERR;
                }
            },
            res = reload_receiver.recv() => match res {
                Some(_) => {
                    match core.update_config(&config_path).await {
                        Ok(_) => {
                            tracing::info!("updated configuration - resetting application");
                        }
                        Err(e) => {
                            tracing::error!(error = ?e, "failed to update configuration - staying on current configuration");
                        }
                    }
                }
                None => {
                    tracing::error!("reload receiver closed unexpectedly");
                    return exitcode::IOERR;
                }
            },

        }
    }
}

#[tokio::main]
async fn main() {
    let args = cli::parse();
    println!("Hello, world!");

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
