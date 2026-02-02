use directories::ProjectDirs;
use thiserror::Error;

use std::{fs, io, path::PathBuf};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Unable to determine project directories")]
    NoProjectDirs,
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

fn project() -> Result<ProjectDirs, Error> {
    let reverse_domain = crate::IDENTIFIER;
    let parts: Vec<&str> = reverse_domain.split('.').collect();
    if parts.len() < 2 {
        return Err(Error::NoProjectDirs);
    }
    let dirs = ProjectDirs::from(parts[0], parts[1], parts[2]);
    dirs.ok_or(Error::NoProjectDirs)
}

pub fn cache_dir(file: &str) -> Result<PathBuf, Error> {
    let dir = if let Ok(home) = std::env::var("GNOSISVPN_HOME") {
        let path = PathBuf::from(home).join("cache");
        tracing::debug!("Using GNOSISVPN_HOME for cache directory: {}", path.display());
        path
    } else {
        let pdir = project()?;
        let dir = pdir.cache_dir();
        tracing::debug!("Ensuring cache directory: {}", dir.display());
        dir.to_path_buf()
    };

    fs::create_dir_all(&dir)?;
    Ok(dir.join(file))
}

pub fn config_dir(file: &str) -> Result<PathBuf, Error> {
    let dir = if let Ok(home) = std::env::var("GNOSISVPN_HOME") {
        let path = PathBuf::from(home);
        tracing::debug!("Using GNOSISVPN_HOME for config directory: {}", path.display());
        path
    } else {
        let pdir = project()?;
        let dir = pdir.config_dir();
        tracing::debug!("Ensuring config directory: {}", dir.display());
        dir.to_path_buf()
    };

    fs::create_dir_all(&dir)?;
    Ok(dir.join(file))
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
