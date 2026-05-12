use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::keypair::errors::KeyPairError;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::{HoprKeys, IdentityRetrievalModes};
use rand::distr::Alphanumeric;
use rand::prelude::*;
use thiserror::Error;

use std::path::Path;
use std::path::PathBuf;

use crate::dirs;

pub const ID_FILE: &str = "gnosisvpn-hopr.id";
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

pub fn from_path(file: PathBuf, password: String) -> Result<HoprKeys, Error> {
    let id_path = file.to_string_lossy().to_string();
    let retrieval_mode = IdentityRetrievalModes::FromFile {
        password: password.as_str(),
        id_path: id_path.as_str(),
    };
    HoprKeys::try_from(retrieval_mode).map_err(Error::KeyPair)
}

pub fn get_identity(file: &Path, password: &str) -> Result<([u8; 32], Address), Error> {
    let keys = from_path(file.to_path_buf(), password.to_string())?;
    let private_key: [u8; 32] = keys
        .chain_key
        .secret()
        .as_ref()
        .try_into()
        .expect("chain key secret is 32 bytes");
    let address = keys.chain_key.public().to_address();
    Ok((private_key, address))
}

pub fn file(state_home: PathBuf) -> PathBuf {
    dirs::config_dir(state_home, ID_FILE)
}

pub fn pass_file(state_home: PathBuf) -> PathBuf {
    dirs::config_dir(state_home, ID_PASS)
}

pub fn generate_pass() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}
