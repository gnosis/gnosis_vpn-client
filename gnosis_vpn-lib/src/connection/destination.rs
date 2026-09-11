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
/// `ConfiguredAndDiscovered` rather than looking identical to a plain `Configured` one: it is the
/// difference between an exit the on-chain registry knows about and one only your config does, so
/// `gvpn-ctl status` renders it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DestinationSource {
    Configured,
    Discovered,
    ConfiguredAndDiscovered,
}

impl DestinationSource {
    /// The origin code rendered beside the title: c is configuration, d is discovery.
    fn code(&self) -> &'static str {
        match self {
            DestinationSource::Configured => "c",
            DestinationSource::Discovered => "d",
            DestinationSource::ConfiguredAndDiscovered => "c+d",
        }
    }
}

/// Longest label value rendered before it is elided.
const META_FIELD_MAX_CHARS: usize = 64;

/// True for characters that would let metadata rewrite or spoof surrounding terminal output:
/// controls plus the zero-width and bidi-override formatting characters.
fn is_display_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// Metadata is operator-published, free-form and unverified, so treat it as untrusted terminal
/// input rather than printing it verbatim.
fn sanitize_for_display(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !is_display_unsafe(*c)).collect();
    if cleaned.chars().count() <= META_FIELD_MAX_CHARS {
        return cleaned;
    }
    let kept: String = cleaned.chars().take(META_FIELD_MAX_CHARS).collect();
    format!("{kept}…")
}

/// Operator-published labels: the keys we understand, plus everything else as published.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    /// The destination's title; for a discovered exit it is the only human-readable name there is.
    pub name: Option<String>,
    pub location: Option<String>,
    pub flag: Option<String>,
    pub description: Option<String>,
    /// Every unrecognized key, kept so nothing an operator publishes is lost.
    pub other: HashMap<String, String>,
}

impl Meta {
    /// The only place the recognized label names are known.
    pub fn from_map(mut labels: HashMap<String, String>) -> Self {
        Self {
            name: labels.remove("name"),
            location: labels.remove("location"),
            flag: labels.remove("flag"),
            description: labels.remove("description"),
            other: labels,
        }
    }
}

/// Discovery publishes no path, so every discovered exit sits at this one.
pub const DEFAULT_HOPS: usize = 1;

pub fn default_path() -> HopRouting {
    HopRouting::try_from(DEFAULT_HOPS).expect("the default hop count is always valid")
}

/// Session targets for a destination that neither configuration nor discovery names.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DefaultTargets {
    pub gnosis_vpn_server: SocketAddr,
    pub wireguard_server: SocketAddr,
}

/// What configuration pinned: the values it set itself, whatever discovery goes on to report.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overrides {
    /// The labels configuration set; every key present is a pin.
    configured_meta: HashMap<String, String>,
    configured_gnosis_vpn_server: Option<SocketAddr>,
    configured_wireguard_server: Option<SocketAddr>,
}

impl Overrides {
    pub fn from_config(
        meta: HashMap<String, String>,
        gnosis_vpn_server: Option<SocketAddr>,
        wireguard_server: Option<SocketAddr>,
    ) -> Self {
        Self {
            configured_meta: meta,
            configured_gnosis_vpn_server: gnosis_vpn_server,
            configured_wireguard_server: wireguard_server,
        }
    }

    /// Configuration's labels where it pinned them, the discovered ones elsewhere, per key.
    fn apply_meta(&self, discovered: &HashMap<String, String>) -> Meta {
        let mut merged = discovered.clone();
        merged.extend(self.configured_meta.clone());
        Meta::from_map(merged)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub meta: Meta,
    #[serde(with = "serde_utils::address")]
    pub address: Address,
    pub routing: HopRouting,
    /// Configuration's value if pinned, else the exit's published endpoint, else the global default.
    pub gnosis_vpn_server: SocketAddr,
    /// The WireGuard-session target, resolved by the same precedence as `gnosis_vpn_server`.
    pub wireguard_server: SocketAddr,
    pub source: DestinationSource,
    /// Absent from an older daemon's payload, where nothing was overridable.
    #[serde(default)]
    pub overrides: Overrides,
}

impl Destination {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        address: Address,
        routing: HopRouting,
        meta: Meta,
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
            overrides: Overrides::default(),
        }
    }

    /// Records what configuration pinned, so a discovery tick cannot overwrite it.
    pub fn with_overrides(mut self, overrides: Overrides) -> Self {
        self.overrides = overrides;
        self
    }

    /// Takes everything discovery reports that configuration did not pin.
    fn adopt(&mut self, info: &ExitNodeInfo) {
        self.meta = self.overrides.apply_meta(&info.meta);
        self.gnosis_vpn_server = self
            .overrides
            .configured_gnosis_vpn_server
            .unwrap_or(info.gnosis_vpn_server);
        self.wireguard_server = self
            .overrides
            .configured_wireguard_server
            .unwrap_or(info.wireguard_server);
    }

    /// Falls back to configuration and the global defaults once discovery stops reporting the exit.
    fn forget_discovered(&mut self, defaults: DefaultTargets) {
        self.meta = Meta::from_map(self.overrides.configured_meta.clone());
        self.gnosis_vpn_server = self
            .overrides
            .configured_gnosis_vpn_server
            .unwrap_or(defaults.gnosis_vpn_server);
        self.wireguard_server = self
            .overrides
            .configured_wireguard_server
            .unwrap_or(defaults.wireguard_server);
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

    /// Reads an unrecognized label; `name`, `location`, `flag` and `description` are typed fields.
    pub fn get_meta(&self, key: &str) -> Option<String> {
        self.meta.other.get(key).cloned()
    }

    /// The `name` when published, bracketing the key when both differ; a discovered key only
    /// repeats `Exit:`.
    fn title(&self) -> String {
        let Some(name) = self.meta.name.as_deref().map(sanitize_for_display) else {
            return self.id.clone();
        };
        let key_adds_nothing = self.source == DestinationSource::Discovered || name == self.id;
        if key_adds_nothing {
            return name;
        }
        format!("{name}({id})", id = self.id)
    }

    /// Identity: the exit and the path to it; every other field is resolved or display state.
    pub fn same_exit(&self, other: &Self) -> bool {
        self.address == other.address && self.routing == other.routing
    }
}

/// Marks a value configuration set itself, where the rest of the entry comes from discovery.
const CONFIG_VALUE: &str = " (c)";

impl Destination {
    /// The recognized labels, plus any target configuration pinned - never otherwise shown.
    fn labels_str(&self) -> String {
        // On a c-only or d-only entry the origin code already says who set every value.
        let mixed_origins = self.source == DestinationSource::ConfiguredAndDiscovered;
        let pinned = |key: &str| mixed_origins && self.overrides.configured_meta.contains_key(key);

        let mut parts = Vec::new();
        // The title already carries the name, but not that configuration chose it.
        if let Some(name) = &self.meta.name
            && pinned("name")
        {
            parts.push(format!("name: {}{CONFIG_VALUE}", sanitize_for_display(name)));
        }
        for (key, value) in [
            ("location", &self.meta.location),
            ("flag", &self.meta.flag),
            ("description", &self.meta.description),
        ] {
            let Some(value) = value else { continue };
            let mark = if pinned(key) { CONFIG_VALUE } else { "" };
            parts.push(format!("{key}: {}{mark}", sanitize_for_display(value)));
        }
        // Targets are parsed socket addresses rather than operator text, so they need no sanitizing.
        for (key, value, configured) in [
            (
                "gnosis_vpn_server",
                self.gnosis_vpn_server,
                self.overrides.configured_gnosis_vpn_server,
            ),
            (
                "wireguard_server",
                self.wireguard_server,
                self.overrides.configured_wireguard_server,
            ),
        ] {
            if mixed_origins && configured.is_some() {
                parts.push(format!("{key}: {value}{CONFIG_VALUE}"));
            }
        }
        parts.join(", ")
    }
}

impl Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let short_addr = log_output::address(&self.address);
        let labels = self.labels_str();
        let labels = if labels.is_empty() {
            String::new()
        } else {
            format!(", {labels}")
        };
        write!(
            f,
            "{id} [{source}] (Exit: {address}, Route: (entry){path}({short_addr}){labels})",
            id = self.title(),
            source = self.source.code(),
            path = self.pretty_print_path(),
            address = self.address.to_checksum(),
            short_addr = short_addr,
        )
    }
}

/// Merges freshly discovered `gvpn:exit` nodes into `destinations`.
///
/// Joined on `(address, path)`, so an entry at another path is a separate destination, left alone.
pub fn merge_discovered(
    destinations: &mut HashMap<String, Destination>,
    discovered: &HashMap<Address, ExitNodeInfo>,
    defaults: DefaultTargets,
) {
    let default_path = default_path();
    let is_discovered = |dest: &Destination| dest.routing == default_path && discovered.contains_key(&dest.address);

    destinations.retain(|_, dest| match dest.source {
        DestinationSource::Discovered if !is_discovered(dest) => false,
        DestinationSource::ConfiguredAndDiscovered if !is_discovered(dest) => {
            dest.source = DestinationSource::Configured;
            dest.forget_discovered(defaults);
            true
        }
        _ => true,
    });

    for (address, info) in discovered {
        if let Some(dest) = destinations
            .values_mut()
            .find(|d| d.address == *address && d.routing == default_path)
        {
            if dest.source == DestinationSource::Configured {
                dest.source = DestinationSource::ConfiguredAndDiscovered;
            }
            dest.adopt(info);
            continue;
        }
        let id = address.to_checksum();
        if destinations.contains_key(&id) {
            tracing::warn!(%id, "configured destination id collides with a discovered exit address - skipping");
            continue;
        }
        let mut dest = Destination::new(
            id.clone(),
            *address,
            default_path,
            Meta::default(),
            defaults.gnosis_vpn_server,
            defaults.wireguard_server,
            DestinationSource::Discovered,
        );
        dest.adopt(info);
        destinations.insert(id, dest);
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
            Meta::default(),
            "172.30.0.1:8000".parse().unwrap(),
            "172.30.0.1:51820".parse().unwrap(),
            DestinationSource::Configured,
        )
    }

    fn defaults() -> DefaultTargets {
        DefaultTargets {
            gnosis_vpn_server: "172.30.0.1:8000".parse().unwrap(),
            wireguard_server: "172.30.0.1:51820".parse().unwrap(),
        }
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

    /// Mirrors `convert_destinations`: effective targets start on the defaults, pins say which are chosen.
    fn pinned(
        id: &str,
        addr: Address,
        labels: HashMap<String, String>,
        bridge: Option<SocketAddr>,
        wg: Option<SocketAddr>,
    ) -> Destination {
        Destination::new(
            id.to_string(),
            addr,
            default_path(),
            Meta::from_map(labels.clone()),
            bridge.unwrap_or(defaults().gnosis_vpn_server),
            wg.unwrap_or(defaults().wireguard_server),
            DestinationSource::Configured,
        )
        .with_overrides(Overrides::from_config(labels, bridge, wg))
    }

    fn merged(dest: Destination, info: ExitNodeInfo) -> Destination {
        let addr = dest.address;
        let mut destinations = HashMap::new();
        destinations.insert(dest.id.clone(), dest);
        let mut discovered = HashMap::new();
        discovered.insert(addr, info);

        merge_discovered(&mut destinations, &discovered, defaults());

        destinations.into_values().next().expect("the destination survives")
    }

    #[test]
    fn a_configured_and_discovered_address_becomes_confirmed() {
        let addr = address(1);

        let dest = merged(configured("dest-1", addr), exit_node(addr));

        assert_eq!(dest.source, DestinationSource::ConfiguredAndDiscovered);
    }

    #[test]
    fn an_unpinned_target_takes_the_exits_own_endpoint() {
        let addr = address(1);
        let info = exit_node(addr);

        let dest = merged(pinned("dest-1", addr, HashMap::new(), None, None), info.clone());

        assert_eq!(dest.gnosis_vpn_server, info.gnosis_vpn_server);
        assert_eq!(dest.wireguard_server, info.wireguard_server);
    }

    #[test]
    fn a_pinned_target_beats_the_exits_own_endpoint() {
        let addr = address(1);
        let bridge: SocketAddr = "192.168.0.1:8000".parse().unwrap();
        let info = exit_node(addr);

        let dest = merged(pinned("dest-1", addr, HashMap::new(), Some(bridge), None), info.clone());

        assert_eq!(dest.gnosis_vpn_server, bridge);
        assert_eq!(dest.wireguard_server, info.wireguard_server);
    }

    /// `(c)` says configuration chose the value, not that it disagreed with the exit.
    #[test]
    fn a_pin_agreeing_with_the_exit_is_still_marked() {
        let addr = address(1);
        let mut config_labels = HashMap::new();
        config_labels.insert("location".to_string(), "Germany".to_string());
        let mut info = exit_node(addr);
        info.meta.insert("location".to_string(), "Germany".to_string());

        let dest = merged(
            pinned("dest-1", addr, config_labels, Some(info.gnosis_vpn_server), None),
            info.clone(),
        );
        let rendered = dest.to_string();

        assert_eq!(dest.gnosis_vpn_server, info.gnosis_vpn_server);
        assert!(rendered.contains("location: Germany (c)"));
        assert!(rendered.contains(&format!("gnosis_vpn_server: {} (c)", info.gnosis_vpn_server)));
    }

    #[test]
    fn configuration_pins_labels_per_key() {
        let addr = address(1);
        let mut config_labels = HashMap::new();
        config_labels.insert("flag".to_string(), "DE".to_string());
        config_labels.insert("operator".to_string(), "acme".to_string());

        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "FRA-1".to_string());
        info.meta.insert("location".to_string(), "France".to_string());
        info.meta.insert("flag".to_string(), "FR".to_string());
        info.meta.insert("operator".to_string(), "globex".to_string());

        let dest = merged(pinned("dest-1", addr, config_labels, None, None), info);

        assert_eq!(Some("DE"), dest.meta.flag.as_deref());
        assert_eq!(Some("acme"), dest.meta.other.get("operator").map(String::as_str));
        assert_eq!(Some("FRA-1"), dest.meta.name.as_deref());
        assert_eq!(Some("France"), dest.meta.location.as_deref());
    }

    #[test]
    fn a_deregistered_exit_falls_back_to_configuration_and_the_defaults() {
        let addr = address(1);
        let mut config_labels = HashMap::new();
        config_labels.insert("flag".to_string(), "DE".to_string());
        let mut destinations = HashMap::new();
        destinations.insert("dest-1".to_string(), pinned("dest-1", addr, config_labels, None, None));
        let mut info = exit_node(addr);
        info.meta.insert("location".to_string(), "France".to_string());
        let mut discovered = HashMap::new();
        discovered.insert(addr, info);
        merge_discovered(&mut destinations, &discovered, defaults());

        merge_discovered(&mut destinations, &HashMap::new(), defaults());

        let dest = &destinations["dest-1"];
        assert_eq!(dest.source, DestinationSource::Configured);
        assert_eq!(Some("DE"), dest.meta.flag.as_deref());
        assert_eq!(None, dest.meta.location.as_deref());
        assert_eq!(dest.gnosis_vpn_server, defaults().gnosis_vpn_server);
    }

    #[test]
    fn a_configured_entry_at_another_path_leaves_the_discovered_one_beside_it() {
        let addr = address(1);
        let mut three_hops = configured("dest-1", addr);
        three_hops.routing = HopRouting::try_from(3).unwrap();
        let mut destinations = HashMap::new();
        destinations.insert("dest-1".to_string(), three_hops);
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        merge_discovered(&mut destinations, &discovered, defaults());

        assert_eq!(2, destinations.len());
        assert_eq!(DestinationSource::Configured, destinations["dest-1"].source);
        let found = &destinations[&addr.to_checksum()];
        assert_eq!(DestinationSource::Discovered, found.source);
        assert_eq!(default_path(), found.routing);
    }

    #[test]
    fn a_configured_id_matching_another_exits_address_is_not_clobbered() {
        let addr = address(1);
        let mut destinations = HashMap::new();
        destinations.insert(addr.to_checksum(), configured(&addr.to_checksum(), address(2)));
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        merge_discovered(&mut destinations, &discovered, defaults());

        assert_eq!(1, destinations.len());
        assert_eq!(address(2), destinations[&addr.to_checksum()].address);
    }

    /// Overriding a label also keeps the operator's version of it off the terminal entirely.
    #[test]
    fn an_overridden_label_shows_the_configured_value_and_never_the_published_one() {
        let addr = address(1);
        let mut config_labels = HashMap::new();
        config_labels.insert("location".to_string(), "Germany".to_string());
        let mut info = exit_node(addr);
        info.meta
            .insert("location".to_string(), format!("\u{1b}[2K\u{202e}{}", "x".repeat(200)));

        let dest = merged(pinned("dest-1", addr, config_labels, None, None), info);
        let rendered = dest.to_string();

        assert!(rendered.contains("location: Germany (c)"));
        assert!(!rendered.contains("xx"));
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{202e}'));
    }

    #[test]
    fn only_configurations_own_labels_are_marked() {
        let addr = address(1);
        let mut config_labels = HashMap::new();
        config_labels.insert("flag".to_string(), "DE".to_string());
        let mut info = exit_node(addr);
        info.meta.insert("flag".to_string(), "FR".to_string());
        info.meta.insert("location".to_string(), "France".to_string());

        let dest = merged(pinned("dest-1", addr, config_labels, None, None), info);
        let rendered = dest.to_string();

        assert!(rendered.contains("flag: DE (c)"));
        assert!(rendered.contains("location: France,") || rendered.contains("location: France)"));
        assert!(!rendered.contains("location: France (c)"));
    }

    /// The title carries the name but not that configuration chose it.
    #[test]
    fn an_overridden_name_is_marked_beside_the_title() {
        let addr = address(1);
        let mut config_labels = HashMap::new();
        config_labels.insert("name".to_string(), "Frankfurt-1".to_string());
        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "FRA-1".to_string());

        let dest = merged(pinned("dest-1", addr, config_labels, None, None), info);
        let rendered = dest.to_string();

        assert!(rendered.starts_with("Frankfurt-1(dest-1) [c+d] (Exit:"));
        assert!(rendered.contains("name: Frankfurt-1 (c)"));
    }

    /// Targets are never rendered otherwise, so a pinned one would be invisible.
    #[test]
    fn a_target_is_rendered_only_when_configuration_pinned_it() {
        let addr = address(1);
        let bridge: SocketAddr = "192.168.0.1:8000".parse().unwrap();
        let info = exit_node(addr);

        let overridden = merged(pinned("dest-1", addr, HashMap::new(), Some(bridge), None), info.clone());
        let inherited = merged(pinned("dest-1", addr, HashMap::new(), None, None), info);

        assert!(
            overridden
                .to_string()
                .contains("gnosis_vpn_server: 192.168.0.1:8000 (c)")
        );
        assert!(!overridden.to_string().contains("wireguard_server:"));
        assert!(!inherited.to_string().contains("gnosis_vpn_server:"));
    }

    #[test]
    fn every_origin_renders_its_own_code() {
        let codes = [
            (DestinationSource::Configured, "[c]"),
            (DestinationSource::Discovered, "[d]"),
            (DestinationSource::ConfiguredAndDiscovered, "[c+d]"),
        ];

        for (source, code) in codes {
            let mut dest = configured("dest-1", address(1));
            dest.source = source;

            assert!(dest.to_string().starts_with(&format!("dest-1 {code} (Exit:")));
        }
    }

    /// A config-only entry is all configuration by definition, so `[c]` is the whole story.
    #[test]
    fn a_config_only_entry_marks_no_individual_value() {
        let mut config_labels = HashMap::new();
        config_labels.insert("location".to_string(), "Germany".to_string());
        let bridge: SocketAddr = "192.168.0.1:8000".parse().unwrap();

        let dest = pinned("dest-1", address(1), config_labels, Some(bridge), None);
        let rendered = dest.to_string();

        assert!(rendered.contains("location: Germany)"));
        assert!(!rendered.contains("(c)"));
        assert!(!rendered.contains("gnosis_vpn_server:"));
    }

    #[test]
    fn discovery_only_address_is_inserted_fresh() {
        let addr = address(2);
        let mut destinations = HashMap::new();
        let mut discovered = HashMap::new();
        let info = exit_node(addr);
        discovered.insert(addr, info.clone());

        merge_discovered(&mut destinations, &discovered, defaults());

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
        merge_discovered(&mut destinations, &discovered, defaults());
        assert_eq!(destinations.len(), 1);

        merge_discovered(&mut destinations, &HashMap::new(), defaults());
        assert!(destinations.is_empty());
    }

    #[test]
    fn confirmed_entry_downgrades_to_configured_once_deregistered() {
        let addr = address(4);
        let mut destinations = HashMap::new();
        destinations.insert("dest-4".to_string(), configured("dest-4", addr));
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));
        merge_discovered(&mut destinations, &discovered, defaults());
        assert_eq!(
            destinations["dest-4"].source,
            DestinationSource::ConfiguredAndDiscovered
        );

        merge_discovered(&mut destinations, &HashMap::new(), defaults());
        assert_eq!(destinations.len(), 1);
        assert_eq!(destinations["dest-4"].source, DestinationSource::Configured);
    }

    #[test]
    fn repeated_merge_with_unchanged_discovered_map_is_idempotent() {
        let addr = address(5);
        let mut destinations = HashMap::new();
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        merge_discovered(&mut destinations, &discovered, defaults());
        merge_discovered(&mut destinations, &discovered, defaults());
        merge_discovered(&mut destinations, &discovered, defaults());

        assert_eq!(destinations.len(), 1);
        assert_eq!(
            destinations.values().next().unwrap().source,
            DestinationSource::Discovered
        );
    }

    #[test]
    fn meta_from_map_moves_known_keys_out_of_other() {
        let mut labels = HashMap::new();
        labels.insert("name".to_string(), "london-1".to_string());
        labels.insert("location".to_string(), "London".to_string());
        labels.insert("flag".to_string(), "GB".to_string());
        labels.insert("description".to_string(), "fast".to_string());
        labels.insert("operator".to_string(), "acme".to_string());

        let meta = Meta::from_map(labels);

        assert_eq!(Some("london-1".to_string()), meta.name);
        assert_eq!(Some("London".to_string()), meta.location);
        assert_eq!(Some("GB".to_string()), meta.flag);
        assert_eq!(Some("fast".to_string()), meta.description);
        // Each key has exactly one home.
        assert_eq!(1, meta.other.len());
        assert_eq!(Some(&"acme".to_string()), meta.other.get("operator"));
    }

    #[test]
    fn meta_from_map_keeps_every_unknown_key() {
        let mut labels = HashMap::new();
        labels.insert("operator".to_string(), "acme".to_string());
        labels.insert("port".to_string(), "51820".to_string());

        let meta = Meta::from_map(labels);

        assert_eq!(None, meta.location);
        assert_eq!(2, meta.other.len());
    }

    #[test]
    fn meta_from_empty_map_is_default() {
        assert_eq!(Meta::default(), Meta::from_map(HashMap::new()));
    }

    #[test]
    fn display_omits_the_label_segment_when_there_are_no_labels() {
        let dest = Destination::new(
            "d".to_string(),
            address(1),
            HopRouting::try_from(1).unwrap(),
            Meta::default(),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:51820".parse().unwrap(),
            DestinationSource::Discovered,
        );

        let rendered = dest.to_string();

        assert!(rendered.ends_with("))"));
        assert!(!rendered.contains(", )"));
    }

    #[test]
    fn display_renders_all_three_known_labels() {
        let mut labels = HashMap::new();
        labels.insert("name".to_string(), "london-1".to_string());
        labels.insert("location".to_string(), "London".to_string());
        labels.insert("flag".to_string(), "GB".to_string());
        labels.insert("description".to_string(), "fast".to_string());
        labels.insert("operator".to_string(), "acme".to_string());
        let dest = Destination::new(
            "d".to_string(),
            address(1),
            HopRouting::try_from(1).unwrap(),
            Meta::from_map(labels),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:51820".parse().unwrap(),
            DestinationSource::Discovered,
        );

        let rendered = dest.to_string();

        assert!(rendered.contains("location: London, flag: GB, description: fast"));
        // `name` is the title, so it must not be repeated among the labels.
        assert!(!rendered.contains("name: london-1"));
        // Unrecognized labels are kept but not displayed for now.
        assert!(!rendered.contains("acme"));
    }

    fn named(id: &str, name: &str, source: DestinationSource) -> Destination {
        let mut labels = HashMap::new();
        labels.insert("name".to_string(), name.to_string());
        Destination::new(
            id.to_string(),
            address(1),
            HopRouting::try_from(1).unwrap(),
            Meta::from_map(labels),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:51820".parse().unwrap(),
            source,
        )
    }

    #[test]
    fn title_is_the_config_key_without_a_name() {
        assert_eq!("my-exit", configured("my-exit", address(1)).title());
    }

    #[test]
    fn title_brackets_the_config_key_next_to_the_name() {
        let dest = named("my-exit", "Frankfurt-1", DestinationSource::ConfiguredAndDiscovered);

        assert_eq!("Frankfurt-1(my-exit)", dest.title());
        assert!(dest.to_string().starts_with("Frankfurt-1(my-exit) [c+d] (Exit:"));
    }

    #[test]
    fn title_is_the_name_alone_for_a_discovered_destination() {
        // The discovered key is only the checksummed address, already rendered as `Exit:`.
        let dest = named(&address(1).to_checksum(), "Frankfurt-1", DestinationSource::Discovered);

        assert_eq!("Frankfurt-1", dest.title());
    }

    #[test]
    fn title_is_the_checksummed_key_for_a_discovered_destination_without_a_name() {
        let mut destinations = HashMap::new();
        let mut discovered = HashMap::new();
        discovered.insert(address(1), exit_node(address(1)));

        merge_discovered(&mut destinations, &discovered, defaults());

        assert_eq!(
            address(1).to_checksum(),
            destinations[&address(1).to_checksum()].title()
        );
    }

    #[test]
    fn title_collapses_when_name_and_key_match() {
        let dest = named("Frankfurt-1", "Frankfurt-1", DestinationSource::ConfiguredAndDiscovered);

        assert_eq!("Frankfurt-1", dest.title());
    }

    #[test]
    fn title_sanitizes_and_elides_a_hostile_name() {
        let hostile = format!("\u{1b}[2K\u{202e}{}", "x".repeat(200));
        let dest = named("my-exit", &hostile, DestinationSource::Discovered);

        let title = dest.title();

        assert!(!title.contains('\u{1b}'));
        assert!(!title.contains('\u{202e}'));
        assert_eq!(format!("[2K{}…", "x".repeat(META_FIELD_MAX_CHARS - 3)), title);
    }

    #[test]
    fn configured_and_discovered_destination_adopts_the_operator_name() {
        let addr = address(1);
        let mut destinations = HashMap::new();
        destinations.insert("dest-1".to_string(), configured("dest-1", addr));

        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "Frankfurt-1".to_string());
        let mut discovered = HashMap::new();
        discovered.insert(addr, info);

        merge_discovered(&mut destinations, &discovered, defaults());

        assert_eq!("Frankfurt-1(dest-1)", destinations["dest-1"].title());
    }

    /// `Destination` crosses the socket whole and ctl never negotiates a version - pin the shape.
    #[test]
    fn destination_serializes_to_the_expected_json_shape() {
        let dest = configured("dest-1", address(1));

        let json = serde_json::to_string(&dest).unwrap();

        assert_eq!(
            json,
            r#"{"id":"dest-1","meta":{"name":null,"location":null,"flag":null,"description":null,"other":{}},"address":"0x0101010101010101010101010101010101010101","routing":1,"gnosis_vpn_server":"172.30.0.1:8000","wireguard_server":"172.30.0.1:51820","source":"Configured","overrides":{"configured_meta":{},"configured_gnosis_vpn_server":null,"configured_wireguard_server":null}}"#
        );
    }

    /// An older daemon's payload carries no `overrides`; a newer ctl must still read it.
    #[test]
    fn a_destination_without_overrides_deserializes() {
        let json = r#"{"id":"dest-1","meta":{"name":null,"location":null,"flag":null,"description":null,"other":{}},"address":"0x0101010101010101010101010101010101010101","routing":1,"gnosis_vpn_server":"172.30.0.1:8000","wireguard_server":"172.30.0.1:51820","source":"Configured"}"#;

        let dest: Destination = serde_json::from_str(json).expect("an older payload still parses");

        assert_eq!(Overrides::default(), dest.overrides);
    }

    #[test]
    fn same_exit_ignores_name_and_source_drift() {
        let base = configured("dest-1", address(1));

        let mut renamed = base.clone();
        renamed.meta.name = Some("Frankfurt-1".to_string());
        renamed.source = DestinationSource::ConfiguredAndDiscovered;
        assert!(base.same_exit(&renamed));

        let mut unnamed = renamed.clone();
        unnamed.meta.name = None;
        assert!(base.same_exit(&unnamed));

        let mut relabelled = base.clone();
        relabelled.meta.location = Some("Germany".to_string());
        assert!(base.same_exit(&relabelled));
    }

    #[test]
    fn same_exit_separates_a_different_exit_or_path() {
        let base = configured("dest-1", address(1));

        assert!(!base.same_exit(&configured("dest-1", address(2))));

        let mut rerouted = base.clone();
        rerouted.routing = HopRouting::try_from(3).unwrap();
        assert!(!base.same_exit(&rerouted));
    }

    /// Targets resolve from discovery, so a tick filling them in must not read as a new exit.
    #[test]
    fn same_exit_ignores_the_session_targets() {
        let base = configured("dest-1", address(1));

        let mut other_bridge = base.clone();
        other_bridge.gnosis_vpn_server = "10.0.0.1:9000".parse().unwrap();
        assert!(base.same_exit(&other_bridge));

        let mut other_wg = base.clone();
        other_wg.wireguard_server = "10.0.0.1:9001".parse().unwrap();
        assert!(base.same_exit(&other_wg));
    }

    /// Without `same_exit` a tick would make the connected exit look like a new one and drop the tunnel.
    #[test]
    fn a_discovery_tick_leaves_a_live_connections_identity_intact() {
        let addr = address(1);
        let mut destinations = HashMap::new();
        destinations.insert("dest-1".to_string(), configured("dest-1", addr));
        let live = destinations["dest-1"].clone();

        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "Frankfurt-1".to_string());
        let mut discovered = HashMap::new();
        discovered.insert(addr, info);

        merge_discovered(&mut destinations, &discovered, defaults());

        let refreshed = &destinations["dest-1"];
        assert!(live.same_exit(refreshed));
        assert_ne!(&live, refreshed);
    }

    #[test]
    fn meta_display_strips_terminal_control_and_spoofing_characters() {
        let mut meta = HashMap::new();
        meta.insert(
            "location".to_string(),
            "Germany\u{1b}[2K\r\nSource: Configured\u{202e}".to_string(),
        );
        let dest = Destination::new(
            "d".to_string(),
            address(1),
            HopRouting::try_from(1).unwrap(),
            Meta::from_map(meta),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:51820".parse().unwrap(),
            DestinationSource::Discovered,
        );

        let rendered = dest.to_string();

        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\r'));
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\u{202e}'));
        // Losing the ESC leaves the sequence body as inert literal text.
        assert!(rendered.contains("location: Germany[2KSource: Configured"));
    }

    #[test]
    fn meta_display_elides_over_long_values() {
        let mut meta = HashMap::new();
        meta.insert("location".to_string(), "x".repeat(200));
        let dest = Destination::new(
            "d".to_string(),
            address(1),
            HopRouting::try_from(1).unwrap(),
            Meta::from_map(meta),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:51820".parse().unwrap(),
            DestinationSource::Discovered,
        );

        let rendered = dest.to_string();

        assert!(rendered.contains(&format!("location: {}…", "x".repeat(META_FIELD_MAX_CHARS))));
        assert!(!rendered.contains(&"x".repeat(META_FIELD_MAX_CHARS + 1)));
    }

    /// No label may precede the origin code; only the title, sanitized and capped alike, may.
    #[test]
    fn the_origin_code_is_rendered_before_meta() {
        let mut meta = HashMap::new();
        meta.insert("location".to_string(), "Germany".to_string());
        let dest = Destination::new(
            "d".to_string(),
            address(1),
            HopRouting::try_from(1).unwrap(),
            Meta::from_map(meta),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:51820".parse().unwrap(),
            DestinationSource::Discovered,
        );

        let rendered = dest.to_string();

        assert!(rendered.find("[d]").unwrap() < rendered.find("location:").unwrap());
        assert!(rendered.find("Route:").unwrap() < rendered.find("location:").unwrap());
    }
}
