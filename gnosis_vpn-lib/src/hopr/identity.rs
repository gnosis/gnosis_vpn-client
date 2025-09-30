use edgli::hopr_lib::exports::crypto::keypair::errors::KeyPairError;
use edgli::hopr_lib::{HoprKeys, IdentityRetrievalModes};
use rand::Rng;
use rand::distr::Alphanumeric;
use thiserror::Error;

use std::fs;
use std::path::{Path, PathBuf};

use crate::dirs;

const ID_FILE: &str = "gnosisvpn-hopr.id";
const ID_PASS: &str = "gnosisvpn-hopr.pass";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Hopr key pair error: {0}")]
    KeyPair(#[from] KeyPairError),
    #[error("Unable to determine project directories")]
    ProjectDirs,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
}

pub fn from_path(file: &Path, pass: String) -> Result<HoprKeys, Error> {
    let id_path_owned = file.to_string_lossy().into_owned();
    let retrieval_mode = IdentityRetrievalModes::FromFile {
        password: pass.as_str(),
        id_path: id_path_owned.as_str(),
    };
    HoprKeys::try_from(retrieval_mode).map_err(Error::KeyPair)
}

pub fn identity_file() -> Result<PathBuf, Error> {
    config_dir(ID_FILE)
}

pub fn identity_pass() -> Result<PathBuf, Error> {
    config_dir(ID_PASS)
}

fn config_dir(file: &str) -> Result<PathBuf, Error> {
    let p_dirs = dirs::project().ok_or(Error::ProjectDirs)?;
    let config_dir = p_dirs.config_dir();
    fs::create_dir_all(config_dir)?;
    Ok(config_dir.join(file))
}

pub fn generate_pass() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}
