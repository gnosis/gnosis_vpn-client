use std::fs::OpenOptions;

use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::{EnvFilter, fmt, prelude::*, reload};

pub type FileFmtLayer =
    fmt::Layer<tracing_subscriber::Registry, fmt::format::DefaultFields, fmt::format::Format, BoxMakeWriter>;

pub type LogReloadHandle = reload::Handle<FileFmtLayer, tracing_subscriber::Registry>;

const DEFAULT_LOG_FILTER: &str = "info";
const ENV_VAR_LOG_FILE: &str = "GNOSISVPN_LOG_FILE";
#[cfg(target_os = "macos")]
const DEFAULT_LOG_FILE_MACOS: &str = "/Library/Logs/GnosisVPN/gnosisvpn.log";
#[cfg(not(target_os = "macos"))]
const DEFAULT_LOG_FILE_LINUX: &str = "/var/log/gnosisvpn.log";

pub fn make_file_fmt_layer(log_path: &str) -> FileFmtLayer {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .unwrap_or_else(|e| panic!("failed to open log file {log_path}: {e}"));

    fmt::layer().with_writer(BoxMakeWriter::new(file)).with_ansi(false)
}

fn log_path() -> String {
    if let Ok(log_path) = std::env::var(ENV_VAR_LOG_FILE) {
        return log_path;
    }

    #[cfg(target_os = "macos")]
    {
        DEFAULT_LOG_FILE_MACOS.to_string()
    }
    #[cfg(not(target_os = "macos"))]
    {
        DEFAULT_LOG_FILE_LINUX.to_string()
    }
}

pub fn init() -> (LogReloadHandle, String) {
    let log_path = log_path();
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));
    let (reload_layer, reload_handle): (
        reload::Layer<FileFmtLayer, tracing_subscriber::Registry>,
        LogReloadHandle,
    ) = reload::Layer::new(make_file_fmt_layer(&log_path));
    tracing_subscriber::registry().with(reload_layer).with(filter).init();
    (reload_handle, log_path)
}
