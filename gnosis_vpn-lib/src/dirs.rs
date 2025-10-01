use directories::ProjectDirs;
use thiserror::Error;

use std::path::PathBuf;
use std::{fs, io};

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
