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
    #[error(transparent)]
    KeyPair(#[from] KeyPairError),
    #[error("Unable to determine project directories")]
    ProjectDirs,
    #[error(transparent)]
    IO(#[from] std::io::Error),
    #[error(transparent)]
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

pub fn file(state_home: PathBuf) -> Result<PathBuf, Error> {
    dirs::config_dir(state_home, ID_FILE).map_err(Error::Dirs)
}

pub fn pass_file(state_home: PathBuf) -> Result<PathBuf, Error> {
    dirs::config_dir(state_home, ID_PASS).map_err(Error::Dirs)
}

pub fn generate_pass() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}
