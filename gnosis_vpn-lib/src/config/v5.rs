use bytesize::ByteSize;
use edgli::hopr_lib::exports::network::types::types::{IpOrHost, SealedHost};
use edgli::hopr_lib::exports::transport::{SessionCapabilities, SessionCapability, SessionTarget};
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Deserializer, Serialize};

use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;

use crate::connection::options;
use crate::hopr::blokli_config::BlokliConfig as HoprBlokliConfig;
use crate::ping;
use crate::wireguard::Config as WireGuardConfig;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Connection {
    #[serde(default, with = "humantime_serde::option")]
    http_timeout: Option<Duration>,
    bridge: Option<ConnectionProtocol>,
    wg: Option<ConnectionProtocol>,
    ping: Option<PingOptions>,
    buffer: Option<BufferOptions>,
    max_surb_upstream: Option<MaxSurbUpstreamOptions>,
    announced_peer_minimum_score: Option<f64>,
    health_check_intervals: Option<HealthCheckIntervalOptions>,
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
struct HealthCheckIntervalOptions {
    #[serde(default, with = "humantime_serde::option")]
    ping: Option<Duration>,
    #[serde(default, deserialize_with = "validate_n_pings")]
    health_every_n_pings: Option<u32>,
    #[serde(default, deserialize_with = "validate_n_pings")]
    version_every_n_pings: Option<u32>,
    #[serde(default, with = "humantime_serde::option")]
    tunnel_ping: Option<Duration>,
    #[serde(default, deserialize_with = "validate_tunnel_ping_max_failures")]
    tunnel_ping_max_failures: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct BufferOptions {
    bridge: Option<ByteSize>,
    ping: Option<ByteSize>,
    main: Option<ByteSize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct MaxSurbUpstreamOptions {
    #[serde(default, with = "human_bandwidth::serde")]
    bridge: Option<Bandwidth>,
    #[serde(default, with = "human_bandwidth::serde")]
    ping: Option<Bandwidth>,
    #[serde(default, with = "human_bandwidth::serde")]
    main: Option<Bandwidth>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct WireGuard {
    pub(super) listen_port: Option<u16>,
    pub(super) allowed_ips: Option<String>,
    pub(super) force_private_key: Option<String>,
    pub(super) dns: Option<WireGuardDNS>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct WireGuardDNS {
    pub overwrite: bool,
    pub servers: Option<String>,
}

impl WireGuardDNS {
    fn default_server() -> String {
        "1.1.1.1,8.8.8.8".to_string()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct BlokliConfig {
    #[serde(default, with = "humantime_serde::option")]
    pub(super) connection_sync_timeout: Option<Duration>,
    pub(super) sync_tolerance: Option<usize>,
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
                for (k, v) in wg.iter() {
                    if k == "listen_port" || k == "allowed_ips" || k == "force_private_key" {
                        continue;
                    }
                    if k == "dns" {
                        if let Some(dns) = v.as_table() {
                            for (k2, _v2) in dns.iter() {
                                if k2 == "overwrite" || k2 == "servers" {
                                    continue;
                                }
                                wrong_keys.push(format!("wireguard.dns.{k2}"));
                            }
                        }
                        continue;
                    }
                    wrong_keys.push(format!("wireguard.{k}"));
                }
            }
            continue;
        }

        // blokli nested struct
        if key == "blokli" {
            if let Some(blokli) = value.as_table() {
                for (k, _v) in blokli.iter() {
                    if k == "connection_sync_timeout" || k == "sync_tolerance" {
                        continue;
                    }
                    wrong_keys.push(format!("blokli.{k}"));
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
                    if k == "bridge" || k == "wg" {
                        if let Some(prot) = v.as_table() {
                            for (k2, _v) in prot.iter() {
                                if k2 == "capabilities" || k2 == "target" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.{k}.{k2}"));
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
                    if k == "health_check_intervals" {
                        if let Some(hci) = v.as_table() {
                            for (k, _v) in hci.iter() {
                                if k == "ping"
                                    || k == "health_every_n_pings"
                                    || k == "version_every_n_pings"
                                    || k == "tunnel_ping"
                                    || k == "tunnel_ping_max_failures"
                                {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.health_check_intervals.{k}"));
                            }
                        }
                        continue;
                    }
                    if k == "announced_peer_minimum_score" {
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
                for (id, v) in destinations.iter() {
                    if let Some(dest) = v.as_table() {
                        for (k, _v) in dest.iter() {
                            if k == "address" || k == "meta" || k == "path" {
                                continue;
                            }
                            wrong_keys.push(format!("destinations.{id}.{k}"));
                        }
                        continue;
                    }
                    wrong_keys.push(format!("destinations.{id}"));
                }
            }
            continue;
        }

        wrong_keys.push(key.clone());
    }
    wrong_keys
}

fn validate_n_pings<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<u32>::deserialize(deserializer)?;
    if value == Some(0) {
        Err(serde::de::Error::custom("value must be greater than zero"))
    } else {
        Ok(value)
    }
}

fn validate_tunnel_ping_max_failures<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<i64>::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(n) if n < 1 => Err(serde::de::Error::custom("tunnel_ping_max_failures must be at least 1")),
        Some(n) => u32::try_from(n)
            .map(Some)
            .map_err(|_| serde::de::Error::custom("tunnel_ping_max_failures is out of range")),
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
            Capability::NoRateControl,
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
        Duration::from_secs(60)
    }

    pub fn default_announced_peer_minimum_score() -> f64 {
        0.1
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

        let def_opts = ping::Options::default();
        let ping_opts = connection
            .and_then(|c| c.ping.as_ref())
            .map(|p| ping::Options {
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
        let http_timeout = connection
            .and_then(|c| c.http_timeout)
            .unwrap_or(Connection::default_http_timeout());

        let timeouts = options::Timeouts { http: http_timeout };

        let announced_peer_minimum_score = connection
            .and_then(|c| c.announced_peer_minimum_score)
            .unwrap_or(Connection::default_announced_peer_minimum_score());

        let def_intervals = options::HealthCheckIntervals::default();
        let health_check_intervals = connection
            .and_then(|c| c.health_check_intervals.as_ref())
            .map(|h| options::HealthCheckIntervals {
                ping: h.ping.unwrap_or(def_intervals.ping),
                health_every_n_pings: h.health_every_n_pings.unwrap_or(def_intervals.health_every_n_pings),
                version_every_n_pings: h.version_every_n_pings.unwrap_or(def_intervals.version_every_n_pings),
                tunnel_ping: h.tunnel_ping.unwrap_or(def_intervals.tunnel_ping),
                tunnel_ping_max_failures: h
                    .tunnel_ping_max_failures
                    .unwrap_or(def_intervals.tunnel_ping_max_failures),
            })
            .unwrap_or(def_intervals);

        options::Options::new(
            sessions,
            ping_opts,
            buffer_sizes,
            max_surb_upstream,
            timeouts,
            announced_peer_minimum_score,
            health_check_intervals,
        )
    }
}

impl From<Option<WireGuard>> for WireGuardConfig {
    fn from(value: Option<WireGuard>) -> Self {
        let listen_port = value.as_ref().and_then(|wg| wg.listen_port);
        let allowed_ips = value.as_ref().and_then(|wg| wg.allowed_ips.clone());
        let force_private_key = value.as_ref().and_then(|wg| wg.force_private_key.clone());
        let dns = value
            .as_ref()
            .and_then(|wg| {
                wg.dns.as_ref().map(|dns| {
                    if dns.overwrite {
                        Some(dns.servers.clone().unwrap_or(WireGuardDNS::default_server()))
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(Some(WireGuardDNS::default_server()));
        WireGuardConfig::new(listen_port, allowed_ips, force_private_key, dns)
    }
}

impl From<Option<BlokliConfig>> for HoprBlokliConfig {
    fn from(value: Option<BlokliConfig>) -> Self {
        let connection_sync_timeout = value
            .as_ref()
            .and_then(|b| b.connection_sync_timeout)
            .unwrap_or_else(|| HoprBlokliConfig::default().connection_sync_timeout);
        // Edge client uses less tolerance than the default of 90%
        let sync_tolerance = value.as_ref().and_then(|b| b.sync_tolerance).unwrap_or(50);
        HoprBlokliConfig {
            connection_sync_timeout,
            sync_tolerance,
        }
    }
}
