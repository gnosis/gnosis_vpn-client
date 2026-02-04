use thiserror::Error;

use std::{fs, io, os::unix::fs::PermissionsExt, path::PathBuf};

#[derive(Debug, Error)]
pub enum Error {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

const CONFIG_DIRECTORY: &str = ".config";
const CACHE_DIRECTORY: &str = ".cache";

pub const ENV_VAR_HOME: &str = "GNOSISVPN_HOME";
pub const DEFAULT_STATE_DIR_LINUX: &str = "/var/lib/gnosisvpn";
pub const DEFAULT_STATE_DIR_MACOS: &str = "/Library/Application Support/GnosisVPN";

pub fn setup(uid: u32, gid: u32) -> Result<PathBuf, Error> {
    let home = home();
    let cache_path = home.join(CACHE_DIRECTORY);
    let config_path = home.join(CONFIG_DIRECTORY);
    fs::create_dir_all(&cache_path)?;
    fs::create_dir_all(&config_path)?;
    for entry in walkdir::WalkDir::new(&home.clone()) {
        let entry = entry.map_err(|e| std::io::Error::other(e))?;

        #[cfg(target_os = "macos")]
        if entry.path().ends_with("uninstall.sh") {
            continue;
        }
        fs::set_permissions(entry.path(), fs::Permissions::from_mode(0o700))?;
        std::os::unix::fs::chown(entry.path(), Some(uid), Some(gid))?;
    }

    tracing::debug!("Using gnosisvpn home directory: {}", home.display());
    Ok(home)
}

pub fn cache_dir(file: &str) -> Result<PathBuf, Error> {
    let cache_path = home().join(CACHE_DIRECTORY);
    let cache_file = cache_path.join(file);
    tracing::debug!("Using cache file: {}", cache_file.display());
    fs::create_dir_all(&cache_path)?;
    Ok(cache_file)
}

pub fn config_dir(file: &str) -> Result<PathBuf, Error> {
    let config_path = home().join(CONFIG_DIRECTORY);
    let config_file = config_path.join(file);
    tracing::debug!("Using config file: {}", config_file.display());
    fs::create_dir_all(&config_path)?;
    Ok(config_file)
}

fn home() -> PathBuf {
    if let Ok(home) = std::env::var(ENV_VAR_HOME) {
        return PathBuf::from(home);
    }

    #[cfg(target_os = "macos")]
    {
        PathBuf::from(DEFAULT_STATE_DIR_MACOS)
    }
    #[cfg(not(target_os = "macos"))]
    {
        PathBuf::from(DEFAULT_STATE_DIR_LINUX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_MUTEX: Mutex<()> = Mutex::new(());

    fn with_env_var<F>(value: Option<&str>, test: F)
    where
        F: FnOnce(),
    {
        let key = ENV_VAR_HOME;
        let _guard = ENV_MUTEX.lock().unwrap();
        let original_value = env::var(key).ok();

        match value {
            Some(v) => unsafe { env::set_var(key, v) },
            None => unsafe { env::remove_var(key) },
        }

        // Run the test
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test));

        // Restore original value
        match original_value {
            Some(v) => unsafe { env::set_var(key, v) },
            None => unsafe { env::remove_var(key) },
        }

        if let Err(err) = result {
            std::panic::resume_unwind(err);
        }
    }

    #[test]
    fn test_custom_gnosisvpn_home() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let temp_path = temp_dir.path().to_path_buf();
        let temp_path_str = temp_path.to_str().unwrap();

        with_env_var(Some(temp_path_str), || {
            let home = home();
            assert_eq!(home, temp_path, "Home should match the custom environment variable");

            let cache = cache_dir("test").unwrap();
            assert!(cache.starts_with(&temp_path), "Cache dir should be under custom home");

            let config = config_dir("test").unwrap();
            assert!(config.starts_with(&temp_path), "Config dir should be under custom home");
        });
    }

    #[test]
    fn test_default_gnosisvpn_home_unset() {
        with_env_var(None, || {
            let home = home();

            #[cfg(target_os = "macos")]
            assert_eq!(home, PathBuf::from(DEFAULT_STATE_DIR_MACOS));

            #[cfg(not(target_os = "macos"))]
            assert_eq!(home, PathBuf::from(DEFAULT_STATE_DIR_LINUX));
        });
    }

    #[test]
    fn test_gnosisvpn_home_directories_created() {
        let temp_dir = tempdir().expect("failed to create temp dir");
        let temp_path = temp_dir.path().to_path_buf();
        let temp_path_str = temp_path.to_str().unwrap();

        with_env_var(Some(temp_path_str), || {
            // Act
            let _ = cache_dir("some_file").unwrap();
            let _ = config_dir("some_file").unwrap();

            // Assert
            let cache_exists = temp_path.join(CACHE_DIRECTORY).is_dir();
            let config_exists = temp_path.join(CONFIG_DIRECTORY).is_dir();

            assert!(cache_exists, ".cache directory should be created");
            assert!(config_exists, "config directory should be created");
        });
    }
}
