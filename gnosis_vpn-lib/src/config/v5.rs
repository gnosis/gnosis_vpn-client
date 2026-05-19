use edgli::hopr_lib::HopRouting;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::collections::HashMap;
use std::vec::Vec;

use crate::config;
use crate::connection::destination::Destination as ConnDestination;

// Shared structs and helpers live in v6 (the canonical, current-version module).
// v5 re-uses them for the connection/wireguard/blokli sections, which are
// schema-identical across both versions.
pub(super) use super::v6::{BlokliConfig, Connection, WireGuard};
use super::v6::{MAX_HOPS, validate_hops};

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
    address: Address,
    meta: Option<HashMap<String, String>>,
    path: Option<DestinationPath>,
}

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum DestinationPath {
    #[serde(alias = "intermediates")]
    Intermediates(#[serde_as(as = "Vec<DisplayFromStr>")] Vec<Address>),
    #[serde(alias = "hops", deserialize_with = "validate_hops")]
    Hops(u8),
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

impl TryFrom<Config> for config::Config {
    type Error = config::Error;

    fn try_from(value: Config) -> Result<Self, Self::Error> {
        let connection = value.connection.into();
        let destinations = convert_destinations(value.destinations)?;
        let wireguard = value.wireguard.into();
        let blokli = value.blokli.into();
        Ok(config::Config {
            connection,
            destinations,
            wireguard,
            blokli,
            strategy: Default::default(),
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
        let path = match dest.path.clone() {
            Some(DestinationPath::Intermediates(p)) => {
                let hop_count = p.len().min(MAX_HOPS as usize);
                tracing::warn!(
                    id,
                    hop_count,
                    "intermediates routing is deprecated; treating as hop count"
                );
                HopRouting::try_from(hop_count)?
            }
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
version = 5

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
version = 5

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
    fn test_minimal_config() -> anyhow::Result<()> {
        let config = r#####"
version = 5
"#####;
        toml::from_str::<Config>(config)?;
        Ok(())
    }

    #[test]
    fn config_parse_single_destination_should_succeed() -> anyhow::Result<()> {
        let config = r#####"
version = 5

[destinations]

[destinations.Germany]
address = "0xD9c11f07BfBC1914877d7395459223aFF9Dc2739"
meta = { location = "Germany" }
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA"] }
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

[destinations.USA]
address = "0xa5Ca174Ef94403d6162a969341a61baeA48F57F8"
meta = { location = "USA" }
path = { intermediates = ["0x25865191AdDe377fd85E91566241178070F4797A"] }

[destinations.Spain]
address = "0x8a6E6200C9dE8d8F8D9b4c08F86500a2E3Fbf254"
meta = { location = "Spain" }
path = { intermediates = ["0x2Cf9E5951C9e60e01b579f654dF447087468fc04"] }

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

[connection.max_surb_upstream]
bridge = "512 Kb/s"
ping = "1 Mb/s"
main = "16 Mb/s"

[connection.buffer]
bridge = "32 kB"
ping = "32 kB"
main = "2 MB"

[wireguard]
listen_port = 51820
allowed_ips = "10.128.0.1/9"
# use if you want to disable key rotation on every connection
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
