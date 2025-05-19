use std::io;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use thiserror::Error;

use crate::command::Command;

#[derive(Debug, Error)]
pub enum Error {
    #[error("service not running")]
    ServiceNotRunning,
    #[error("error accessing socket at `{socket_path}`: {error}")]
    SocketPathIO { socket_path: PathBuf, error: io::Error },
    #[error("error connecting socket at `{socket_path:?}`: {error:?}")]
    ConnectSocketIO { socket_path: PathBuf, error: io::Error },
    #[error("failed serializing command: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("error writing to socket at: {0}")]
    WriteSocketIO(io::Error),
    #[error("error reading from socket: {0}")]
    ReadSocketIO(io::Error),
}

pub const DEFAULT_PATH: &str = "/var/run/gnosis_vpn.sock";
pub const ENV_VAR: &str = "GNOSISVPN_SOCKET_PATH";

// #[cfg(target_family = "windows")]
// pub fn socket_path() -> PathBuf {
// PathBuf::from("//./pipe/Gnosis VPN")
// }

pub fn process_cmd(socket_path: &Path, cmd: &Command) -> Result<String, Error> {
    check_path(socket_path)?;

    let mut stream = UnixStream::connect(socket_path).map_err(|x| Error::ConnectSocketIO {
        socket_path: socket_path.to_path_buf(),
        error: x,
    })?;

    let json_cmd = serde_json::to_string(cmd)?;
    push_command(&mut stream, &json_cmd)?;
    pull_response(&mut stream)
}

fn check_path(socket_path: &Path) -> Result<(), Error> {
    match socket_path.try_exists() {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::ServiceNotRunning),
        Err(x) => Err(Error::SocketPathIO {
            socket_path: socket_path.to_path_buf(),
            error: x,
        }),
    }
}

fn push_command(socket: &mut UnixStream, json_cmd: &str) -> Result<(), Error> {
    // flush is not enough to push the command
    // we need to shutdown the write channel to signal the other side that all data was transferred
    socket
        .write_all(json_cmd.as_bytes())
        .map(|_| socket.flush())
        .and_then(|_| socket.shutdown(std::net::Shutdown::Write))
        .map_err(Error::WriteSocketIO)
}

fn pull_response(socket: &mut UnixStream) -> Result<String, Error> {
    let mut response = String::new();
    let res = socket.read_to_string(&mut response);
    match res {
        Ok(_) => Ok(response),
        Err(x) => Err(Error::ReadSocketIO(x)),
    }
}
