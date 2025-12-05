use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use std::os::unix::fs::PermissionsExt;

use crate::dirs;

pub async fn available() -> Result<(), Error> {
    let code = Command::new("which")
        .arg("wg-quick")
        // suppress log output
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await?;
    if code.success() {
        Ok(())
    } else {
        Err(Error::NotAvailable("wg-quick".to_string()))
    }
}

pub async fn executable() -> Result<(), Error> {
    let output = Command::new("wg-quick")
        .arg("-h")
        // suppress stdout
        .stdout(std::process::Stdio::null())
        .output()
        .await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::NotExecutable(
            "wg-quick".to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}

const WG_CONFIG_FILE: &str = "wg0_gnosisvpn.conf";

async fn generate_key() -> Result<String, Error> {
    let output = Command::new("wg").arg("genkey").output().await?;

    if output.status.success() {
        let key = String::from_utf8(output.stdout).map(|s| s.trim().to_string())?;
        Ok(key)
    } else {
        Err(Error::WgGenKey(
            output.status.code().unwrap_or_default(),
            format!("wg genkey failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}

async fn public_key(priv_key: &str) -> Result<String, Error> {
    let mut command = Command::new("wg")
        .arg("pubkey")
        .stdin(std::process::Stdio::piped()) // Enable piping to stdin
        .stdout(std::process::Stdio::piped()) // Capture stdout
        .spawn()?;

    if let Some(stdin) = command.stdin.as_mut() {
        stdin.write_all(priv_key.as_bytes()).await?
    }

    let output = command.wait_with_output().await?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    } else {
        Err(Error::WgGenKey(
            output.status.code().unwrap_or_default(),
            format!("wg pubkey failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}

pub trait WireGuardExt {
    pub async fn from_config(config: Config) -> Result<Self, Error>
    pub async fn up(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> Result<(), Error>
}


impl WireGuardExt for WireGuard {
    pub async fn from_config(config: Config) -> Result<Self, Error> {
        let priv_key = match config.force_private_key.clone() {
            Some(key) => key,
            None => generate_key().await?,
        };
        let public_key = public_key(&priv_key).await?;
        let key_pair = KeyPair { priv_key, public_key };
        Ok(WireGuard { config, key_pair })
    }

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
