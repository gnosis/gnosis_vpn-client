use thiserror::Error;
use tokio::process::Command;
use tokio::time;
use uzers::os::unix::UserExt;

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::dirs::{self, UserDirs};
use crate::shell_command_ext::{self, Logs, ShellCommandExt};

#[derive(Debug, Error)]
pub enum Error {
    #[error("user {uid} not found")]
    UserNotFound { uid: u32 },
    #[error("home directory {} of user {uid} is not a directory", home.display())]
    HomeMissing { uid: u32, home: PathBuf },
    #[error("worker binary {} not found", path.display())]
    BinaryNotFound { path: PathBuf },
    #[error("worker binary {} is not a file", path.display())]
    NotAFile { path: PathBuf },
    #[error("worker binary version mismatch: expected {expected}, found {found}")]
    VersionMismatch { expected: String, found: String },
    #[error("worker binary version probe timed out after {0:?}")]
    ProbeTimeout(Duration),
    #[error("worker binary version probe failed: {0}")]
    Probe(#[from] shell_command_ext::Error),
}

pub const DEFAULT_WORKER_BINARY: &str = "./gnosis_vpn-worker";
pub const ENV_VAR_WORKER_BINARY: &str = "GNOSISVPN_WORKER_BINARY";

/// The probe runs inline in root's daemon loop, so a hanging binary must not hang the service.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The local user a worker runs as; root records it for the lifetime of that worker.
#[derive(Debug, Clone)]
pub struct WorkerOwner {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    /// The passwd home: upstream crates read `HOME`, nothing in this repo does.
    pub home: PathBuf,
    pub dirs: UserDirs,
}

impl WorkerOwner {
    pub fn from_uid(uid: u32, state_home_override: Option<&Path>) -> Result<Self, Error> {
        let user = uzers::get_user_by_uid(uid).ok_or(Error::UserNotFound { uid })?;
        let home = user.home_dir().to_path_buf();
        if !home.is_dir() {
            return Err(Error::HomeMissing { uid, home });
        }
        Ok(Self {
            uid,
            gid: user.primary_group_id(),
            name: user.name().to_string_lossy().to_string(),
            dirs: dirs::user_dirs(&home, state_home_override),
            home,
        })
    }
}

/// Resolves the worker binary once at startup as root; whether the owner may execute it is probed at spawn.
pub fn resolve_binary(path: &Path) -> Result<PathBuf, Error> {
    let canonical = path.canonicalize().map_err(|_| Error::BinaryNotFound {
        path: path.to_path_buf(),
    })?;
    if !canonical.is_file() {
        return Err(Error::NotAFile { path: canonical });
    }
    Ok(canonical)
}

/// Runs `<binary> --version` as the owner: one check for exec permission and version in a single step.
pub async fn probe_version(binary: &Path, owner: &WorkerOwner, expected: &str) -> Result<(), Error> {
    let mut command = Command::new(binary);
    // A timed-out probe must not survive as an orphan.
    command
        .arg("--version")
        .uid(owner.uid)
        .gid(owner.gid)
        .kill_on_drop(true);
    let output = time::timeout(VERSION_PROBE_TIMEOUT, command.run_stdout(Logs::Print))
        .await
        .map_err(|_| Error::ProbeTimeout(VERSION_PROBE_TIMEOUT))??;
    let found = output.split_whitespace().nth(1).unwrap_or_default();
    if found != expected {
        return Err(Error::VersionMismatch {
            expected: expected.to_string(),
            found: found.to_string(),
        });
    }
    Ok(())
}
