use serde::{Deserialize, Serialize};
use std::default::Default;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

use crate::dirs;

#[derive(Default, Debug, Deserialize, Serialize)]
pub struct State {
    wg_private_key: Option<String>,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("State folder error")]
    NoStateFolder,
    #[error("State file not found")]
    NoFile,
    #[error("Error determining parent folder")]
    NoParentFolder,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Serialization/Deserialization error: {0}")]
    BinCodeError(#[from] bincode::Error),
}

fn path() -> Option<PathBuf> {
    let project_dirs = dirs::project()?;
    let data_dir = project_dirs.data_local_dir();
    let state_file = data_dir.join("state.bin");
    Some(state_file)
}

pub fn read() -> Result<State, Error> {
    let p = path().ok_or(Error::NoStateFolder)?;
    let content = fs::read(p).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    let state: State = bincode::deserialize(&content[..]).map_err(Error::BinCodeError)?;
    Ok(state)
}

impl State {
    pub fn set_wg_private_key(&mut self, key: String) -> Result<(), Error> {
        self.wg_private_key = Some(key);
        self.write()
    }

    pub fn wg_private_key(&self) -> Option<String> {
        self.wg_private_key.clone()
    }

    fn write(&self) -> Result<(), Error> {
        let path = path().ok_or(Error::NoStateFolder)?;
        let content = bincode::serialize(&self)?;
        let parent = path.parent().ok_or(Error::NoParentFolder)?;
        fs::create_dir_all(parent)?;
        fs::write(path, content).map_err(Error::IO)
    }
}
