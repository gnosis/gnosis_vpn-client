/// v6 keeps its older destinations dialect but reuses v7's shared schema before converting to `v7::Config`.
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::collections::HashMap;

use crate::config;

pub(super) use super::v7::{BlokliConfig, Connection, DestinationPath, Strategy, WireGuard};

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

<<<<<<< HEAD
=======
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct ConnectionProtocol {
    pub(super) capabilities: Option<Vec<Capability>>,
    pub(super) target: Option<SocketAddr>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct PingOptions {
    pub(super) address: Option<IpAddr>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) timeout: Option<Duration>,
    pub(super) ttl: Option<u32>,
    pub(super) seq_count: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct HealthCheckIntervalOptions {
    #[serde(default, with = "humantime_serde::option")]
    pub(super) version: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) ping: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) load: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) tunnel_ping: Option<Duration>,
    #[serde(default, deserialize_with = "validate_tunnel_ping_max_failures")]
    pub(super) tunnel_ping_max_failures: Option<u32>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) tunnel_ping_max_rtt: Option<Duration>,
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
pub(super) struct SurbRampConfig {
    #[serde(default, with = "humantime_serde::option")]
    interval: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    duration: Option<Duration>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct SurbBalancingConfig {
    ping: Option<SessionSurbConfig>,
    main: Option<SessionSurbConfig>,
    bridge: Option<SessionSurbConfig>,
    ramp: Option<SurbRampConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct WireGuard {
    pub(super) allowed_ips: Option<String>,
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
    #[serde(default, with = "humantime_serde::option")]
    pub(super) request_timeout: Option<Duration>,
    #[serde(default, with = "humantime_serde::option")]
    pub(super) resolve_timeout: Option<Duration>,
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
        // NoDelay on a stateless session preserves UDP/WireGuard datagram boundaries over the
        // session (hoprnet#8356): each datagram is delivered as exactly one read, so neptun never
        // sees a split/coalesced buffer.
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
}

/// Zero intervals feed `time::sleep(0)` and hammer the exit; a zero RTT ceiling calls every reply slow.
pub(super) fn reject_zero_intervals(intervals: &options::HealthCheckIntervals) -> Result<(), config::Error> {
    let any_zero = [
        intervals.version,
        intervals.ping,
        intervals.load,
        intervals.tunnel_ping,
        intervals.tunnel_ping_max_rtt,
    ]
    .iter()
    .any(Duration::is_zero);
    if any_zero {
        return Err(config::Error::HealthCheckIntervalZero);
    }
    Ok(())
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

        let surb_cfg = connection.and_then(|c| c.surb_balancing.clone());
        let def = options::SurbBalancing::default();
        let surb_balancing = options::SurbBalancing {
            ping: apply_session_surb(surb_cfg.as_ref().and_then(|s| s.ping.clone()), def.ping),
            main: apply_session_surb(surb_cfg.as_ref().and_then(|s| s.main.clone()), def.main),
            bridge: apply_session_surb(surb_cfg.as_ref().and_then(|s| s.bridge.clone()), def.bridge),
            ramp: options::SurbRampOptions {
                interval: surb_cfg
                    .as_ref()
                    .and_then(|s| s.ramp.as_ref())
                    .and_then(|r| r.interval)
                    .unwrap_or(def.ramp.interval),
                duration: surb_cfg
                    .as_ref()
                    .and_then(|s| s.ramp.as_ref())
                    .and_then(|r| r.duration)
                    .unwrap_or(def.ramp.duration),
            },
        };
        let http_timeout = connection
            .and_then(|c| c.http_timeout)
            .unwrap_or(Connection::default_http_timeout());

        let timeouts = options::Timeouts { http: http_timeout };

        let def_intervals = options::HealthCheckIntervals::default();
        let health_check_intervals = connection
            .and_then(|c| c.health_check_intervals.as_ref())
            .map(|h| options::HealthCheckIntervals {
                version: h.version.unwrap_or(def_intervals.version),
                ping: h.ping.unwrap_or(def_intervals.ping),
                load: h.load.unwrap_or(def_intervals.load),
                tunnel_ping: h.tunnel_ping.unwrap_or(def_intervals.tunnel_ping),
                tunnel_ping_max_failures: h
                    .tunnel_ping_max_failures
                    .unwrap_or(def_intervals.tunnel_ping_max_failures),
                tunnel_ping_max_rtt: h.tunnel_ping_max_rtt.unwrap_or(def_intervals.tunnel_ping_max_rtt),
            })
            .unwrap_or(def_intervals);

        options::Options {
            sessions,
            ping_options: ping_opts,
            surb_balancing,
            timeouts,
            health_check_intervals,
            lan_lockdown: connection.and_then(|c| c.lan_lockdown).unwrap_or(false),
            probe_local_addresses: connection.and_then(|c| c.probe_local_addresses).unwrap_or(false),
            path_planner_min_ack_rate: connection
                .and_then(|c| c.path_planner_min_ack_rate)
                .unwrap_or(options::DEFAULT_PATH_PLANNER_MIN_ACK_RATE),
            path_planner: connection.and_then(|c| c.path_planner.clone()).unwrap_or_default(),
        }
    }
}

impl From<Option<WireGuard>> for WireGuardConfig {
    fn from(value: Option<WireGuard>) -> Self {
        let allowed_ips = value.as_ref().and_then(|wg| wg.allowed_ips.clone());
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
        WireGuardConfig::new(allowed_ips, dns)
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
        let request_timeout = value
            .as_ref()
            .and_then(|b| b.request_timeout)
            .unwrap_or_else(|| HoprBlokliConfig::default().request_timeout);
        let resolve_timeout = value
            .as_ref()
            .and_then(|b| b.resolve_timeout)
            .unwrap_or_else(|| HoprBlokliConfig::default().resolve_timeout);
        HoprBlokliConfig {
            connection_sync_timeout,
            sync_tolerance,
            request_timeout,
            resolve_timeout,
        }
    }
}

// ── v6-specific items ─────────────────────────────────────────────────────────

>>>>>>> dc09334 (feat(root): re-resolve the Blokli address before every first killswitch arm, the startup pin is the fallback (GNO-1025) (#957))
pub fn wrong_keys(table: &toml::Table) -> Vec<String> {
    let mut wrong = Vec::new();
    for (key, value) in table.iter() {
        if key == "version" {
            continue;
        }
        if key == "wireguard" {
            if let Some(wg) = value.as_table() {
                for (k, v) in wg.iter() {
                    if k == "allowed_ips" {
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
                    let is_known = k == "connection_sync_timeout"
                        || k == "sync_tolerance"
                        || k == "request_timeout"
                        || k == "resolve_timeout";
                    if is_known {
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
                        || k == "lan_lockdown"
                        || k == "probe_local_addresses"
                        || k == "path_planner_min_ack_rate"
                    {
                        continue;
                    }
                    if k == "path_planner" {
                        if let Some(pp) = v.as_table() {
                            for (k2, _) in pp.iter() {
                                if matches!(
                                    k2.as_str(),
                                    "max_cache_capacity"
                                        | "cache_ttl"
                                        | "refresh_period"
                                        | "max_cached_paths"
                                        | "edge_penalty"
                                        | "min_paths_anonymity_floor"
                                        | "latency_halflife"
                                        | "capacity_reference"
                                        | "return_path_weight_temper"
                                        | "return_path_exploration"
                                        | "max_plausible_loopback_rtt"
                                ) {
                                    continue;
                                }
                                wrong.push(format!("connection.path_planner.{k2}"));
                            }
                        }
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
                                if k2 == "address" || k2 == "timeout" || k2 == "ttl" || k2 == "seq_count" {
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
                                if k2 == "ping" || k2 == "main" || k2 == "bridge" {
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
                                if k2 == "ramp" {
                                    if let Some(ramp) = v2.as_table() {
                                        for (k3, _) in ramp.iter() {
                                            if k3 == "interval" || k3 == "duration" {
                                                continue;
                                            }
                                            wrong.push(format!("connection.surb_balancing.ramp.{k3}"));
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
                                if k2 == "version"
                                    || k2 == "ping"
                                    || k2 == "load"
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
                            if k == "address" || k == "meta" || k == "path" {
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
                    if matches!(
                        k.as_str(),
                        "min_open_channels"
                            | "target_open_channels"
                            | "channel_capacity"
                            | "topup_capacity"
                            | "lower_capacity_threshold"
                    ) {
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
                    if k == "sizing_mode" {
                        if let Some(mode) = v.as_table() {
                            for (k2, v2) in mode.iter() {
                                if k2 == "deterministic" {
                                    continue;
                                }
                                if k2 != "probabilistic" {
                                    wrong.push(format!("strategy.sizing_mode.{k2}"));
                                    continue;
                                }
                                if let Some(probabilistic) = v2.as_table() {
                                    for (k3, _) in probabilistic.iter() {
                                        if k3 == "success_probability" {
                                            continue;
                                        }
                                        wrong.push(format!("strategy.sizing_mode.probabilistic.{k3}"));
                                    }
                                }
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

impl TryFrom<Config> for super::v7::Config {
    type Error = config::Error;

    fn try_from(value: Config) -> Result<Self, Self::Error> {
        let destinations = value.destinations.map(|dests| {
            dests
                .into_iter()
                .map(|(id, d)| {
                    (
                        id,
                        super::v7::Destination {
                            address: d.address,
                            meta: d.meta,
                            path: d.path,
                            gnosis_vpn_server: None,
                            wireguard_server: None,
                        },
                    )
                })
                .collect()
        });

        // Reported as unsupported by `wrong_keys`, so the shared struct must not apply it either.
        let mut connection = value.connection;
        if let Some(c) = connection.as_mut() {
            c.pix = None;
            if let Some(h) = c.health_check_intervals.as_mut() {
                h.tunnel_ping_max_rtt = None;
            }
        }

        Ok(super::v7::Config {
            version: value.version,
            destinations,
            connection,
            wireguard: value.wireguard,
            blokli: value.blokli,
            strategy: value.strategy,
            pix_strategy: None,
            geoip_db: Some(crate::geoip::DEFAULT_DB_PATH.into()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, wrong_keys};

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("valid TOML")
    }

    /// v6 has no direct conversion into the runtime config — it forward-converts through v7.
    fn runtime_config(cfg: Config) -> crate::config::Config {
        let v7_cfg: super::super::v7::Config = cfg.try_into().expect("should forward-convert");
        v7_cfg.try_into().expect("should succeed")
    }

    /// End-to-end regression: a v6 file forward-converts through v7 into the runtime config,
    /// with its destination resolved to a plain `Configured` entry carrying no target override.
    #[test]
<<<<<<< HEAD
    fn v6_file_forward_converts_into_the_runtime_config() {
=======
    fn blokli_request_timeout_is_optional() {
        let cfg = parse(
            r#####"
version = 6

[blokli]
sync_tolerance = 50
"#####,
        );
        let blokli = cfg.blokli.expect("blokli section present");
        assert_eq!(blokli.request_timeout, None);
    }

    #[test]
    fn absent_blokli_request_timeout_falls_back_to_the_default() {
        let cfg = parse(
            r#####"
version = 6

[blokli]
sync_tolerance = 50
"#####,
        );
        let blokli: HoprBlokliConfig = cfg.blokli.into();
        assert_eq!(blokli.request_timeout, HoprBlokliConfig::default().request_timeout);
    }

    #[test]
    fn configured_blokli_request_timeout_wins_over_the_default() {
        let cfg = parse(
            r#####"
version = 6

[blokli]
request_timeout = "45s"
"#####,
        );
        let blokli: HoprBlokliConfig = cfg.blokli.into();
        assert_eq!(blokli.request_timeout, Duration::from_secs(45));
    }

    #[test]
    fn absent_blokli_resolve_timeout_falls_back_to_the_default() {
        let cfg = parse("version = 6\n\n[blokli]\nsync_tolerance = 50\n");
        let blokli: HoprBlokliConfig = cfg.blokli.into();
        assert_eq!(blokli.resolve_timeout, HoprBlokliConfig::default().resolve_timeout);
    }

    #[test]
    fn configured_blokli_resolve_timeout_wins_over_the_default() {
        let cfg = parse("version = 6\n\n[blokli]\nresolve_timeout = \"7s\"\n");
        let blokli: HoprBlokliConfig = cfg.blokli.into();
        assert_eq!(blokli.resolve_timeout, Duration::from_secs(7));
    }

    #[test]
    fn blokli_request_timeout_is_a_supported_key() {
        let table = r#####"
version = 6

[blokli]
connection_sync_timeout = "30s"
sync_tolerance = 50
request_timeout = "10s"
resolve_timeout = "3s"
nonsense = 1
"#####
            .parse::<toml::Table>()
            .expect("valid TOML");

        assert_eq!(wrong_keys(&table), vec!["blokli.nonsense".to_string()]);
    }

    #[test]
    fn convert_destinations_hops_path_preserved() {
>>>>>>> dc09334 (feat(root): re-resolve the Blokli address before every first killswitch arm, the startup pin is the fallback (GNO-1025) (#957))
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { hops = 2 }
"#####,
        );
        let result = runtime_config(cfg);

        let dest = result
            .destinations
            .by_connect_id("Germany")
            .expect("destination present");
        assert_eq!(dest.routing, edgli::hopr_lib::HopRouting::try_from(2).unwrap());
        assert_eq!(
            dest.gnosis_vpn_server,
            super::super::v7::Connection::default_bridge_socket()
        );
        assert_eq!(dest.wireguard_server, super::super::v7::Connection::default_wg_socket());
    }

    #[test]
    fn v6_file_without_destinations_still_loads() {
        // v6 historically required at least one destination; now that the requirement lives
        // only in v7's `convert_destinations`, forward-converting relaxes it here too — a v6
        // file with none is no longer an error.
        let cfg = parse("version = 6\n");
        let result = runtime_config(cfg);
        assert!(result.destinations.is_empty());
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
    fn wireguard_listen_port_is_reported_as_a_wrong_key() {
        let table: toml::Table = r#####"
version = 6

[wireguard]
listen_port = 51820
allowed_ips = "10.0.0.0/8"
"#####
            .parse()
            .expect("valid TOML");
        assert_eq!(wrong_keys(&table), vec!["wireguard.listen_port".to_string()]);
    }

    #[test]
    fn destinations_gnosis_vpn_server_is_not_a_supported_key_in_v6() {
        let table = r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
gnosis_vpn_server = "172.30.0.1:8000"
"#####
            .parse::<toml::Table>()
            .expect("valid TOML");

        assert_eq!(
            wrong_keys(&table),
            vec!["destinations.Germany.gnosis_vpn_server".to_string()]
        );
    }

    /// The knob a `release/hoprdv4` config carries; an upgrading user must be told it is inert.
    #[test]
    fn obsolete_min_safe_capacity_is_flagged() {
        let table = r#####"
version = 6

[strategy]
min_safe_capacity_required = "640 MiB"
"#####
            .parse::<toml::Table>()
            .expect("valid TOML");

        assert_eq!(
            wrong_keys(&table),
            vec!["strategy.min_safe_capacity_required".to_string()]
        );
    }

    /// `[connection]` is shared with v7, so a v6 file may carry the path-planner overrides too.
    #[test]
    fn v6_file_still_accepts_the_path_planner_overrides() {
        let table = r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

[connection.path_planner]
min_paths_anonymity_floor = 4
latency_halflife = "50ms"
"#####
            .parse::<toml::Table>()
            .expect("valid TOML");

        assert!(wrong_keys(&table).is_empty());
    }

    /// `[connection]` is shared with v7, so a v6 file may carry the SURB ramp pacing too.
    #[test]
    fn v6_file_still_accepts_the_surb_ramp() {
        let table = r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

[connection.surb_balancing.ramp]
interval = "500ms"
duration = "10s"
"#####
            .parse::<toml::Table>()
            .expect("valid TOML");

        assert!(wrong_keys(&table).is_empty());
    }

    /// Warned about, and dropped — not left to leak through the shared `[connection]` struct.
    #[test]
    fn pix_sections_are_not_supported_keys_in_v6() {
        let toml = r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

[connection.pix.bridge]
enabled = true

[connection.pix.dimensions]
num_ssa_parts     = 8
ssa_part_size     = 2
additional_shares = 2

[pix_strategy]
max_deposit_retries = 5
"#####;
        assert_eq!(
            wrong_keys(&toml.parse::<toml::Table>().expect("valid TOML")),
            vec!["connection.pix".to_string(), "pix_strategy".to_string()]
        );

        let result = runtime_config(parse(toml));
        assert_eq!(result.connection.pix, crate::connection::options::PixOptions::default());
        assert_eq!(result.pix_strategy, crate::hopr::pix_config::PixConfig::default());
    }

    #[test]
    fn tunnel_ping_max_rtt_is_not_a_supported_key_in_v6() {
        let toml = r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"

[connection.health_check_intervals]
tunnel_ping_max_rtt = "9s"
"#####;
        assert_eq!(
            wrong_keys(&toml.parse::<toml::Table>().expect("valid TOML")),
            vec!["connection.health_check_intervals.tunnel_ping_max_rtt".to_string()]
        );

        let result = runtime_config(parse(toml));
        assert_eq!(
            result.connection.health_check_intervals.tunnel_ping_max_rtt,
            crate::connection::options::HealthCheckIntervals::default().tunnel_ping_max_rtt
        );
    }

    #[test]
    fn geoip_db_is_not_a_supported_key_in_v6() {
        let toml = r#####"
version = 6
geoip_db = "/usr/share/gnosisvpn/geoip.mmdb"

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
"#####;
        assert_eq!(
            wrong_keys(&toml.parse::<toml::Table>().expect("valid TOML")),
            vec!["geoip_db".to_string()]
        );

        assert_eq!(
            std::path::PathBuf::from(crate::geoip::DEFAULT_DB_PATH),
            runtime_config(parse(toml)).geoip_db
        );
    }
}
