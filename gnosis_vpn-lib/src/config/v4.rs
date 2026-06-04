use edgli::hopr_lib::HopRouting;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::cmp::PartialEq;
use std::collections::HashMap;
use std::vec::Vec;

use crate::config;
use crate::config::v5;
use crate::connection::destination::{Destination as ConnDestination, RoutingMode};

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
    meta: Option<HashMap<String, String>>,
    path: Option<v5::DestinationPath>,
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
    value: Option<HashMap<Address, Destination>>,
) -> Result<HashMap<String, ConnDestination>, config::Error> {
    let config_dests = value.ok_or(config::Error::NoDestinations)?;
    if config_dests.is_empty() {
        return Err(config::Error::NoDestinations);
    }

    let mut result = HashMap::new();
    for (address, dest) in config_dests.iter() {
        let routing = match dest.path.clone() {
            Some(v5::DestinationPath::Intermediates(addrs)) if addrs.is_empty() => {
                return Err(config::Error::EmptyExplicitPath(address.to_string()));
            }
            Some(v5::DestinationPath::Intermediates(addrs)) if addrs.len() > config::MAX_INTERMEDIATES => {
                return Err(config::Error::TooManyIntermediates(address.to_string()));
            }
            Some(v5::DestinationPath::Intermediates(addrs)) => RoutingMode::ExplicitPath(addrs),
            Some(v5::DestinationPath::Hops(h)) => RoutingMode::HopBased(HopRouting::try_from(h as usize)?),
            None => RoutingMode::HopBased(HopRouting::try_from(1)?),
        };

        let meta = dest.meta.clone().unwrap_or_default();

        let dest = ConnDestination::new(address.to_string(), *address, routing, meta);
        result.insert(address.to_string(), dest);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{Config, convert_destinations};
    use crate::connection::destination::RoutingMode;
    use edgli::hopr_lib::HopRouting;

    fn parse(toml: &str) -> Config {
        toml::from_str(toml).expect("valid TOML")
    }

    #[test]
    fn convert_destinations_hops_path_preserved() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
path = { hops = 2 }
"#####,
        );
        let result = convert_destinations(cfg.destinations).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(d.routing, RoutingMode::HopBased(HopRouting::try_from(2).unwrap()));
    }

    #[test]
    fn convert_destinations_intermediates_preserved_as_explicit_path() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
path = { intermediates = ["0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA", "0x25865191AdDe377fd85E91566241178070F4797A"] }
"#####,
        );
        let result = convert_destinations(cfg.destinations).expect("should succeed");
        let d = result.values().next().unwrap();
        let RoutingMode::ExplicitPath(addrs) = &d.routing else {
            panic!("expected ExplicitPath, got {:?}", d.routing);
        };
        assert_eq!(addrs.len(), 2);
    }

    #[test]
    fn convert_destinations_none_path_defaults_to_1_hop() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
"#####,
        );
        let result = convert_destinations(cfg.destinations).expect("should succeed");
        let d = result.values().next().unwrap();
        assert_eq!(d.routing, RoutingMode::HopBased(HopRouting::try_from(1).unwrap()));
    }

    #[test]
    fn convert_destinations_empty_intermediates_errors() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
path = { intermediates = [] }
"#####,
        );
        let result = convert_destinations(cfg.destinations);
        assert!(
            matches!(result, Err(crate::config::Error::EmptyExplicitPath(_))),
            "empty intermediates list must be a config error, got: {result:?}"
        );
    }

    #[test]
    fn convert_destinations_too_many_intermediates_errors() {
        let cfg = parse(
            r#####"
version = 4

[destinations.0xD9c11f07BfBC1914877d7395459223aFF9Dc2739]
path = { intermediates = [
    "0xD88064F7023D5dA2Efa35eAD1602d5F5d86BB6BA",
    "0x25865191AdDe377fd85E91566241178070F4797A",
    "0x2Cf9E5951C9e60e01b579f654dF447087468fc04",
    "0x8a6E6200C9dE8d8F8D9b4c08F86500a2E3Fbf254"
] }
"#####,
        );
        let result = convert_destinations(cfg.destinations);
        assert!(
            matches!(result, Err(crate::config::Error::TooManyIntermediates(_))),
            "4 intermediates must be a config error, got: {result:?}"
        );
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
version = 4
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
