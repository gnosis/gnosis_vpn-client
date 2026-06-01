//! Linux apt-based install engine.
//!
//! Replaces the manifest → download → SHA verify → dpkg pipeline on Linux:
//! the gnosisvpn install script (`install/linux.sh` in the packaging repo)
//! already configures `/etc/apt/sources.list.d/gnosisvpn.sources`, so we
//! delegate the upgrade to apt.
//!
//! Emits the same [`UpdateStatus`] schema as the macOS engine. Apt does its
//! own check/download/verify, so we collapse those stages into a single
//! `Installing` event and finish with `Completed { new_version }` or
//! `Failed { stage: Install, .. }`.

use std::path::{Path, PathBuf};

use tokio::process::Command;
use tokio::sync::mpsc;

use crate::check_update::{self, Channel};
use crate::update::{UpdateStage, UpdateStatus};

const SOURCES_PATH: &str = "/etc/apt/sources.list.d/gnosisvpn.sources";
const PACKAGE_NAME: &str = "gnosisvpn";

/// Map a [`Channel`] to the apt suite + component the packaging repo publishes
/// under. Must stay in sync with `install/linux.sh` in the gnosis_vpn repo.
fn channel_suite_component(c: Channel) -> (&'static str, &'static str) {
    match c {
        Channel::Stable => ("stable", "main"),
        Channel::Snapshot => ("snapshot", "snapshot"),
    }
}

/// `skip_vpn_check = true` mirrors the macOS `force` flag: it bypasses the
/// VPN-connected gate (insecure — apt traffic will not traverse the tunnel).
pub fn install_engine(channel: Channel, socket_path: PathBuf, skip_vpn_check: bool) -> mpsc::Receiver<UpdateStatus> {
    let (tx, rx) = mpsc::channel(32);
    tokio::spawn(async move {
        let last = match drive(channel, &socket_path, skip_vpn_check, &tx).await {
            Ok(version) => UpdateStatus::Completed { new_version: version },
            Err((stage, error)) => UpdateStatus::Failed { stage, error },
        };
        let _ = tx.send(last).await;
    });
    rx
}

async fn drive(
    channel: Channel,
    socket_path: &Path,
    skip_vpn_check: bool,
    tx: &mpsc::Sender<UpdateStatus>,
) -> Result<String, (UpdateStage, String)> {
    let _ = tx.send(UpdateStatus::Checking).await;
    if !skip_vpn_check {
        check_update::ensure_vpn_connected(socket_path)
            .await
            .map_err(|e| (UpdateStage::Check, e.to_string()))?;
    }
    ensure_channel(channel).await.map_err(|e| (UpdateStage::Check, e))?;
    let _ = tx.send(UpdateStatus::Installing).await;
    apt_update().await.map_err(|e| (UpdateStage::Download, e))?;
    apt_upgrade().await.map_err(|e| (UpdateStage::Install, e))?;
    installed_version().await.map_err(|e| (UpdateStage::Install, e))
}

async fn ensure_channel(channel: Channel) -> Result<(), String> {
    let (suite, component) = channel_suite_component(channel);
    let content = tokio::fs::read_to_string(SOURCES_PATH)
        .await
        .map_err(|e| format!("apt sources file {SOURCES_PATH} not found ({e}); re-run the gnosisvpn install script"))?;

    let cur_suite = field(&content, "Suites");
    let cur_component = field(&content, "Components");
    if cur_suite.as_deref() == Some(suite) && cur_component.as_deref() == Some(component) {
        return Ok(());
    }

    let mut next = rewrite_field(&content, "Suites", suite);
    next = rewrite_field(&next, "Components", component);
    tokio::fs::write(SOURCES_PATH, next)
        .await
        .map_err(|e| format!("write {SOURCES_PATH}: {e}"))?;
    tracing::info!(suite, component, "switched apt channel");
    Ok(())
}

fn field(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    content
        .lines()
        .find_map(|l| l.strip_prefix(&prefix).map(|v| v.trim().to_string()))
}

fn rewrite_field(content: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}:");
    let trailing_newline = content.ends_with('\n');
    let mut found = false;
    let mut lines: Vec<String> = content
        .lines()
        .map(|line| {
            if line.starts_with(&prefix) {
                found = true;
                format!("{key}: {value}")
            } else {
                line.to_string()
            }
        })
        .collect();
    if !found {
        lines.push(format!("{key}: {value}"));
    }
    let mut out = lines.join("\n");
    if trailing_newline {
        out.push('\n');
    }
    out
}

async fn apt_update() -> Result<(), String> {
    run_apt(Command::new("apt-get").arg("update")).await
}

async fn apt_upgrade() -> Result<(), String> {
    run_apt(
        Command::new("apt-get")
            .arg("install")
            .arg("--only-upgrade")
            .arg("-y")
            .arg(PACKAGE_NAME),
    )
    .await
}

async fn run_apt(cmd: &mut Command) -> Result<(), String> {
    cmd.env("DEBIAN_FRONTEND", "noninteractive");
    let output = cmd.output().await.map_err(|e| format!("spawn {cmd:?}: {e}"))?;
    if output.status.success() {
        if !output.stderr.is_empty() {
            tracing::info!(stderr = %String::from_utf8_lossy(&output.stderr), cmd = ?cmd, "apt finished");
        }
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!("{cmd:?} exited with {}: {stderr}", output.status))
}

async fn installed_version() -> Result<String, String> {
    let output = Command::new("dpkg-query")
        .arg("-W")
        .arg("-f=${Version}")
        .arg(PACKAGE_NAME)
        .output()
        .await
        .map_err(|e| format!("spawn dpkg-query: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "dpkg-query failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let v = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if v.is_empty() {
        return Err("dpkg-query returned empty version".to_string());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Types: deb\n\
URIs: https://download.gnosisvpn.io/linux/apt\n\
Suites: stable\n\
Components: main\n\
Architectures: amd64\n\
Signed-By: /etc/apt/keyrings/gnosisvpn-archive-keyring.gpg\n";

    #[test]
    fn parses_existing_fields() {
        assert_eq!(field(SAMPLE, "Suites").as_deref(), Some("stable"));
        assert_eq!(field(SAMPLE, "Components").as_deref(), Some("main"));
    }

    #[test]
    fn rewrites_suite_in_place() {
        let next = rewrite_field(SAMPLE, "Suites", "snapshot");
        let next = rewrite_field(&next, "Components", "snapshot");
        assert_eq!(field(&next, "Suites").as_deref(), Some("snapshot"));
        assert_eq!(field(&next, "Components").as_deref(), Some("snapshot"));
        assert!(next.contains("URIs: https://download.gnosisvpn.io/linux/apt"));
        assert!(next.contains("Signed-By: /etc/apt/keyrings/gnosisvpn-archive-keyring.gpg"));
        assert!(next.ends_with('\n'));
    }

    #[test]
    fn rewrite_appends_missing_field() {
        let stripped = SAMPLE
            .lines()
            .filter(|l| !l.starts_with("Components:"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let next = rewrite_field(&stripped, "Components", "snapshot");
        assert_eq!(field(&next, "Components").as_deref(), Some("snapshot"));
    }
}
