use directories::ProjectDirs;
use thiserror::Error;

use std::{fs, io, path::PathBuf};

const DOMAIN: &str = "org";
const COMPANY: &str = "hoprnet";
const PRODUCT: &str = "gnosisvpn";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Unable to determine project directories")]
    NoProjectDirs,
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

fn project() -> Option<ProjectDirs> {
    ProjectDirs::from(DOMAIN, COMPANY, PRODUCT)
}

pub fn cache_dir(file: &str) -> Result<PathBuf, Error> {
    let p_dirs = project().ok_or(Error::NoProjectDirs)?;
    let cache_dir = p_dirs.cache_dir();
    fs::create_dir_all(cache_dir)?;
    Ok(cache_dir.join(file))
}

pub fn config_dir(file: &str) -> Result<PathBuf, Error> {
    let p_dirs = project().ok_or(Error::NoProjectDirs)?;
    let config_dir = p_dirs.config_dir();
    fs::create_dir_all(config_dir)?;
    Ok(config_dir.join(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "fails in the CI as nix is a readonly environment"]
    fn test_cache_dir_creates_directory_and_returns_file_path() {
        let file_name = "test_cache.txt";
        let path = cache_dir(file_name).expect("cache_dir should succeed");

        path.parent().expect("Path should have a parent");
        assert!(path.ends_with(file_name), "Returned path should end with file name");
    }

    #[test]
    #[ignore = "fails in the CI as nix is a readonly environment"]
    fn test_config_dir_creates_directory_and_returns_file_path() {
        let file_name = "test_config.toml";
        let path = config_dir(file_name).expect("config_dir should succeed");

        path.parent().expect("Path should have a parent");
        assert!(path.ends_with(file_name), "Returned path should end with file name");
    }
}
