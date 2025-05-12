use serde::{Deserialize, Serialize};
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::default::Default;
use std::fmt::Display;
use std::vec::Vec;
use url::Url;

use super::ConfigError;

use crate::connection::Destination as ConnDestination;
use crate::entry_node::EntryNode;
use crate::peer_id::PeerId;
use crate::wireguard::config::Config as WireGuardConfig;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub hoprd_node: Option<EntryNodeConfig>,
    pub connection: Option<SessionConfig>,
    pub wireguard: Option<OldWireGuardConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EntryNodeConfig {
    pub endpoint: Url,
    pub api_token: String,
    pub internal_connection_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub capabilities: Option<Vec<SessionCapabilitiesConfig>>,
    pub destination: PeerId,
    pub listen_host: Option<String>,
    pub path: Option<SessionPathConfig>,
    pub target: Option<SessionTargetConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OldWireGuardConfig {
    pub address: String,
    pub server_public_key: String,
    pub allowed_ips: Option<String>,
    pub preshared_key: Option<String>,
    pub private_key: Option<String>,
    pub listen_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionTargetConfig {
    pub type_: Option<SessionTargetType>,
    pub host: Option<String>,
    pub port: Option<u16>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum SessionCapabilitiesConfig {
    #[default]
    #[serde(alias = "segmentation")]
    Segmentation,
    #[serde(alias = "retransmission")]
    Retransmission,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum SessionTargetType {
    #[default]
    Plain,
    Sealed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionPathConfig {
    #[serde(alias = "hop")]
    Hop(u8),
    #[serde(alias = "intermediates")]
    Intermediates(Vec<PeerId>),
}

const DEFAULT_PATH: &str = "/etc/gnosisvpn/config.toml";

impl Default for Config {
    fn default() -> Self {
        Config {
            version: 1,
            hoprd_node: None,
            connection: None,
            wireguard: None,
        }
    }
}

impl Default for SessionPathConfig {
    fn default() -> Self {
        SessionPathConfig::Hop(1)
    }
}

impl Default for SessionTargetConfig {
    fn default() -> Self {
        SessionTargetConfig {
            type_: Some(SessionTargetType::Plain),
            host: Some(default_session_target_host()),
            port: Some(default_session_target_port()),
        }
    }
}

impl Display for SessionTargetType {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            SessionTargetType::Plain => write!(f, "Plain"),
            SessionTargetType::Sealed => write!(f, "Sealed"),
        }
    }
}

pub fn default_session_target_host() -> String {
    "172.17.0.1".to_string()
}

pub fn default_session_target_port() -> u16 {
    51820
}

impl Config {
    pub fn entry_node(&self) -> Result<EntryNode, ConfigError> {
        let hoprd_node = self.hoprd_node.as_ref().ok_or(ConfigError::HoprdNodeMissing)?;

        let internal_connection_port = hoprd_node.internal_connection_port.map(|p| format!(":{}", p));

        let listen_host = self
            .connection
            .as_ref()
            .and_then(|c| c.listen_host.clone())
            .or(internal_connection_port)
            .unwrap_or(":1422".to_string());

        Ok(EntryNode::new(
            &hoprd_node.endpoint,
            &hoprd_node.api_token,
            &listen_host,
        ))
    }

    pub fn destinations(&self) -> HashMap<String, ConnDestination> {
        HashMap::new()
    }

    pub fn wireguard(&self) -> WireGuardConfig {
        let listen_port = self.wireguard.as_ref().and_then(|wg| wg.listen_port);
        WireGuardConfig::new(&listen_port)
    }
}
