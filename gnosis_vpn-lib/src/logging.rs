use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

use std::fs::OpenOptions;
use std::os::unix::fs::{self, OpenOptionsExt};
use std::path::PathBuf;

use crate::worker::Worker;

pub type FileFmtLayer =
    fmt::Layer<tracing_subscriber::Registry, fmt::format::DefaultFields, fmt::format::Format, BoxMakeWriter>;

pub type LogReloadHandle = reload::Handle<FileFmtLayer, tracing_subscriber::Registry>;

const DEFAULT_LOG_FILTER: &str = "info";
pub const ENV_VAR_LOG_FILE: &str = "GNOSISVPN_LOG_FILE";
#[cfg(target_os = "macos")]
pub const DEFAULT_LOG_FILE: &str = "/Library/Logs/GnosisVPN/gnosisvpn.log";
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_LOG_FILE: &str = "/var/log/gnosisvpn.log";

/// Creates a [`FileFmtLayer`] for structured logging to a file.
///
/// Opens (or creates) the log file at the given `log_path` in append mode
/// and constructs a `tracing_subscriber` formatting layer that writes to it
/// with ANSI colors disabled (suitable for file output).
///
/// # Log Rotation
///
/// This function is also called during log rotation to reopen the log file
/// after it has been rotated by an external tool. On macOS, `newsyslog`
/// handles rotation and sends a `SIGHUP` signal to the process afterward,
/// which should then call this function to reopen the new log file and
/// reload the layer using the [`LogReloadHandle`].
///
/// # Errors
///
/// Returns an [`std::io::Error`] if the log file at `log_path` cannot be
/// opened or created.
///
/// # Arguments
///
/// * `log_path` - Filesystem path to the log file.
///
/// # Returns
///
/// A `Result` containing the [`FileFmtLayer`] configured to append logs to
/// the specified file.
pub fn make_file_fmt_layer(log_path: &str, worker: &Worker) -> Result<FileFmtLayer, std::io::Error> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(log_path)?;

    fs::chown(&log_path, Some(worker.uid), Some(worker.gid))?;

    Ok(fmt::layer().with_writer(BoxMakeWriter::new(file)).with_ansi(false))
}

/// Uses existing log file for logging without changing ownership or permissions.
pub fn use_file_fmt_layer(log_path: &str) -> Result<FileFmtLayer, std::io::Error> {
    let file = OpenOptions::new().append(true).open(log_path)?;
    Ok(fmt::layer().with_writer(BoxMakeWriter::new(file)).with_ansi(false))
}

/// Initializes the global `tracing` subscriber with a reloadable file logging layer.
///
/// Sets up a [`tracing_subscriber::Registry`] with two layers:
///
/// 1. A **reloadable file layer** — created via [`make_file_fmt_layer` or `use_file_fmt_layer`] — that
///    writes structured logs to the file at `log_path`.
/// 2. An **[`EnvFilter`]** that controls log verbosity. The filter is read from
///    the `RUST_LOG` environment variable; if that is unset or invalid, it
///    defaults to `"info"`.
///
/// The returned [`LogReloadHandle`] allows the file layer to be swapped at
/// runtime without restarting the process. This is essential for log rotation:
/// on macOS, `newsyslog` rotates the log file and then sends `SIGHUP` to the
/// process, which should then call [`make_file_fmt_layer`] to reopen the
/// rotated log file and reload it via the [`LogReloadHandle`].
///
/// # Errors
///
/// Returns an error if the log file cannot be opened or created.
///
/// # Arguments
///
/// * `file_fmt_layer` - A [`FileFmtLayer`] configured to write logs to the desired file.
///
/// # Returns
///
/// A `Result` containing the [`LogReloadHandle`] that can be used to replace
/// the file logging layer at runtime (e.g., in response to `SIGHUP`).
pub fn setup_log_file(file_fmt_layer: FileFmtLayer) -> Result<LogReloadHandle, std::io::Error> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let (reload_layer, reload_handle): (
        reload::Layer<FileFmtLayer, tracing_subscriber::Registry>,
        LogReloadHandle,
    ) = reload::Layer::new(file_fmt_layer);
    tracing_subscriber::registry().with(reload_layer).with(filter).init();
    Ok(reload_handle)
}

/// Initializes the global `tracing` subscriber with stdout/stderr logging.
///
/// Sets up a [`tracing_subscriber::Registry`] with two layers:
///
/// 1. A **formatting layer** that writes structured logs to stdout with
///    ANSI colors enabled (suitable for terminal output).
/// 2. An **[`EnvFilter`]** that controls log verbosity. The filter is read from
///    the `RUST_LOG` environment variable; if that is unset or invalid, it
///    defaults to `"info"`.
///
/// This setup does not support log rotation since it writes directly to
/// stdout/stderr.
pub fn setup_stdout() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::registry()
        .with(fmt::layer().with_ansi(true))
        .with(filter)
        .init();
}
