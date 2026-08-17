/// Config v5: like v7 except the `surb_balancing` section is instead two separate `buffer` and
/// `max_surb_upstream` sections, and destination paths may still use the deprecated
/// `intermediates` form. Forward-converts into `v7::Config`.
use bytesize::ByteSize;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use human_bandwidth::re::bandwidth::Bandwidth;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::collections::HashMap;
use std::time::Duration;
use std::vec::Vec;

use crate::config;

// Types from v7 that are schema-identical in v5 are re-used directly.
// Connection, BufferOptions, and MaxSurbUpstreamOptions are defined below because
// v5 uses separate `buffer` and `max_surb_upstream` sections instead of `surb_balancing`.
use super::v7::MAX_HOPS;
pub(super) use super::v7::{
    BlokliConfig, ConnectionProtocol, HealthCheckIntervalOptions, PingOptions, SessionSurbConfig, SurbBalancingConfig,
    WireGuard,
};

// v5 defines its own Connection to carry the separate `buffer` and `max_surb_upstream`
// sections which v7 (already true as of v6) replaced with the unified `surb_balancing` section.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Connection {
    #[serde(default, with = "humantime_serde::option")]
    pub(super) http_timeout: Option<Duration>,
    pub(super) bridge: Option<ConnectionProtocol>,
    pub(super) wg: Option<ConnectionProtocol>,
    pub(super) ping: Option<PingOptions>,
    pub(super) buffer: Option<BufferOptions>,
    pub(super) max_surb_upstream: Option<MaxSurbUpstreamOptions>,
    /// Parsed but never consumed — dropped silently when forward-converting into
    /// `v7::Connection`, exactly as it was silently unused in the runtime conversion before.
    pub(super) announced_peer_minimum_score: Option<f64>,
    pub(super) health_check_intervals: Option<HealthCheckIntervalOptions>,
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

/// Maps v5's separate `buffer`/`max_surb_upstream` sections onto v7's unified `surb_balancing`
/// shape. `enabled`/`always_max_out_surbs` are left to v7's own defaults (`ping`/`main` on,
/// `bridge`/`health_check` off) since v5 never had per-session enable flags.
fn to_surb_balancing_config(buf: Option<BufferOptions>, surbs: Option<MaxSurbUpstreamOptions>) -> SurbBalancingConfig {
    let buf = buf.unwrap_or(BufferOptions {
        bridge: None,
        ping: None,
        main: None,
    });
    let surbs = surbs.unwrap_or(MaxSurbUpstreamOptions {
        bridge: None,
        ping: None,
        main: None,
    });
    let session = |buffer: Option<ByteSize>, max_surb_upstream: Option<Bandwidth>| SessionSurbConfig {
        enabled: None,
        buffer,
        max_surb_upstream,
        always_max_out_surbs: None,
    };
    SurbBalancingConfig {
        ping: Some(session(buf.ping, surbs.ping)),
        main: Some(session(buf.main, surbs.main)),
        bridge: Some(session(buf.bridge, surbs.bridge)),
        health_check: None,
    }
}

impl From<Option<Connection>> for super::v7::Connection {
    fn from(conn: Option<Connection>) -> Self {
        let conn = conn.as_ref();
        super::v7::Connection {
            http_timeout: conn.and_then(|c| c.http_timeout),
            bridge: conn.and_then(|c| c.bridge.clone()),
            wg: conn.and_then(|c| c.wg.clone()),
            ping: conn.and_then(|c| c.ping.clone()),
            surb_balancing: Some(to_surb_balancing_config(
                conn.and_then(|c| c.buffer.clone()),
                conn.and_then(|c| c.max_surb_upstream.clone()),
            )),
            health_check_intervals: conn.and_then(|c| c.health_check_intervals.clone()),
            lan_lockdown: None,
            probe_local_addresses: None,
            path_planner_min_ack_rate: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub(super) destinations: Option<HashMap<String, Destination>>,
    pub(super) connection: Option<Connection>,
    pub(super) wireguard: Option<WireGuard>,
    pub(super) blokli: Option<BlokliConfig>,
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Destination {
    #[serde_as(as = "DisplayFromStr")]
    pub(super) address: Address,
    pub(super) meta: Option<HashMap<String, String>>,
    pub(super) path: Option<DestinationPath>,
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum DestinationPath {
    #[serde(alias = "intermediates")]
    Intermediates(#[serde_as(as = "Vec<DisplayFromStr>")] Vec<Address>),
    #[serde(alias = "hops", deserialize_with = "super::v7::validate_hops")]
    Hops(u8),
}

/// Resolves a v5/v4 destination path (which may still use the deprecated `intermediates` form)
/// into v7's hop-count-only shape, logging when the deprecated form is in play.
pub(super) fn resolve_path(id: &str, path: Option<DestinationPath>) -> super::v7::DestinationPath {
    match path {
        Some(DestinationPath::Intermediates(p)) => {
            let hop_count = p.len().min(MAX_HOPS as usize) as u8;
            tracing::warn!(
                id,
                hop_count,
                "intermediates routing is deprecated; treating as hop count"
            );
            super::v7::DestinationPath::Hops(hop_count)
        }
        Some(DestinationPath::Hops(h)) => super::v7::DestinationPath::Hops(h),
        None => super::v7::DestinationPath::Hops(1),
    }
}

pub fn wrong_keys(table: &toml::Table) -> Vec<String> {
    let mut wrong_keys = Vec::new();
    for (key, value) in table.iter() {
        if key == "version" {
            continue;
        }
        if key == "wireguard" {
            if let Some(wg) = value.as_table() {
                for (k, _v) in wg.iter() {
                    if k == "listen_port" || k == "allowed_ips" || k == "force_private_key" {
                        continue;
                    }
                    if k == "dns" {
                        if let Some(dns) = _v.as_table() {
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
        if key == "connection" {
            if let Some(connection) = value.as_table() {
                for (k, v) in connection.iter() {
                    if k == "http_timeout" || k == "announced_peer_minimum_score" || k == "lan_lockdown" {
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
                            for (k2, _v) in ping.iter() {
                                if k2 == "address" || k2 == "timeout" || k2 == "ttl" || k2 == "seq_count" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.ping.{k2}"));
                            }
                        }
                        continue;
                    }
                    if k == "buffer" {
                        if let Some(buffer) = v.as_table() {
                            for (k2, _v) in buffer.iter() {
                                if k2 == "bridge" || k2 == "ping" || k2 == "main" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.buffer.{k2}"));
                            }
                        }
                        continue;
                    }
                    if k == "max_surb_upstream" {
                        if let Some(surbs) = v.as_table() {
                            for (k2, _v) in surbs.iter() {
                                if k2 == "bridge" || k2 == "ping" || k2 == "main" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.max_surb_upstream.{k2}"));
                            }
                        }
                        continue;
                    }
                    if k == "health_check_intervals" {
                        if let Some(hci) = v.as_table() {
                            for (k2, _v) in hci.iter() {
                                if k2 == "ping"
                                    || k2 == "health_every_n_pings"
                                    || k2 == "version_every_n_pings"
                                    || k2 == "tunnel_ping"
                                    || k2 == "tunnel_ping_max_failures"
                                {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.health_check_intervals.{k2}"));
                            }
                        }
                        continue;
                    }
                    wrong_keys.push(format!("connection.{k}"));
                }
            }
            continue;
        }
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

impl TryFrom<Config> for super::v7::Config {
    type Error = config::Error;

    fn try_from(value: Config) -> Result<Self, Self::Error> {
        let destinations = value.destinations.map(convert_destinations);
        Ok(super::v7::Config {
            version: value.version,
            destinations,
            connection: Some(value.connection.into()),
            wireguard: value.wireguard,
            blokli: value.blokli,
            strategy: None,
        })
    }
}

fn convert_destinations(value: HashMap<String, Destination>) -> HashMap<String, super::v7::Destination> {
    value
        .into_iter()
        .map(|(id, dest)| {
            let path = Some(resolve_path(&id, dest.path));
            (
                id,
                super::v7::Destination {
                    address: dest.address,
                    meta: dest.meta,
                    path,
                    gnosis_vpn_server: None,
                    wireguard_server: None,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::Config;
    use edgli::hopr_lib::HopRouting;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("valid TOML")
    }

    fn forward_convert(cfg: Config) -> crate::config::Config {
        let v7_cfg: super::super::v7::Config = cfg.try_into().expect("should forward-convert");
        v7_cfg.try_into().expect("should succeed")
    }

    #[test]
    fn hops_path_preserved() {
        let cfg = parse(
            r#####"
version = 5

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { hops = 2 }
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.get("Germany").unwrap();
        assert_eq!(d.routing, HopRouting::try_from(2).unwrap());
    }

    #[test]
    fn none_path_defaults_to_1_hop() {
        let cfg = parse(
            r#####"
version = 5

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.get("Germany").unwrap();
        assert_eq!(d.routing, HopRouting::try_from(1).unwrap());
    }

    #[test]
    fn intermediates_treated_as_hop_count() {
        let cfg = parse(
            r#####"
version = 5

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA", "0x25865191AdDe377fd85E91566241178070F4797A"] }
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.get("Germany").unwrap();
        assert_eq!(d.routing, HopRouting::try_from(2).unwrap());
    }

    #[test]
    fn intermediates_clamped_to_max_hops() {
        let cfg = parse(
            r#####"
version = 5

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA", "0x25865191AdDe377fd85E91566241178070F4797A", "0x8a6E6200C9dE8d8F8D9b4c08F86500a2E3Fbf254", "0xa5Ca174Ef94403d6162a969341a61baeA48F57F8"] }
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.get("Germany").unwrap();
        assert_eq!(d.routing, HopRouting::try_from(3).unwrap());
    }

    #[test]
    fn no_destinations_no_longer_errors() {
        let cfg = parse("version = 5\n");
        let result = forward_convert(cfg);
        assert!(result.destinations.is_empty());
    }

    #[test]
    fn test_minimal_config() -> anyhow::Result<()> {
        let config = r#####"
version = 5
"#####;
        toml::from_str::<Config>(config)?;
        Ok(())
    }

    #[test]
    fn full_config_should_be_parsable() -> anyhow::Result<()> {
        let config = r#####"
version = 5

[destinations]

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
meta = { location = "Germany" }
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA"] }

[connection]
http_timeout = "60s"

[connection.bridge]
capabilities = [ "segmentation", "retransmission", "retransmission_ack_only", "no_rate_control" ]
target = "127.0.0.1:8000"

[connection.wg]
capabilities = [ "segmentation", "no_delay" ]
target = "127.0.0.1:51820"

[connection.ping]
address = "10.128.0.1"
timeout = "7s"
ttl = 6
seq_count = 1

[connection.buffer]
bridge = "32 kB"
ping = "1 MB"
main = "10 MB"

[connection.max_surb_upstream]
bridge = "512 Kb/s"
ping = "512 Kb/s"
main = "16 Mb/s"

[wireguard]
listen_port = 51820
allowed_ips = "10.128.0.1/9"
force_private_key = "QLWiv7VCpJl8DNc09NGp9QRpLjrdZ7vd990qub98V3Q="
dns = { overwrite = true, servers = "1.1.1.1,8.8.8.8" }

[blokli]
connection_sync_timeout = "30s"
sync_tolerance = 90
"#####;
        toml::from_str::<Config>(config)?;

        Ok(())
    }
}
