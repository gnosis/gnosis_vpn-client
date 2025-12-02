use thiserror::Error;
use tokio::process::Command;
use users::os::unix::UserExt;

use std::path::PathBuf;

use crate::util::CommandExt;

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
    #[error("Command error: {0}")]
    Command(#[from] crate::util::Error),
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
    pub group_name: String,
    pub binary: String,
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
        let worker_user = users::get_user_by_name(input.user.as_str()).ok_or(Error::UserNotFound)?;
        let home = worker_user.home_dir();
        let path = home.join(input.binary);
        let binary = path.to_str().ok_or(Error::InvalidBinaryPath)?;
        // check if binary exists
        if !path.exists() {
            tracing::error!(path = %binary, home = %home.display(), user = %input.user, "Worker binary not found");
            return Err(Error::BinaryNotFound);
        }

        let uid = worker_user.uid();
        let gid = worker_user.primary_group_id();
        let group = worker_user
            .groups()
            .ok_or(Error::PrimaryGroupMissing)?
            .into_iter()
            .find(|g| g.gid() == gid)
            .ok_or(Error::PrimaryGroupMissing)?;
        let group_name = group.name().to_string_lossy().to_string();

        let actual = Command::new(binary)
            .arg("--version")
            .uid(uid)
            .gid(gid)
            .run_stdout()
            .await?;
        if actual == input.version {
            Ok(Worker {
                uid,
                binary: binary.to_string(),
                gid,
                group_name,
            })
        } else {
            tracing::error!(expected = input.version, actual = %actual, "Worker binary version mismatch");
            Err(Error::VersionMismatch)
        }
    }
}
