use edgli::hopr_lib::{HoprKeys, IdentityRetrievalModes};
use rand::distr::Alphanumeric;
use rand::prelude::*;
use thiserror::Error;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const ID_FILE: &str = "hopr.id";
const ID_PASS: &str = "hopr.pass";

#[derive(Debug, Error)]
pub enum Error {
    #[error("keypair error: {0}")]
    KeyPair(String),
    #[error(transparent)]
    IO(#[from] std::io::Error),
}

pub fn from_path(file: PathBuf, pass: String) -> Result<HoprKeys, Error> {
    let id_path = file.to_string_lossy().to_string();
    let retrieval_mode = IdentityRetrievalModes::FromFile {
        password: pass.as_str(),
        id_path: id_path.as_str(),
    };
    let keys = HoprKeys::try_from(retrieval_mode).map_err(|e| Error::KeyPair(e.to_string()))?;
    restrict_to_owner(&file)?;
    Ok(keys)
}

/// hopr-types writes the id file without a mode, so it comes out with the umask's group/other bits.
fn restrict_to_owner(file: &Path) -> Result<(), Error> {
    let old_mode = std::fs::metadata(file)?.permissions().mode() & 0o777;
    let readable_by_others = old_mode & 0o077 != 0;
    if readable_by_others {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))?;
        tracing::info!(path = ?file, old_mode = format!("{old_mode:o}"), "restricted identity file to its owner");
    }
    Ok(())
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
