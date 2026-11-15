use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Root passes `<logs>/worker.log`; the worker writes `worker.log.YYYY-MM-DD-HH.NNNN` (UTC hour, then a 4-digit
/// sequence within the hour) next to it.
pub const WORKER_LOG_FILE: &str = "worker.log";
/// A worker log file ends with its hour or at about this size, whichever comes first.
pub const WORKER_MAX_LOG_BYTES: u64 = 100 * 1024 * 1024;
/// The worker's log files together stay within about this size.
pub const WORKER_LOG_BUDGET_BYTES: u64 = 1024 * 1024 * 1024;
/// A week of hourly files, the history a quiet worker keeps.
pub const WORKER_MAX_LOG_FILES: usize = 7 * 24;

/// The worker's log layer: lives in the user's own log dir, so it rotates itself instead of relying on a system rotator.
pub fn make_rolling_fmt_layer(log_path: &Path) -> Result<FileFmtLayer, std::io::Error> {
    let (Some(log_dir), Some(prefix)) = (log_path.parent(), log_path.file_name()) else {
        return Err(std::io::Error::other("log file path has no directory or file name"));
    };
    dirs::ensure_private_dir(log_dir, log_dir)?;
    let log = RollingLog::open(
        log_dir.to_path_buf(),
        prefix.to_string_lossy().into_owned(),
        WORKER_MAX_LOG_BYTES,
        WORKER_LOG_BUDGET_BYTES,
        WORKER_MAX_LOG_FILES,
        SystemTime::now(),
    )?;
    Ok(fmt::layer()
        .with_writer(BoxMakeWriter::new(Mutex::new(log)))
        .with_ansi(false))
}

/// Log files `<prefix>.<UTC hour>.<sequence>`, whose names sort oldest to newest. The next one starts with a new
/// hour or before a write would take the current one past `max_bytes`; the oldest go once the files exceed
/// `budget_bytes` or `max_files`.
struct RollingLog {
    dir: PathBuf,
    prefix: String,
    max_bytes: u64,
    budget_bytes: u64,
    max_files: usize,
    /// Hours since the Unix epoch.
    hour: u64,
    name: String,
    file: File,
    written: u64,
}

impl RollingLog {
    /// Appends to this hour's newest file while it has room, so a restarted worker keeps its history together.
    fn open(
        dir: PathBuf,
        prefix: String,
        max_bytes: u64,
        budget_bytes: u64,
        max_files: usize,
        now: SystemTime,
    ) -> std::io::Result<Self> {
        let hour = hour_index(now);
        let (name, file, written) = hour_file(&dir, &prefix, &hour_stamp(hour), Some(max_bytes))?;
        let log = Self {
            dir,
            prefix,
            max_bytes,
            budget_bytes,
            max_files,
            hour,
            name,
            file,
            written,
        };
        log.prune();
        Ok(log)
    }

    /// Writes all of `buf` to one file, so a new file never starts inside an event: the fmt layer writes each in one call.
    fn write_at(&mut self, buf: &[u8], now: SystemTime) -> std::io::Result<usize> {
        let hour = hour_index(now);
        let full = self.written > 0 && self.written + buf.len() as u64 > self.max_bytes;
        if hour != self.hour || full {
            // A file that cannot be started keeps the current one: a log must never stop the worker.
            if let Ok((name, file, written)) = hour_file(&self.dir, &self.prefix, &hour_stamp(hour), None) {
                self.hour = hour;
                self.name = name;
                self.file = file;
                self.written = written;
                self.prune();
            }
        }
        self.file.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }

    /// Keeps the open file and the newest others within the budget and file count; everything older goes,
    /// including an earlier release's daily `<prefix>.YYYY-MM-DD` files.
    fn prune(&self) {
        let Ok(names) = log_names(&self.dir, &self.prefix) else {
            return;
        };
        // The open file counts at its cap, the size it grows to before the next prune.
        let (mut files, mut bytes, mut over) = (1, self.max_bytes.max(self.written), false);
        // The open file is skipped by name: after the clock steps back it is not the newest.
        for name in names.iter().rev().filter(|name| **name != self.name) {
            let path = self.dir.join(name);
            let len = std::fs::metadata(&path).map_or(0, |meta| meta.len());
            over = over || files >= self.max_files || bytes + len > self.budget_bytes;
            if over {
                let _ = std::fs::remove_file(path);
            } else {
                files += 1;
                bytes += len;
            }
        }
    }
}

impl Write for RollingLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write_at(buf, SystemTime::now())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn hour_index(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() / 3600)
}

/// `YYYY-MM-DD-HH` in UTC.
fn hour_stamp(hour: u64) -> String {
    // `YYYY-MM-DDTHH:MM:SSZ`, fixed width through the year 9999.
    let rfc3339 = humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(hour * 3600)).to_string();
    rfc3339[.."YYYY-MM-DDTHH".len()].replace('T', "-")
}

/// The prefix's log files, oldest first.
fn log_names(dir: &Path, prefix: &str) -> std::io::Result<Vec<String>> {
    let dotted = format!("{prefix}.");
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| name.starts_with(&dotted))
        .collect();
    names.sort();
    Ok(names)
}

/// The hour's newest file when `reuse_below` is given and its size is under it, else the hour's next file.
fn hour_file(dir: &Path, prefix: &str, hour: &str, reuse_below: Option<u64>) -> std::io::Result<(String, File, u64)> {
    let hour_prefix = format!("{prefix}.{hour}.");
    let newest = log_names(dir, prefix)?
        .into_iter()
        .filter_map(|name| name.strip_prefix(&hour_prefix)?.parse::<u32>().ok())
        .max();
    let has_room = |seq: u32| {
        reuse_below.is_some_and(|max| {
            std::fs::metadata(dir.join(format!("{hour_prefix}{seq:04}"))).is_ok_and(|meta| meta.len() < max)
        })
    };
    let seq = match newest {
        Some(seq) if has_room(seq) => seq,
        Some(seq) => seq + 1,
        None => 0,
    };
    let name = format!("{hour_prefix}{seq:04}");
    let file = OpenOptions::new().create(true).append(true).open(dir.join(&name))?;
    let written = file.metadata()?.len();
    Ok((name, file, written))
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

    fn log_files(dir: &Path) -> Vec<(String, u64)> {
        let mut files: Vec<(String, u64)> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap())
            .map(|e| (e.file_name().into_string().unwrap(), e.metadata().unwrap().len()))
            .collect();
        files.sort();
        files
    }

    fn at(rfc3339: &str) -> SystemTime {
        humantime::parse_rfc3339(rfc3339).unwrap()
    }

    fn open_log(dir: &Path, max_bytes: u64, budget_bytes: u64, max_files: usize, now: SystemTime) -> RollingLog {
        RollingLog::open(
            dir.to_path_buf(),
            "worker.log".into(),
            max_bytes,
            budget_bytes,
            max_files,
            now,
        )
        .unwrap()
    }

    fn read(dir: &Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    #[test]
    fn a_loud_worker_rolls_at_the_size_cap_and_keeps_its_files_within_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("worker.log.2026-11-01"),
            "an earlier release's daily file\n",
        )
        .unwrap();
        let now = at("2026-11-15T20:47:35Z");
        let mut log = open_log(dir.path(), 100, 270, 10, now);
        // 25 lines of 27 bytes: three fit under the 100 byte cap, so the ninth file holds only the last line.
        for i in 0..25 {
            log.write_at(format!("line {i:02} padded to 27 bytes\n").as_bytes(), now)
                .unwrap();
        }
        // The open file at its 100 byte cap plus two full 81 byte files fit the 270 byte budget, a fourth would not.
        let files = log_files(dir.path());
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "worker.log.2026-11-15-20.0006",
                "worker.log.2026-11-15-20.0007",
                "worker.log.2026-11-15-20.0008"
            ]
        );
        assert_eq!(
            read(dir.path(), names[1]),
            "line 21 padded to 27 bytes\nline 22 padded to 27 bytes\nline 23 padded to 27 bytes\n"
        );
        assert_eq!(read(dir.path(), names[2]), "line 24 padded to 27 bytes\n");
    }

    #[test]
    fn a_quiet_worker_starts_a_file_each_hour_and_keeps_the_newest_max_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = open_log(dir.path(), 1000, 1_000_000, 3, at("2026-11-15T20:00:00Z"));
        for hour in [
            "2026-11-15T20:59:59Z",
            "2026-11-15T21:00:00Z",
            "2026-11-15T22:30:00Z",
            "2026-11-15T23:00:00Z",
            "2026-11-16T00:10:00Z",
        ] {
            log.write_at(format!("{hour}\n").as_bytes(), at(hour)).unwrap();
        }
        let files = log_files(dir.path());
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "worker.log.2026-11-15-22.0000",
                "worker.log.2026-11-15-23.0000",
                "worker.log.2026-11-16-00.0000"
            ]
        );
        assert_eq!(read(dir.path(), names[2]), "2026-11-16T00:10:00Z\n");
    }

    #[test]
    fn a_restarted_worker_appends_to_this_hours_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = open_log(dir.path(), 1000, 1_000_000, 3, at("2026-11-15T20:10:00Z"));
        log.write_at(b"before restart\n", at("2026-11-15T20:10:00Z")).unwrap();
        drop(log);
        let mut log = open_log(dir.path(), 1000, 1_000_000, 3, at("2026-11-15T20:50:00Z"));
        log.write_at(b"after restart\n", at("2026-11-15T20:50:00Z")).unwrap();
        let files = log_files(dir.path());
        assert_eq!(files.len(), 1, "{files:?}");
        assert_eq!(files[0].0, "worker.log.2026-11-15-20.0000");
        assert_eq!(read(dir.path(), &files[0].0), "before restart\nafter restart\n");
    }

    #[test]
    fn a_clock_stepping_back_never_prunes_the_open_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut log = open_log(dir.path(), 1000, 1_000_000, 2, at("2026-11-15T20:00:00Z"));
        log.write_at(b"at 20\n", at("2026-11-15T20:00:00Z")).unwrap();
        log.write_at(b"at 21\n", at("2026-11-15T21:00:00Z")).unwrap();
        // The open file now sorts oldest; the newest other file stays with it.
        log.write_at(b"stepped back\n", at("2026-11-15T10:00:00Z")).unwrap();
        log.write_at(b"still there\n", at("2026-11-15T10:01:00Z")).unwrap();
        let files = log_files(dir.path());
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            ["worker.log.2026-11-15-10.0000", "worker.log.2026-11-15-21.0000"]
        );
        assert_eq!(read(dir.path(), names[0]), "stepped back\nstill there\n");
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
