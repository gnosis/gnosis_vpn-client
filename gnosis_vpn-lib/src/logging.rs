use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::dirs;

pub type FileFmtLayer =
    fmt::Layer<tracing_subscriber::Registry, fmt::format::DefaultFields, fmt::format::Format, BoxMakeWriter>;

pub type LogReloadHandle = reload::Handle<FileFmtLayer, tracing_subscriber::Registry>;

const DEFAULT_LOG_FILTER: &str = "info";
pub const ENV_VAR_LOG_FILE: &str = "GNOSISVPN_LOG_FILE";

/// Root's log layer: append to one file that logrotate/newsyslog rotate, after which SIGHUP calls this again to reopen it.
pub fn make_file_fmt_layer(log_path: &Path) -> Result<FileFmtLayer, std::io::Error> {
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
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
pub fn make_rolling_fmt_layer(log_path: &Path) -> Result<FileFmtLayer, std::io::Error> {
    let (Some(log_dir), Some(prefix)) = (log_path.parent(), log_path.file_name()) else {
        return Err(std::io::Error::other("log file path has no directory or file name"));
    };
    dirs::ensure_private_dir(log_dir)?;
    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(prefix.to_string_lossy())
        .max_log_files(WORKER_MAX_LOG_FILES)
        .build(log_dir)
        .map_err(std::io::Error::other)?;
    Ok(fmt::layer().with_writer(BoxMakeWriter::new(appender)).with_ansi(false))
}

/// Installs the file layer, or stdout when it could not be opened: a log must never keep the client from running.
pub fn setup_file_or_stdout(log_path: &Path, layer: Result<FileFmtLayer, std::io::Error>) -> Option<LogReloadHandle> {
    match layer {
        Ok(layer) => Some(setup_log_file(layer)),
        Err(err) => {
            // No subscriber exists yet and RUST_LOG must not be able to hide this line.
            eprintln!(
                "log file {} not writable ({err}), logging to stdout only",
                log_path.display()
            );
            setup_stdout();
            None
        }
    }
}

/// Installs the global subscriber with a reloadable file layer and the `RUST_LOG` filter (default `info`).
pub fn setup_log_file(file_fmt_layer: FileFmtLayer) -> LogReloadHandle {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let (reload_layer, reload_handle): (
        reload::Layer<FileFmtLayer, tracing_subscriber::Registry>,
        LogReloadHandle,
    ) = reload::Layer::new(file_fmt_layer);
    tracing_subscriber::registry().with(reload_layer).with(filter).init();
    reload_handle
}

/// Installs the global subscriber writing to stdout with the `RUST_LOG` filter (default `info`).
pub fn setup_stdout() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    tracing_subscriber::registry()
        .with(fmt::layer().with_ansi(true))
        .with(filter)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_layer_creates_the_parent_and_the_rolling_layer_the_private_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(make_file_fmt_layer(&dir.path().join("root/gnosisvpn.log")).is_ok());
        assert!(dir.path().join("root").is_dir());
        assert!(make_rolling_fmt_layer(&dir.path().join("worker/worker.log")).is_ok());
        assert!(dir.path().join("worker").is_dir());
    }

    #[test]
    fn an_unwritable_location_is_an_error_not_a_panic() {
        // A file where a directory is needed makes both opens fail without needing privileges.
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        assert!(make_file_fmt_layer(&blocker.join("gnosisvpn.log")).is_err());
        assert!(make_rolling_fmt_layer(&blocker.join("worker.log")).is_err());
        assert!(make_rolling_fmt_layer(std::path::Path::new("/")).is_err());
    }
}
