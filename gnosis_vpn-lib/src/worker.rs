use thiserror::Error;
use tokio::process::Command;
use uzers::os::unix::UserExt;

use std::io;
use std::path::PathBuf;

use crate::shell_command_ext::{self, Logs, ShellCommandExt};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Worker user not found")]
    UserNotFound,
    #[error("Worker binary not found")]
    BinaryNotFound,
    #[error("Worker executable check failed")]
    NotExecutable,
    #[error("Worker binary version mismatch")]
    VersionMismatch,
    #[error("Worker primary group missing")]
    PrimaryGroupMissing,
    #[error("Invalid worker binary path")]
    InvalidBinaryPath,
    #[error("Shell command error: {0}")]
    ShellCommandExt(#[from] shell_command_ext::Error),
    #[error("IO error: {0}")]
    IO(#[from] io::Error),
}

#[derive(Debug)]
pub struct Input {
    user: String,
    binary: PathBuf,
    version: String,
}

pub const USERNAME: &str = "gnosisvpn";
pub const GROUPNAME: &str = "gnosisvpn";

pub const DEFAULT_WORKER_BINARY: &str = "./gnosis_vpn-worker";
pub const DEFAULT_WORKER_USER: &str = USERNAME;
pub const ENV_VAR_WORKER_BINARY: &str = "GNOSISVPN_WORKER_BINARY";
pub const ENV_VAR_WORKER_USER: &str = "GNOSISVPN_WORKER_USER";

#[derive(Debug, Clone)]
pub struct Worker {
    pub uid: u32,
    pub gid: u32,
    pub group_name: String,
    pub binary: String,
    pub home: PathBuf,
}

impl Input {
    pub fn new(user: String, binary: PathBuf, version: &str) -> Self {
        Self {
            user,
            binary,
            version: version.to_string(),
        }
    }
}

impl Worker {
    pub async fn from_system(input: Input) -> Result<Self, Error> {
        let worker_user = uzers::get_user_by_name(input.user.as_str()).ok_or(Error::UserNotFound)?;
        let path = input.binary.canonicalize()?;
        let binary = path.to_str().ok_or(Error::InvalidBinaryPath)?;
        // check if binary exists
        if !path.exists() {
            tracing::error!(path = %binary, ?worker_user, "Worker binary not found");
            return Err(Error::BinaryNotFound);
        }
        if !path.is_file() {
            tracing::error!(path = %binary, ?worker_user, "Worker binary is not a file");
            return Err(Error::NotExecutable);
        }
        // check if binary exists
        let uid = worker_user.uid();
        let gid = worker_user.primary_group_id();
        let group = worker_user
            .groups()
            .ok_or(Error::PrimaryGroupMissing)?
            .into_iter()
            .find(|g| g.gid() == gid)
            .ok_or(Error::PrimaryGroupMissing)?;
        let group_name = group.name().to_string_lossy().to_string();

        tracing::debug!(path = %binary, ?worker_user, "Verifying worker binary executable permissions");
        let version_output = Command::new(binary)
            .arg("--version")
            .uid(uid)
            .gid(gid)
            .run_stdout(Logs::Print)
            .await?;

        let version = version_output.split_whitespace().nth(1).unwrap_or_default();
        if version == input.version {
            let home = worker_user.home_dir().to_path_buf();
            Ok(Worker {
                uid,
                binary: binary.to_string(),
                gid,
                group_name,
                home,
            })
        } else {
            tracing::error!(expected = input.version, found = %version, "Worker binary version mismatch");
            Err(Error::VersionMismatch)
        }
    }
}
