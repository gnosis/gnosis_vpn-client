use std::fs::OpenOptions;
use std::path::PathBuf;

use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

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
/// handles rotation and sends a `SIGHUP` signal to the process afterward.
///
/// # Panics
///
/// Panics if the log file at `log_path` cannot be opened or created.
///
/// # Arguments
///
/// * `log_path` - Filesystem path to the log file.
///
/// # Returns
///
/// A [`FileFmtLayer`] configured to append logs to the specified file.
pub fn make_file_fmt_layer(log_path: &str) -> FileFmtLayer {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .unwrap_or_else(|e| panic!("failed to open log file {log_path}: {e}"));

    fmt::layer().with_writer(BoxMakeWriter::new(file)).with_ansi(false)
}

/// Initializes the global `tracing` subscriber with a reloadable file logging layer.
///
/// Sets up a [`tracing_subscriber::Registry`] with two layers:
///
/// 1. A **reloadable file layer** — created via [`make_file_fmt_layer`] — that
///    writes structured logs to the file at `log_path`.
/// 2. An **[`EnvFilter`]** that controls log verbosity. The filter is read from
///    the `RUST_LOG` environment variable; if that is unset or invalid, it
///    defaults to `"info"`.
///
/// The returned [`LogReloadHandle`] allows the file layer to be swapped at
/// runtime without restarting the process. This is essential for log rotation:
/// on macOS, `newsyslog` rotates the log file and then sends `SIGHUP` to the
/// process.
///
/// # Panics
///
/// Panics if the log file cannot be opened (propagated from
/// [`make_file_fmt_layer`]) or if a global subscriber has already been set.
///
/// # Arguments
///
/// * `log_path` - Filesystem path to the log file.
///
/// # Returns
///
/// A [`LogReloadHandle`] that can be used to replace the file logging layer
/// at runtime (e.g. in response to `SIGHUP`).
pub fn setup_log_file(log_path: PathBuf) -> LogReloadHandle {
    let log_path = log_path.to_string_lossy().to_string();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let (reload_layer, reload_handle): (
        reload::Layer<FileFmtLayer, tracing_subscriber::Registry>,
        LogReloadHandle,
    ) = reload::Layer::new(make_file_fmt_layer(&log_path));
    tracing_subscriber::registry().with(reload_layer).with(filter).init();
    tracing::debug!("logging initialized with file output: {}", log_path);
    reload_handle
}

/// Initializes the global `tracing` subscriber with stdout/stderr logging.
///
/// Sets up a [`tracing_subscriber::Registry`] with:
///
/// 1. A **formatting layer** that writes structured logs to stdout.
/// 2. An **[`EnvFilter`]** that controls log verbosity. The filter is read from
///    the `RUST_LOG` environment variable; if that is unset or invalid, it
///    defaults to `"info"`.
///
/// This setup does not support log rotation since it writes to stdout/stderr.
///
/// # Panics
///
/// Panics if a global subscriber has already been set.
pub fn setup_stdout() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::registry()
        .with(fmt::layer().with_ansi(true))
        .with(filter)
        .init();
    tracing::debug!("logging initialized with stdout/stderr output");
}
