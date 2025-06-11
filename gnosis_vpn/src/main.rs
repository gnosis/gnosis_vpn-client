use ctrlc::Error as CtrlcError;
use gnosis_vpn_lib::command::Command;
use notify::{RecursiveMode, Watcher};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net;
use std::path::Path;
use std::process;
use std::thread;
use std::time::{Duration, Instant};

mod cli;
mod core;
mod event;

fn ctrlc_channel() -> Result<crossbeam_channel::Receiver<()>, exitcode::ExitCode> {
    let (sender, receiver) = crossbeam_channel::bounded(2);
    match ctrlc::set_handler(move || match sender.send(()) {
        Ok(_) => (),
        Err(e) => {
            tracing::error!(error = ?e, "sending incoming data");
        }
    }) {
        Ok(_) => Ok(receiver),
        Err(CtrlcError::NoSuchSignal(signal_type)) => {
            tracing::error!(?signal_type, "no such signal");
            Err(exitcode::OSERR)
        }
        Err(CtrlcError::MultipleHandlers) => {
            tracing::error!("multiple handlers");
            Err(exitcode::UNAVAILABLE)
        }
        Err(CtrlcError::System(e)) => {
            tracing::error!(error = ?e, "system error");
            Err(exitcode::IOERR)
        }
    }
}

fn config_channel(
    param_config_path: &Path,
) -> Result<
    (
        notify::RecommendedWatcher,
        crossbeam_channel::Receiver<notify::Result<notify::Event>>,
    ),
    exitcode::ExitCode,
> {
    match param_config_path.try_exists() {
        Ok(true) => {}
        Ok(false) => {
            tracing::error!(config_file=%param_config_path.display(), "cannot find configuration file");
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
        Some(dir) => dir,
        None => {
            tracing::error!("config path has no parent");
            return Err(exitcode::UNAVAILABLE);
        }
    };

    let (sender, receiver) = crossbeam_channel::unbounded::<notify::Result<notify::Event>>();

    let mut watcher = match notify::recommended_watcher(sender) {
        Ok(watcher) => watcher,
        Err(e) => {
            tracing::error!(error = ?e, "error creating config watcher");
            return Err(exitcode::IOERR);
        }
    };

    match watcher.watch(parent, RecursiveMode::NonRecursive) {
        Ok(_) => (),
        Err(e) => {
            tracing::error!(error = ?e, "error watching config directory");
            return Err(exitcode::IOERR);
        }
    };

    Ok((watcher, receiver))
}

fn socket_channel(socket_path: &Path) -> Result<crossbeam_channel::Receiver<net::UnixStream>, exitcode::ExitCode> {
    match socket_path.try_exists() {
        Ok(true) => {
            tracing::error!("socket path already exists");
            return Err(exitcode::TEMPFAIL);
        }
        Ok(false) => (),
        Err(e) => {
            tracing::error!(error = ?e, "error checking socket path");
            return Err(exitcode::IOERR);
        }
    };

    let stream = match net::UnixListener::bind(socket_path) {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!(error = ?e, "error binding socket");
            return Err(exitcode::OSFILE);
        }
    };

    // update permissions to allow unprivileged access
    // TODO this would better be handled by allowing group access and let the installer create a
    // gvpn group and additionally add users to it
    match fs::set_permissions(socket_path, fs::Permissions::from_mode(0o666)) {
        Ok(_) => (),
        Err(e) => {
            tracing::error!(error = ?e, "error setting socket permissions");
            return Err(exitcode::NOPERM);
        }
    }

    let (sender, receiver) = crossbeam_channel::unbounded::<net::UnixStream>();
    thread::spawn(move || {
        for strm in stream.incoming() {
            match strm {
                Ok(s) => match sender.send(s) {
                    Ok(_) => (),
                    Err(e) => {
                        tracing::error!(error = ?e, "sending incoming data");
                    }
                },
                Err(e) => {
                    tracing::error!(error = ?e, "waiting for incoming message");
                }
            };
        }
    });

    Ok(receiver)
}

fn incoming_stream(core: &mut core::Core, res_stream: Result<net::UnixStream, crossbeam_channel::RecvError>) {
    let mut stream: net::UnixStream = match res_stream {
        Ok(strm) => strm,
        Err(e) => {
            tracing::error!(error = ?e, "error receiving stream");
            return;
        }
    };

    let mut msg = String::new();
    if let Err(e) = stream.read_to_string(&mut msg) {
        tracing::error!(error = ?e, "error reading message");
        return;
    };

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

    if let Err(e) = stream.write_all(str_resp.as_bytes()) {
        tracing::error!(error = %e, "error writing response");
        return;
    }

    if let Err(e) = stream.flush() {
        tracing::error!(error = ?e, "error flushing stream")
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

fn incoming_config_fs_event(
    res_event: Result<notify::Result<notify::Event>, crossbeam_channel::RecvError>,
    config_path: &Path,
) -> Option<crossbeam_channel::Receiver<Instant>> {
    let event: notify::Result<notify::Event> = match res_event {
        Ok(evt) => evt,
        Err(e) => {
            tracing::error!(error = ?e, "error receiving config event");
            return None;
        }
    };

    tracing::debug!(event = ?event, "incoming config event");

    match event {
        Ok(notify::Event { kind, paths, attrs: _ })
            if (kind == notify::event::EventKind::Create(notify::event::CreateKind::File)
                || kind
                    == notify::event::EventKind::Modify(notify::event::ModifyKind::Data(
                        notify::event::DataChange::Any,
                    ))
                || kind == notify::event::EventKind::Remove(notify::event::RemoveKind::File))
                && paths == vec![config_path] =>
        {
            tracing::debug!(?kind, "config file change detected");
            Some(crossbeam_channel::after(CONFIG_GRACE_PERIOD))
        }
        Ok(_) => None,
        Err(e) => {
            tracing::error!(error = ?e, "error watching config folder");
            None
        }
    }
}

fn daemon(socket_path: &Path, config_path: &Path) -> exitcode::ExitCode {
    let ctrlc_receiver = match ctrlc_channel() {
        Ok(receiver) => receiver,
        Err(exit) => return exit,
    };

    // keep config watcher in scope so it does not get dropped
    let (_config_watcher, config_receiver) = match config_channel(config_path) {
        Ok(receiver) => receiver,
        Err(exit) => return exit,
    };

    let socket_receiver = match socket_channel(socket_path) {
        Ok(receiver) => receiver,
        Err(exit) => return exit,
    };

    let exit_code = loop_daemon(&ctrlc_receiver, &config_receiver, &socket_receiver, config_path);

    // cleanup
    match fs::remove_file(socket_path) {
        Ok(_) => (),
        Err(e) => {
            tracing::warn!(error = %e, "failed removing socket");
        }
    }

    exit_code
}

fn loop_daemon(
    ctrlc_receiver: &crossbeam_channel::Receiver<()>,
    config_receiver: &crossbeam_channel::Receiver<notify::Result<notify::Event>>,
    socket_receiver: &crossbeam_channel::Receiver<net::UnixStream>,
    config_path: &Path,
) -> exitcode::ExitCode {
    let (sender, core_receiver) = crossbeam_channel::unbounded::<event::Event>();
    let mut core = match core::Core::init(config_path, sender) {
        Ok(core) => core,
        Err(e) => {
            tracing::error!(error = ?e, "failed to initialize core logic");
            return exitcode::OSERR;
        }
    };

    let mut read_config_receiver: crossbeam_channel::Receiver<Instant> = crossbeam_channel::never();
    let mut shutdown_receiver: crossbeam_channel::Receiver<()> = crossbeam_channel::never();
    let mut ctrc_already_triggered = false;

    tracing::info!("enter listening mode");
    // run continously until interrupted via signal
    loop {
        crossbeam_channel::select! {
            recv(ctrlc_receiver) -> _ => {
                if ctrc_already_triggered {
                    tracing::info!("force shutdown immediately");
                    return exitcode::OK;
                } else {
                    ctrc_already_triggered = true;
                    tracing::info!("initiate shutdown");
                    shutdown_receiver = core.shutdown();
                }
            }
            recv(shutdown_receiver) -> _ => {
                return exitcode::OK;
            }
            recv(socket_receiver) -> stream => incoming_stream(&mut core, stream),
            recv(core_receiver) -> event => incoming_event(&mut core, event),
            recv(config_receiver) -> event => {
                let resp = incoming_config_fs_event(event, config_path);
                if let Some(r) = resp {
                    read_config_receiver = r
                }
            },
            recv(read_config_receiver) -> _ => {
                match core.update_config(config_path) {
                    Ok(_) => {
                        tracing::info!("updated configuration - resetting application");
                    }
                    Err(e) => {
                        tracing::error!(error = ?e, "failed to update configuration - staying on current configuration");
                    }
                }
            }
        }
    }
}

fn main() {
    let args = cli::parse();

    // install global collector configured based on RUST_LOG env var.
    tracing_subscriber::fmt::init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting {}",
        env!("CARGO_PKG_NAME")
    );

    let exit = daemon(&args.socket_path, &args.config_path);

    if exit != exitcode::OK {
        tracing::warn!("abnormal exit");
    }

    process::exit(exit)
}
