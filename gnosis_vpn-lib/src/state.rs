use bincode::{Decode, Encode};
use std::default::Default;
use std::fs::{self, File};
use std::path::PathBuf;
use thiserror::Error;

use crate::dirs;

#[derive(Default, Debug, Decode, Encode)]
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
    #[error("Serialization error: {0}")]
    BinCodeEncodeError(#[from] bincode::error::EncodeError),
    #[error("Deserialization error: {0}")]
    BinCodeDecodeError(#[from] bincode::error::DecodeError),
}

fn path() -> Option<PathBuf> {
    let project_dirs = dirs::project()?;
    let data_dir = project_dirs.data_local_dir();
    let state_file = data_dir.join("state-v2.bin");
    Some(state_file)
}

pub fn read() -> Result<State, Error> {
    let p = path().ok_or(Error::NoStateFolder)?;
    let mut f = File::open(&p).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::NoFile
        } else {
            Error::IO(e)
        }
    })?;
    bincode::decode_from_std_read(&mut f, bincode::config::standard()).map_err(Error::BinCodeDecodeError)
}

impl State {
    pub fn set_wg_private_key(&mut self, key: String) -> Result<usize, Error> {
        self.wg_private_key = Some(key);
        self.write()
    }

    pub fn wg_private_key(&self) -> Option<String> {
        self.wg_private_key.clone()
    }

    fn write(&self) -> Result<usize, Error> {
        let path = path().ok_or(Error::NoStateFolder)?;
        let parent = path.parent().ok_or(Error::NoParentFolder)?;
        fs::create_dir_all(parent)?;
        let mut f = File::create(&path)?;
        bincode::encode_into_std_write(&self, &mut f, bincode::config::standard()).map_err(Error::BinCodeEncodeError)
    }
}
