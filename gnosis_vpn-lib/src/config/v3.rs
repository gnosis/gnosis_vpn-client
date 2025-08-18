use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

use std::cmp::PartialEq;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;
use std::vec::Vec;

use crate::address::Address;
use crate::connection::{destination::Destination as ConnDestination, options};
use crate::entry_node::{self, EntryNode};
use crate::monitor;
use crate::session;
use crate::wg_tooling::Config as WireGuardConfig;

const MAX_HOPS: u8 = 3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub(super) hoprd_node: HoprdNode,
    pub(super) destinations: Option<HashMap<Address, Destination>>,
    pub(super) connection: Option<Connection>,
    pub(super) wireguard: Option<WireGuard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct HoprdNode {
    endpoint: Url,
    api_token: String,
    internal_connection_port: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Destination {
    meta: Option<HashMap<String, String>>,
    path: Option<DestinationPath>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum DestinationPath {
    #[serde(alias = "intermediates")]
    Intermediates(Vec<Address>),
    #[serde(alias = "hops", deserialize_with = "validate_hops")]
    Hops(u8),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Connection {
    listen_host: Option<String>,
    #[serde(default, with = "humantime_serde::option")]
    session_timeout: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    ping_retry_timeout: Option<Duration>,
    bridge: Option<ConnectionProtocol>,
    wg: Option<ConnectionProtocol>,
    ping: Option<PingOptions>,
    buffer: Option<BufferOptions>,
    max_surb_upstream: Option<MaxSurbUpstreamOptions>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ConnectionProtocol {
    capabilities: Option<Vec<SessionCapability>>,
    target: Option<SocketAddr>,
    target_type: Option<SessionTargetType>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum SessionCapability {
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
struct PingOptions {
    address: Option<IpAddr>,
    #[serde(default, with = "humantime_serde::option")]
    timeout: Option<Duration>,
    ttl: Option<u32>,
    seq_count: Option<u16>,
    #[serde(default, deserialize_with = "validate_ping_interval")]
    interval: Option<PingInterval>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct PingInterval {
    min: u8,
    max: u8,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct BufferOptions {
    // could be improved by using bytesize crates parser
    bridge: Option<String>,
    ping: Option<String>,
    main: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct MaxSurbUpstreamOptions {
    // could be improved by using human-bandwidth crates parser
    bridge: Option<String>,
    ping: Option<String>,
    main: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct WireGuard {
    pub(super) listen_port: Option<u16>,
    pub(super) allowed_ips: Option<String>,
    pub(super) force_private_key: Option<String>,
}

pub fn wrong_keys(table: &toml::Table) -> Vec<String> {
    let mut wrong_keys = Vec::new();
    for (key, value) in table.iter() {
        // version plain key
        if key == "version" {
            continue;
        }
        // hoprnode simple struct
        if key == "hoprd_node" {
            if let Some(hopr_node) = value.as_table() {
                for (k, _v) in hopr_node.iter() {
                    if k == "endpoint" || k == "api_token" || k == "internal_connection_port" {
                        continue;
                    }
                    wrong_keys.push(format!("hoprd_node.{k}"));
                }
            }
            continue;
        }
        // wireguard nested struct
        if key == "wireguard" {
            if let Some(wg) = value.as_table() {
                for (k, _v) in wg.iter() {
                    if k == "listen_port" || k == "allowed_ips" || k == "force_private_key" {
                        continue;
                    }
                    wrong_keys.push(format!("wireguard.{k}"));
                }
            }
            continue;
        }
        // connection nested struct
        if key == "connection" {
            if let Some(connection) = value.as_table() {
                for (k, v) in connection.iter() {
                    if k == "listen_host" {
                        continue;
                    }
                    if k == "session_timeout" {
                        continue;
                    }
                    if k == "ping_retry_timeout" {
                        continue;
                    }
                    if k == "bridge" || k == "wg" {
                        if let Some(prot) = v.as_table() {
                            for (k, _v) in prot.iter() {
                                if k == "capabilities" || k == "target" || k == "target_type" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.bridge.{k}"));
                            }
                        }
                        continue;
                    }
                    if k == "ping" {
                        if let Some(ping) = v.as_table() {
                            for (k, v) in ping.iter() {
                                if k == "address" || k == "timeout" || k == "ttl" || k == "seq_count" {
                                    continue;
                                }
                                if k == "interval" {
                                    if let Some(interval) = v.as_table() {
                                        for (k, _v) in interval.iter() {
                                            if k == "min" || k == "max" {
                                                continue;
                                            }
                                            wrong_keys.push(format!("connection.ping.interval.{k}"));
                                        }
                                    }
                                    continue;
                                }
                                wrong_keys.push(format!("connection.ping.{k}"));
                            }
                        }
                        continue;
                    }
                    if k == "buffer" {
                        if let Some(buffer) = v.as_table() {
                            for (k, _v) in buffer.iter() {
                                if k == "bridge" || k == "ping" || k == "main" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.buffer.{k}"));
                            }
                        }
                        continue;
                    }
                    if k == "max_surb_upstream" {
                        if let Some(surbs) = v.as_table() {
                            for (k, _v) in surbs.iter() {
                                if k == "bridge" || k == "ping" || k == "main" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.max_surb_upstream.{k}"));
                            }
                        }
                        continue;
                    }
                    wrong_keys.push(format!("connection.{k}"));
                }
            }
            continue;
        }
        // destinations hashmap of simple structs
        if key == "destinations" {
            if let Some(destinations) = value.as_table() {
                for (address, v) in destinations.iter() {
                    if let Some(dest) = v.as_table() {
                        for (k, _v) in dest.iter() {
                            if k == "meta" || k == "path" {
                                continue;
                            }
                            wrong_keys.push(format!("destinations.{address}.{k}"));
                        }
                        continue;
                    }
                    wrong_keys.push(format!("destinations.{address}"));
                }
            }
            continue;
        }
        wrong_keys.push(key.clone());
    }
    wrong_keys
}

fn validate_hops<'de, D>(deserializer: D) -> Result<u8, D::Error>
where
    D: Deserializer<'de>,
{
    let value = u8::deserialize(deserializer)?;
    if value <= MAX_HOPS {
        Ok(value)
    } else {
        Err(serde::de::Error::custom(format!(
            "hops must be less than or equal to {MAX_HOPS}"
        )))
    }
}

fn validate_ping_interval<'de, D>(deserializer: D) -> Result<Option<PingInterval>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<PingInterval> = Option::deserialize(deserializer)?;
    match value {
        Some(interval) => {
            if interval.min < interval.max {
                Ok(Some(interval))
            } else {
                Err(serde::de::Error::custom("min must be less than max"))
            }
        }
        None => Ok(None),
    }
}

impl From<&SessionCapability> for session::Capability {
    fn from(val: &SessionCapability) -> Self {
        match val {
            SessionCapability::Segmentation => session::Capability::Segmentation,
            SessionCapability::Retransmission => session::Capability::Retransmission,
        }
    }
}

impl Connection {
    pub fn default_bridge_capabilities() -> Vec<session::Capability> {
        vec![session::Capability::Segmentation, session::Capability::Retransmission]
    }

    pub fn default_wg_capabilities() -> Vec<session::Capability> {
        vec![session::Capability::Segmentation]
    }

    pub fn default_bridge_target() -> SocketAddr {
        SocketAddr::from(([172, 30, 0, 1], 8000))
    }

    pub fn default_wg_target() -> SocketAddr {
        SocketAddr::from(([172, 30, 0, 1], 51820))
    }

    pub fn default_listen_host() -> String {
        ":1422".to_string()
    }

    pub fn default_session_timeout() -> Duration {
        Duration::from_secs(15)
    }

    pub fn default_ping_retry_timeout() -> Duration {
        Duration::from_secs(10)
    }

    pub fn default_ping_interval() -> PingInterval {
        PingInterval { min: 5, max: 10 }
    }

    pub fn default_bridge_buffer_size() -> String {
        "0 B".to_string()
    }

    pub fn default_ping_buffer_size() -> String {
        "0 B".to_string()
    }
    pub fn default_main_buffer_size() -> String {
        "1.5 MB".to_string()
    }

    pub fn default_bridge_max_surb_upstream() -> String {
        "0 bps".to_string()
    }

    pub fn default_ping_max_surb_upstream() -> String {
        "0 bps".to_string()
    }

    pub fn default_main_max_surb_upstream() -> String {
        "1 MB/s".to_string()
    }
}

impl Default for options::Options {
    fn default() -> Self {
        let bridge_target = session::Target::Plain(Connection::default_bridge_target());
        let wg_target = session::Target::Plain(Connection::default_wg_target());
        let bridge_caps = Connection::default_bridge_capabilities();
        let wg_caps = Connection::default_wg_capabilities();
        options::Options::new(
            options::SessionParameters::new(bridge_target, bridge_caps),
            options::SessionParameters::new(wg_target, wg_caps),
            Connection::default_ping_interval().min..Connection::default_ping_interval().max,
            monitor::PingOptions::default(),
            options::BufferSizes::from(BufferOptions {
                bridge: None,
                ping: None,
                main: None,
            }),
            options::MaxSurbUpstream::from(MaxSurbUpstreamOptions {
                bridge: None,
                ping: None,
                main: None,
            }),
            Connection::default_ping_retry_timeout(),
        )
    }
}

impl From<BufferOptions> for options::BufferSizes {
    fn from(buffer: BufferOptions) -> Self {
        options::BufferSizes::new(
            buffer.bridge.unwrap_or(Connection::default_bridge_buffer_size()),
            buffer.ping.unwrap_or(Connection::default_ping_buffer_size()),
            buffer.main.unwrap_or(Connection::default_main_buffer_size()),
        )
    }
}

impl Default for options::BufferSizes {
    fn default() -> Self {
        options::BufferSizes::new(
            Connection::default_bridge_buffer_size(),
            Connection::default_ping_buffer_size(),
            Connection::default_main_buffer_size(),
        )
    }
}

impl From<MaxSurbUpstreamOptions> for options::MaxSurbUpstream {
    fn from(surbs: MaxSurbUpstreamOptions) -> Self {
        options::MaxSurbUpstream::new(
            surbs.bridge.unwrap_or(Connection::default_bridge_max_surb_upstream()),
            surbs.ping.unwrap_or(Connection::default_ping_max_surb_upstream()),
            surbs.main.unwrap_or(Connection::default_main_max_surb_upstream()),
        )
    }
}

impl Default for options::MaxSurbUpstream {
    fn default() -> Self {
        options::MaxSurbUpstream::new(
            Connection::default_bridge_max_surb_upstream(),
            Connection::default_ping_max_surb_upstream(),
            Connection::default_main_max_surb_upstream(),
        )
    }
}

impl Config {
    pub fn entry_node(&self) -> EntryNode {
        let internal_connection_port = self.hoprd_node.internal_connection_port.map(|p| format!(":{p}"));
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
            self.hoprd_node.endpoint.clone(),
            self.hoprd_node.api_token.clone(),
            listen_host,
            session_timeout,
            entry_node::APIVersion::V4,
        )
    }

    pub fn destinations(&self) -> HashMap<Address, ConnDestination> {
        let config_dests = self.destinations.clone().unwrap_or_default();
        config_dests
            .iter()
            .map(|(k, v)| {
                let path = match v.path.clone() {
                    Some(DestinationPath::Intermediates(p)) => session::Path::IntermediatePath(p),
                    Some(DestinationPath::Hops(h)) => session::Path::Hops(h),
                    None => session::Path::default(),
                };

                let meta = v.meta.clone().unwrap_or_default();

                let dest = ConnDestination::new(*k, path, meta);
                (*k, dest)
            })
            .collect()
    }

    pub fn connection(&self) -> options::Options {
        let connection = self.connection.as_ref();
        let bridge_caps = connection
            .and_then(|c| c.bridge.as_ref())
            .and_then(|b| b.capabilities.clone())
            .map(|caps| caps.iter().map(|cap| cap.into()).collect::<Vec<session::Capability>>())
            .unwrap_or(Connection::default_bridge_capabilities());
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
        let params_bridge = options::SessionParameters::new(bridge_target, bridge_caps);
        let wg_caps = connection
            .and_then(|c| c.wg.as_ref())
            .and_then(|w| w.capabilities.clone())
            .map(|caps| caps.iter().map(|cap| cap.into()).collect::<Vec<session::Capability>>())
            .unwrap_or(Connection::default_wg_capabilities());
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
        let params_wg = options::SessionParameters::new(wg_target, wg_caps);

        let interval = connection
            .and_then(|c| c.ping.as_ref())
            .and_then(|p| p.interval.clone())
            .unwrap_or(Connection::default_ping_interval());

        let def_opts = monitor::PingOptions::default();
        let ping_opts = connection
            .and_then(|c| c.ping.as_ref())
            .map(|p| monitor::PingOptions {
                address: p.address.unwrap_or(def_opts.address),
                timeout: p.timeout.unwrap_or(def_opts.timeout),
                ttl: p.ttl.unwrap_or(def_opts.ttl),
                seq_count: p.seq_count.unwrap_or(def_opts.seq_count),
            })
            .unwrap_or(def_opts);
        let ping_range = interval.min..interval.max;

        let buffer_sizes = connection
            .and_then(|c| c.buffer.clone())
            .map(|b| b.into())
            .unwrap_or(options::BufferSizes::default());
        let max_surb_upstream = connection
            .and_then(|c| c.max_surb_upstream.clone())
            .map(|b| b.into())
            .unwrap_or(options::MaxSurbUpstream::default());
        let ping_retry_timeout = connection
            .and_then(|c| c.ping_retry_timeout)
            .unwrap_or(Connection::default_ping_retry_timeout());

        options::Options::new(
            params_bridge,
            params_wg,
            ping_range,
            ping_opts,
            buffer_sizes,
            max_surb_upstream,
            ping_retry_timeout,
        )
    }

    pub fn wireguard(&self) -> WireGuardConfig {
        let listen_port = self.wireguard.as_ref().and_then(|wg| wg.listen_port);
        let allowed_ips = self.wireguard.as_ref().and_then(|wg| wg.allowed_ips.clone());
        let force_private_key = self.wireguard.as_ref().and_then(|wg| wg.force_private_key.clone());
        WireGuardConfig::new(listen_port, allowed_ips, force_private_key)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn test_minimal_config() {
        let config = r#####"
version = 3
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse minimal config");
    }

    #[test]
    fn test_ping_without_interval() {
        let config = r#####"
version = 3
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"

[connection.ping]
address = "10.128.0.1"

"#####;
        toml::from_str::<Config>(config).expect("Failed to parse minimal ping");
    }

    #[test]
    fn test_full_config() {
        let config = r#####"
version = 3
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"
internal_connection_port = 1422

[destinations]

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
meta = { location = "Germany" }
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA"] }

[destinations.0xa5Ca174Ef94403d6162a969341a61baeA48F57F8]
meta = { location = "USA" }
path = { intermediates = ["0x25865191AdDe377fd85E91566241178070F4797A"] }

[destinations.0x8a6E6200C9dE8d8F8D9b4c08F86500a2E3Fbf254]
meta = { location = "Spain" }
path = { intermediates = ["0x2Cf9E5951C9e60e01b579f654dF447087468fc04"] }

[connection]
listen_host = "0.0.0.0:1422"
session_timeout = "15s"
ping_retry_timeout = "10s"

[connection.bridge]
capabilities = [ "segmentation", "retransmission" ]
target = "127.0.0.1:8000"
target_type = "plain"

[connection.wg]
capabilities = [ "segmentation" ]
target = "127.0.0.1:51820"
target_type = "sealed"

[connection.ping]
address = "10.128.0.1"
timeout = "4s"
ttl = 5
seq_count = 1
[connection.ping.interval]
min = 5
max = 10

[connection.max_surb_upstream]
bridge = "0 Mb/s"
ping = "1 Mb/s"
main = "2 Mb/s"

[connection.buffer]
bridge = "0 kB"
ping = "32 kB"
main = "8 MB"

[wireguard]
listen_port = 51820
allowed_ips = "10.128.0.1/9"
# use if you want to disable key rotation on every connection
force_private_key = "QLWiv7VCpJl8DNc09NGp9QRpLjrdZ7vd990qub98V3Q="
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse full config");
    }
}
