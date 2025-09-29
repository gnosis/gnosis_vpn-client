use edgli::hopr_lib::exports::crypto::keypair::errors::KeyPairError;
use edgli::hopr_lib::{HoprKeys, IdentityRetrievalModes};
use thiserror::Error;

use std::path::Path;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Hopr key pair error: {0}")]
    KeyPair(#[from] KeyPairError),
}

pub fn from_path(file: &Path, pass: String) -> Result<HoprKeys, Error> {
    let id_path_owned = file.to_string_lossy().into_owned();
    let retrieval_mode = IdentityRetrievalModes::FromFile {
        password: pass.as_str(),
        id_path: id_path_owned.as_str(),
    };
    HoprKeys::try_from(retrieval_mode).map_err(Error::KeyPair)
}
