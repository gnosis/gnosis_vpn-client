use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use thiserror::Error;

use crate::dirs;

#[derive(Error, Debug)]
pub enum Error {
    #[error("implementation not available")]
    NotAvailable,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("encoding error: {0}")]
    FromUtf8Error(#[from] std::string::FromUtf8Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::ser::Error),
    #[error("monitoring error: {0}")]
    Monitoring(String),
    #[error("wireguard error [status: {0}]: {1}")]
    WgError(i32, String),
    #[error("Unable to determine project directories")]
    ProjectDirs,
}

#[derive(Clone, Debug)]
pub struct WireGuard {
    pub config: Config,
    pub key_pair: KeyPair,
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub address: String,
    pub allowed_ips: Option<String>,
    pub listen_port: Option<u16>,
    pub mtu: u16,
}

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub public_key: String,
    pub endpoint: String,
}

#[derive(Clone, Debug)]
pub struct KeyPair {
    pub priv_key: String,
    pub public_key: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub listen_port: Option<u16>,
    pub force_private_key: Option<String>,
    pub allowed_ips: Option<String>,
}

impl Config {
    pub(crate) fn new<L, M, S>(listen_port: Option<L>, allowed_ips: Option<M>, force_private_key: Option<S>) -> Self
    where
        L: Into<u16>,
        M: Into<String>,
        S: Into<String>,
    {
        Config {
            listen_port: listen_port.map(Into::into),
            allowed_ips: allowed_ips.map(Into::into),
            force_private_key: force_private_key.map(Into::into),
        }
    }
}

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

const TMP_FILE: &str = "wg0_gnosisvpn.conf";

fn wg_config_file() -> Result<PathBuf, Error> {
    let p_dirs = dirs::project().ok_or(Error::ProjectDirs)?;
    let cache_dir = p_dirs.cache_dir();
    fs::create_dir_all(cache_dir)?;
    Ok(cache_dir.join(TMP_FILE))
}

fn generate_key() -> Result<String, Error> {
    let output = Command::new("wg").arg("genkey").output()?;

    if output.status.success() {
        let key = String::from_utf8(output.stdout).map(|s| s.trim().to_string())?;
        Ok(key)
    } else {
        Err(Error::WgError(
            output.status.code().unwrap_or_default(),
            format!("wg genkey failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}

fn public_key(priv_key: &str) -> Result<String, Error> {
    let mut command = Command::new("wg")
        .arg("pubkey")
        .stdin(Stdio::piped()) // Enable piping to stdin
        .stdout(Stdio::piped()) // Capture stdout
        .spawn()?;

    if let Some(stdin) = command.stdin.as_mut() {
        stdin.write_all(priv_key.as_bytes())?
    }

    let output = command.wait_with_output()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(stdout.trim().to_string())
    } else {
        Err(Error::WgError(
            output.status.code().unwrap_or_default(),
            format!("wg pubkey failed: {}", String::from_utf8_lossy(&output.stderr)),
        ))
    }
}

impl WireGuard {
    pub fn from_config(config: Config) -> Result<Self, Error> {
        let priv_key = match config.force_private_key.clone() {
            Some(key) => key,
            None => generate_key()?,
        };
        let public_key = public_key(&priv_key)?;
        let key_pair = KeyPair { priv_key, public_key };
        Ok(WireGuard { config, key_pair })
    }

    pub fn connect_session(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> Result<(), Error> {
        let conf_file = wg_config_file()?;
        let config = self.to_file_string(interface, peer);
        let content = config.as_bytes();
        fs::write(&conf_file, content)?;
        fs::set_permissions(&conf_file, fs::Permissions::from_mode(0o600))?;

        let output = Command::new("wg-quick").arg("up").arg(conf_file).output()?;
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
            Err(Error::WgError(
                output.status.code().unwrap_or_default(),
                format!("wg-quick up failed: {}", String::from_utf8_lossy(&output.stderr)),
            ))
        }
    }

    pub fn close_session(&self) -> Result<(), Error> {
        let conf_file = wg_config_file()?;

        let output = Command::new("wg-quick").arg("down").arg(conf_file).output()?;
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
            Err(Error::WgError(
                output.status.code().unwrap_or_default(),
                format!("wg-quick down failed: {}", String::from_utf8_lossy(&output.stderr)),
            ))
        }
    }

    fn to_file_string(&self, interface: &InterfaceInfo, peer: &PeerInfo) -> String {
        let allowed_ips = match &interface.allowed_ips {
            Some(allowed_ips) => allowed_ips.clone(),
            None => interface.address.split('.').take(2).collect::<Vec<&str>>().join(".") + ".0.0/9",
        };
        let listen_port_line = interface
            .listen_port
            .map(|port| format!("ListenPort = {port}\n"))
            .unwrap_or_default();

        format!(
            "[Interface]
PrivateKey = {private_key}
Address = {address}
{listen_port_line}
MTU = {mtu}

[Peer]
PublicKey = {public_key}
Endpoint = {endpoint}
AllowedIPs = {allowed_ips}
",
            private_key = self.key_pair.priv_key,
            address = interface.address,
            public_key = peer.public_key,
            endpoint = peer.endpoint,
            allowed_ips = allowed_ips,
            listen_port_line = listen_port_line,
            // WireGuard has differnently sized packets not exactly adhering to MTU
            // so we postpone optimizing on this level for now
            mtu = interface.mtu,
        )
    }
}
