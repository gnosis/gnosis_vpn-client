use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config;
use crate::dirs;
use crate::wireguard::{ConnectSession, Error, /*VerifySession,*/ WireGuard};

#[derive(Debug)]
pub struct Tooling {}

pub fn available() -> Result<bool, Error> {
    let code = Command::new("which")
        .arg("wg-quick")
        // suppress log output
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| Error::IO(e.to_string()))?;
    Ok(code.success())
}

impl Tooling {
    pub fn new() -> Self {
        Tooling {}
    }
}

// const NETWORK: &str = "wg0_gnosisvpn";
const TMP_FILE: &str = "wg0_gnosisvpn.conf";

fn wg_config_file() -> Result<PathBuf, Error> {
    let p_dirs = dirs::project().ok_or(Error::IO("unable to create project directories".to_string()))?;
    let cache_dir = p_dirs.cache_dir();
    fs::create_dir_all(cache_dir).map_err(|e| Error::IO(e.to_string()))?;

    Ok(cache_dir.join(TMP_FILE))
}

impl WireGuard for Tooling {
    fn generate_key(&self) -> Result<String, Error> {
        let output = Command::new("wg")
            .arg("genkey")
            .output()
            .map_err(|e| Error::IO(e.to_string()))?;
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .map_err(Error::FromUtf8Error)
    }

    fn connect_session(&self, session: &ConnectSession) -> Result<(), Error> {
        let conf_file = wg_config_file()?;
        let config = session.to_file_string();
        let content = config.as_bytes();
        fs::write(&conf_file, content).map_err(|e| Error::IO(e.to_string()))?;

        let output = Command::new("wg-quick")
            .arg("up")
            .arg(conf_file)
            .output()
            .map_err(|e| Error::IO(e.to_string()))?;

        tracing::info!("wg-quick up output: {:?}", output);
        Ok(())
    }

    fn close_session(&self) -> Result<(), Error> {
        let conf_file = wg_config_file()?;

        let output = Command::new("wg-quick")
            .arg("down")
            .arg(conf_file)
            .output()
            .map_err(|e| Error::IO(e.to_string()))?;

        tracing::info!("wg-quick down output: {:?}", output);
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
            .spawn()
            .map_err(|e| Error::IO(e.to_string()))?;

        if let Some(stdin) = command.stdin.as_mut() {
            stdin
                .write_all(priv_key.as_bytes())
                .map_err(|e| Error::IO(e.to_string()))?;
        }

        let output = command.wait_with_output().map_err(|e| Error::IO(e.to_string()))?;

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

impl ConnectSession {
    fn to_file_string(&self) -> String {
        let allowed_ips = match &self.interface.allowed_ips {
            Some(allowed_ips) => allowed_ips.clone(),
            None => {
                self.interface
                    .address
                    .split('.')
                    .take(2)
                    .collect::<Vec<&str>>()
                    .join(".")
                    + ".0.0/9"
            }
        };
        let listen_port = self.interface.listen_port.unwrap_or(config::default_listen_port());

        format!(
            "[Interface]
PrivateKey = {private_key}
Address = {address}
ListenPort = {listen_port}

[Peer]
PublicKey = {public_key}
Endpoint = {endpoint}
AllowedIPs = {allowed_ips}
PersistentKeepalive = 30
",
            private_key = self.interface.private_key,
            address = self.interface.address,
            public_key = self.peer.public_key,
            endpoint = self.peer.endpoint,
            allowed_ips = allowed_ips,
            listen_port = listen_port,
        )
    }
}
