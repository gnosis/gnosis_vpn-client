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
    pub(super) surb_balancing: Option<SurbBalancingConfig>,
    pub(super) health_check_intervals: Option<HealthCheckIntervalOptions>,
    pub(super) lan_lockdown: Option<bool>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) session_pseudonym_ttl: Option<Duration>,
    #[serde(default, deserialize_with = "validate_path_planner_min_ack_rate")]
    pub(super) path_planner_min_ack_rate: Option<f64>,
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
    pub(super) capabilities: Option<Vec<Capability>>,
    pub(super) target: Option<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct PingOptions {
    #[serde(default, with = "humantime_serde::option")]
    pub(super) timeout: Option<Duration>,
    pub(super) ttl: Option<u32>,
    pub(super) seq_count: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct HealthCheckIntervalOptions {
    #[serde(default, with = "humantime_serde::option")]
    pub(super) ping: Option<Duration>,
    #[serde(default, deserialize_with = "validate_n_pings")]
    pub(super) health_every_n_pings: Option<u32>,
    #[serde(default, deserialize_with = "validate_n_pings")]
    pub(super) version_every_n_pings: Option<u32>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) tunnel_ping: Option<Duration>,
    #[serde(default, deserialize_with = "validate_tunnel_ping_max_failures")]
    pub(super) tunnel_ping_max_failures: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct SessionSurbConfig {
    enabled: Option<bool>,
    buffer: Option<ByteSize>,
    #[serde(default, with = "human_bandwidth::serde")]
    max_surb_upstream: Option<Bandwidth>,
    always_max_out_surbs: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct SurbBalancingConfig {
    ping: Option<SessionSurbConfig>,
    main: Option<SessionSurbConfig>,
    bridge: Option<SessionSurbConfig>,
    health_check: Option<SessionSurbConfig>,
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

fn validate_path_planner_min_ack_rate<'de, D>(deserializer: D) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<f64>::deserialize(deserializer)?;
    match value {
        Some(v) if !(0.0..=1.0).contains(&v) => Err(serde::de::Error::custom(
            "path_planner_min_ack_rate must be in the range [0.0, 1.0]",
        )),
        other => Ok(other),
    }
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

pub(super) fn to_flags(caps: Vec<Capability>) -> SessionCapabilities {
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
    // Bridge capabilities must never include retransmissions — those require additional SURBs.
    pub fn default_bridge_capabilities() -> Vec<Capability> {
        vec![Capability::Segmentation, Capability::NoRateControl]
    }

    pub fn default_wg_capabilities() -> Vec<Capability> {
        vec![Capability::Segmentation, Capability::NoDelay]
    }

    pub fn default_http_timeout() -> Duration {
        Duration::from_secs(60)
    }
}

fn apply_session_surb(cfg: Option<SessionSurbConfig>, def: options::SessionSurbOptions) -> options::SessionSurbOptions {
    match cfg {
        None => def,
        Some(c) => {
            let enabled = c.enabled.unwrap_or(def.enabled);
            options::SessionSurbOptions {
                enabled,
                buffer: c.buffer.unwrap_or(def.buffer),
                max_surb_upstream: c.max_surb_upstream.unwrap_or(def.max_surb_upstream),
                always_max_out_surbs: c.always_max_out_surbs.unwrap_or(enabled),
            }
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
            .unwrap_or(options::default_bridge_target());
        let bridge_caps = connection
            .and_then(|c| c.bridge.as_ref())
            .and_then(|b| b.capabilities.clone())
            .unwrap_or(Connection::default_bridge_capabilities());
        let params_bridge = options::SessionParameters::new(bridge_target, to_flags(bridge_caps));

        let wg_target = connection
            .and_then(|c| c.wg.as_ref())
            .and_then(|w| w.target)
            .map(|socket| SessionTarget::UdpStream(SealedHost::Plain(IpOrHost::Ip(socket))))
            .unwrap_or(options::default_wg_target());
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
        // `address` is a placeholder here — every real ping is issued with
        // `destination.ping_address` substituted in at the call site.
        let ping_opts = connection
            .and_then(|c| c.ping.as_ref())
            .map(|p| ping::Options {
                address: def_opts.address,
                timeout: p.timeout.unwrap_or(def_opts.timeout),
                ttl: p.ttl.unwrap_or(def_opts.ttl),
                seq_count: p.seq_count.unwrap_or(def_opts.seq_count),
            })
            .unwrap_or(def_opts);

        let surb_cfg = connection.and_then(|c| c.surb_balancing.clone());
        let def = options::SurbBalancing::default();
        let surb_balancing = options::SurbBalancing {
            ping: apply_session_surb(surb_cfg.as_ref().and_then(|s| s.ping.clone()), def.ping),
            main: apply_session_surb(surb_cfg.as_ref().and_then(|s| s.main.clone()), def.main),
            bridge: apply_session_surb(surb_cfg.as_ref().and_then(|s| s.bridge.clone()), def.bridge),
            health_check: apply_session_surb(surb_cfg.as_ref().and_then(|s| s.health_check.clone()), def.health_check),
        };
        let http_timeout = connection
            .and_then(|c| c.http_timeout)
            .unwrap_or(Connection::default_http_timeout());

        let timeouts = options::Timeouts { http: http_timeout };

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

        // 1s effectively disables pseudonym caching; revert once hopr-lib supports PIX
        let session_pseudonym_ttl = connection
            .and_then(|c| c.session_pseudonym_ttl)
            .unwrap_or(Duration::from_secs(1));

        options::Options {
            sessions,
            ping_options: ping_opts,
            surb_balancing,
            timeouts,
            health_check_intervals,
            lan_lockdown: connection.and_then(|c| c.lan_lockdown).unwrap_or(false),
            session_pseudonym_ttl,
            path_planner_min_ack_rate: connection
                .and_then(|c| c.path_planner_min_ack_rate)
                .unwrap_or(options::DEFAULT_PATH_PLANNER_MIN_ACK_RATE),
        }
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

pub fn wrong_keys(table: &toml::Table) -> Vec<String> {
    let mut wrong = Vec::new();
    for (key, value) in table.iter() {
        if key == "version" {
            continue;
        }
        if key == "wireguard" {
            if let Some(wg) = value.as_table() {
                for (k, v) in wg.iter() {
                    if k == "listen_port" || k == "allowed_ips" || k == "force_private_key" {
                        continue;
                    }
                    if k == "dns" {
                        if let Some(dns) = v.as_table() {
                            for (k2, _) in dns.iter() {
                                if k2 == "overwrite" || k2 == "servers" {
                                    continue;
                                }
                                wrong.push(format!("wireguard.dns.{k2}"));
                            }
                        }
                        continue;
                    }
                    wrong.push(format!("wireguard.{k}"));
                }
            }
            continue;
        }
        if key == "blokli" {
            if let Some(blokli) = value.as_table() {
                for (k, _) in blokli.iter() {
                    if k == "connection_sync_timeout" || k == "sync_tolerance" {
                        continue;
                    }
                    wrong.push(format!("blokli.{k}"));
                }
            }
            continue;
        }
        if key == "connection" {
            if let Some(connection) = value.as_table() {
                for (k, v) in connection.iter() {
                    if k == "http_timeout"
                        || k == "announced_peer_minimum_score"
                        || k == "lan_lockdown"
                        || k == "session_pseudonym_ttl"
                        || k == "path_planner_min_ack_rate"
                    {
                        continue;
                    }
                    if k == "bridge" || k == "wg" {
                        if let Some(prot) = v.as_table() {
                            for (k2, _) in prot.iter() {
                                if k2 == "capabilities" || k2 == "target" {
                                    continue;
                                }
                                wrong.push(format!("connection.{k}.{k2}"));
                            }
                        }
                        continue;
                    }
                    if k == "ping" {
                        if let Some(ping) = v.as_table() {
                            for (k2, _) in ping.iter() {
                                if k2 == "timeout" || k2 == "ttl" || k2 == "seq_count" {
                                    continue;
                                }
                                wrong.push(format!("connection.ping.{k2}"));
                            }
                        }
                        continue;
                    }
                    if k == "surb_balancing" {
                        if let Some(surb) = v.as_table() {
                            for (k2, v2) in surb.iter() {
                                if k2 == "ping" || k2 == "main" || k2 == "bridge" || k2 == "health_check" {
                                    if let Some(session) = v2.as_table() {
                                        for (k3, _) in session.iter() {
                                            if k3 == "enabled"
                                                || k3 == "buffer"
                                                || k3 == "max_surb_upstream"
                                                || k3 == "always_max_out_surbs"
                                            {
                                                continue;
                                            }
                                            wrong.push(format!("connection.surb_balancing.{k2}.{k3}"));
                                        }
                                    }
                                    continue;
                                }
                                wrong.push(format!("connection.surb_balancing.{k2}"));
                            }
                        }
                        continue;
                    }
                    if k == "health_check_intervals" {
                        if let Some(hci) = v.as_table() {
                            for (k2, _) in hci.iter() {
                                if k2 == "ping"
                                    || k2 == "health_every_n_pings"
                                    || k2 == "version_every_n_pings"
                                    || k2 == "tunnel_ping"
                                    || k2 == "tunnel_ping_max_failures"
                                {
                                    continue;
                                }
                                wrong.push(format!("connection.health_check_intervals.{k2}"));
                            }
                        }
                        continue;
                    }
                    wrong.push(format!("connection.{k}"));
                }
            }
            continue;
        }
        if key == "destinations" {
            if let Some(destinations) = value.as_table() {
                for (id, v) in destinations.iter() {
                    if let Some(dest) = v.as_table() {
                        for (k, _) in dest.iter() {
                            if k == "address"
                                || k == "meta"
                                || k == "path"
                                || k == "bridge_target"
                                || k == "wg_target"
                                || k == "ping_address"
                            {
                                continue;
                            }
                            wrong.push(format!("destinations.{id}.{k}"));
                        }
                        continue;
                    }
                    wrong.push(format!("destinations.{id}"));
                }
            }
            continue;
        }
        if key == "strategy" {
            if let Some(strategy) = value.as_table() {
                for (k, v) in strategy.iter() {
                    if k == "desired_message_count" || k == "min_open_channels" || k == "target_open_channels" {
                        continue;
                    }
                    if k == "channel_allowlist" {
                        if let Some(allowlist) = v.as_table() {
                            for (k2, _) in allowlist.iter() {
                                if k2 == "enabled" || k2 == "peers" {
                                    continue;
                                }
                                wrong.push(format!("strategy.channel_allowlist.{k2}"));
                            }
                        }
                        continue;
                    }
                    wrong.push(format!("strategy.{k}"));
                }
            }
            continue;
        }
        wrong.push(key.clone());
    }
    wrong
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct ChannelAllowlistConfig {
    pub(super) enabled: bool,
    #[serde_as(as = "Vec<DisplayFromStr>")]
    pub(super) peers: Vec<Address>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Strategy {
    pub(super) desired_message_count: Option<u64>,
    pub(super) min_open_channels: Option<usize>,
    pub(super) target_open_channels: Option<usize>,
    pub(super) channel_allowlist: Option<ChannelAllowlistConfig>,
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
            channel_allowlist: v
                .as_ref()
                .and_then(|s| s.channel_allowlist.as_ref())
                .and_then(|c| c.enabled.then(|| c.peers.iter().cloned().collect())),
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
    /// Overrides the exit-side address the bridge session connects to for this destination only.
    pub(super) bridge_target: Option<SocketAddr>,
    /// Overrides the exit-side address the wg session connects to for this destination only.
    pub(super) wg_target: Option<SocketAddr>,
    /// Overrides the tunnel health-check ping address for this destination only.
    /// Defaults to the host part of this destination's `wg_target` when absent.
    pub(super) ping_address: Option<IpAddr>,
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
        let connection: options::Options = value.connection.into();
        if connection.surb_balancing.ping.enabled != connection.surb_balancing.main.enabled {
            return Err(config::Error::SurbBalancingMismatch);
        }
        let destinations = convert_destinations(
            value.destinations,
            &connection.sessions.bridge.target,
            &connection.sessions.wg.target,
        )?;
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

/// Extracts the plain IP from a `SessionTarget` built by this module — always
/// `TcpStream`/`UdpStream(SealedHost::Plain(IpOrHost::Ip(_)))`, since that's the only
/// shape `bridge_target`/`wg_target` are ever constructed in (see `default_bridge_target`,
/// `default_wg_target`, and the `SocketAddr`-only TOML `target`/`bridge_target`/`wg_target` fields).
pub(super) fn ip_of(target: &SessionTarget) -> IpAddr {
    match target {
        SessionTarget::TcpStream(SealedHost::Plain(IpOrHost::Ip(addr)))
        | SessionTarget::UdpStream(SealedHost::Plain(IpOrHost::Ip(addr))) => addr.ip(),
        other => unreachable!("bridge/wg targets are always plain IP sockets, got {other:?}"),
    }
}

pub fn convert_destinations(
    value: Option<HashMap<String, Destination>>,
    default_bridge_target: &SessionTarget,
    default_wg_target: &SessionTarget,
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
        let bridge_target = dest
            .bridge_target
            .map(|addr| SessionTarget::TcpStream(SealedHost::Plain(IpOrHost::Ip(addr))))
            .unwrap_or_else(|| default_bridge_target.clone());
        let wg_target = dest
            .wg_target
            .map(|addr| SessionTarget::UdpStream(SealedHost::Plain(IpOrHost::Ip(addr))))
            .unwrap_or_else(|| default_wg_target.clone());
        let ping_address = dest.ping_address.unwrap_or_else(|| ip_of(&wg_target));
        let dest = ConnDestination::new(
            id.to_string(),
            dest.address,
            path,
            meta,
            bridge_target,
            wg_target,
            ping_address,
        );
        result.insert(id.to_string(), dest);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{ChannelAllowlistConfig, Config, Strategy, convert_destinations};
    use crate::connection::options;
    use crate::hopr::strategy_config::StrategyConfig;
    use edgli::hopr_lib::HopRouting;
    use edgli::hopr_lib::api::types::primitive::prelude::Address;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("valid TOML")
    }

    fn convert(
        cfg: &Config,
    ) -> Result<std::collections::HashMap<String, crate::connection::destination::Destination>, crate::config::Error>
    {
        convert_destinations(
            cfg.destinations.clone(),
            &options::default_bridge_target(),
            &options::default_wg_target(),
        )
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
        let result = convert(&cfg).expect("should succeed");
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
        let result = convert(&cfg).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(d.routing, HopRouting::try_from(1).unwrap());
    }

    #[test]
    fn convert_destinations_empty_map_errors() {
        let result = convert_destinations(
            Some(std::collections::HashMap::new()),
            &options::default_bridge_target(),
            &options::default_wg_target(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn convert_destinations_none_errors() {
        let result = convert_destinations(None, &options::default_bridge_target(), &options::default_wg_target());
        assert!(result.is_err());
    }

    #[test]
    fn convert_destinations_target_overrides_are_applied() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
bridge_target = "10.0.0.5:8000"
wg_target = "10.0.0.5:51820"
ping_address = "10.0.0.9"
"#####,
        );
        let result = convert(&cfg).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(
            d.bridge_target,
            edgli::hopr_lib::exports::transport::SessionTarget::TcpStream(
                edgli::hopr_lib::exports::network::types::types::SealedHost::Plain(
                    edgli::hopr_lib::exports::network::types::types::IpOrHost::Ip("10.0.0.5:8000".parse().unwrap())
                )
            )
        );
        assert_eq!(
            d.wg_target,
            edgli::hopr_lib::exports::transport::SessionTarget::UdpStream(
                edgli::hopr_lib::exports::network::types::types::SealedHost::Plain(
                    edgli::hopr_lib::exports::network::types::types::IpOrHost::Ip("10.0.0.5:51820".parse().unwrap())
                )
            )
        );
        assert_eq!(d.ping_address, "10.0.0.9".parse::<std::net::IpAddr>().unwrap());
    }

    #[test]
    fn convert_destinations_ping_address_defaults_to_overridden_wg_target_ip() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
wg_target = "10.0.0.5:51820"
"#####,
        );
        let result = convert(&cfg).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(d.ping_address, "10.0.0.5".parse::<std::net::IpAddr>().unwrap());
    }

    #[test]
    fn convert_destinations_falls_back_to_global_defaults() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
"#####,
        );
        let result = convert(&cfg).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(d.bridge_target, options::default_bridge_target());
        assert_eq!(d.wg_target, options::default_wg_target());
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

    #[test]
    fn path_planner_min_ack_rate_defaults_to_point_one() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
"#####,
        );
        let result: crate::config::Config = cfg.try_into().expect("should succeed");
        assert_eq!(result.connection.path_planner_min_ack_rate, 0.1);
    }

    #[test]
    fn path_planner_min_ack_rate_reads_from_connection() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

[connection]
path_planner_min_ack_rate = 0.5
"#####,
        );
        let result: crate::config::Config = cfg.try_into().expect("should succeed");
        assert_eq!(result.connection.path_planner_min_ack_rate, 0.5);
    }

    #[test]
    fn path_planner_min_ack_rate_rejects_out_of_range() {
        for bad in &[-0.1_f64, 1.1, 2.0, -1.0] {
            let toml = format!(
                r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

[connection]
path_planner_min_ack_rate = {bad}
"#####
            );
            let result = toml::from_str::<Config>(&toml);
            assert!(
                result.is_err(),
                "expected rejection for path_planner_min_ack_rate = {bad}"
            );
        }
    }

    #[test]
    fn strategy_channel_allowlist_enabled_produces_some() {
        let addr: Address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739".parse().unwrap();
        let strategy = Some(Strategy {
            desired_message_count: None,
            min_open_channels: None,
            target_open_channels: None,
            channel_allowlist: Some(ChannelAllowlistConfig {
                enabled: true,
                peers: vec![addr.clone()],
            }),
        });
        let cfg: StrategyConfig = strategy.into();
        assert_eq!(cfg.channel_allowlist, Some(std::collections::HashSet::from([addr])));
    }

    #[test]
    fn strategy_channel_allowlist_disabled_produces_none() {
        let addr: Address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739".parse().unwrap();
        let strategy = Some(Strategy {
            desired_message_count: None,
            min_open_channels: None,
            target_open_channels: None,
            channel_allowlist: Some(ChannelAllowlistConfig {
                enabled: false,
                peers: vec![addr],
            }),
        });
        let cfg: StrategyConfig = strategy.into();
        assert!(cfg.channel_allowlist.is_none());
    }
}
