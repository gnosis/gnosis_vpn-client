/// Config v6: identical to v7 except `[destinations]` was still required (non-empty) and a
/// destination could not carry `gnosis_vpn_server`/`wireguard_server`. Forward-converts into
/// `v7::Config`; the shared schema (connection, wireguard, blokli, strategy) is defined once in
/// `v7` and reused here unchanged.
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

/// Same key set v6 has always accepted — a v6 file never had `gnosis_vpn_server`/
/// `wireguard_server`, so those still surface as unsupported keys here; upgrade to
/// `version = 7` to use them.
pub fn wrong_keys(table: &toml::Table) -> Vec<String> {
    let mut wrong = Vec::new();
    for (key, value) in table.iter() {
        if key == "version" {
            continue;
        }
        if key == "wireguard" {
            if let Some(wg) = value.as_table() {
                for (k, v) in wg.iter() {
                    if k == "allowed_ips" || k == "force_private_key" {
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
                    if k == "connection_sync_timeout" || k == "sync_tolerance" || k == "request_timeout" {
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
                        || k == "probe_local_addresses"
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
                        "min_open_channels" | "target_open_channels" | "channel_capacity"
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

        Ok(super::v7::Config {
            version: value.version,
            destinations,
            connection: value.connection,
            wireguard: value.wireguard,
            blokli: value.blokli,
            strategy: value.strategy,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("valid TOML")
    }

    /// End-to-end regression: a v6 file forward-converts through v7 into the runtime config,
    /// with its destination resolved to a plain `Configured` entry carrying no target override.
    #[test]
    fn v6_file_forward_converts_into_the_runtime_config() {
        let cfg = parse(
            r#####"
version = 6

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
path = { hops = 2 }
"#####,
        );
        let v7_cfg: super::super::v7::Config = cfg.try_into().expect("should forward-convert");
        let result: crate::config::Config = v7_cfg.try_into().expect("should succeed");

        let dest = result.destinations.get("Germany").expect("destination present");
        assert_eq!(dest.routing, edgli::hopr_lib::HopRouting::try_from(2).unwrap());
        assert_eq!(dest.gnosis_vpn_server, None);
        assert_eq!(dest.wireguard_server, None);
    }

    #[test]
    fn v6_file_without_destinations_still_loads() {
        // v6 historically required at least one destination; now that the requirement lives
        // only in v7's `convert_destinations`, forward-converting relaxes it here too — a v6
        // file with none is no longer an error.
        let cfg = parse("version = 6\n");
        let v7_cfg: super::super::v7::Config = cfg.try_into().expect("should forward-convert");
        let result: crate::config::Config = v7_cfg.try_into().expect("should succeed");
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
        assert_eq!(super::wrong_keys(&table), vec!["wireguard.listen_port".to_string()]);
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
            super::wrong_keys(&table),
            vec!["destinations.Germany.gnosis_vpn_server".to_string()]
        );
    }
}
