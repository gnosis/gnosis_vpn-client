use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use thiserror::Error;

use crate::connection::{Destination, SessionParameters};
use crate::entry_node::EntryNode;
use crate::session;

pub mod v1;
mod v2;

const CONFIG_VERSION: u8 = 2;
const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";

pub type Config = v2::Config;

#[cfg(target_family = "unix")]
pub fn path() -> PathBuf {
    match std::env::var("GNOSISVPN_CONFIG_PATH") {
        Ok(path) => PathBuf::from(path),
        Err(std::env::VarError::NotPresent) => PathBuf::from(DEFAULT_PATH),
        Err(e) => {
            tracing::warn!(warn = ?e, "using default config path");
            PathBuf::from(DEFAULT_PATH)
        }
    }
}

#[derive(Error, Debug)]
pub enum ReadError {
    #[error("Config file not found")]
    NoFile,
    #[error("IO error: {0}")]
    IO(#[from] std::io::Error),
    #[error("Deserialization error: {0}")]
    Deserialization(#[from] toml::de::Error),
    #[error("Unsupported config version")]
    VersionMismatch(u8),
}

pub fn read() -> Result<Config, ReadError> {
    let content = fs::read_to_string(path()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ReadError::NoFile
        } else {
            ReadError::IO(e)
        }
    })?;

    let res_v2 = toml::from_str::<v2::Config>(&content);
    match res_v2 {
        Ok(config) => {
            if config.version == CONFIG_VERSION {
                return Ok(config);
            } else {
                return Err(ReadError::VersionMismatch(config.version));
            }
        }
        Err(err) => {
            let res_v1 = toml::from_str::<v1::Config>(&content);
            if res_v1.is_ok() {
                return Err(ReadError::VersionMismatch(1));
            } else {
                return Err(ReadError::Deserialization(err));
            }
        }
    }
}

impl Config {
    pub fn entry_node(&self) -> EntryNode {
        let internal_connection_port = self.hoprd_node.internal_connection_port.map(|p| format!(":{}", p));
        let listen_host = self
            .connection
            .as_ref()
            .and_then(|c| c.listen_host.clone())
            .or(internal_connection_port);
        EntryNode::new(
            self.hoprd_node.endpoint.clone(),
            self.hoprd_node.api_token.clone(),
            listen_host,
        )
    }

    pub fn destinations(&self) -> HashMap<String, Destination> {
        let config_dests = self.destinations.unwrap_or(HashMap::new());
        config_dests
            .iter()
            .map(|(k, v)| {
                let path = match v.path {
                    v2::DestinationPath::Intermediates(p) => session::Path::Intermediates(p),
                    v2::DestinationPath::Hops(h) => session::Path::Hops(h),
                };

                let bridge_caps = self
                    .connection
                    .and_then(|c| c.bridge)
                    .and_then(|b| b.capabilities.clone())
                    .unwrap_or(v2::Connection::default_bridge_capabilities())
                    .iter()
                    .map(|cap| <v2::SessionCapability as Into<session::Capability>>::into(cap.clone()))
                    .collect::<Vec<session::Capability>>();
                let bridge_target_socket = self
                    .connection
                    .and_then(|c| c.bridge)
                    .and_then(|b| b.target)
                    .unwrap_or(v2::Connection::default_bridge_target());
                let bridge_target_type = self
                    .connection
                    .and_then(|c| c.bridge)
                    .and_then(|b| b.target_type)
                    .unwrap_or(v2::SessionTargetType::default());
                let bridge_target = match bridge_target_type {
                    v2::SessionTargetType::Plain => session::Target::Plain(bridge_target_socket),
                    v2::SessionTargetType::Sealed => session::Target::Sealed(bridge_target_socket),
                };
                let params_bridge = SessionParameters::new(&bridge_target, &bridge_caps);

                let wg_caps = self
                    .connection
                    .and_then(|c| c.wg)
                    .and_then(|w| w.capabilities.clone())
                    .unwrap_or(v2::Connection::default_wg_capabilities())
                    .iter()
                    .map(|cap| <v2::SessionCapability as Into<session::Capability>>::into(cap.clone()))
                    .collect::<Vec<session::Capability>>();
                let wg_target_socket = self
                    .connection
                    .and_then(|c| c.wg)
                    .and_then(|w| w.target)
                    .unwrap_or(v2::Connection::default_wg_target());
                let wg_target_type = self
                    .connection
                    .and_then(|c| c.wg)
                    .and_then(|w| w.target_type)
                    .unwrap_or(v2::SessionTargetType::default());
                let wg_target = match wg_target_type {
                    v2::SessionTargetType::Plain => session::Target::Plain(wg_target_socket),
                    v2::SessionTargetType::Sealed => session::Target::Sealed(wg_target_socket),
                };
                let params_wg = SessionParameters::new(&wg_target, &wg_caps);

                let dest = Destination::new(&v.peer_id, &path, &params_bridge, &params_wg);
                (k.clone(), dest)
            })
            .collect()
    }
}
