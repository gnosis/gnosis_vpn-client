use serde::{Deserialize, Serialize};
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::vec::Vec;
use url::Url;

use super::ConfigError;
use crate::connection::{Destination as ConnDestination, SessionParameters};
use crate::entry_node::EntryNode;
use crate::peer_id::PeerId;
use crate::session;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    hoprd_node: HoprdNode,
    destinations: Option<HashMap<String, Destination>>,
    connection: Option<Connection>,
    wireguard: Option<WireGuard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct HoprdNode {
    endpoint: Url,
    api_token: String,
    internal_connection_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Destination {
    peer_id: PeerId,
    path: DestinationPath,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum DestinationPath {
    #[serde(alias = "intermediates")]
    Intermediates(Vec<PeerId>),
    #[serde(alias = "hops")]
    Hops(u8),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Connection {
    listen_host: Option<String>,
    bridge: Option<ConnectionProtocol>,
    wg: Option<ConnectionProtocol>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ConnectionProtocol {
    capabilities: Option<Vec<SessionCapability>>,
    target: Option<SocketAddr>,
    target_type: Option<SessionTargetType>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum SessionCapability {
    #[serde(alias = "segmentation")]
    Segmentation,
    #[serde(alias = "retransmission")]
    Retransmission,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
enum SessionTargetType {
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

impl Config {
    pub fn entry_node(&self) -> Result<EntryNode, ConfigError> {
        let internal_connection_port = self.hoprd_node.internal_connection_port.map(|p| format!(":{}", p));
        let listen_host = self
            .connection
            .as_ref()
            .and_then(|c| c.listen_host.clone())
            .or(internal_connection_port);
        Ok(EntryNode::new(
            self.hoprd_node.endpoint.clone(),
            self.hoprd_node.api_token.clone(),
            listen_host,
        ))
    }

    pub fn destinations(&self) -> HashMap<String, ConnDestination> {
        let config_dests = self.destinations.clone().unwrap_or(HashMap::new());
        let connection = self.connection.as_ref();
        config_dests
            .iter()
            .map(|(k, v)| {
                let path = match v.path.clone() {
                    DestinationPath::Intermediates(p) => session::Path::Intermediates(p),
                    DestinationPath::Hops(h) => session::Path::Hops(h),
                };

                let bridge_caps = connection
                    .and_then(|c| c.bridge.as_ref())
                    .and_then(|b| b.capabilities.clone())
                    .unwrap_or(Connection::default_bridge_capabilities())
                    .iter()
                    .map(|cap| <SessionCapability as Into<session::Capability>>::into(cap.clone()))
                    .collect::<Vec<session::Capability>>();
                let bridge_target_socket = connection
                    .and_then(|c| c.bridge.as_ref())
                    .and_then(|b| b.target)
                    .unwrap_or(Connection::default_bridge_target());
                let bridge_target_type = connection
                    .and_then(|c| c.bridge.as_ref())
                    .and_then(|b| b.target_type.clone())
                    .unwrap_or(SessionTargetType::default());
                let bridge_target = match bridge_target_type {
                    SessionTargetType::Plain => session::Target::Plain(bridge_target_socket),
                    SessionTargetType::Sealed => session::Target::Sealed(bridge_target_socket),
                };
                let params_bridge = SessionParameters::new(&bridge_target, &bridge_caps);

                let wg_caps = connection
                    .and_then(|c| c.wg.as_ref())
                    .and_then(|w| w.capabilities.clone())
                    .unwrap_or(Connection::default_wg_capabilities())
                    .iter()
                    .map(|cap| <SessionCapability as Into<session::Capability>>::into(cap.clone()))
                    .collect::<Vec<session::Capability>>();
                let wg_target_socket = connection
                    .and_then(|c| c.wg.as_ref())
                    .and_then(|w| w.target)
                    .unwrap_or(Connection::default_wg_target());
                let wg_target_type = connection
                    .and_then(|c| c.wg.as_ref())
                    .and_then(|w| w.target_type.clone())
                    .unwrap_or(SessionTargetType::default());
                let wg_target = match wg_target_type {
                    SessionTargetType::Plain => session::Target::Plain(wg_target_socket),
                    SessionTargetType::Sealed => session::Target::Sealed(wg_target_socket),
                };
                let params_wg = SessionParameters::new(&wg_target, &wg_caps);

                let dest = ConnDestination::new(&v.peer_id, &path, &params_bridge, &params_wg);
                (k.clone(), dest)
            })
            .collect()
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
