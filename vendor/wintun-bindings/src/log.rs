use crate::{util, wintun_raw, Wintun};
use std::sync::atomic::{AtomicBool, Ordering};

/// Sets the logger wintun will use when logging. Maps to the WintunSetLogger C function
pub fn set_logger(wintun: &Wintun, f: wintun_raw::WINTUN_LOGGER_CALLBACK) {
    unsafe { wintun.WintunSetLogger(f) };
}

pub fn reset_logger(wintun: &Wintun) {
    set_logger(wintun, None);
}

static SET_LOGGER: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct LogItem {
    pub(crate) level: log::Level,
    pub(crate) msg: String,
    pub(crate) timestamp: u64,
}

impl LogItem {
    #[allow(dead_code)]
    pub(crate) fn new(level: log::Level, msg: String, timestamp: u64) -> Self {
        Self { level, msg, timestamp }
    }
}

#[cfg(feature = "enable_inner_logging")]
static LOG_CONTAINER: std::sync::LazyLock<std::sync::Mutex<std::collections::VecDeque<LogItem>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::VecDeque::new()));

/// The logger that is active by default. Logs messages to the log crate
///
/// # Safety
/// `message` must be a valid pointer that points to an aligned null terminated UTF-16 string
pub unsafe extern "stdcall" fn default_logger(
    level: wintun_raw::WINTUN_LOGGER_LEVEL,
    _timestamp: wintun_raw::DWORD64,
    message: windows_sys::core::PCWSTR,
) {
    //Wintun will always give us a valid UTF16 null termineted string
    let utf8_msg = util::win_pwstr_to_string(message as *mut u16).unwrap_or_else(|e| e.to_string());

    let _l = match level {
        wintun_raw::WINTUN_LOGGER_LEVEL_WINTUN_LOG_INFO => {
            log::info!("WinTun: {}", utf8_msg);
            log::Level::Info
        }
        wintun_raw::WINTUN_LOGGER_LEVEL_WINTUN_LOG_WARN => {
            log::warn!("WinTun: {}", utf8_msg);
            log::Level::Warn
        }
        wintun_raw::WINTUN_LOGGER_LEVEL_WINTUN_LOG_ERR => log::Level::Error,
        _ => log::Level::Error,
    };

    #[cfg(feature = "enable_inner_logging")]
    if let Err(e) = LOG_CONTAINER.lock().map(|mut log| {
        log.push_back(LogItem::new(_l, utf8_msg, _timestamp));
    }) {
        log::error!("Failed to log message: {}", e);
    }
    #[cfg(not(feature = "enable_inner_logging"))]
    if _l == log::Level::Error {
        log::error!("WinTun: {}", utf8_msg);
    }
}

#[cfg(feature = "enable_inner_logging")]
fn get_log() -> Vec<LogItem> {
    LOG_CONTAINER
        .lock()
        .map(|mut log| log.drain(..).collect())
        .unwrap_or_else(|_e| Vec::new())
}

#[cfg(feature = "enable_inner_logging")]
fn get_worst_log_msg(container: &[LogItem]) -> Option<&LogItem> {
    container.iter().max_by_key(|item| match item.level {
        log::Level::Error => 2,
        log::Level::Warn => 1,
        log::Level::Info => 0,
        _ => 0,
    })
}

pub(crate) fn extract_wintun_log_error<T>(prifix: &str) -> Result<T, String> {
    #[cfg(not(feature = "enable_inner_logging"))]
    let info = "No inner logs".to_string();
    #[cfg(feature = "enable_inner_logging")]
    let info = get_worst_log_msg(&get_log())
        .map(|item| item.msg.clone())
        .unwrap_or_else(|| "No inner logs".to_string());
    Err(format!("{} \"{}\"", prifix, info))
}

pub(crate) fn set_default_logger_if_unset(wintun: &Wintun) {
    if SET_LOGGER
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
    {
        set_logger(wintun, Some(default_logger));
    }
}
