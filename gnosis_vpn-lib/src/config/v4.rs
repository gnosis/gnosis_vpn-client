use bytesize::ByteSize;
use edgli::hopr_lib::exports::network::types::types::{IpOrHost, RoutingOptions, SealedHost};
use edgli::hopr_lib::{Address, SessionCapabilities, SessionCapability, SessionTarget};
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::cmp::PartialEq;
use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;
use std::vec::Vec;

use crate::config;
use crate::connection::{destination::Destination as ConnDestination, options};
use crate::ping;
use crate::wg_tooling::Config as WireGuardConfig;

const MAX_HOPS: u8 = 3;

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    #[serde_as(as = "Option<HashMap<DisplayFromStr, _>>")]
    pub(super) destinations: Option<HashMap<Address, Destination>>,
    pub(super) connection: Option<Connection>,
    pub(super) wireguard: Option<WireGuard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Destination {
    meta: Option<HashMap<String, String>>,
    path: Option<DestinationPath>,
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum DestinationPath {
    #[serde(alias = "intermediates")]
    Intermediates(#[serde_as(as = "Vec<DisplayFromStr>")] Vec<Address>),
    #[serde(alias = "hops", deserialize_with = "validate_hops")]
    Hops(u8),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Connection {
    #[serde(default, with = "humantime_serde::option")]
    http_timeout: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    ping_retries_timeout: Option<Duration>,
    bridge: Option<ConnectionProtocol>,
    wg: Option<ConnectionProtocol>,
    ping: Option<PingOptions>,
    buffer: Option<BufferOptions>,
    max_surb_upstream: Option<MaxSurbUpstreamOptions>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum Capability {
    #[serde(alias = "segmentation")]
    Segmentation,
    #[serde(alias = "retransmission")]
    Retransmission,
    #[serde(alias = "retransmission_ack_only")]
    RetransmissionAckOnly,
    #[serde(alias = "no_delay")]
    NoDelay,
    #[serde(alias = "no_rate_control")]
    NoRateControl,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ConnectionProtocol {
    capabilities: Option<Vec<Capability>>,
    target: Option<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PingOptions {
    address: Option<IpAddr>,
    #[serde(default, with = "humantime_serde::option")]
    timeout: Option<Duration>,
    ttl: Option<u32>,
    seq_count: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct BufferOptions {
    // could be improved by using bytesize crates parser
    bridge: Option<ByteSize>,
    ping: Option<ByteSize>,
    main: Option<ByteSize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct MaxSurbUpstreamOptions {
    #[serde(with = "human_bandwidth::serde")]
    bridge: Option<Bandwidth>,
    #[serde(with = "human_bandwidth::serde")]
    ping: Option<Bandwidth>,
    #[serde(with = "human_bandwidth::serde")]
    main: Option<Bandwidth>,
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
                    if k == "http_timeout" {
                        continue;
                    }
                    if k == "ping_retries_timeout" {
                        continue;
                    }
                    if k == "bridge" || k == "wg" {
                        if let Some(prot) = v.as_table() {
                            for (k, _v) in prot.iter() {
                                if k == "capabilities" || k == "target" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.bridge.{k}"));
                            }
                        }
                        continue;
                    }
                    if k == "ping" {
                        if let Some(ping) = v.as_table() {
                            for (k, _v) in ping.iter() {
                                if k == "address" || k == "timeout" || k == "ttl" || k == "seq_count" {
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

fn to_flags(caps: Vec<Capability>) -> SessionCapabilities {
    let mut flags = SessionCapabilities::empty();
    for cap in caps {
        let cap = match cap {
            Capability::Segmentation => SessionCapability::Segmentation,
            Capability::Retransmission => SessionCapability::RetransmissionNack,
            Capability::RetransmissionAckOnly => SessionCapability::RetransmissionAck,
            Capability::NoDelay => SessionCapability::NoDelay,
            Capability::NoRateControl => SessionCapability::NoRateControl,
        };
        flags |= cap;
    }
    flags
}

impl Connection {
    pub fn default_bridge_capabilities() -> Vec<Capability> {
        vec![
            Capability::Segmentation,
            Capability::Retransmission,
            Capability::RetransmissionAckOnly,
        ]
    }

    pub fn default_wg_capabilities() -> Vec<Capability> {
        vec![Capability::Segmentation, Capability::NoDelay]
    }

    pub fn default_bridge_target() -> SessionTarget {
        SessionTarget::TcpStream(SealedHost::Plain(IpOrHost::Ip(SocketAddr::from((
            [172, 30, 0, 1],
            8000,
        )))))
    }

    pub fn default_wg_target() -> SessionTarget {
        SessionTarget::UdpStream(SealedHost::Plain(IpOrHost::Ip(SocketAddr::from((
            [172, 30, 0, 1],
            51820,
        )))))
    }

    pub fn default_http_timeout() -> Duration {
        Duration::from_secs(15)
    }

    pub fn default_ping_retry_timeout() -> Duration {
        Duration::from_secs(10)
    }
}

impl From<BufferOptions> for options::BufferSizes {
    fn from(buffer: BufferOptions) -> Self {
        let def = options::BufferSizes::default();
        options::BufferSizes {
            bridge: buffer.bridge.unwrap_or(def.bridge),
            ping: buffer.ping.unwrap_or(def.ping),
            main: buffer.main.unwrap_or(def.main),
        }
    }
}

impl From<MaxSurbUpstreamOptions> for options::MaxSurbUpstream {
    fn from(surbs: MaxSurbUpstreamOptions) -> Self {
        let def = options::MaxSurbUpstream::default();
        options::MaxSurbUpstream {
            bridge: surbs.bridge.unwrap_or(def.bridge),
            ping: surbs.ping.unwrap_or(def.ping),
            main: surbs.main.unwrap_or(def.main),
        }
    }
}

impl From<Option<Connection>> for options::Options {
    fn from(conn: Option<Connection>) -> Self {
        let connection = conn.as_ref();
        let bridge_target = connection
            .and_then(|c| c.bridge.as_ref())
            .and_then(|b| b.target)
            .map(|socket| SessionTarget::TcpStream(SealedHost::Plain(IpOrHost::Ip(socket))))
            .unwrap_or(Connection::default_bridge_target());
        let bridge_caps = connection
            .and_then(|c| c.bridge.as_ref())
            .and_then(|b| b.capabilities.clone())
            .unwrap_or(Connection::default_bridge_capabilities());
        let params_bridge = options::SessionParameters::new(bridge_target, to_flags(bridge_caps));

        let wg_target = connection
            .and_then(|c| c.wg.as_ref())
            .and_then(|w| w.target)
            .map(|socket| SessionTarget::UdpStream(SealedHost::Plain(IpOrHost::Ip(socket))))
            .unwrap_or(Connection::default_wg_target());
        let wg_caps = connection
            .and_then(|c| c.wg.as_ref())
            .and_then(|w| w.capabilities.clone())
            .unwrap_or(Connection::default_wg_capabilities());
        let params_wg = options::SessionParameters::new(wg_target, to_flags(wg_caps));
        let sessions = options::Sessions {
            bridge: params_bridge,
            wg: params_wg,
        };

        let def_opts = ping::PingOptions::default();
        let ping_opts = connection
            .and_then(|c| c.ping.as_ref())
            .map(|p| ping::PingOptions {
                address: p.address.unwrap_or(def_opts.address),
                timeout: p.timeout.unwrap_or(def_opts.timeout),
                ttl: p.ttl.unwrap_or(def_opts.ttl),
                seq_count: p.seq_count.unwrap_or(def_opts.seq_count),
            })
            .unwrap_or(def_opts);

        let buffer_sizes = connection
            .and_then(|c| c.buffer.clone())
            .map(|b| b.into())
            .unwrap_or_default();
        let max_surb_upstream = connection
            .and_then(|c| c.max_surb_upstream.clone())
            .map(|b| b.into())
            .unwrap_or_default();
        let ping_retries_timeout = connection
            .and_then(|c| c.ping_retries_timeout)
            .unwrap_or(Connection::default_ping_retry_timeout());
        let http_timeout = connection
            .and_then(|c| c.http_timeout)
            .unwrap_or(Connection::default_http_timeout());

        let timeouts = options::Timeouts {
            ping_retries: ping_retries_timeout,
            http: http_timeout,
        };

        options::Options::new(sessions, ping_opts, buffer_sizes, max_surb_upstream, timeouts)
    }
}

impl From<Option<WireGuard>> for WireGuardConfig {
    fn from(value: Option<WireGuard>) -> Self {
        let listen_port = value.as_ref().and_then(|wg| wg.listen_port);
        let allowed_ips = value.as_ref().and_then(|wg| wg.allowed_ips.clone());
        let force_private_key = value.as_ref().and_then(|wg| wg.force_private_key.clone());
        WireGuardConfig::new(listen_port, allowed_ips, force_private_key)
    }
}

impl TryFrom<Config> for config::Config {
    type Error = config::Error;

    fn try_from(value: Config) -> Result<Self, Self::Error> {
        let connection = value.connection.into();
        let destinations = convert_destinations(value.destinations)?;
        let wireguard = value.wireguard.into();
        Ok(config::Config {
            connection,
            destinations,
            wireguard,
        })
    }
}

pub fn convert_destinations(
    value: Option<HashMap<Address, Destination>>,
) -> Result<HashMap<Address, ConnDestination>, config::Error> {
    let config_dests = value.ok_or(config::Error::NoDestinations)?;
    if config_dests.is_empty() {
        return Err(config::Error::NoDestinations);
    }

    let mut result = HashMap::new();
    for (address, dest) in config_dests.iter() {
        let path = match dest.path.clone() {
            Some(DestinationPath::Intermediates(p)) => RoutingOptions::IntermediatePath(p.try_into()?),
            Some(DestinationPath::Hops(h)) => RoutingOptions::Hops(h.try_into()?),
            None => RoutingOptions::Hops(1.try_into()?),
        };

        let meta = dest.meta.clone().unwrap_or_default();

        let dest = ConnDestination::new(*address, path, meta);
        result.insert(*address, dest);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn test_minimal_config() -> anyhow::Result<()> {
        let config = r#####"
version = 4
"#####;
        toml::from_str::<Config>(config)?;
        Ok(())
    }

    #[test]
    fn test_ping_without_interval() -> anyhow::Result<()> {
        let config = r#####"
version = 4

[connection.ping]
address = "10.128.0.1"

"#####;
        toml::from_str::<Config>(config)?;
        Ok(())
    }

    #[test]
    fn config_parse_single_destination_should_succeed() -> anyhow::Result<()> {
        let config = r#####"
version = 4

[destinations]

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
meta = { location = "Germany" }
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA"] }
"#####;

        toml::from_str::<Config>(config)?;
        Ok(())
    }

    #[test]
    fn full_config_should_be_parsable() -> anyhow::Result<()> {
        let config = r#####"
version = 4

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
http_timeout = "15s"
ping_retries_timeout = "20s"

[connection.bridge]
capabilities = [ "segmentation", "retransmission" ]
target = "127.0.0.1:8000"

[connection.wg]
capabilities = [ "segmentation", "no_delay" ]
target = "127.0.0.1:51820"

[connection.ping]
address = "10.128.0.1"
timeout = "7s"
ttl = 6
seq_count = 1

[connection.max_surb_upstream]
bridge = "0 Mb/s"
ping = "1 Mb/s"
main = "16 Mb/s"

[connection.buffer]
bridge = "0 B"
ping = "32 kB"
main = "2 MB"

[wireguard]
listen_port = 51820
allowed_ips = "10.128.0.1/9"
# use if you want to disable key rotation on every connection
force_private_key = "QLWiv7VCpJl8DNc09NGp9QRpLjrdZ7vd990qub98V3Q="
"#####;
        toml::from_str::<Config>(config)?;

        Ok(())
    }
}
