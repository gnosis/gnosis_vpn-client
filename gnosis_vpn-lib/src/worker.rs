use thiserror::Error;
use tokio::process::Command;
use users::os::unix::UserExt;

use std::path::PathBuf;

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
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
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

#[derive(Debug)]
pub struct Worker {
    pub uid: u32,
    pub gid: u32,
    pub binary: PathBuf,
}

impl Worker {
    pub async fn from_system(input: Input) -> Result<Self, Error> {
        let worker_user = users::get_user_by_name(input.worker_user.as_str()).ok_or(Error::UserNotFound)?;
        let home = worker_user.home_dir();
        let path = home.join(input.worker_binary);
        // check if path exists
        if !path.exists() {
            tracing::error!(path = path.display() , %home, user = worker_user.username, "Worker binary not found");
            return Err(Error::BinaryNotFound);
        }

        let uid = worker_user.uid();
        let gid = worker_user.primary_group_id();
        // check if executable and version matches
        let output = Command::new(path).arg("--version").uid(uid).gid(gid).output().await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let status_code = output.status.code();
            tracing::error!(?status_code, %stdout, %stderr, "Failed to check worker binary version");
            return Err(Error::NotExecutable);
        }

        if stdout.trim() == input.version {
            Ok(Worker { uid, gid, binary: path })
        } else {
            tracing::error!(expected = input.version, actual = %stdout.trim(), "Worker binary version mismatch");
            Err(Error::VersionMismatch)
        }
    }
}
