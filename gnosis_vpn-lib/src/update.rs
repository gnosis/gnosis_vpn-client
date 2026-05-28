//! Update install engine + client wrapper.
//!
//! The engine ([`install_engine`]) runs inside the root daemon and drives the
//! download → verify → install pipeline, emitting [`UpdateStatus`] on a tokio
//! mpsc channel. The IPC layer forwards each status to subscribed sockets
//! framed as newline-delimited JSON.
//!
//! The client wrapper ([`install_stream`]) is consumed by both `gnosis_vpn-ctl`
//! and the Tauri GUI (in a separate repo); it opens a streaming socket
//! connection to the daemon, sends `Command::StartUpdate`, and yields the
//! same `UpdateStatus` values until the stream closes.

use std::cmp::Ordering;
use std::path::PathBuf;
#[cfg(not(target_os = "linux"))]
use std::time::{Duration, SystemTime};

use bytesize::ByteSize;
use futures_util::Stream;
use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
#[cfg(not(target_os = "linux"))]
use sha2::{Digest, Sha256};
#[cfg(not(target_os = "linux"))]
use tokio::fs::OpenOptions;
#[cfg(not(target_os = "linux"))]
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
#[cfg(not(target_os = "linux"))]
use tokio::time::Instant;

use crate::check_update::{self, Channel, ChannelRelease};

pub use check_update::Channel as PublicChannel;

/// Install-gate failure modes — distinct from `check_update::Error`, which
/// covers the manifest-fetch path. These are the rejection reasons that
/// apply *after* a manifest is in hand and we're deciding whether a
/// specific `ChannelRelease` should be installed on this host.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("Channel {0} has no release in this manifest")]
    NoReleaseForChannel(Channel),
    #[error("Candidate {candidate} requires app {required} (have {current}); upgrade to an intermediate first")]
    AppTooOld {
        current: String,
        required: String,
        candidate: String,
    },
    #[error("Candidate {candidate} is older than installed {current}; pass --allow-downgrade to override")]
    Downgrade { current: String, candidate: String },
    #[error("Candidate {candidate} is already installed")]
    AlreadyInstalled { candidate: String },
}

/// Componentwise compare for version-like strings.
///
/// Splits on `.`, `-`, and `+`, parses each component as an integer (treating
/// non-numeric chunks as `0`), and compares left-to-right padding the shorter
/// list with `0`. Used for app versions ("0.86.0") and date-based snapshot
/// versions with build metadata ("2026.04.24+build.030921"). Build metadata
/// after `+` therefore participates in the compare, which matches the
/// publishing pipeline's intent (newer builds sort higher).
///
/// Limitation: does **not** implement semver prerelease ordering (`-rc1` <
/// release). If the publishing pipeline ever adopts that style, swap this
/// for the `semver` crate.
pub fn compare_components(a: &str, b: &str) -> Ordering {
    fn parts(s: &str) -> Vec<u64> {
        s.split(|c: char| c == '.' || c == '-' || c == '+')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect()
    }
    let a = parts(a);
    let b = parts(b);
    let n = a.len().max(b.len());
    for i in 0..n {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            Ordering::Less => return Ordering::Less,
            Ordering::Greater => return Ordering::Greater,
            Ordering::Equal => {}
        }
    }
    Ordering::Equal
}

/// Validate a candidate release against the currently running app.
///
/// `current_app_version` is typically `env!("CARGO_PKG_VERSION")`.
/// `allow_downgrade` is the explicit user override — without it,
/// strictly-lower candidates are rejected.
///
/// The manifest's `min_os_version` field is **not** consulted. On Linux the
/// manifest carries Ubuntu-style values that don't compare meaningfully
/// against Debian/Fedora versions, and the `.deb`/`.rpm` artifacts already
/// declare their real package dependencies. On macOS the `.pkg` postinstall
/// surfaces an OS-too-old failure if applicable.
pub fn ensure_installable(
    release: &ChannelRelease,
    current_app_version: &str,
    allow_downgrade: bool,
) -> Result<Ordering, GateError> {
    if compare_components(current_app_version, &release.min_app_version) == Ordering::Less {
        return Err(GateError::AppTooOld {
            current: current_app_version.to_string(),
            required: release.min_app_version.clone(),
            candidate: release.version.clone(),
        });
    }

    let ordering = compare_components(&release.version, current_app_version);
    match ordering {
        Ordering::Equal => Err(GateError::AlreadyInstalled {
            candidate: release.version.clone(),
        }),
        Ordering::Less if !allow_downgrade => Err(GateError::Downgrade {
            current: current_app_version.to_string(),
            candidate: release.version.clone(),
        }),
        _ => Ok(ordering),
    }
}

/// Stage labels carried by `UpdateStatus::Failed`. Mirrors the high-level
/// phases the engine moves through.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum UpdateStage {
    Check,
    Download,
    Verify,
    Install,
}

impl std::fmt::Display for UpdateStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateStage::Check => f.write_str("check"),
            UpdateStage::Download => f.write_str("download"),
            UpdateStage::Verify => f.write_str("verify"),
            UpdateStage::Install => f.write_str("install"),
        }
    }
}

/// Streaming status emitted by the install engine. Each variant is a
/// snapshot — callers should treat the sequence as a state machine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum UpdateStatus {
    Idle,
    Checking,
    Available(ChannelRelease),
    Downloading {
        bytes_done: u64,
        bytes_total: u64,
    },
    Verifying,
    Installing,
    RestartingService,
    Completed {
        new_version: String,
    },
    Failed {
        stage: UpdateStage,
        error: String,
    },
}

impl UpdateStatus {
    /// `true` if no further status will follow this one (channel will close).
    pub fn is_terminal(&self) -> bool {
        matches!(self, UpdateStatus::Completed { .. } | UpdateStatus::Failed { .. })
    }
}

impl std::fmt::Display for UpdateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpdateStatus::Idle => f.write_str("idle"),
            UpdateStatus::Checking => f.write_str("checking for updates"),
            UpdateStatus::Available(r) => write!(f, "update available: {}", r.version),
            UpdateStatus::Downloading {
                bytes_done,
                bytes_total,
            } => write!(
                f,
                "downloading: {} / {}",
                ByteSize::b(*bytes_done),
                ByteSize::b(*bytes_total)
            ),
            UpdateStatus::Verifying => f.write_str("verifying artifact"),
            UpdateStatus::Installing => f.write_str("installing"),
            UpdateStatus::RestartingService => f.write_str("restarting service"),
            UpdateStatus::Completed { new_version } => write!(f, "completed: {new_version}"),
            UpdateStatus::Failed { stage, error } => write!(f, "failed at {stage}: {error}"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("socket error: {0}")]
    Socket(#[from] crate::socket::root::Error),
    #[error("deserialize response: {0}")]
    Deserialize(#[from] serde_json::Error),
    #[error("unexpected response: {0}")]
    Unexpected(String),
}

/// Engine input. Constructed by the daemon before spawning the engine task.
#[derive(Clone, Debug)]
pub struct EngineInput {
    /// HTTPS client to use for manifest + artifact fetch.
    pub client: Client,
    /// Channel to install from.
    pub channel: Channel,
    /// Whether to permit installing an older release.
    pub allow_downgrade: bool,
    /// Compiled-in app version (`env!("CARGO_PKG_VERSION")` at daemon build).
    pub current_app_version: String,
    /// Root-owned directory where the artifact is downloaded.
    pub download_dir: PathBuf,
    /// Optional path to write `last_update_attempt.json` to.
    pub attempt_state_path: Option<PathBuf>,
    /// Optional audit log path; appended to on every terminal status.
    pub audit_log_path: Option<PathBuf>,
    /// Bypass the VPN-connected gate. False in production.
    pub skip_vpn_check: bool,
    /// Socket path for the `ensure_vpn_connected` check.
    pub socket_path: PathBuf,
}

/// Spawn the install engine task and return an `mpsc::Receiver` that yields
/// each `UpdateStatus` until terminal, then closes.
///
/// The engine sequence:
/// 1. `Checking` → manifest fetch + integrity (current: SHA only; PGP TODO)
/// 2. `Available(..)` → release picked
/// 3. `Downloading{..}` → progress emitted at most every ~100 ms
/// 4. `Verifying` → SHA-256 vs manifest
/// 5. `Installing` → platform installer invoked (detached)
/// 6. `RestartingService` → write attempt-state file, then `Completed`
///
/// On any failure the engine emits `Failed { stage, error }` and stops.
///
/// On Linux the engine delegates to apt — see [`crate::update_apt`]. The
/// `channel`, `socket_path`, and `skip_vpn_check` fields are honoured there
/// (the apt path uses the same `ensure_vpn_connected` gate as macOS).
/// `download_dir`, `attempt_state_path`, `audit_log_path`, `allow_downgrade`,
/// and `current_app_version` are macOS-only.
pub fn install_engine(input: EngineInput) -> mpsc::Receiver<UpdateStatus> {
    #[cfg(target_os = "linux")]
    {
        return crate::update_apt::install_engine(input.channel, input.socket_path, input.skip_vpn_check);
    }
    #[cfg(not(target_os = "linux"))]
    {
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(async move { run_engine(input, tx).await });
        rx
    }
}

#[cfg(not(target_os = "linux"))]
async fn run_engine(input: EngineInput, tx: mpsc::Sender<UpdateStatus>) {
    let outcome = drive_engine(&input, &tx).await;
    let last = match outcome {
        Ok(version) => UpdateStatus::Completed { new_version: version },
        Err((stage, error)) => UpdateStatus::Failed { stage, error },
    };
    persist_attempt(&input, &last).await;
    audit_log(&input, &last).await;
    let _ = tx.send(last).await;
}

#[cfg(not(target_os = "linux"))]
async fn drive_engine(
    input: &EngineInput,
    tx: &mpsc::Sender<UpdateStatus>,
) -> Result<String, (UpdateStage, String)> {
    let _ = tx.send(UpdateStatus::Checking).await;

    let socket_gate = (!input.skip_vpn_check).then_some(input.socket_path.as_path());
    let manifest = check_update::download(&input.client, socket_gate)
        .await
        .map_err(|e| (UpdateStage::Check, e.to_string()))?;

    let release = manifest
        .pick(input.channel)
        .cloned()
        .ok_or_else(|| (UpdateStage::Check, GateError::NoReleaseForChannel(input.channel).to_string()))?;

    ensure_installable(&release, &input.current_app_version, input.allow_downgrade)
        .map_err(|e| (UpdateStage::Check, e.to_string()))?;

    let _ = tx.send(UpdateStatus::Available(release.clone())).await;

    let artifact_path = download_artifact(input, &release, tx)
        .await
        .map_err(|e| (UpdateStage::Download, e.to_string()))?;

    let _ = tx.send(UpdateStatus::Verifying).await;
    verify_sha256(&artifact_path, &release)
        .await
        .map_err(|e| (UpdateStage::Verify, e))?;

    let _ = tx.send(UpdateStatus::Installing).await;
    crate::update::install_platform::install(&artifact_path)
        .await
        .map_err(|e| (UpdateStage::Install, e))?;

    let _ = tx.send(UpdateStatus::RestartingService).await;
    Ok(release.version.clone())
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug, thiserror::Error)]
enum DownloadError {
    #[error("http: {0}")]
    Http(#[from] reqwest::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("not enough disk space: need {needed}, free {free}")]
    InsufficientSpace { needed: u64, free: u64 },
    #[error("download path is a symlink — refusing to write")]
    SymlinkDownloadPath,
    #[error("manifest size {expected} but downloaded {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
}

#[cfg(not(target_os = "linux"))]
const FREE_SPACE_HEADROOM: u64 = 500 * 1024 * 1024; // plan: size + 500 MB headroom

#[cfg(not(target_os = "linux"))]
async fn download_artifact(
    input: &EngineInput,
    release: &ChannelRelease,
    tx: &mpsc::Sender<UpdateStatus>,
) -> Result<PathBuf, DownloadError> {
    tokio::fs::create_dir_all(&input.download_dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&input.download_dir, std::fs::Permissions::from_mode(0o700)).await;
    }

    let filename = release
        .download_url
        .path_segments()
        .and_then(|mut s| s.next_back())
        .filter(|s| !s.is_empty())
        .unwrap_or("artifact.bin")
        .to_string();
    let target = input.download_dir.join(&filename);

    if let Ok(meta) = tokio::fs::symlink_metadata(&target).await {
        if meta.file_type().is_symlink() {
            return Err(DownloadError::SymlinkDownloadPath);
        }
        // pre-existing regular file: remove it so we always create_new
        let _ = tokio::fs::remove_file(&target).await;
    }

    let expected = release.size_bytes.as_u64();
    let need = expected + FREE_SPACE_HEADROOM;
    if let Some(free) = free_bytes(&input.download_dir) {
        if free < need {
            return Err(DownloadError::InsufficientSpace { needed: need, free });
        }
    }

    let mut response = input.client.get(release.download_url.clone()).send().await?.error_for_status()?;
    let total = response.content_length().unwrap_or(expected);

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .await;
    }

    let mut bytes_done: u64 = 0;
    let mut last_tick = Instant::now();
    let _ = tx
        .send(UpdateStatus::Downloading {
            bytes_done: 0,
            bytes_total: total,
        })
        .await;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk).await?;
        bytes_done += chunk.len() as u64;
        if last_tick.elapsed() >= Duration::from_millis(100) {
            last_tick = Instant::now();
            let _ = tx
                .send(UpdateStatus::Downloading {
                    bytes_done,
                    bytes_total: total,
                })
                .await;
        }
    }
    file.flush().await?;
    drop(file);

    if expected != 0 && bytes_done != expected {
        let _ = tokio::fs::remove_file(&target).await;
        return Err(DownloadError::SizeMismatch {
            expected,
            actual: bytes_done,
        });
    }

    let _ = tx
        .send(UpdateStatus::Downloading {
            bytes_done,
            bytes_total: total,
        })
        .await;
    Ok(target)
}

#[cfg(not(target_os = "linux"))]
async fn verify_sha256(path: &std::path::Path, release: &ChannelRelease) -> Result<(), String> {
    let bytes = tokio::fs::read(path).await.map_err(|e| e.to_string())?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let got = hasher.finalize();
    if got.as_slice() != release.sha256.0.as_slice() {
        return Err(format!(
            "sha256 mismatch: expected {}, got {:x}",
            release.sha256, got
        ));
    }
    Ok(())
}

/// Best-effort free-space probe using `statvfs(3)` on Unix. Returns `None` if
/// the call fails — callers should treat that as "skip the check" rather than
/// blocking the install.
#[cfg(not(target_os = "linux"))]
fn free_bytes(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let c = CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut buf: libc::statvfs = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::statvfs(c.as_ptr(), &mut buf) };
        if rc != 0 {
            return None;
        }
        Some(buf.f_bavail as u64 * buf.f_frsize as u64)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// Persisted across daemon restarts so a crash mid-install can be reported
/// once on first boot. See `LastUpdateAttempt::take_if_present`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LastUpdateAttempt {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub channel: Channel,
    pub candidate_version: Option<String>,
    pub final_status: UpdateStatus,
}

impl LastUpdateAttempt {
    /// Read + delete the attempt file. The daemon should call this on startup,
    /// emit one terminal status, and then proceed normally. Returns None if no
    /// attempt file is present.
    pub async fn take_if_present(path: &std::path::Path) -> Option<LastUpdateAttempt> {
        let bytes = tokio::fs::read(path).await.ok()?;
        let attempt: LastUpdateAttempt = serde_json::from_slice(&bytes).ok()?;
        let _ = tokio::fs::remove_file(path).await;
        Some(attempt)
    }
}

#[cfg(not(target_os = "linux"))]
async fn persist_attempt(input: &EngineInput, last: &UpdateStatus) {
    let Some(path) = input.attempt_state_path.as_ref() else {
        return;
    };
    let attempt = LastUpdateAttempt {
        timestamp: SystemTime::now().into(),
        channel: input.channel,
        candidate_version: None,
        final_status: last.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec(&attempt) {
        if let Some(parent) = path.parent() {
            let _ = tokio::fs::create_dir_all(parent).await;
        }
        let _ = tokio::fs::write(path, bytes).await;
    }
}

#[cfg(not(target_os = "linux"))]
async fn audit_log(input: &EngineInput, last: &UpdateStatus) {
    let Some(path) = input.audit_log_path.as_ref() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let ts: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
    let line = format!("{ts}\tchannel={}\tstatus={}\n", input.channel, last);
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path).await {
        let _ = f.write_all(line.as_bytes()).await;
    }
}

/// Connect to the daemon and stream `UpdateStatus` events for an install.
///
/// Used by the `gnosis_vpn-ctl install-update` subcommand and by any external
/// consumer (e.g. the Tauri GUI in its own repo). The returned `Stream` ends
/// either with a terminal status (`Completed` / `Failed`) or with the
/// connection closing — callers should treat both as final.
///
/// `force = true` bypasses the VPN-connected gate (default: refuse to fetch
/// the manifest or download an artifact unless the VPN is up).
pub async fn install_stream(
    socket_path: &std::path::Path,
    channel: Channel,
    allow_downgrade: bool,
    force: bool,
) -> Result<impl Stream<Item = Result<UpdateStatus, Error>>, Error> {
    use crate::command::{Command, Response};
    use crate::socket;

    let raw = socket::root::stream_cmd(
        socket_path,
        &Command::StartUpdate {
            channel,
            allow_downgrade,
            force,
        },
    )
    .await?;

    Ok(raw.map(|item| {
        item.map_err(Error::from).and_then(|resp| match resp {
            Response::UpdateStatus(s) => Ok(s),
            Response::StartUpdateRejected(msg) => Ok(UpdateStatus::Failed {
                stage: UpdateStage::Check,
                error: msg,
            }),
            other => Err(Error::Unexpected(format!("{:?}", other))),
        })
    }))
}

/// Platform-specific install invocation. macOS only — Linux goes through
/// [`crate::update_apt`] and never calls this.
#[cfg(target_os = "macos")]
pub(crate) mod install_platform {
    use crate::shell_command_ext::{Logs, ShellCommandExt};
    use std::path::Path;
    use tokio::process::Command;

    /// Spawn `installer(8)` for the downloaded `.pkg` and wait for it to exit.
    ///
    /// The daemon should be ready to die immediately after this returns:
    /// the postinstall reloads launchd which respawns the new binary. Do
    /// **not** keep state past this point.
    pub async fn install(path: &Path) -> Result<(), String> {
        Command::new("installer")
            .arg("-pkg")
            .arg(path)
            .arg("-target")
            .arg("/")
            .run(Logs::Print)
            .await
            .map_err(|e| format!("installer failed: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check_update::Hash;
    use url::Url;

    fn release(version: &str, min_app: &str, min_os: &str) -> ChannelRelease {
        ChannelRelease {
            version: version.to_string(),
            published_at: "2024-01-01T00:00:00Z".parse().unwrap(),
            download_url: Url::parse("https://download.gnosisvpn.io/artifact.pkg").unwrap(),
            size_bytes: ByteSize::mb(10),
            sha256: Hash([0u8; 32]),
            artifact_signature: String::new(),
            release_notes: String::new(),
            min_os_version: min_os.to_string(),
            min_app_version: min_app.to_string(),
        }
    }

    #[test]
    fn compare_components_handles_app_and_snapshot_versions() {
        assert_eq!(compare_components("0.86.1", "0.86.0"), Ordering::Greater);
        assert_eq!(compare_components("0.86.0", "0.86.0"), Ordering::Equal);
        assert_eq!(compare_components("0.85.0", "0.86.0"), Ordering::Less);
        // date-based snapshot version with build metadata (per the fixtures)
        assert_eq!(
            compare_components("2026.04.24+build.030922", "2026.04.24+build.030921"),
            Ordering::Greater,
        );
        assert_eq!(
            compare_components("2026.04.25+build.000001", "2026.04.24+build.999999"),
            Ordering::Greater,
        );
    }

    #[test]
    fn ensure_installable_rejects_downgrade_unless_allowed() {
        let r = release("0.85.0", "0.80.0", "0.0");
        let err = ensure_installable(&r, "0.86.0", false).unwrap_err();
        assert!(matches!(err, GateError::Downgrade { .. }));
        let ord = ensure_installable(&r, "0.86.0", true).unwrap();
        assert_eq!(ord, Ordering::Less);
    }

    #[test]
    fn ensure_installable_rejects_when_app_too_old() {
        let r = release("1.0.0", "0.90.0", "0.0");
        let err = ensure_installable(&r, "0.86.0", false).unwrap_err();
        assert!(matches!(err, GateError::AppTooOld { .. }));
    }

    #[test]
    fn ensure_installable_ignores_min_os_version() {
        // Linux: manifest says "22.04" but the host is "12" (Debian). No OS
        // gate runs; `dpkg`/`rpm` catch real incompatibility at install time.
        let r = release("0.87.0", "0.80.0", "22.04");
        let ord = ensure_installable(&r, "0.86.0", false).unwrap();
        assert_eq!(ord, Ordering::Greater);
    }

    #[test]
    fn ensure_installable_rejects_same_version() {
        let r = release("0.86.0", "0.80.0", "0.0");
        let err = ensure_installable(&r, "0.86.0", false).unwrap_err();
        assert!(matches!(err, GateError::AlreadyInstalled { .. }));
    }

    #[test]
    fn ensure_installable_accepts_upgrade() {
        let r = release("0.87.0", "0.80.0", "0.0");
        let ord = ensure_installable(&r, "0.86.0", false).unwrap();
        assert_eq!(ord, Ordering::Greater);
    }

    #[test]
    fn update_status_terminal_variants() {
        assert!(UpdateStatus::Completed {
            new_version: "1.0.0".into()
        }
        .is_terminal());
        assert!(UpdateStatus::Failed {
            stage: UpdateStage::Check,
            error: "x".into()
        }
        .is_terminal());
        assert!(!UpdateStatus::Checking.is_terminal());
        assert!(!UpdateStatus::Downloading {
            bytes_done: 0,
            bytes_total: 0
        }
        .is_terminal());
    }

    #[test]
    fn update_status_roundtrips_through_json() {
        let s = UpdateStatus::Downloading {
            bytes_done: 42,
            bytes_total: 100,
        };
        let j = serde_json::to_string(&s).expect("ser");
        let back: UpdateStatus = serde_json::from_str(&j).expect("de");
        match back {
            UpdateStatus::Downloading {
                bytes_done,
                bytes_total,
            } => {
                assert_eq!(bytes_done, 42);
                assert_eq!(bytes_total, 100);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
