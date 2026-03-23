use thiserror::Error;

use std::fs::DirBuilder;
use std::os::unix::fs::{self as unix_fs, DirBuilderExt};
use std::path::PathBuf;

pub const ENV_VAR_STATE_HOME: &str = "GNOSISVPN_HOME";

#[cfg(target_os = "linux")]
pub const DEFAULT_STATE_HOME: &str = "/var/lib/gnosisvpn";
#[cfg(target_os = "macos")]
pub const DEFAULT_STATE_HOME: &str = "/Library/Application Support/GnosisVPN";

const CONFIG_DIRECTORY: &str = ".config";
const CACHE_DIRECTORY: &str = ".cache";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Error ensuring home directory: {0}")]
    HomeFolder(DirError),
    #[error("Error ensuring cache directory: {0}")]
    CacheFolder(DirError),
    #[error("Error ensuring config directory: {0}")]
    ConfigFolder(DirError),
}

#[derive(Debug, Error)]
pub enum DirError {
    #[error("Cannot create directory: {0}")]
    Creation(String),
    #[error("Cannot adjust ownership: {0}")]
    Ownership(String),
}

// Sets up the required directories for the worker, ensuring they are owned by the worker user
// tracing is not yet enabled so we cannot use it
pub fn setup_home(home: PathBuf, uid: u32, gid: u32) -> Result<(), Error> {
    ensure_dir(home.clone(), 0o755, uid, gid).map_err(Error::HomeFolder)?;
    let cache_path = home.join(CACHE_DIRECTORY);
    ensure_dir(cache_path, 0o700, uid, gid).map_err(Error::CacheFolder)?;
    let config_path = home.join(CONFIG_DIRECTORY);
    ensure_dir(config_path, 0o700, uid, gid).map_err(Error::ConfigFolder)?;
    Ok(())
}

pub fn cache_dir(home: PathBuf, file: &str) -> PathBuf {
    home.join(CACHE_DIRECTORY).join(file)
}

pub fn config_dir(home: PathBuf, file: &str) -> PathBuf {
    home.join(CONFIG_DIRECTORY).join(file)
}

// Ensures that the specified directory exists with the given permissions and ownership.
pub fn ensure_dir(path: PathBuf, mode: u32, uid: u32, gid: u32) -> Result<(), DirError> {
    DirBuilder::new()
        .recursive(true)
        .mode(mode)
        .create(path.clone())
        .map_err(|error| {
            let msg = format!("Failed to create directory at {path}: {error:?}", path = path.display());
            DirError::Creation(msg)
        })?;

    unix_fs::chown(path.clone(), Some(uid), Some(gid)).map_err(|error| {
        let msg = format!("Failed to set ownership at {path}: {error:?}", path = path.display());
        DirError::Ownership(msg)
    })?;
    Ok(())
}
