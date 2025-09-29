use edgli::hopr_lib::Address;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};
use url::Url;

use std::cmp::PartialEq;
use std::collections::HashMap;
use std::vec::Vec;

use crate::config;
use crate::config::v4;

#[serde_as]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub version: u8,
    pub(super) hoprd_node: HoprdNode,
    #[serde_as(as = "Option<HashMap<DisplayFromStr, _>>")]
    pub(super) destinations: Option<HashMap<Address, v4::Destination>>,
    pub(super) connection: Option<v4::Connection>,
    pub(super) wireguard: Option<v4::WireGuard>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct HoprdNode {
    endpoint: Url,
    api_token: String,
    internal_connection_port: Option<u16>,
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
                    if k == "listen_host" {
                        continue;
                    }
                    if k == "http_timeout" {
                        continue;
                    }
                    if k == "session_timeout" {
                        continue;
                    }
                    if k == "ping_retries_timeout" {
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
        let destinations = v4::convert_destinations(value.destinations)?;
        let wireguard = value.wireguard.into();
        Ok(config::Config {
            connection,
            destinations,
            wireguard,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn test_minimal_config() {
        let config = r#####"
version = 3
[hoprd_node]
endpoint = "http://127.0.0.1:3001"
api_token = "1234567890"
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse minimal config");
    }

    #[test]
    fn test_ping_without_interval() {
        let config = r#####"
version = 3
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
version = 3
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
http_timeout = "5s"
session_timeout = "15s"
ping_retries_timeout = "20s"

[connection.bridge]
capabilities = [ "segmentation", "retransmission" ]
target = "127.0.0.1:8000"
target_type = "plain"

[connection.wg]
capabilities = [ "segmentation", "no_delay" ]
target = "127.0.0.1:51820"
target_type = "sealed"

[connection.ping]
address = "10.128.0.1"
timeout = "7s"
ttl = 6
seq_count = 1
[connection.ping.interval]
min = 5
max = 10

[connection.max_surb_upstream]
bridge = "0 Mb/s"
ping = "1 Mb/s"
main = "16 Mb/s"

[connection.buffer]
bridge = "0 B"
ping = "32 kB"
main = "2 MB"

[wireguard]
listen_port = 51820
allowed_ips = "10.128.0.1/9"
# use if you want to disable key rotation on every connection
force_private_key = "QLWiv7VCpJl8DNc09NGp9QRpLjrdZ7vd990qub98V3Q="
"#####;
        toml::from_str::<Config>(config).expect("Failed to parse full config");
    }
}
