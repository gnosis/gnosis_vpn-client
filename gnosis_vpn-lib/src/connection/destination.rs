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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub meta: HashMap<String, String>,
    #[serde(with = "serde_utils::address")]
    pub address: Address,
    pub routing: HopRouting,
    /// Overrides the global `[connection.bridge].target` default when present.
    pub gnosis_vpn_server: Option<SocketAddr>,
    /// Overrides the global `[connection.wg].target` default when present.
    pub wireguard_server: Option<SocketAddr>,
    pub source: DestinationSource,
}

impl Destination {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        address: Address,
        routing: HopRouting,
        meta: HashMap<String, String>,
        gnosis_vpn_server: Option<SocketAddr>,
        wireguard_server: Option<SocketAddr>,
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

    /// The bridge-session target: this destination's own `gnosis_vpn_server` if set, else
    /// `fallback` (the global `[connection.bridge].target`).
    pub fn bridge_target(&self, fallback: &SessionTarget) -> SessionTarget {
        self.gnosis_vpn_server
            .map(|addr| SessionTarget::TcpStream(SealedHost::Plain(IpOrHost::Ip(addr))))
            .unwrap_or_else(|| fallback.clone())
    }

    /// The WireGuard-session target: this destination's own `wireguard_server` if set, else
    /// `fallback` (the global `[connection.wg].target`).
    pub fn wg_target(&self, fallback: &SessionTarget) -> SessionTarget {
        self.wireguard_server
            .map(|addr| SessionTarget::UdpStream(SealedHost::Plain(IpOrHost::Ip(addr))))
            .unwrap_or_else(|| fallback.clone())
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
            "{id} (Exit: {address}, Route: (entry){path}({short_addr}), {meta})",
            id = self.id,
            meta = self.meta_str(),
            path = self.pretty_print_path(),
            address = self.address.to_checksum(),
            short_addr = short_addr,
        )
    }
}
