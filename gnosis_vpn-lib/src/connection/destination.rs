use edgli::ExitNodeInfo;
pub use edgli::hopr_lib::HopRouting;
pub use edgli::hopr_lib::api::types::primitive::prelude::Address;
use edgli::hopr_lib::exports::network::types::types::{IpOrHost, SealedHost};
use edgli::hopr_lib::exports::transport::SessionTarget;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt::{self, Display};
use std::net::SocketAddr;

use crate::log_output;
use crate::serde_utils;

/// Where a [`Destination`] came from.
///
/// A configured destination whose address discovery also independently reports is tagged
/// `ConfiguredAndDiscovered` rather than looking identical to a plain `Configured` one — the
/// configured target values still govern in that case, but the confirmation is worth surfacing
/// (e.g. in `gvpn-ctl status`) rather than being silently indistinguishable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DestinationSource {
    Configured,
    Discovered,
    ConfiguredAndDiscovered,
}

impl Display for DestinationSource {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DestinationSource::Configured => write!(f, "Configured"),
            DestinationSource::Discovered => write!(f, "Discovered"),
            DestinationSource::ConfiguredAndDiscovered => write!(f, "Configured+Discovered"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub meta: HashMap<String, String>,
    #[serde(with = "serde_utils::address")]
    pub address: Address,
    pub routing: HopRouting,
    /// The bridge-session target: this destination's own value if configured, else the global
    /// `[connection.bridge].target` default, resolved once at config-load time.
    pub gnosis_vpn_server: SocketAddr,
    /// The WireGuard-session target: this destination's own value if configured, else the
    /// global `[connection.wg].target` default, resolved once at config-load time.
    pub wireguard_server: SocketAddr,
    pub source: DestinationSource,
}

impl Destination {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        address: Address,
        routing: HopRouting,
        meta: HashMap<String, String>,
        gnosis_vpn_server: SocketAddr,
        wireguard_server: SocketAddr,
        source: DestinationSource,
    ) -> Self {
        Self {
            id,
            address,
            routing,
            meta,
            gnosis_vpn_server,
            wireguard_server,
            source,
        }
    }

    /// The bridge-session target: this destination's own `gnosis_vpn_server`.
    pub fn bridge_target(&self) -> SessionTarget {
        SessionTarget::TcpStream(SealedHost::Plain(IpOrHost::Ip(self.gnosis_vpn_server)))
    }

    /// The WireGuard-session target: this destination's own `wireguard_server`.
    pub fn wg_target(&self) -> SessionTarget {
        SessionTarget::UdpStream(SealedHost::Plain(IpOrHost::Ip(self.wireguard_server)))
    }

    pub fn pretty_print_path(&self) -> String {
        let nr = self.routing.hop_count();
        let path = (0..nr).map(|_| "()").collect::<Vec<&str>>().join("->");
        if nr > 0 {
            format!("->{path}->")
        } else {
            "->".to_string()
        }
    }

    fn meta_str(&self) -> String {
        let mut metas = self
            .meta
            .iter()
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<String>>();
        metas.sort_unstable();
        metas.join(", ")
    }

    pub fn get_meta(&self, key: &str) -> Option<String> {
        self.meta.get(key).cloned()
    }
}

impl Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let short_addr = log_output::address(&self.address);
        write!(
            f,
            "{id} (Exit: {address}, Route: (entry){path}({short_addr}), {meta}, Source: {source})",
            id = self.id,
            meta = self.meta_str(),
            path = self.pretty_print_path(),
            address = self.address.to_checksum(),
            short_addr = short_addr,
            source = self.source,
        )
    }
}

/// Merges freshly discovered `gvpn:exit` nodes into `destinations`.
///
/// A discovered address that matches an existing configured destination only flips that
/// destination's `source` to [`DestinationSource::ConfiguredAndDiscovered`] — the configured
/// target/meta values keep governing, since discovery there is just a confirmation, not an
/// override. A discovered address with no configured match is inserted fresh, keyed by its own
/// checksummed address (no human-chosen id exists for it). A previously discovered destination
/// whose registration disappeared is removed outright; a previously confirmed
/// (`ConfiguredAndDiscovered`) one is downgraded back to `Configured` rather than removed, since
/// it is still valid, statically configured data.
pub fn merge_discovered(destinations: &mut HashMap<String, Destination>, discovered: &HashMap<Address, ExitNodeInfo>) {
    destinations.retain(|_, dest| match dest.source {
        DestinationSource::Discovered if !discovered.contains_key(&dest.address) => false,
        DestinationSource::ConfiguredAndDiscovered if !discovered.contains_key(&dest.address) => {
            dest.source = DestinationSource::Configured;
            true
        }
        _ => true,
    });

    for (address, info) in discovered {
        if let Some(dest) = destinations.values_mut().find(|d| d.address == *address) {
            if dest.source == DestinationSource::Configured {
                dest.source = DestinationSource::ConfiguredAndDiscovered;
            }
            continue;
        }
        let id = address.to_checksum();
        destinations.insert(
            id.clone(),
            Destination::new(
                id,
                *address,
                HopRouting::try_from(1).expect("1 is always a valid hop count"),
                info.meta.clone(),
                info.gnosis_vpn_server,
                info.wireguard_server,
                DestinationSource::Discovered,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn address(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn configured(id: &str, addr: Address) -> Destination {
        Destination::new(
            id.to_string(),
            addr,
            HopRouting::try_from(1).unwrap(),
            HashMap::new(),
            "172.30.0.1:8000".parse().unwrap(),
            "172.30.0.1:51820".parse().unwrap(),
            DestinationSource::Configured,
        )
    }

    fn exit_node(addr: Address) -> ExitNodeInfo {
        ExitNodeInfo {
            node: addr,
            safe: address(99),
            gnosis_vpn_server: "10.0.0.1:9000".parse().unwrap(),
            wireguard_server: "10.0.0.1:9001".parse().unwrap(),
            meta: HashMap::new(),
            registered_at: SystemTime::now(),
            updated_at: SystemTime::now(),
        }
    }

    #[test]
    fn configured_and_discovered_address_becomes_confirmed_and_keeps_configured_values() {
        let addr = address(1);
        let mut destinations = HashMap::new();
        destinations.insert("dest-1".to_string(), configured("dest-1", addr));

        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        merge_discovered(&mut destinations, &discovered);

        assert_eq!(destinations.len(), 1);
        let dest = &destinations["dest-1"];
        assert_eq!(dest.source, DestinationSource::ConfiguredAndDiscovered);
        assert_eq!(dest.gnosis_vpn_server, "172.30.0.1:8000".parse::<SocketAddr>().unwrap());
        assert_eq!(dest.wireguard_server, "172.30.0.1:51820".parse::<SocketAddr>().unwrap());
    }

    #[test]
    fn discovery_only_address_is_inserted_fresh() {
        let addr = address(2);
        let mut destinations = HashMap::new();
        let mut discovered = HashMap::new();
        let info = exit_node(addr);
        discovered.insert(addr, info.clone());

        merge_discovered(&mut destinations, &discovered);

        assert_eq!(destinations.len(), 1);
        let dest = destinations.values().next().unwrap();
        assert_eq!(dest.id, addr.to_checksum());
        assert_eq!(dest.source, DestinationSource::Discovered);
        assert_eq!(dest.routing, HopRouting::try_from(1).unwrap());
        assert_eq!(dest.gnosis_vpn_server, info.gnosis_vpn_server);
        assert_eq!(dest.wireguard_server, info.wireguard_server);
    }

    #[test]
    fn discovered_only_entry_is_removed_once_deregistered() {
        let addr = address(3);
        let mut destinations = HashMap::new();
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));
        merge_discovered(&mut destinations, &discovered);
        assert_eq!(destinations.len(), 1);

        merge_discovered(&mut destinations, &HashMap::new());
        assert!(destinations.is_empty());
    }

    #[test]
    fn confirmed_entry_downgrades_to_configured_once_deregistered() {
        let addr = address(4);
        let mut destinations = HashMap::new();
        destinations.insert("dest-4".to_string(), configured("dest-4", addr));
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));
        merge_discovered(&mut destinations, &discovered);
        assert_eq!(
            destinations["dest-4"].source,
            DestinationSource::ConfiguredAndDiscovered
        );

        merge_discovered(&mut destinations, &HashMap::new());
        assert_eq!(destinations.len(), 1);
        assert_eq!(destinations["dest-4"].source, DestinationSource::Configured);
    }

    #[test]
    fn repeated_merge_with_unchanged_discovered_map_is_idempotent() {
        let addr = address(5);
        let mut destinations = HashMap::new();
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        merge_discovered(&mut destinations, &discovered);
        merge_discovered(&mut destinations, &discovered);
        merge_discovered(&mut destinations, &discovered);

        assert_eq!(destinations.len(), 1);
        assert_eq!(
            destinations.values().next().unwrap().source,
            DestinationSource::Discovered
        );
    }
}
