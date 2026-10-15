use edgli::hopr_lib::{HoprKeys, IdentityRetrievalModes};
use rand::distr::Alphanumeric;
use rand::prelude::*;
use thiserror::Error;

use std::path::{Path, PathBuf};

pub const ID_FILE: &str = "hopr.id";
const ID_PASS: &str = "hopr.pass";

#[derive(Debug, Error)]
pub enum Error {
    #[error("keypair error: {0}")]
    KeyPair(#[source] anyhow::Error),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

pub fn from_path(file: PathBuf, pass: String) -> Result<HoprKeys, Error> {
    let id_path = file.to_string_lossy().to_string();
    let retrieval_mode = IdentityRetrievalModes::FromFile {
        password: pass.as_str(),
        id_path: id_path.as_str(),
    };
    HoprKeys::try_from(retrieval_mode).map_err(|e| Error::KeyPair(anyhow::anyhow!(e)))
}

pub fn file(network_data_dir: &Path) -> PathBuf {
    network_data_dir.join(ID_FILE)
}

pub fn pass_file(network_data_dir: &Path) -> PathBuf {
    network_data_dir.join(ID_PASS)
}

pub fn generate_pass() -> String {
    rand::rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}
