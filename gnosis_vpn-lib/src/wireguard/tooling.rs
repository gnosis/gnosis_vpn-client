use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::dirs;
use crate::wireguard::{ConnectSession, Error, /*VerifySession,*/ WireGuard};

#[derive(Debug)]
pub struct Tooling {}

pub fn available() -> Result<(), Error> {
    let code = Command::new("which")
        .arg("wg-quick")
        // suppress log output
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if code.success() {
        Ok(())
    } else {
        Err(Error::NotAvailable)
    }
}

impl Tooling {
    pub fn new() -> Self {
        Tooling {}
    }
}

const TMP_FILE: &str = "wg0_gnosisvpn.conf";

fn wg_config_file() -> Result<PathBuf, Error> {
    let p_dirs = dirs::project().ok_or(Error::ProjectDirs)?;
    let cache_dir = p_dirs.cache_dir();
    fs::create_dir_all(cache_dir)?;

    Ok(cache_dir.join(TMP_FILE))
}

impl WireGuard for Tooling {
    fn generate_key(&self) -> Result<String, Error> {
        let output = Command::new("wg").arg("genkey").output()?;
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .map_err(Error::FromUtf8Error)
    }

    fn connect_session(&self, session: &ConnectSession) -> Result<(), Error> {
        let conf_file = wg_config_file()?;
        let config = session.to_file_string();
        let content = config.as_bytes();
        fs::write(&conf_file, content)?;
        fs::set_permissions(&conf_file, fs::Permissions::from_mode(0o600))?;

        let output = Command::new("wg-quick").arg("up").arg(conf_file).output()?;

        if !output.status.success() {
            tracing::info!("wg-quick up status: {}", output.status);
            tracing::info!("wg-quick up stderr: {:?}", String::from_utf8_lossy(&output.stderr));
        }
        if !output.stdout.is_empty() {
            tracing::info!("wg-quick up stdout: {:?}", String::from_utf8_lossy(&output.stdout));
        }
        Ok(())
    }

    fn close_session(&self) -> Result<(), Error> {
        let conf_file = wg_config_file()?;

        let output = Command::new("wg-quick").arg("down").arg(conf_file).output()?;

        if !output.status.success() {
            tracing::info!("wg-quick down status: {}", output.status);
            tracing::info!("wg-quick down stderr: {:?}", String::from_utf8_lossy(&output.stderr));
        }
        if !output.stdout.is_empty() {
            tracing::info!("wg-quick down stdout: {:?}", String::from_utf8_lossy(&output.stdout));
        }
        Ok(())
    }

    /*
    fn verify_session(&self, session: &VerifySession) -> Result<(), Error> {
        let output = Command::new("wg")
            .arg("show")
            .arg(NETWORK)
            .arg("latest-handshakes")
            .output()
            .map_err(|e| Error::IO(e.to_string()))?;

        tracing::info!("wg show output: {:?}", output);
        if !output.status.success() {
            let err = String::from_utf8(output.stderr).map_err(Error::FromUtf8Error)?;
            return Err(Error::Monitoring(err));
        }

        let output = String::from_utf8(output.stdout).map_err(Error::FromUtf8Error)?;
        let parts: Vec<&str> = output.split_whitespace().collect();
        if parts.len() != 2 {
            return Err(Error::Monitoring("unexpected output from wg show".to_string()));
        }
        let first = parts[0];
        let second = parts[0];
        if first == session.peer_public_key && second == "0" {
            return Err(Error::Monitoring("wg server peer has not handshaked".to_string()));
        }
        let split = output.split(" ");
        tracing::info!("wg show split: {:?}", split);
        if split.take(1).collect::<Vec<&str>>().join("") != session.peer_public_key {
            return Err(Error::Monitoring("wg server peer does now match".to_string()));
        }
        if split.take(1).collect::<Vec<&str>>().join("") == "0" {
            tracing::warn!("Handshake not working, it seems that your public key is not yet registered");
            return Err(Error::Monitoring("wg server peer has not handshaked".to_string()));
        }
        Ok(())
    }
    */

    fn public_key(&self, priv_key: &str) -> Result<String, Error> {
        let mut command = Command::new("wg")
            .arg("pubkey")
            .stdin(Stdio::piped()) // Enable piping to stdin
            .stdout(Stdio::piped()) // Capture stdout
            .spawn()?;

        if let Some(stdin) = command.stdin.as_mut() {
            stdin.write_all(priv_key.as_bytes())?
        }

        let output = command.wait_with_output()?;

        // Print the command output
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout.trim().to_string())
        } else {
            Err(Error::WgError(format!(
                "Command failed with stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            )))
        }
    }
}
