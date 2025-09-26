use serde::{Deserialize, Serialize};

use std::cmp::PartialEq;
use std::collections::HashMap;
use std::vec::Vec;

use crate::address::Address;
use crate::config::v3;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    hoprd_node: v3::HoprdNode,
    destinations: Option<HashMap<Address, v3::Destination>>,
    connection: Option<v3::Connection>,
    wireguard: Option<WireGuard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WireGuard {
    listen_port: Option<u16>,
    allowed_ips: Option<String>,
    manual_mode: Option<WgManualMode>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WgManualMode {
    public_key: String,
}

pub fn wrong_keys(table: &toml::Table) -> Vec<String> {
    let mut wrong_keys = Vec::new();
    for (key, value) in table.iter() {
        // version plain key
        if key == "version" {
            continue;
        }
        // hoprnode simple struct
        if key == "hoprd_node" {
            if let Some(hopr_node) = value.as_table() {
                for (k, _v) in hopr_node.iter() {
                    if k == "endpoint" || k == "api_token" || k == "internal_connection_port" {
                        continue;
                    }
                    wrong_keys.push(format!("hoprd_node.{k}"));
                }
            }
            continue;
        }
        // wireguard nested struct
        if key == "wireguard" {
            if let Some(wg) = value.as_table() {
                for (k, v) in wg.iter() {
                    if k == "listen_port" || k == "allowed_ips" {
                        continue;
                    }
                    if k == "manual_mode" {
                        if let Some(manual_mode) = v.as_table() {
                            for (k, _v) in manual_mode.iter() {
                                if k == "public_key" {
                                    continue;
                                }
                                wrong_keys.push(format!("wireguard.manual_mode.{k}"));
                            }
                        }
                        continue;
                    };
                    wrong_keys.push(format!("wireguard.{k}"));
                }
            }
            continue;
        }
        // connection nested struct
        if key == "connection" {
            if let Some(connection) = value.as_table() {
                for (k, v) in connection.iter() {
                    if k == "listen_host" {
                        continue;
                    }
                    if k == "session_timeout" {
                        continue;
                    }
                    if k == "bridge" || k == "wg" {
                        if let Some(prot) = v.as_table() {
                            for (k, _v) in prot.iter() {
                                if k == "capabilities" || k == "target" || k == "target_type" {
                                    continue;
                                }
                                wrong_keys.push(format!("connection.bridge.{k}"));
                            }
                        }
                        continue;
                    }
                    if k == "ping" {
                        if let Some(ping) = v.as_table() {
                            for (k, v) in ping.iter() {
                                if k == "address" || k == "timeout" || k == "ttl" || k == "seq_count" {
                                    continue;
                                }
                                if k == "interval" {
                                    if let Some(interval) = v.as_table() {
                                        for (k, _v) in interval.iter() {
                                            if k == "min" || k == "max" {
                                                continue;
                                            }
                                            wrong_keys.push(format!("connection.ping.interval.{k}"));
                                        }
                                    }
                                    continue;
                                }
                                wrong_keys.push(format!("connection.ping.{k}"));
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

impl From<&Config> for v3::Config {
    fn from(config: &Config) -> Self {
        v3::Config {
            version: config.version,
            hoprd_node: config.hoprd_node.clone(),
            destinations: config.destinations.clone(),
            connection: config.connection.clone(),
            wireguard: config.wireguard.as_ref().map(|wg| wg.into()),
        }
    }
}

impl From<&WireGuard> for v3::WireGuard {
    fn from(wg: &WireGuard) -> Self {
        v3::WireGuard {
            listen_port: wg.listen_port,
            allowed_ips: wg.allowed_ips.clone(),
            force_private_key: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn test_minimal_config() {
        let config = r#####"
version = 2
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse minimal config");
    }

    #[test]
    fn test_ping_without_interval() {
        let config = r#####"
version = 2
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"

[connection.ping]
address = "10.128.0.1"
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse minimal ping");
    }

    #[test]
    fn test_full_config() {
        let config = r#####"
version = 2
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"
internal_connection_port = 1422

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
listen_host = "0.0.0.0:1422"

[connection.bridge]
capabilities = [ "segmentation", "retransmission" ]
target = "127.0.0.1:8000"
target_type = "plain"

[connection.wg]
capabilities = [ "segmentation" ]
target = "127.0.0.1:51820"
target_type = "sealed"

[connection.ping]
address = "10.128.0.1"
timeout = "4s"
ttl = 5
seq_count = 1
[connection.ping.interval]
min = 5
max = 10

[wireguard]
listen_port = 51820
allowed_ips = "10.128.0.1/9"
# only specify this if you want to manually connect via WireGuard
manual_mode = { public_key = "VbezNcrZstuGTkXc7uNwHHB1BA8fLgL8IAQO/pWTpSw=" }
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse full config");
    }
}
