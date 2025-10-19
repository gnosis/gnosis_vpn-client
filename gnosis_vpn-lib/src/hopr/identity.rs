use edgli::hopr_lib::exports::crypto::keypair::errors::KeyPairError;
use edgli::hopr_lib::{HoprKeys, IdentityRetrievalModes};
use rand::Rng;
use rand::distr::Alphanumeric;
use thiserror::Error;

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
    #[error("Project directory error: {0}")]
    Dirs(#[from] crate::dirs::Error),
}

pub fn from_path(file: &Path, pass: String) -> Result<HoprKeys, Error> {
    let id_path_owned = file.to_string_lossy().into_owned();
    let retrieval_mode = IdentityRetrievalModes::FromFile {
        password: pass.as_str(),
        id_path: id_path_owned.as_str(),
    };
    HoprKeys::try_from(retrieval_mode).map_err(Error::KeyPair)
}

pub fn file() -> Result<PathBuf, Error> {
    dirs::config_dir(ID_FILE).map_err(Error::Dirs)
}

pub fn pass_file() -> Result<PathBuf, Error> {
    dirs::config_dir(ID_PASS).map_err(Error::Dirs)
}

pub fn generate_pass() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}
