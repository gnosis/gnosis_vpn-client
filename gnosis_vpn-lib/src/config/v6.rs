/// Config v6: identical to v5 except `Intermediates` is removed from
/// `DestinationPath`. Only hop-count based routing is supported.
///
/// Existing v4/v5 configs with `intermediates` must be migrated by replacing
/// `path = { intermediates = [...] }` with `path = { hops = <count> }`.
use edgli::hopr_lib::HopRouting;
use edgli::hopr_lib::api::types::primitive::prelude::Address;
use serde::{Deserialize, Serialize};
use serde_with::{DisplayFromStr, serde_as};

use std::collections::HashMap;

use crate::config;
use crate::connection::destination::Destination as ConnDestination;

/// Re-use all v5 connection/wireguard/blokli types — the TOML schema for
/// those sections is unchanged in v6.
pub(super) use super::v5::{BlokliConfig, Connection, WireGuard, wrong_keys};

#[serde_as]
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

/// Routing path for v6 — only hop-count routing is supported.
///
/// `Intermediates` is intentionally absent; configs that previously used
/// `path = { intermediates = [...] }` must be updated to
/// `path = { hops = <count> }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) enum DestinationPath {
    #[serde(alias = "hops")]
    Hops(u8),
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
}
