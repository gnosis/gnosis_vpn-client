use serde::{Deserialize, Serialize};
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::vec::Vec;
use url::Url;

use crate::peer_id::PeerId;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub hoprd_node: Option<HoprdNode>,
    destinations: Option<HashMap<String, Destination>>,
    pub connection: Option<Connection>,
    wireguard: Option<WireGuard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct HoprdNode {
    pub endpoint: Url,
    pub api_token: String,
    pub internal_connection_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Destination {
    peer_id: PeerId,
    path: DestinationPath,
    target: Option<ConnectionTarget>,
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
    pub listen_host: Option<String>,
    tcp: Option<ConnectionProtocol>,
    udp: Option<ConnectionProtocol>,
    target: Option<ConnectionTarget>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ConnectionProtocol {
    capabilities: Vec<SessionCapability>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SessionCapability {
    #[serde(alias = "segmentation")]
    Segmentation,
    #[serde(alias = "retransmission")]
    Retransmission,
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

impl Default for Config {
    fn default() -> Self {
        Config {
            version: 2,
            hoprd_node: None,
            destinations: None,
            connection: None,
            wireguard: None,
        }
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
[destinations.spain.target]
bridge = "127.0.0.1:8001"
wg = "127.0.0.1:51821"

[connection]
listen_host = "0.0.0.0:1422"

[connection.tcp]
capabilities = [ "segmentation", "retransmission" ]
[connection.udp]
capabilities = [ "segmentation" ]

[connection.target]
bridge = "127.0.0.1:8000"
wg = "127.0.0.1:51820"

[wireguard]
listen_port = 51820
"#####;
        let result = toml::from_str::<Config>(config);
        assert!(result.is_ok());
    }
}
