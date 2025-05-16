use std::cmp::PartialEq;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;
use std::vec::Vec;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::connection::{Destination as ConnDestination, SessionParameters};
use crate::entry_node::EntryNode;
use crate::peer_id::PeerId;
use crate::session;
use crate::wireguard::config::{Config as WireGuardConfig, ManualMode as WireGuardManualMode};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    hoprd_node: HoprdNode,
    destinations: Option<HashMap<PeerId, Destination>>,
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
    meta: Option<HashMap<String, String>>,
    path: Option<DestinationPath>,
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
    #[serde(default, with = "humantime_serde::option")]
    session_timeout: Option<Duration>,
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
    manual_mode: Option<WgManualMode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WgManualMode {
    public_key: String,
}

impl From<SessionCapability> for session::Capability {
    fn from(val: SessionCapability) -> Self {
        match val {
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
        SocketAddr::from(([172, 17, 0, 1], 8000))
    }
    pub fn default_wg_target() -> SocketAddr {
        SocketAddr::from(([172, 17, 0, 1], 51820))
    }
    pub fn default_listen_host() -> String {
        ":1422".to_string()
    }
    pub fn default_session_timeout() -> Duration {
        Duration::from_secs(15)
    }
}

impl Config {
    pub fn entry_node(&self) -> EntryNode {
        let internal_connection_port = self.hoprd_node.internal_connection_port.map(|p| format!(":{}", p));
        let listen_host = self
            .connection
            .as_ref()
            .and_then(|c| c.listen_host.clone())
            .or(internal_connection_port)
            .unwrap_or(Connection::default_listen_host());
        let session_timeout = self
            .connection
            .as_ref()
            .and_then(|c| c.session_timeout)
            .unwrap_or(Connection::default_session_timeout());
        EntryNode::new(
            &self.hoprd_node.endpoint,
            &self.hoprd_node.api_token,
            &listen_host,
            &session_timeout,
        )
    }

    pub fn destinations(&self) -> HashMap<PeerId, ConnDestination> {
        let config_dests = self.destinations.clone().unwrap_or_default();
        let connection = self.connection.as_ref();
        config_dests
            .iter()
            .map(|(k, v)| {
                let path = match v.path.clone() {
                    Some(DestinationPath::Intermediates(p)) => session::Path::Intermediates(p),
                    Some(DestinationPath::Hops(h)) => session::Path::Hops(h),
                    None => session::Path::default(),
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
                    .unwrap_or_default();
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
                    .unwrap_or_default();
                let wg_target = match wg_target_type {
                    SessionTargetType::Plain => session::Target::Plain(wg_target_socket),
                    SessionTargetType::Sealed => session::Target::Sealed(wg_target_socket),
                };
                let params_wg = SessionParameters::new(&wg_target, &wg_caps);
                let meta = v.meta.clone().unwrap_or_default();

                let dest = ConnDestination::new(k, &path, &meta, &params_bridge, &params_wg);
                (*k, dest)
            })
            .collect()
    }

    pub fn wireguard(&self) -> WireGuardConfig {
        let listen_port = self.wireguard.as_ref().and_then(|wg| wg.listen_port);
        let manual_mode = self
            .wireguard
            .as_ref()
            .and_then(|wg| wg.manual_mode.as_ref())
            .map(|wgm| WireGuardManualMode::new(wgm.public_key.as_str()));
        WireGuardConfig::new(&listen_port, &manual_mode)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn test_minimal_config() {
        let config = r#####"
version = 2
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse minimal config");
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

[destinations.12D3KooWMEXkxWMitwu9apsHmjgDZ7imVHgEsjXfcyZfrqYMYjW7]
meta = { location = "Germany" }
path = { intermediates = [ "12D3KooWFUD4BSzjopNzEzhSi9chAkZXRKGtQJzU482rJnyd2ZnP" ] }

[destinations.12D3KooWBRB3y81TmtqC34JSd61uS8BVeUqWxCSBijD5nLhL6HU5]
meta = { location = "USA" }
path = { intermediates = [ "12D3KooWQLTR4zdLyXToQGx3YKs9LJmeL4MKJ3KMp4rfVibhbqPQ" ] }

[destinations.12D3KooWGdcnCwJ3645cFgo4drvSN3TKmxQFYEZK7HMPA6wx1bjL]
meta = { location = "Spain" }
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
# only specify this if you want to manually connect via WireGuard
manual_mode = { public_key = "VbezNcrZstuGTkXc7uNwHHB1BA8fLgL8IAQO/pWTpSw=" }
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse full config");
    }
}
