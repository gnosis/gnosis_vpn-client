use file_rotate::compression::Compression;
use file_rotate::suffix::AppendCount;
use file_rotate::{ContentLimit, FileRotate};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::sync::Mutex;

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

/// Root passes `<logs>/worker.log`; rotation renames it to `worker.log.1`, `worker.log.2` and so on, oldest highest.
pub const WORKER_LOG_FILE: &str = "worker.log";
/// Files the worker keeps in total: the active one plus the rotated ones.
pub const WORKER_MAX_LOG_FILES: usize = 7;
/// Size in bytes (500 MiB) at which the worker rolls to the next file, so debug volume stays bounded.
pub const WORKER_MAX_LOG_BYTES: usize = 500 * 1024 * 1024;

/// Turns a panic in the wrapped writer into an I/O error.
///
/// `FileRotate` panics when the log dir vanishes during a rotation. Caught here, the panic cannot poison the
/// writer's mutex, which would make every later log call panic too.
struct PanicSafe<W>(W);

impl<W: Write> Write for PanicSafe<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        panic::catch_unwind(AssertUnwindSafe(|| self.0.write(buf)))
            .unwrap_or_else(|_| Err(std::io::Error::other("log writer panicked")))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        panic::catch_unwind(AssertUnwindSafe(|| self.0.flush()))
            .unwrap_or_else(|_| Err(std::io::Error::other("log writer panicked")))
    }
}

/// The worker's log layer: lives in the user's own log dir, so it rotates itself instead of relying on a system rotator.
pub fn make_rolling_fmt_layer(log_path: &Path) -> Result<FileFmtLayer, std::io::Error> {
    make_rolling_fmt_layer_with(log_path, WORKER_MAX_LOG_BYTES, WORKER_MAX_LOG_FILES)
}

fn make_rolling_fmt_layer_with(
    log_path: &Path,
    max_bytes: usize,
    max_files: usize,
) -> Result<FileFmtLayer, std::io::Error> {
    let Some(log_dir) = log_path.parent().filter(|_| log_path.file_name().is_some()) else {
        return Err(std::io::Error::other("log file path has no directory or file name"));
    };
    dirs::ensure_private_dir(log_dir, log_dir)?;
    // `FileRotate` swallows a failed open and then drops every line, so fail here and let the caller use stdout.
    OpenOptions::new().create(true).append(true).open(log_path)?;
    // `AppendCount` counts rotated files only, so one less leaves room for the active file.
    // `BytesSurpassed` finishes the line that crosses the limit, so no line is split across files.
    let appender = FileRotate::new(
        log_path,
        AppendCount::new(max_files.saturating_sub(1)),
        ContentLimit::BytesSurpassed(max_bytes),
        Compression::None,
        None,
    );
    Ok(fmt::layer()
        .with_writer(BoxMakeWriter::new(Mutex::new(PanicSafe(appender))))
        .with_ansi(false))
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
    use std::os::unix::fs::PermissionsExt;

    use anyhow::Context;

    use super::*;

    struct Panicking;

    impl Write for Panicking {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            panic!("rotation failed")
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn panic_safe_writer_should_return_an_error_and_keep_the_mutex_usable_when_the_writer_panics() -> anyhow::Result<()>
    {
        let shared = Mutex::new(PanicSafe(Panicking));
        for _ in 0..2 {
            let mut guard = shared
                .lock()
                .map_err(|_| anyhow::anyhow!("mutex poisoned by an earlier panic"))?;
            anyhow::ensure!(
                guard.write(b"line").is_err(),
                "a panicking write must surface as an error"
            );
        }
        Ok(())
    }

    #[test]
    fn rolling_layer_should_fail_when_the_active_log_file_cannot_be_opened() -> anyhow::Result<()> {
        // A read-only file makes the open fail without needing privileges; the caller then falls back to stdout.
        let dir = tempfile::tempdir().context("create temp dir")?;
        let log = dir.path().join("worker.log");
        std::fs::write(&log, "x").context("write log")?;
        std::fs::set_permissions(&log, std::fs::Permissions::from_mode(0o400)).context("chmod log")?;
        anyhow::ensure!(
            make_rolling_fmt_layer_with(&log, 1024, 3).is_err(),
            "an unopenable active file must not look like a working log"
        );
        Ok(())
    }

    #[test]
    fn file_layer_creates_the_parent_and_the_rolling_layer_the_private_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(make_file_fmt_layer(&dir.path().join("root/gnosisvpn.log")).is_ok());
        assert!(dir.path().join("root").is_dir());
        assert!(make_rolling_fmt_layer(&dir.path().join("worker/worker.log")).is_ok());
        assert!(dir.path().join("worker").is_dir());
    }

    #[test]
    fn rolling_layer_should_cap_file_count_and_size_when_log_volume_exceeds_the_limit() -> anyhow::Result<()> {
        let dir = tempfile::tempdir().context("create temp dir")?;
        let layer = make_rolling_fmt_layer_with(&dir.path().join("worker.log"), 1024, 3).context("make layer")?;
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
            for i in 0..200 {
                tracing::info!("line {i:04} {}", "x".repeat(80));
            }
        });
        let sizes = std::fs::read_dir(dir.path())
            .context("read log dir")?
            .map(|entry| Ok(entry?.metadata()?.len()))
            .collect::<std::io::Result<Vec<u64>>>()
            .context("read log file sizes")?;
        anyhow::ensure!(
            sizes.len() == 3,
            "active file plus rotated ones are capped at max_files: {sizes:?}"
        );
        // A file rolls once it passes the limit, so it may overshoot by one line at most.
        anyhow::ensure!(
            sizes.iter().all(|size| *size < 1024 + 256),
            "no file grows past the limit: {sizes:?}"
        );
        Ok(())
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
