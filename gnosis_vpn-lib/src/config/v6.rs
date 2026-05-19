/// Config v6: identical to v5 except `Intermediates` is removed from
/// `DestinationPath`. Only hop-count based routing is supported.
///
/// Existing v4/v5 configs with `intermediates` must be migrated by replacing
/// `path = { intermediates = [...] }` with `path = { hops = <count> }`.
use bytesize::ByteSize;
use edgli::hopr_lib::HopRouting;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use edgli::hopr_lib::exports::network::types::types::{IpOrHost, SealedHost};
use edgli::hopr_lib::exports::transport::{SessionCapabilities, SessionCapability, SessionTarget};
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::time::Duration;
use std::vec::Vec;

use crate::config;
use crate::connection::{destination::Destination as ConnDestination, options};
use crate::hopr::blokli_config::BlokliConfig as HoprBlokliConfig;
use crate::hopr::strategy_config::StrategyConfig;
use crate::ping;
use crate::wireguard::Config as WireGuardConfig;

// Maximum supported hop count — used in both v5 and v6 conversion.
pub(super) const MAX_HOPS: u8 = 3;

// ── Shared structs (used by both v5 and v6) ──────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Connection {
    #[serde(default, with = "humantime_serde::option")]
    pub(super) http_timeout: Option<Duration>,
    pub(super) bridge: Option<ConnectionProtocol>,
    pub(super) wg: Option<ConnectionProtocol>,
    pub(super) ping: Option<PingOptions>,
    pub(super) buffer: Option<BufferOptions>,
    pub(super) max_surb_upstream: Option<MaxSurbUpstreamOptions>,
    pub(super) announced_peer_minimum_score: Option<f64>,
    pub(super) health_check_intervals: Option<HealthCheckIntervalOptions>,
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
pub(super) struct ConnectionProtocol {
    capabilities: Option<Vec<Capability>>,
    target: Option<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct PingOptions {
    address: Option<IpAddr>,
    #[serde(default, with = "humantime_serde::option")]
    timeout: Option<Duration>,
    ttl: Option<u32>,
    seq_count: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct HealthCheckIntervalOptions {
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
pub(super) struct BufferOptions {
    bridge: Option<ByteSize>,
    ping: Option<ByteSize>,
    main: Option<ByteSize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct MaxSurbUpstreamOptions {
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

// ── Shared helpers ────────────────────────────────────────────────────────────

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

pub(super) fn validate_hops<'de, D>(deserializer: D) -> Result<u8, D::Error>
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

// ── Shared From impls ─────────────────────────────────────────────────────────

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
        let sync_tolerance = value
            .as_ref()
            .and_then(|b| b.sync_tolerance)
            .unwrap_or_else(|| HoprBlokliConfig::default().sync_tolerance);
        HoprBlokliConfig {
            connection_sync_timeout,
            sync_tolerance,
        }
    }
}

// ── v6-specific items ─────────────────────────────────────────────────────────

/// v6 extends v5's wrong_keys by recognising the `[strategy]` section.
pub fn wrong_keys(table: &toml::Table) -> Vec<String> {
    let mut wrong = super::v5::wrong_keys(table);
    // `strategy` is valid in v6 — remove it from the "unknown" list.
    wrong.retain(|k| k != "strategy");
    // Check for unknown sub-keys inside [strategy].
    if let Some(strategy) = table.get("strategy").and_then(|v| v.as_table()) {
        for k in strategy.keys() {
            if k != "desired_message_count" && k != "min_open_channels" && k != "target_open_channels" {
                wrong.push(format!("strategy.{k}"));
            }
        }
    }
    wrong
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Strategy {
    pub(super) desired_message_count: Option<u64>,
    pub(super) min_open_channels: Option<usize>,
    pub(super) target_open_channels: Option<usize>,
}

impl From<Option<Strategy>> for StrategyConfig {
    fn from(v: Option<Strategy>) -> Self {
        let def = StrategyConfig::default();
        Self {
            desired_message_count: v
                .as_ref()
                .and_then(|s| s.desired_message_count)
                .unwrap_or(def.desired_message_count),
            min_open_channels: v
                .as_ref()
                .and_then(|s| s.min_open_channels)
                .unwrap_or(def.min_open_channels),
            target_open_channels: v
                .as_ref()
                .and_then(|s| s.target_open_channels)
                .unwrap_or(def.target_open_channels),
        }
    }
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub(super) destinations: Option<HashMap<String, Destination>>,
    pub(super) connection: Option<Connection>,
    pub(super) wireguard: Option<WireGuard>,
    pub(super) blokli: Option<BlokliConfig>,
    pub(super) strategy: Option<Strategy>,
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Destination {
    #[serde_as(as = "DisplayFromStr")]
    pub(super) address: Address,
    pub(super) meta: Option<HashMap<String, String>>,
    pub(super) path: Option<DestinationPath>,
}

/// Routing path for v6 — only hop-count routing is supported.
///
/// `Intermediates` is intentionally absent; configs that previously used
/// `path = { intermediates = [...] }` must be updated to
/// `path = { hops = <count> }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum DestinationPath {
    #[serde(alias = "hops", deserialize_with = "validate_hops")]
    Hops(u8),
}

impl TryFrom<Config> for config::Config {
    type Error = config::Error;

    fn try_from(value: Config) -> Result<Self, Self::Error> {
        let connection = value.connection.into();
        let destinations = convert_destinations(value.destinations)?;
        let wireguard = value.wireguard.into();
        let blokli = value.blokli.into();
        let strategy = value.strategy.into();
        Ok(config::Config {
            connection,
            destinations,
            wireguard,
            blokli,
            strategy,
        })
    }
}

pub fn convert_destinations(
    value: Option<HashMap<String, Destination>>,
) -> Result<HashMap<String, ConnDestination>, config::Error> {
    let config_dests = value.ok_or(config::Error::NoDestinations)?;
    if config_dests.is_empty() {
        return Err(config::Error::NoDestinations);
    }

    let mut result = HashMap::new();
    for (id, dest) in config_dests.iter() {
        let path = match dest.path {
            Some(DestinationPath::Hops(h)) => HopRouting::try_from(h as usize)?,
            None => HopRouting::try_from(1)?,
        };

        let meta = dest.meta.clone().unwrap_or_default();
        let dest = ConnDestination::new(id.to_string(), dest.address, path, meta);
        result.insert(id.to_string(), dest);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{Config, convert_destinations};
    use edgli::hopr_lib::HopRouting;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("valid TOML")
    }

    #[test]
    fn convert_destinations_hops_path_preserved() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { hops = 2 }
"#####,
        );
        let result = convert_destinations(cfg.destinations).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(d.routing, HopRouting::try_from(2).unwrap());
    }

    #[test]
    fn convert_destinations_none_path_defaults_to_1_hop() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
"#####,
        );
        let result = convert_destinations(cfg.destinations).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(d.routing, HopRouting::try_from(1).unwrap());
    }

    #[test]
    fn convert_destinations_empty_map_errors() {
        let result = convert_destinations(Some(std::collections::HashMap::new()));
        assert!(result.is_err());
    }

    #[test]
    fn convert_destinations_none_errors() {
        let result = convert_destinations(None);
        assert!(result.is_err());
    }

    #[test]
    fn intermediates_path_rejected_in_v6() {
        // v6 does not support the deprecated `intermediates` key — deserialization
        // must fail when it appears in a destination path.
        let result = toml::from_str::<Config>(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA"] }
"#####,
        );
        assert!(result.is_err(), "v6 must reject intermediates path");
    }

    #[test]
    fn hops_validation_rejects_above_max() {
        let result = toml::from_str::<Config>(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { hops = 4 }
"#####,
        );
        assert!(result.is_err(), "v6 must reject hops > MAX_HOPS");
    }
}
