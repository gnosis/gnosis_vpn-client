use thiserror::Error;

use std::fs::DirBuilder;
use std::io::{self, ErrorKind};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::{self as unix_fs};
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
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

// Sets up the required directories for the worker, ensuring they are owned by the worker user
// tracing is not yet enabled so we avoid it
pub fn setup_worker(home: PathBuf, uid: u32, gid: u32) -> Result<PathBuf, String> {
    // home folder will be created by installer
    let cache_path = home.join(CACHE_DIRECTORY);
    let config_path = home.join(CONFIG_DIRECTORY);
    ensure_dir_with_owner(&cache_path, uid, gid).map_err(|error| {
        format!("Failed to create cache directory at {cache}: {error:?}", cache = cache_path.display())
    })?;
    ensure_dir_with_owner(&config_path, uid, gid).map_err(|error| {
        format!("Failed to create config directory at {config}: {error:?}", config = config_path.display())
    })?;
    Ok(home)
}

pub fn cache_dir(home: PathBuf, file: &str) -> Result<PathBuf, Error> {
    let cache_file = home.join(CACHE_DIRECTORY).join(file);
    tracing::debug!("Using cache file: {}", cache_file.display());
    Ok(cache_file)
}

pub fn config_dir(home: PathBuf, file: &str) -> Result<PathBuf, Error> {
    let config_file = home.join(CONFIG_DIRECTORY).join(file);
    tracing::debug!("Using config file: {}", config_file.display());
    Ok(config_file)
}

fn ensure_dir_with_owner(path: &PathBuf, uid: u32, gid: u32) -> Result<(), io::Error> {
    let res = DirBuilder::new().mode(0o700).create(path);
    match res {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }?;
    unix_fs::chown(path, Some(uid), Some(gid))
}
