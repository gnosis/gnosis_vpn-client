use edgli::ExitNodeInfo;
pub use edgli::hopr_lib::HopRouting;
pub use edgli::hopr_lib::api::types::primitive::prelude::Address;
use edgli::hopr_lib::exports::network::types::types::{IpOrHost, SealedHost};
use edgli::hopr_lib::exports::transport::SessionTarget;
use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Display};
use std::net::SocketAddr;

use crate::log_output;
use crate::serde_utils;

/// Where a [`Destination`] came from - the registry knows this exit, or only your config does.
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

/// A destination's identity; every other field is display state a discovery tick may rewrite.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExitKey {
    pub address: Address,
    pub routing: HopRouting,
}

impl ExitKey {
    /// Pins a discovered id to its key so no later same-named exit can take it; FNV-1a by hand, as `DefaultHasher` drifts per rustc.
    fn discriminator(&self) -> String {
        const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;
        const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";

        let mut hash = OFFSET;
        for byte in self.to_string().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
        (0..4)
            .map(|_| {
                let c = ALPHABET[(hash % ALPHABET.len() as u64) as usize] as char;
                hash /= ALPHABET.len() as u64;
                c
            })
            .collect()
    }

    /// `HopRouting` is not `Ord`, and unstable key order would shuffle connect ids between ticks.
    fn sort_key(&self) -> (String, usize) {
        (self.address.to_checksum(), self.routing.hop_count())
    }
}

impl Display for ExitKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}@{}", self.address.to_checksum(), self.routing.hop_count())
    }
}

impl std::str::FromStr for ExitKey {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (address, hops) = s.split_once('@').ok_or_else(|| format!("not an exit key: {s}"))?;
        Ok(Self {
            address: address.parse::<Address>().map_err(|e| e.to_string())?,
            routing: hops
                .parse::<usize>()
                .map_err(|e| e.to_string())
                .and_then(|h| HopRouting::try_from(h).map_err(|e| e.to_string()))?,
        })
    }
}

/// Longest connect id minted from a published name.
const SLUG_MAX_CHARS: usize = 32;

/// A published name as one shell-safe token - bash completion splits connect ids on whitespace.
fn slug(name: &str) -> Option<String> {
    let mut out = String::new();
    for c in name.chars() {
        if out.len() >= SLUG_MAX_CHARS {
            break;
        }
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let slug = out.trim_matches('-');
    // A `0x…` slug would shadow the address handle in `Destinations::resolve`.
    if slug.is_empty() || slug.starts_with("0x") {
        return None;
    }
    Some(slug.to_string())
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
    /// The handle `status` shows and `connect` accepts; still `id` on the wire that ctl and the app read.
    #[serde(rename = "id")]
    pub connect_id: String,
    pub meta: Meta,
    #[serde(with = "serde_utils::address")]
    pub address: Address,
    pub routing: HopRouting,
    /// Configuration's value if pinned, else the exit's published endpoint, else the global default.
    pub gnosis_vpn_server: SocketAddr,
    /// The WireGuard-session target, resolved by the same precedence as `gnosis_vpn_server`.
    pub wireguard_server: SocketAddr,
    pub source: DestinationSource,
    pub overrides: Overrides,
}

impl Destination {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        connect_id: String,
        address: Address,
        routing: HopRouting,
        meta: Meta,
        gnosis_vpn_server: SocketAddr,
        wireguard_server: SocketAddr,
        source: DestinationSource,
    ) -> Self {
        Self {
            connect_id,
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

    /// Identity: the exit and the path to it; every other field is resolved or display state.
    pub fn key(&self) -> ExitKey {
        ExitKey {
            address: self.address,
            routing: self.routing,
        }
    }

    pub fn same_exit(&self, other: &Self) -> bool {
        self.key() == other.key()
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
        if let Some(name) = &self.meta.name {
            let shown = sanitize_for_display(name);
            // A discovered id is this name slugged plus the key hash, so only a name spelled like it says nothing new.
            let key_hash = format!("-{}", self.key().discriminator());
            let own_id = self.connect_id.strip_suffix(&key_hash).unwrap_or(&self.connect_id);
            let adds_nothing = shown == own_id;
            if pinned("name") || !adds_nothing {
                let mark = if pinned("name") { CONFIG_VALUE } else { "" };
                parts.push(format!("name: {shown}{mark}"));
            }
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
            id = self.connect_id,
            source = self.source.code(),
            path = self.pretty_print_path(),
            address = self.address.to_checksum(),
            short_addr = short_addr,
        )
    }
}

/// Why a connect token named no single destination.
#[derive(Clone, Debug, PartialEq)]
pub enum Unresolved {
    NotFound,
    /// One exit reached by several paths - the caller must pick a connect id.
    Ambiguous(Vec<String>),
}

/// Keyed by identity, addressed by `connect_id` - one type, so the two cannot drift apart.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Destinations {
    by_exit: HashMap<ExitKey, Destination>,
}

impl Destinations {
    pub fn get(&self, key: &ExitKey) -> Option<&Destination> {
        self.by_exit.get(key)
    }

    pub fn get_mut(&mut self, key: &ExitKey) -> Option<&mut Destination> {
        self.by_exit.get_mut(key)
    }

    pub fn by_connect_id(&self, connect_id: &str) -> Option<&Destination> {
        self.by_exit.values().find(|d| d.connect_id == connect_id)
    }

    /// Returns the entry this one replaced, which is how a duplicate is detected at config load.
    pub fn insert(&mut self, dest: Destination) -> Option<Destination> {
        self.by_exit.insert(dest.key(), dest)
    }

    pub fn contains_key(&self, key: &ExitKey) -> bool {
        self.by_exit.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &ExitKey> {
        self.by_exit.keys()
    }

    pub fn values(&self) -> impl Iterator<Item = &Destination> {
        self.by_exit.values()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ExitKey, &Destination)> {
        self.by_exit.iter()
    }

    pub fn len(&self) -> usize {
        self.by_exit.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_exit.is_empty()
    }

    /// Connect ids in a stable order, for `gvpn-ctl destinations` and shell completion.
    pub fn connect_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.by_exit.values().map(|d| d.connect_id.clone()).collect();
        ids.sort_unstable();
        ids
    }

    /// A connect id, or a bare exit address when it names one destination - so a saved checksum works.
    pub fn resolve(&self, token: &str) -> Result<&Destination, Unresolved> {
        if let Some(dest) = self.by_exit.values().find(|d| d.connect_id == token) {
            return Ok(dest);
        }
        let Ok(address) = token.parse::<Address>() else {
            return Err(Unresolved::NotFound);
        };
        let mut matched: Vec<&Destination> = self.by_exit.values().filter(|d| d.address == address).collect();
        match matched.len() {
            0 => Err(Unresolved::NotFound),
            1 => Ok(matched.remove(0)),
            _ => {
                let mut ids: Vec<String> = matched.iter().map(|d| d.connect_id.clone()).collect();
                ids.sort_unstable();
                Err(Unresolved::Ambiguous(ids))
            }
        }
    }

    /// Joined on `(address, path)`, so a configured entry at another path keeps its own destination.
    pub fn merge_discovered(&mut self, discovered: &HashMap<Address, ExitNodeInfo>, defaults: DefaultTargets) {
        let default_path = default_path();
        let is_discovered = |dest: &Destination| dest.routing == default_path && discovered.contains_key(&dest.address);

        self.by_exit.retain(|_, dest| match dest.source {
            DestinationSource::Discovered if !is_discovered(dest) => false,
            DestinationSource::ConfiguredAndDiscovered if !is_discovered(dest) => {
                dest.source = DestinationSource::Configured;
                dest.forget_discovered(defaults);
                true
            }
            _ => true,
        });

        for (address, info) in discovered {
            let key = ExitKey {
                address: *address,
                routing: default_path,
            };
            if let Some(dest) = self.by_exit.get_mut(&key) {
                if dest.source == DestinationSource::Configured {
                    dest.source = DestinationSource::ConfiguredAndDiscovered;
                }
                dest.adopt(info);
                continue;
            }
            let mut dest = Destination::new(
                address.to_checksum(),
                *address,
                default_path,
                Meta::default(),
                defaults.gnosis_vpn_server,
                defaults.wireguard_server,
                DestinationSource::Discovered,
            );
            dest.adopt(info);
            self.by_exit.insert(key, dest);
        }

        self.assign_connect_ids();
    }

    /// Configured ids win: published metadata is unverified and must never take a user's own id.
    fn assign_connect_ids(&mut self) {
        let mut taken: HashSet<String> = self
            .by_exit
            .values()
            .filter(|d| d.source != DestinationSource::Discovered)
            .map(|d| d.connect_id.clone())
            .collect();

        // Sorted, because HashMap order would shuffle which entry gets which numbered fallback.
        let mut discovered: Vec<ExitKey> = self
            .by_exit
            .iter()
            .filter(|(_, d)| d.source == DestinationSource::Discovered)
            .map(|(key, _)| *key)
            .collect();
        discovered.sort_unstable_by_key(|key| key.sort_key());

        for key in discovered {
            let dest = &self.by_exit[&key];
            let name = dest.meta.name.as_deref().map(sanitize_for_display);
            let candidate = name
                .as_deref()
                .and_then(slug)
                .unwrap_or_else(|| key.address.to_checksum());

            // The key hash pins the id to this exit, so a same-named exit found later cannot take it.
            let pinned = format!("{candidate}-{}", key.discriminator());
            // Configured ids are arbitrary, so even the key's own form is checked before use.
            let numbered = (2..).map(|n| format!("{key}-{n}"));
            let connect_id = [pinned, key.to_string()]
                .into_iter()
                .chain(numbered)
                .find(|id| !taken.contains(id))
                .expect("the numbered candidates never run out");
            taken.insert(connect_id.clone());
            if let Some(dest) = self.by_exit.get_mut(&key) {
                dest.connect_id = connect_id;
            }
        }
    }
}

impl Serialize for Destinations {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.by_exit.len()))?;
        for (key, dest) in &self.by_exit {
            m.serialize_entry(&key.to_string(), dest)?;
        }
        m.end()
    }
}

impl<'de> Deserialize<'de> for Destinations {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = HashMap::<String, Destination>::deserialize(d)?;
        let by_exit = raw
            .into_iter()
            .map(|(key, dest)| Ok((key.parse::<ExitKey>().map_err(serde::de::Error::custom)?, dest)))
            .collect::<Result<_, D::Error>>()?;
        Ok(Self { by_exit })
    }
}

impl FromIterator<Destination> for Destinations {
    fn from_iter<I: IntoIterator<Item = Destination>>(iter: I) -> Self {
        Self {
            by_exit: iter.into_iter().map(|dest| (dest.key(), dest)).collect(),
        }
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
        let mut destinations = Destinations::default();
        destinations.insert(dest);
        let mut discovered = HashMap::new();
        discovered.insert(addr, info);

        destinations.merge_discovered(&discovered, defaults());

        destinations.values().next().expect("the destination survives").clone()
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
        let mut destinations = Destinations::default();
        destinations.insert(pinned("dest-1", addr, config_labels, None, None));
        let mut info = exit_node(addr);
        info.meta.insert("location".to_string(), "France".to_string());
        let mut discovered = HashMap::new();
        discovered.insert(addr, info);
        destinations.merge_discovered(&discovered, defaults());

        destinations.merge_discovered(&HashMap::new(), defaults());

        let dest = &destinations.by_connect_id("dest-1").unwrap();
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
        let mut destinations = Destinations::default();
        destinations.insert(three_hops);
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        destinations.merge_discovered(&discovered, defaults());

        assert_eq!(2, destinations.len());
        assert_eq!(
            DestinationSource::Configured,
            destinations.by_connect_id("dest-1").unwrap().source
        );
        let found = &destinations
            .by_connect_id(&pinned_id(addr, &addr.to_checksum()))
            .unwrap();
        assert_eq!(DestinationSource::Discovered, found.source);
        assert_eq!(default_path(), found.routing);
    }

    /// Keyed by identity, a configured id spelling another exit's address no longer hides it.
    #[test]
    fn a_configured_id_spelling_another_exits_address_no_longer_hides_it() {
        let addr = address(1);
        let mut destinations = Destinations::default();
        destinations.insert(configured(&addr.to_checksum(), address(2)));
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        destinations.merge_discovered(&discovered, defaults());

        assert_eq!(2, destinations.len());
        // The configured entry keeps the id it was given, so the discovered exit takes a suffix.
        assert_eq!(
            address(2),
            destinations.by_connect_id(&addr.to_checksum()).unwrap().address
        );
        let discovered = destinations
            .values()
            .find(|d| d.source == DestinationSource::Discovered)
            .expect("the discovered exit survives");
        assert_eq!(addr, discovered.address);
        assert_ne!(addr.to_checksum(), discovered.connect_id);
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

    /// The connect id leads the line; the name says which value configuration chose.
    #[test]
    fn an_overridden_name_is_marked_beside_the_connect_id() {
        let addr = address(1);
        let mut config_labels = HashMap::new();
        config_labels.insert("name".to_string(), "Frankfurt-1".to_string());
        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "FRA-1".to_string());

        let dest = merged(pinned("dest-1", addr, config_labels, None, None), info);
        let rendered = dest.to_string();

        assert!(rendered.starts_with("dest-1 [c+d] (Exit:"), "{rendered}");
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
        let mut destinations = Destinations::default();
        let mut discovered = HashMap::new();
        let info = exit_node(addr);
        discovered.insert(addr, info.clone());

        destinations.merge_discovered(&discovered, defaults());

        assert_eq!(destinations.len(), 1);
        let dest = destinations.values().next().unwrap();
        assert_eq!(dest.connect_id, pinned_id(addr, &addr.to_checksum()));
        assert_eq!(dest.source, DestinationSource::Discovered);
        assert_eq!(dest.routing, HopRouting::try_from(1).unwrap());
        assert_eq!(dest.gnosis_vpn_server, info.gnosis_vpn_server);
        assert_eq!(dest.wireguard_server, info.wireguard_server);
    }

    #[test]
    fn discovered_only_entry_is_removed_once_deregistered() {
        let addr = address(3);
        let mut destinations = Destinations::default();
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));
        destinations.merge_discovered(&discovered, defaults());
        assert_eq!(destinations.len(), 1);

        destinations.merge_discovered(&HashMap::new(), defaults());
        assert!(destinations.is_empty());
    }

    #[test]
    fn confirmed_entry_downgrades_to_configured_once_deregistered() {
        let addr = address(4);
        let mut destinations = Destinations::default();
        destinations.insert(configured("dest-4", addr));
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));
        destinations.merge_discovered(&discovered, defaults());
        assert_eq!(
            destinations.by_connect_id("dest-4").unwrap().source,
            DestinationSource::ConfiguredAndDiscovered
        );

        destinations.merge_discovered(&HashMap::new(), defaults());
        assert_eq!(destinations.len(), 1);
        assert_eq!(
            destinations.by_connect_id("dest-4").unwrap().source,
            DestinationSource::Configured
        );
    }

    #[test]
    fn repeated_merge_with_unchanged_discovered_map_is_idempotent() {
        let addr = address(5);
        let mut destinations = Destinations::default();
        let mut discovered = HashMap::new();
        discovered.insert(addr, exit_node(addr));

        destinations.merge_discovered(&discovered, defaults());
        destinations.merge_discovered(&discovered, defaults());
        destinations.merge_discovered(&discovered, defaults());

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
            "london-1".to_string(),
            address(1),
            HopRouting::try_from(1).unwrap(),
            Meta::from_map(labels),
            "127.0.0.1:8000".parse().unwrap(),
            "127.0.0.1:51820".parse().unwrap(),
            DestinationSource::Discovered,
        );

        let rendered = dest.to_string();

        assert!(rendered.contains("location: London, flag: GB, description: fast"));
        // The connect id is this name slugged, so repeating it among the labels would be noise.
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
    fn the_connect_id_leads_the_line() {
        let dest = configured("my-exit", address(1));

        assert!(dest.to_string().starts_with("my-exit [c] (Exit:"));
    }

    #[test]
    fn a_name_the_connect_id_does_not_already_say_is_shown_beside_it() {
        let dest = named("my-exit", "Frankfurt-1", DestinationSource::ConfiguredAndDiscovered);

        let rendered = dest.to_string();
        assert!(rendered.starts_with("my-exit [c+d] (Exit:"));
        assert!(rendered.contains("name: Frankfurt-1"), "{rendered}");
    }

    /// Only a name spelled exactly like the connect id is noise; anything else is the operator's.
    #[test]
    fn a_name_the_connect_id_already_says_is_not_repeated() {
        let dest = named("frankfurt-1", "frankfurt-1", DestinationSource::Discovered);

        assert!(!dest.to_string().contains("name:"), "{dest}");
    }

    #[test]
    fn a_name_the_connect_id_only_slugs_keeps_its_casing_and_spacing() {
        let dest = named("frankfurt-1", "Frankfurt 1", DestinationSource::Discovered);

        let rendered = dest.to_string();
        assert!(rendered.contains("name: Frankfurt 1"), "{rendered}");
    }

    #[test]
    fn a_discovered_name_becomes_the_connect_id() {
        let addr = address(1);
        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "Frankfurt 1".to_string());
        let mut destinations = Destinations::default();

        destinations.merge_discovered(&HashMap::from([(addr, info)]), defaults());

        assert_eq!(vec![pinned_id(addr, "frankfurt-1")], destinations.connect_ids());
    }

    #[test]
    fn a_discovered_exit_without_a_name_is_addressed_by_its_address_and_key_hash() {
        let addr = address(1);
        let mut destinations = Destinations::default();

        destinations.merge_discovered(&HashMap::from([(addr, exit_node(addr))]), defaults());

        assert_eq!(vec![pinned_id(addr, &addr.to_checksum())], destinations.connect_ids());
    }

    /// The id a discovered exit gets on the default path: its slug pinned by the key hash.
    fn pinned_id(addr: Address, slug: &str) -> String {
        let key = ExitKey {
            address: addr,
            routing: default_path(),
        };
        format!("{slug}-{}", key.discriminator())
    }

    /// A published name is attacker-controlled; none of it may reach the terminal or shell.
    #[test]
    fn a_hostile_name_cannot_shape_the_connect_id() {
        let hostile = format!("\u{1b}[2K\u{202e} rm -rf / {}", "x".repeat(200));

        let slug = slug(&sanitize_for_display(&hostile)).expect("something usable is left");

        assert!(slug.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'), "{slug}");
        assert!(slug.len() <= SLUG_MAX_CHARS);
    }

    #[test]
    fn a_name_that_slugs_to_nothing_falls_back_to_the_address() {
        assert_eq!(None, slug("!!!"));
        assert_eq!(None, slug(""));
    }

    /// `0x…` is the address handle, so a name may not mint one and shadow a real exit.
    #[test]
    fn a_name_may_not_slug_into_something_that_looks_like_an_address() {
        assert_eq!(None, slug("0xdeadbeef"));
    }

    fn named_exit(addr: Address, name: &str) -> ExitNodeInfo {
        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), name.to_string());
        info
    }

    #[test]
    fn two_exits_publishing_one_name_get_told_apart() {
        let (first, second) = (address(1), address(2));
        let mut destinations = Destinations::default();

        destinations.merge_discovered(
            &HashMap::from([
                (first, named_exit(first, "Berlin")),
                (second, named_exit(second, "Berlin")),
            ]),
            defaults(),
        );

        let mut expected = vec![pinned_id(first, "berlin"), pinned_id(second, "berlin")];
        expected.sort_unstable();
        assert_eq!(expected, destinations.connect_ids());
    }

    /// A stored target must keep naming the exit it connected to when a same-named exit appears at a lower address.
    #[test]
    fn a_same_named_exit_arriving_later_cannot_take_an_id() {
        let (later, mine) = (address(1), address(2));
        let mut destinations = Destinations::default();
        destinations.merge_discovered(&HashMap::from([(mine, named_exit(mine, "Berlin"))]), defaults());
        let id = destinations.connect_ids().remove(0);

        destinations.merge_discovered(
            &HashMap::from([(mine, named_exit(mine, "Berlin")), (later, named_exit(later, "Berlin"))]),
            defaults(),
        );

        assert_eq!(pinned_id(mine, "berlin"), id);
        assert_eq!(mine, destinations.by_connect_id(&id).unwrap().address);
    }

    /// Exit metadata is attacker-controlled, so a published name may never take a configured id.
    #[test]
    fn a_published_name_cannot_take_a_configured_id() {
        let (mine, theirs) = (address(1), address(2));
        let mut destinations = Destinations::default();
        destinations.insert(configured("berlin", mine));

        destinations.merge_discovered(&HashMap::from([(theirs, named_exit(theirs, "Berlin"))]), defaults());

        assert_eq!(mine, destinations.by_connect_id("berlin").unwrap().address);
    }

    #[test]
    fn an_exit_resolves_by_connect_id_and_by_its_address() {
        let addr = address(1);
        let destinations = Destinations::from_iter([configured("my-exit", addr)]);

        assert_eq!(addr, destinations.resolve("my-exit").unwrap().address);
        assert_eq!(addr, destinations.resolve(&addr.to_checksum()).unwrap().address);
        assert_eq!(Err(Unresolved::NotFound), destinations.resolve("nope").map(|_| ()));
    }

    /// One exit at two paths is two destinations, so its bare address names neither.
    #[test]
    fn an_address_reached_by_two_paths_resolves_to_neither() {
        let addr = address(1);
        let mut far = configured("my-exit-far", addr);
        far.routing = HopRouting::try_from(3).unwrap();
        let destinations = Destinations::from_iter([configured("my-exit", addr), far]);

        let err = destinations.resolve(&addr.to_checksum()).map(|_| ()).unwrap_err();

        assert_eq!(
            Unresolved::Ambiguous(vec!["my-exit".to_string(), "my-exit-far".to_string()]),
            err
        );
    }

    /// The address alone cannot express this, which is why the discriminator hashes the key.
    #[test]
    fn one_exit_at_two_paths_gets_two_discriminators() {
        let addr = address(1);
        let one = ExitKey {
            address: addr,
            routing: HopRouting::try_from(1).unwrap(),
        };
        let three = ExitKey {
            address: addr,
            routing: HopRouting::try_from(3).unwrap(),
        };

        assert_ne!(one.discriminator(), three.discriminator());
    }

    /// Pins the hash: connect ids may churn on an update, but never silently.
    #[test]
    fn the_discriminator_of_a_known_key_does_not_drift() {
        let key = ExitKey {
            address: address(1),
            routing: HopRouting::try_from(1).unwrap(),
        };

        let discriminator = key.discriminator();

        assert_eq!(4, discriminator.len());
        assert!(
            discriminator
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        );
        assert_eq!("yqak", discriminator);
    }

    /// The pinned id is taken, so the key itself has to carry the entry.
    #[test]
    fn the_key_itself_is_the_last_resort_connect_id() {
        let addr = address(1);
        let key = ExitKey {
            address: addr,
            routing: default_path(),
        };
        let mut destinations = Destinations::default();
        destinations.insert(configured(&format!("berlin-{}", key.discriminator()), address(3)));

        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "Berlin".to_string());
        destinations.merge_discovered(&HashMap::from([(addr, info)]), defaults());

        assert_eq!(addr, destinations.by_connect_id(&key.to_string()).unwrap().address);
    }

    /// The key's form is a legal configured id too, so the fallback has to keep going past it.
    #[test]
    fn a_configured_id_spelling_the_key_pushes_the_fallback_to_a_numbered_one() {
        let addr = address(1);
        let key = ExitKey {
            address: addr,
            routing: default_path(),
        };
        let mut destinations = Destinations::default();
        destinations.insert(configured(&format!("berlin-{}", key.discriminator()), address(3)));
        destinations.insert(configured(&key.to_string(), address(4)));

        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "Berlin".to_string());
        destinations.merge_discovered(&HashMap::from([(addr, info)]), defaults());

        assert_eq!(addr, destinations.by_connect_id(&format!("{key}-2")).unwrap().address);
    }

    /// A restarted worker holds a discovered exit's id before discovery has run again.
    #[test]
    fn a_discovered_id_resolves_only_once_discovery_has_published_it() {
        let addr = address(1);
        let mut destinations = Destinations::default();
        let id = pinned_id(addr, "berlin");
        assert!(destinations.by_connect_id(&id).is_none());

        destinations.merge_discovered(&HashMap::from([(addr, named_exit(addr, "Berlin"))]), defaults());

        assert_eq!(addr, destinations.by_connect_id(&id).unwrap().address);
    }

    #[test]
    fn an_exit_key_survives_a_round_trip_through_its_string_form() {
        let key = ExitKey {
            address: address(1),
            routing: HopRouting::try_from(3).unwrap(),
        };

        assert_eq!(Ok(key), key.to_string().parse::<ExitKey>());
    }

    /// Daemon and ctl ship as one version, so this pins the socket shape against accidental drift.
    #[test]
    fn destination_serializes_to_the_expected_json_shape() {
        let dest = configured("dest-1", address(1));

        let json = serde_json::to_string(&dest).unwrap();

        assert_eq!(
            json,
            r#"{"id":"dest-1","meta":{"name":null,"location":null,"flag":null,"description":null,"other":{}},"address":"0x0101010101010101010101010101010101010101","routing":1,"gnosis_vpn_server":"172.30.0.1:8000","wireguard_server":"172.30.0.1:51820","source":"Configured","overrides":{"configured_meta":{},"configured_gnosis_vpn_server":null,"configured_wireguard_server":null}}"#
        );
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
        let mut destinations = Destinations::default();
        destinations.insert(configured("dest-1", addr));
        let live = destinations.by_connect_id("dest-1").unwrap().clone();

        let mut info = exit_node(addr);
        info.meta.insert("name".to_string(), "Frankfurt-1".to_string());
        let mut discovered = HashMap::new();
        discovered.insert(addr, info);

        destinations.merge_discovered(&discovered, defaults());

        let refreshed = destinations.by_connect_id("dest-1").unwrap();
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
