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

    /// The recognized labels sanitized for terminal output, empty when none are set.
    fn display_str(&self) -> String {
        [
            ("location", &self.location),
            ("flag", &self.flag),
            ("description", &self.description),
        ]
        .into_iter()
        .filter_map(|(name, value)| {
            value
                .as_ref()
                .map(|value| format!("{name}: {}", sanitize_for_display(value)))
        })
        .collect::<Vec<String>>()
        .join(", ")
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Destination {
    pub id: String,
    pub meta: Meta,
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

impl Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let short_addr = log_output::address(&self.address);
        let labels = self.meta.display_str();
        let labels = if labels.is_empty() {
            String::new()
        } else {
            format!(", {labels}")
        };
        write!(
            f,
            "{id} (Exit: {address}, Route: (entry){path}({short_addr}), Source: {source}{labels})",
            id = self.title(),
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
/// destination's `source` to [`DestinationSource::ConfiguredAndDiscovered`] and adopts the
/// operator's current `name` — the configured target values keep governing, since discovery there
/// is just a confirmation, not an override. A discovered address with no configured match is
/// inserted fresh, keyed by its own checksummed address (no human-chosen id exists for it). A
/// previously discovered destination whose registration disappeared is removed outright; a
/// previously confirmed (`ConfiguredAndDiscovered`) one is downgraded back to `Configured` rather
/// than removed, since it is still valid, statically configured data.
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
            // The discovered name wins: it is the operator's current label for the node.
            dest.meta.name = Meta::from_map(info.meta.clone()).name;
            continue;
        }
        let id = address.to_checksum();
        destinations.insert(
            id.clone(),
            Destination::new(
                id,
                *address,
                HopRouting::try_from(1).expect("1 is always a valid hop count"),
                Meta::from_map(info.meta.clone()),
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
            Meta::default(),
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

        assert!(rendered.ends_with("Source: Discovered)"));
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
        assert!(dest.to_string().starts_with("Frankfurt-1(my-exit) (Exit:"));
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

        merge_discovered(&mut destinations, &discovered);

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

        merge_discovered(&mut destinations, &discovered);

        assert_eq!("Frankfurt-1(dest-1)", destinations["dest-1"].title());
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

        merge_discovered(&mut destinations, &discovered);

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

    /// No label may precede the source marker; only the title, sanitized and capped alike, may.
    #[test]
    fn source_is_rendered_before_meta() {
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

        assert!(rendered.find("Source:").unwrap() < rendered.find("location:").unwrap());
    }
}
