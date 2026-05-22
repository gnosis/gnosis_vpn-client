//! Hardcoded, root-owned filesystem locations used by the update engine.
//!
//! Centralized so IPC callers cannot influence where artifacts land or where
//! the audit log + attempt-state file live.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
fn base_dir() -> PathBuf {
    PathBuf::from("/Library/Application Support/GnosisVPN")
}

#[cfg(target_os = "linux")]
fn base_dir() -> PathBuf {
    PathBuf::from("/var/lib/gnosisvpn")
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn base_dir() -> PathBuf {
    PathBuf::from("/tmp/gnosisvpn")
}

pub fn download_dir() -> PathBuf {
    base_dir().join("updates")
}

pub fn attempt_state_path() -> PathBuf {
    base_dir().join("last_update_attempt.json")
}

#[cfg(target_os = "macos")]
pub fn audit_log_path() -> PathBuf {
    PathBuf::from("/var/log/gnosisvpn/updates.log")
}

#[cfg(not(target_os = "macos"))]
pub fn audit_log_path() -> PathBuf {
    PathBuf::from("/var/log/gnosisvpn/updates.log")
}
