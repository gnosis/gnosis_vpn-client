use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub type FileFmtLayer =
    fmt::Layer<tracing_subscriber::Registry, fmt::format::DefaultFields, fmt::format::Format, BoxMakeWriter>;

pub type LogReloadHandle = reload::Handle<FileFmtLayer, tracing_subscriber::Registry>;

const DEFAULT_LOG_FILTER: &str = "info";
pub const ENV_VAR_LOG_FILE: &str = "GNOSISVPN_LOG_FILE";

#[cfg(target_os = "linux")]
pub const DEFAULT_LOG_FILE: &str = "/var/log/gnosisvpn.log";
#[cfg(target_os = "macos")]
pub const DEFAULT_LOG_FILE: &str = "/Library/Logs/GnosisVPN/gnosisvpn.log";

/// Root's log layer: append to one file that logrotate/newsyslog rotate, after which SIGHUP calls this again to reopen it.
pub fn make_file_fmt_layer(log_path: &str) -> Result<FileFmtLayer, std::io::Error> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(log_path)?;
    Ok(fmt::layer().with_writer(BoxMakeWriter::new(file)).with_ansi(false))
}

/// Root passes `<logs>/worker.log`; the appender writes `worker.log.YYYY-MM-DD` next to it.
pub const WORKER_LOG_FILE: &str = "worker.log";
/// Generations the worker keeps of its daily files.
pub const WORKER_MAX_LOG_FILES: usize = 7;

/// The worker's log layer: lives in the user's own log dir, so it rotates itself instead of relying on a system rotator.
pub fn make_rolling_fmt_layer(log_dir: &Path, prefix: &str) -> Result<FileFmtLayer, std::io::Error> {
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix)
        .max_log_files(WORKER_MAX_LOG_FILES)
        .build(log_dir)
        .map_err(std::io::Error::other)?;
    Ok(fmt::layer().with_writer(BoxMakeWriter::new(appender)).with_ansi(false))
}

/// Installs the global subscriber with a reloadable file layer and the `RUST_LOG` filter (default `info`).
pub fn setup_log_file(file_fmt_layer: FileFmtLayer) -> Result<LogReloadHandle, std::io::Error> {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let (reload_layer, reload_handle): (
        reload::Layer<FileFmtLayer, tracing_subscriber::Registry>,
        LogReloadHandle,
    ) = reload::Layer::new(file_fmt_layer);
    tracing_subscriber::registry().with(reload_layer).with(filter).init();
    Ok(reload_handle)
}

/// Installs the global subscriber writing to stdout with the `RUST_LOG` filter (default `info`).
pub fn setup_stdout() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::registry()
        .with(fmt::layer().with_ansi(true))
        .with(filter)
        .init();
}
