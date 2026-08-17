/// Config v4: like v5 except destinations are keyed by address directly (no separate `id`) and
/// carry no `address` field of their own. Forward-converts into `v7::Config`.
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::cmp::PartialEq;
use std::collections::HashMap;
use std::vec::Vec;

use crate::config;
use crate::config::v5;

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    #[serde_as(as = "Option<HashMap<DisplayFromStr, _>>")]
    pub(super) destinations: Option<HashMap<Address, Destination>>,
    pub(super) connection: Option<v5::Connection>,
    pub(super) wireguard: Option<v5::WireGuard>,
    pub(super) blokli: Option<v5::BlokliConfig>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Destination {
    pub(super) meta: Option<HashMap<String, String>>,
    pub(super) path: Option<v5::DestinationPath>,
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

pub(super) fn convert_destinations(value: HashMap<Address, Destination>) -> HashMap<String, super::v7::Destination> {
    value
        .into_iter()
        .map(|(address, dest)| {
            let id = address.to_string();
            let path = Some(v5::resolve_path(&id, dest.path));
            (
                id,
                super::v7::Destination {
                    address,
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
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
path = { hops = 2 }
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.values().next().unwrap();
        assert_eq!(d.routing, HopRouting::try_from(2).unwrap());
    }

    #[test]
    fn intermediates_treated_as_hop_count() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA", "0x25865191AdDe377fd85E91566241178070F4797A"] }
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.values().next().unwrap();
        assert_eq!(d.routing, HopRouting::try_from(2).unwrap());
    }

    #[test]
    fn intermediates_clamped_to_max_hops() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA", "0x25865191AdDe377fd85E91566241178070F4797A", "0x8a6E6200C9dE8d8F8D9b4c08F86500a2E3Fbf254", "0xa5Ca174Ef94403d6162a969341a61baeA48F57F8"] }
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.values().next().unwrap();
        assert_eq!(d.routing, HopRouting::try_from(3).unwrap());
    }

    #[test]
    fn none_path_defaults_to_1_hop() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
"#####,
        );
        let result = forward_convert(cfg);
        let d = result.destinations.values().next().unwrap();
        assert_eq!(d.routing, HopRouting::try_from(1).unwrap());
    }

    #[test]
    fn no_destinations_no_longer_errors() {
        let cfg = parse("version = 4\n");
        let result = forward_convert(cfg);
        assert!(result.destinations.is_empty());
    }

    #[test]
    fn test_minimal_config() -> anyhow::Result<()> {
        let config = r#####"
version = 4
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

[connection]
http_timeout = "60s"

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
"#####;
        toml::from_str::<Config>(config)?;

        Ok(())
    }
}
