use serde::{Deserialize, Serialize};
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::vec::Vec;
use url::Url;

use crate::peer_id::PeerId;
use crate::session;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub hoprd_node: HoprdNode,
    pub destinations: Option<HashMap<String, Destination>>,
    pub connection: Option<Connection>,
    pub wireguard: Option<WireGuard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HoprdNode {
    pub endpoint: Url,
    pub api_token: String,
    pub internal_connection_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub peer_id: PeerId,
    pub path: DestinationPath,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DestinationPath {
    #[serde(alias = "intermediates")]
    Intermediates(Vec<PeerId>),
    #[serde(alias = "hops")]
    Hops(u8),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub listen_host: Option<String>,
    pub bridge: Option<ConnectionProtocol>,
    pub wg: Option<ConnectionProtocol>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConnectionProtocol {
    pub capabilities: Option<Vec<SessionCapability>>,
    pub target: Option<SocketAddr>,
    pub target_type: Option<SessionTargetType>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionCapability {
    #[serde(alias = "segmentation")]
    Segmentation,
    #[serde(alias = "retransmission")]
    Retransmission,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum SessionTargetType {
    #[default]
    #[serde(alias = "plain")]
    Plain,
    #[serde(alias = "sealed")]
    Sealed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ConnectionTarget {
    bridge: Option<SocketAddr>,
    wg: Option<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WireGuard {
    listen_port: Option<u16>,
}

impl Into<session::Capability> for SessionCapability {
    fn into(self) -> session::Capability {
        match self {
            SessionCapability::Segmentation => session::Capability::Segmentation,
            SessionCapability::Retransmission => session::Capability::Retransmission,
        }
    }
}

impl Connection {
    pub fn default_bridge_capabilities() -> Vec<SessionCapability> {
        vec![SessionCapability::Segmentation, SessionCapability::Retransmission]
    }
    pub fn default_wg_capabilities() -> Vec<SessionCapability> {
        vec![SessionCapability::Segmentation]
    }
    pub fn default_bridge_target() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 8000))
    }
    pub fn default_wg_target() -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], 51820))
    }
    pub fn default_listen_host() -> String {
        ":1422".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn test_minimal_config() {
        let config = r#####"
version = 2
"#####;
        let result = toml::from_str::<Config>(config);
        assert!(result.is_ok());
    }

    #[test]
    fn test_full_config() {
        let config = r#####"
version = 2
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"
internal_connection_port = 1422

[destinations]
[destinations.germany]
peer_id = "12D3KooWMEXkxWMitwu9apsHmjgDZ7imVHgEsjXfcyZfrqYMYjW7"
path = { intermediates = [ "12D3KooWFUD4BSzjopNzEzhSi9chAkZXRKGtQJzU482rJnyd2ZnP" ] }

[destinations.usa]
peer_id = "12D3KooWBRB3y81TmtqC34JSd61uS8BVeUqWxCSBijD5nLhL6HU5"
path = { intermediates = [ "12D3KooWQLTR4zdLyXToQGx3YKs9LJmeL4MKJ3KMp4rfVibhbqPQ" ] }

[destinations.spain]
peer_id = "12D3KooWGdcnCwJ3645cFgo4drvSN3TKmxQFYEZK7HMPA6wx1bjL"
path = { intermediates = [ "12D3KooWFnMnefPQp2k3XA3yNViBH4hnUCXcs9LasLUSv6WAgKSr" ] }

[connection]
listen_host = "0.0.0.0:1422"

[connection.bridge]
capabilities = [ "segmentation", "retransmission" ]
target = "127.0.0.1:8000"
target_type = "plain"

[connection.wg]
capabilities = [ "segmentation" ]
target = "127.0.0.1:51820"
target_type = "sealed"

[wireguard]
listen_port = 51820
"#####;
        let result = toml::from_str::<Config>(config);
        assert!(result.is_ok());
    }
}
