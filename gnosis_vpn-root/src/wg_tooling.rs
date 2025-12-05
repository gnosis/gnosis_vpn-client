use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::os::unix::fs::PermissionsExt;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;

use crate::dirs;

pub async fn available() -> Result<(), Error> {
    Command::new("which")
        .arg("wg-quick")
        // suppress log output
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .run()
        .await
        .map_err(|_| Error::NotAvailable("wg-quick".to_string()))
}

pub async fn executable() -> Result<(), Error> {
    Command::new("wg-quick")
        .arg("-h")
        // suppress stdout
        .stdout(std::process::Stdio::null())
        .run()
        .await
        .map_err(|_| Error::NotExecutable("wg-quick".to_string()))
}

pub trait WireGuardExt {
    pub async fn up(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> Result<(), Error>
}


impl WireGuardExt for WireGuard {
    pub async fn up(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> Result<(), Error> {
        let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;
        let config = self.to_file_string(interface, peer);
        let content = config.as_bytes();
        fs::write(&conf_file, content).await?;
        fs::set_permissions(&conf_file, std::fs::Permissions::from_mode(0o600)).await?;

        let output = Command::new("wg-quick").arg("up").arg(conf_file).output().await?;
        if !output.stdout.is_empty() {
            tracing::info!("wg-quick up stdout: {}", String::from_utf8_lossy(&output.stdout));
        }

        if output.status.success() {
            if !output.stderr.is_empty() {
                // wg-quick populates stderr with info and warnings, log those in debug mode
                tracing::debug!("wg-quick up stderr: {}", String::from_utf8_lossy(&output.stderr));
            }
            Ok(())
        } else {
            Err(Error::WgQuick(
                output.status.code().unwrap_or_default(),
                format!("wg-quick up failed: {}", String::from_utf8_lossy(&output.stderr)),
            ))
        }
    }
}

pub async fn down() -> Result<(), Error> {
    let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;

    let output = Command::new("wg-quick").arg("down").arg(conf_file).output().await?;
    if !output.stdout.is_empty() {
        tracing::info!("wg-quick down stdout: {}", String::from_utf8_lossy(&output.stdout));
    }

    if output.status.success() {
        if !output.stderr.is_empty() {
            // wg-quick populates stderr with info and warnings, log those in debug mode
            tracing::debug!("wg-quick down stderr: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(())
    } else {
        Err(Error::WgQuick(
            output.status.code().unwrap_or_default(),
            format!("wg-quick down failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}
