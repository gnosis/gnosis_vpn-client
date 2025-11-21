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
    fn cache_dir_creates_directory_and_appends_file_name() -> anyhow::Result<()> {
        let file_name = "test_cache.txt";
        let path = cache_dir(file_name).expect("cache dir should be creatable on writable systems");

        path.parent().expect("cache path should include parent directories");
        assert!(path.ends_with(file_name), "cache path keeps the provided filename");
        Ok(())
    }

    #[test]
    #[ignore = "fails in the CI as nix is a readonly environment"]
    fn config_dir_creates_directory_and_appends_file_name() -> anyhow::Result<()> {
        let file_name = "test_config.toml";
        let path = config_dir(file_name).expect("config dir should be creatable on writable systems");

        path.parent().expect("config path should include parent directories");
        assert!(path.ends_with(file_name), "config path keeps the provided filename");
        Ok(())
    }
}
