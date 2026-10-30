//! Turns frames into observations from link-layer and IP headers only; payload bytes are never read.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use gnosis_vpn_lib::traffic::{Delta, Direction};

/// Ethernet, plus up to two stacked VLAN tags (802.1Q / 802.1ad).
const ETHERNET_HEADER_LEN: usize = 14;
const VLAN_TAG_LEN: usize = 4;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86dd;
const ETHERTYPE_VLAN: [u16; 3] = [0x8100, 0x88a8, 0x9100];
const IPV4_HEADER_LEN: usize = 20;
const IPV6_HEADER_LEN: usize = 40;

/// Where a capture observes traffic relative to a VPN.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    /// Ethernet frames; the remote is the next hop, such as a VPN server or HOPR peer.
    #[default]
    Wire,
    /// Bare IP packets; the remote is the destination sent into the VPN.
    Tunnel,
}

/// What identifies the capturing host on its interface.
pub struct Host {
    /// Addresses the interface reports as its own.
    pub addresses: HashSet<IpAddr>,
    /// Decides direction when neither IP endpoint is ours, e.g. CLAT46 or a new IPv6 privacy address.
    pub mac: Option<[u8; 6]>,
    /// Endpoints inside these are LAN peers such as the router, not targets.
    pub networks: Vec<(IpAddr, u8)>,
    /// Decides how frames are laid out.
    pub side: Side,
}

/// A packet that involves the captured interface exactly once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    /// The remote endpoint. Published to the consumer; never rendered as text.
    pub target: IpAddr,
    pub direction: Direction,
    /// Complete IP-layer size: IP header plus IP payload.
    pub length: u32,
}

/// `None` unless the frame is IP traffic with exactly one local endpoint.
pub fn observe(frame: &[u8], host: &Host) -> Option<Observation> {
    let ip = match host.side {
        Side::Wire => ip_layer(frame)?,
        // A tunnel device hands over the IP packet itself.
        Side::Tunnel => frame,
    };
    if ip.is_empty() {
        return None;
    }
    let (source, destination, length) = match ip[0] >> 4 {
        4 => ipv4_endpoints(ip)?,
        6 => ipv6_endpoints(ip)?,
        _ => return None,
    };

    let source_is_local = match (host.addresses.contains(&source), host.addresses.contains(&destination)) {
        (true, false) => true,
        (false, true) => false,
        (true, true) => return None,
        (false, false) => sent_by_host(frame, host.mac?)?,
    };

    let (target, direction) = if source_is_local {
        (destination, Direction::Upstream)
    } else {
        (source, Direction::Downstream)
    };
    let is_lan_peer = host.networks.iter().any(|network| within(*network, target));
    if is_unusable_target(target) || is_lan_peer {
        return None;
    }

    Some(Observation {
        target,
        direction,
        length,
    })
}

/// `Some(true)` when the host sent the frame, `Some(false)` when it was addressed to the host.
fn sent_by_host(frame: &[u8], mac: [u8; 6]) -> Option<bool> {
    let destination = frame.get(0..6)?;
    let source = frame.get(6..12)?;
    match (source == mac, destination == mac) {
        (true, false) => Some(true),
        (false, true) => Some(false),
        _ => None,
    }
}

fn ip_layer(frame: &[u8]) -> Option<&[u8]> {
    if frame.len() < ETHERNET_HEADER_LEN {
        return None;
    }
    let mut offset = 12;
    let mut ethertype = be_u16(frame.get(offset..offset + 2)?)?;
    offset += 2;
    while ETHERTYPE_VLAN.contains(&ethertype) {
        // Skip the tag's control information, then read the next ethertype.
        offset += VLAN_TAG_LEN / 2;
        ethertype = be_u16(frame.get(offset..offset + 2)?)?;
        offset += 2;
    }
    match ethertype {
        ETHERTYPE_IPV4 | ETHERTYPE_IPV6 => frame.get(offset..),
        _ => None,
    }
}

fn ipv4_endpoints(ip: &[u8]) -> Option<(IpAddr, IpAddr, u32)> {
    if ip.len() < IPV4_HEADER_LEN {
        return None;
    }
    let total_length = u32::from(be_u16(ip.get(2..4)?)?);
    let source = Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15]);
    let destination = Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19]);
    Some((
        IpAddr::V4(source),
        IpAddr::V4(destination),
        total_length.max(IPV4_HEADER_LEN as u32),
    ))
}

fn ipv6_endpoints(ip: &[u8]) -> Option<(IpAddr, IpAddr, u32)> {
    if ip.len() < IPV6_HEADER_LEN {
        return None;
    }
    let payload_length = u32::from(be_u16(ip.get(4..6)?)?);
    let source: [u8; 16] = ip.get(8..24)?.try_into().ok()?;
    let destination: [u8; 16] = ip.get(24..40)?.try_into().ok()?;
    Some((
        IpAddr::V6(Ipv6Addr::from(source)),
        IpAddr::V6(Ipv6Addr::from(destination)),
        IPV6_HEADER_LEN as u32 + payload_length,
    ))
}

fn be_u16(bytes: &[u8]) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}

/// Private, link-local and shared address space never leaves the local or provider network.
fn is_unusable_target(target: IpAddr) -> bool {
    let is_special = target.is_loopback() || target.is_multicast() || target.is_unspecified();
    let is_local_scope = match target {
        IpAddr::V4(v4) => {
            let is_shared = v4.octets()[0] == 100 && v4.octets()[1] & 0xc0 == 64;
            v4 == Ipv4Addr::BROADCAST || v4.is_private() || v4.is_link_local() || is_shared
        }
        IpAddr::V6(v6) => {
            let is_link_local = v6.segments()[0] & 0xffc0 == 0xfe80;
            let is_unique_local = v6.segments()[0] & 0xfe00 == 0xfc00;
            is_link_local || is_unique_local
        }
    };
    is_special || is_local_scope
}

/// Whether `address` lies inside the network given as address and prefix length.
fn within((network, prefix): (IpAddr, u8), address: IpAddr) -> bool {
    let (network, address, bits) = match (network, address) {
        (IpAddr::V4(network), IpAddr::V4(address)) => {
            (u128::from(u32::from(network)), u128::from(u32::from(address)), 32)
        }
        (IpAddr::V6(network), IpAddr::V6(address)) => (u128::from(network), u128::from(address), 128),
        _ => return false,
    };
    let host_bits = bits - u32::from(prefix).min(bits);
    network.checked_shr(host_bits).unwrap_or(0) == address.checked_shr(host_bits).unwrap_or(0)
}

/// Per-second byte totals keyed by target and direction.
#[derive(Default)]
pub struct Aggregates {
    entries: HashMap<(u64, IpAddr, Direction), u64>,
    side: Side,
}

impl Aggregates {
    pub fn new(side: Side) -> Self {
        Self {
            entries: HashMap::new(),
            side,
        }
    }

    pub fn record(&mut self, second: u64, observation: Observation) {
        let entry = self
            .entries
            .entry((second, observation.target, observation.direction))
            .or_insert(0);
        *entry = entry.saturating_add(u64::from(observation.length));
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drains everything collected since the last flush.
    pub fn take(&mut self) -> Vec<Delta> {
        let tunnel = self.side == Side::Tunnel;
        self.entries
            .drain()
            .map(|((second, target, direction), bytes)| Delta {
                second,
                target,
                direction,
                bytes,
                // The session marks peers; it holds the announced addresses.
                hopr_peer: false,
                tunnel,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL_V4: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

    const LOCAL_MAC: [u8; 6] = [0x02, 0x00, 0x5e, 0x10, 0x00, 0x01];
    const ROUTER_MAC: [u8; 6] = [0x02, 0x00, 0x5e, 0x10, 0x00, 0xfe];
    /// A CLAT46-style source the interface does not report as its own.
    const UNREPORTED_V4: Ipv4Addr = Ipv4Addr::new(192, 0, 0, 9);

    fn local() -> Host {
        Host {
            addresses: [IpAddr::V4(LOCAL_V4)].into_iter().collect(),
            mac: Some(LOCAL_MAC),
            networks: vec![(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 0)), 24)],
            side: Side::Wire,
        }
    }

    const TUNNEL_V4: Ipv4Addr = Ipv4Addr::new(10, 2, 0, 2);

    /// A VPN tunnel device: no link-layer address, a host route of its own.
    fn tunnel() -> Host {
        Host {
            addresses: [IpAddr::V4(TUNNEL_V4)].into_iter().collect(),
            mac: None,
            networks: vec![(IpAddr::V4(TUNNEL_V4), 32)],
            side: Side::Tunnel,
        }
    }

    fn ethernet(ethertype: u16, payload: &[u8]) -> Vec<u8> {
        ethernet_link([0; 6], [0; 6], ethertype, payload)
    }

    fn ethernet_link(destination: [u8; 6], source: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = destination.to_vec();
        frame.extend_from_slice(&source);
        frame.extend_from_slice(&ethertype.to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    fn ipv4(source: Ipv4Addr, destination: Ipv4Addr, payload_len: u16) -> Vec<u8> {
        let total = IPV4_HEADER_LEN as u16 + payload_len;
        // version/IHL, DSCP, total length, id, flags, TTL, protocol, checksum
        let mut header = vec![0x45, 0, 0, 0, 0, 0, 0x40, 64, 17, 0, 0, 0];
        header[2..4].copy_from_slice(&total.to_be_bytes());
        header.extend_from_slice(&source.octets());
        header.extend_from_slice(&destination.octets());
        header.resize(total as usize, 0);
        header
    }

    #[test]
    fn outgoing_packet_is_upstream_with_ip_layer_size() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let frame = ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, remote, 100));
        let observation = observe(&frame, &local()).expect("observed");
        assert_eq!(observation.target, IpAddr::V4(remote));
        assert_eq!(observation.direction, Direction::Upstream);
        assert_eq!(observation.length, 120);
    }

    #[test]
    fn incoming_packet_is_downstream() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let frame = ethernet(ETHERTYPE_IPV4, &ipv4(remote, LOCAL_V4, 8));
        let observation = observe(&frame, &local()).expect("observed");
        assert_eq!(observation.target, IpAddr::V4(remote));
        assert_eq!(observation.direction, Direction::Downstream);
        assert_eq!(observation.length, 28);
    }

    #[test]
    fn packets_with_no_local_endpoint_are_ignored() {
        let frame = ethernet(
            ETHERTYPE_IPV4,
            &ipv4(Ipv4Addr::new(203, 0, 113, 9), Ipv4Addr::new(198, 51, 100, 7), 8),
        );
        assert_eq!(observe(&frame, &local()), None);
    }

    #[test]
    fn packets_with_both_endpoints_local_are_ignored() {
        let frame = ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, LOCAL_V4, 8));
        assert_eq!(observe(&frame, &local()), None);
    }

    #[test]
    fn unreported_local_address_sent_from_the_interface_is_upstream() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let frame = ethernet_link(ROUTER_MAC, LOCAL_MAC, ETHERTYPE_IPV4, &ipv4(UNREPORTED_V4, remote, 8));
        let observation = observe(&frame, &local()).expect("observed");
        assert_eq!(observation.target, IpAddr::V4(remote));
        assert_eq!(observation.direction, Direction::Upstream);
        assert_eq!(observation.length, 28);
    }

    #[test]
    fn unreported_local_address_addressed_to_the_interface_is_downstream() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let frame = ethernet_link(LOCAL_MAC, ROUTER_MAC, ETHERTYPE_IPV4, &ipv4(remote, UNREPORTED_V4, 8));
        let observation = observe(&frame, &local()).expect("observed");
        assert_eq!(observation.target, IpAddr::V4(remote));
        assert_eq!(observation.direction, Direction::Downstream);
    }

    #[test]
    fn unreported_addresses_are_ignored_without_a_link_address() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let frame = ethernet_link(ROUTER_MAC, LOCAL_MAC, ETHERTYPE_IPV4, &ipv4(UNREPORTED_V4, remote, 8));
        let host = Host { mac: None, ..local() };
        assert_eq!(observe(&frame, &host), None);
    }

    #[test]
    fn local_addresses_decide_before_the_link_layer() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let frame = ethernet_link(LOCAL_MAC, ROUTER_MAC, ETHERTYPE_IPV4, &ipv4(LOCAL_V4, remote, 8));
        let observation = observe(&frame, &local()).expect("observed");
        assert_eq!(observation.direction, Direction::Upstream);
    }

    #[test]
    fn non_ip_frames_are_ignored() {
        let frame = ethernet(0x0806, &[0u8; 28]);
        assert_eq!(observe(&frame, &local()), None);
        assert_eq!(observe(&[0u8; 8], &local()), None);
    }

    #[test]
    fn vlan_tags_are_skipped() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let mut frame = ethernet(0x8100, &[0x00, 0x64]);
        frame.extend_from_slice(&ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, remote, 8))[12..]);
        let observation = observe(&frame, &local()).expect("observed");
        assert_eq!(observation.target, IpAddr::V4(remote));
    }

    fn ipv6(source: Ipv6Addr, destination: Ipv6Addr, payload_len: u16) -> Vec<u8> {
        // version/traffic class/flow label, payload length, next header, hop limit
        let mut header = vec![0x60u8, 0, 0, 0];
        header.extend_from_slice(&payload_len.to_be_bytes());
        header.push(17);
        header.push(64);
        header.extend_from_slice(&source.octets());
        header.extend_from_slice(&destination.octets());
        header
    }

    #[test]
    fn ipv6_uses_payload_length_plus_fixed_header() {
        let remote: Ipv6Addr = "2001:db8::7".parse().expect("ip");
        let frame = ethernet(ETHERTYPE_IPV6, &ipv6(Ipv6Addr::LOCALHOST, remote, 64));
        let mut local = local();
        local.addresses.insert(IpAddr::V6(Ipv6Addr::LOCALHOST));
        let observation = observe(&frame, &local).expect("observed");
        assert_eq!(observation.target, IpAddr::V6(remote));
        assert_eq!(observation.direction, Direction::Upstream);
        assert_eq!(observation.length, 40 + 64);
    }

    #[test]
    fn unusable_targets_are_rejected() {
        for target in [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::BROADCAST,
            Ipv4Addr::new(224, 0, 0, 1),
            Ipv4Addr::UNSPECIFIED,
        ] {
            let frame = ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, target, 8));
            assert_eq!(observe(&frame, &local()), None, "{target}");
        }
    }

    #[test]
    fn non_public_targets_are_rejected() {
        for target in [
            Ipv4Addr::new(10, 1, 2, 3),
            Ipv4Addr::new(172, 16, 5, 4),
            Ipv4Addr::new(192, 168, 7, 1),
            Ipv4Addr::new(169, 254, 1, 1),
            Ipv4Addr::new(100, 64, 0, 1),
        ] {
            let frame = ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, target, 8));
            assert_eq!(observe(&frame, &local()), None, "{target}");
        }
        let local_v6: Ipv6Addr = "2001:db8:1::20".parse().expect("ip");
        let mut host = local();
        host.addresses.insert(IpAddr::V6(local_v6));
        for target in ["fe80::1", "fd12:3456::1"] {
            let target: Ipv6Addr = target.parse().expect("ip");
            let frame = ethernet(ETHERTYPE_IPV6, &ipv6(local_v6, target, 8));
            assert_eq!(observe(&frame, &host), None, "{target}");
        }
    }

    #[test]
    fn targets_on_the_interface_network_are_rejected() {
        let local_v6: Ipv6Addr = "2001:db8:1::20".parse().expect("ip");
        let mut host = local();
        host.addresses.insert(IpAddr::V6(local_v6));
        host.networks
            .push((IpAddr::V6("2001:db8:1::".parse().expect("ip")), 64));

        let neighbour: Ipv6Addr = "2001:db8:1::99".parse().expect("ip");
        let frame = ethernet(ETHERTYPE_IPV6, &ipv6(local_v6, neighbour, 8));
        assert_eq!(observe(&frame, &host), None);

        let remote: Ipv6Addr = "2001:db8:2::1".parse().expect("ip");
        let frame = ethernet(ETHERTYPE_IPV6, &ipv6(local_v6, remote, 8));
        assert_eq!(observe(&frame, &host).expect("observed").target, IpAddr::V6(remote));
    }

    #[test]
    fn tunnel_frames_are_bare_ip_packets() {
        // Byte layout seen on a macOS utun device: the IP header starts at offset 0.
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let out = observe(&ipv4(TUNNEL_V4, remote, 100), &tunnel()).expect("observed");
        assert_eq!(out.target, IpAddr::V4(remote));
        assert_eq!(out.direction, Direction::Upstream);
        assert_eq!(out.length, 120);

        let back = observe(&ipv4(remote, TUNNEL_V4, 8), &tunnel()).expect("observed");
        assert_eq!(back.direction, Direction::Downstream);
        assert_eq!(back.length, 28);
    }

    #[test]
    fn tunnel_frames_that_are_not_ip_are_ignored() {
        assert_eq!(observe(&[0u8; 40], &tunnel()), None);
        assert_eq!(observe(&[], &tunnel()), None);
        // An Ethernet frame is not a bare IP packet.
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let frame = ethernet(ETHERTYPE_IPV4, &ipv4(TUNNEL_V4, remote, 8));
        assert_eq!(observe(&frame, &tunnel()), None);
    }

    #[test]
    fn wire_frames_are_not_read_as_bare_ip_packets() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        assert_eq!(observe(&ipv4(LOCAL_V4, remote, 8), &local()), None);
    }

    #[test]
    fn aggregates_mark_the_side_they_were_captured_on() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let observation = observe(&ipv4(TUNNEL_V4, remote, 8), &tunnel()).expect("observed");
        let mut on_tunnel = Aggregates::new(Side::Tunnel);
        on_tunnel.record(0, observation);
        assert!(on_tunnel.take().iter().all(|delta| delta.tunnel));

        let mut on_wire = Aggregates::new(Side::Wire);
        on_wire.record(0, observation);
        assert!(on_wire.take().iter().all(|delta| !delta.tunnel));
    }

    #[test]
    fn aggregates_combine_packets_of_one_second_target_and_direction() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let mut aggregates = Aggregates::default();
        for len in [100u16, 300, 200] {
            let frame = ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, remote, len));
            let observation = observe(&frame, &local()).expect("observed");
            aggregates.record(4, observation);
        }
        let deltas = aggregates.take();
        assert_eq!(deltas.len(), 1);
        let delta = &deltas[0];
        assert_eq!(delta.second, 4);
        assert_eq!(delta.target, IpAddr::V4(remote));
        assert_eq!(delta.direction, Direction::Upstream);
        assert_eq!(delta.bytes, (120 + 320 + 220) as u64);
        assert!(aggregates.is_empty());
    }

    #[test]
    fn aggregates_keep_directions_apart() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let mut aggregates = Aggregates::default();
        let out = observe(&ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, remote, 10)), &local()).expect("observed");
        let back = observe(&ethernet(ETHERTYPE_IPV4, &ipv4(remote, LOCAL_V4, 10)), &local()).expect("observed");
        aggregates.record(1, out);
        aggregates.record(1, back);
        aggregates.record(1, out);
        let mut deltas = aggregates.take();
        deltas.sort_unstable_by_key(|delta| delta.direction == Direction::Upstream);
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].direction, Direction::Downstream);
        assert_eq!(deltas[0].bytes, 30);
        assert_eq!(deltas[1].direction, Direction::Upstream);
        assert_eq!(deltas[1].bytes, 60);
    }

    #[test]
    fn aggregates_keep_targets_apart() {
        let first = Ipv4Addr::new(203, 0, 113, 9);
        let second = Ipv4Addr::new(198, 51, 100, 4);
        let mut aggregates = Aggregates::default();
        let to_first = observe(&ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, first, 10)), &local()).expect("observed");
        let to_second = observe(&ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, second, 40)), &local()).expect("observed");
        aggregates.record(1, to_first);
        aggregates.record(1, to_second);
        let mut deltas = aggregates.take();
        deltas.sort_unstable_by_key(|delta| delta.bytes);
        assert_eq!(deltas.len(), 2);
        assert_eq!(deltas[0].target, IpAddr::V4(first));
        assert_eq!(deltas[0].bytes, 30);
        assert_eq!(deltas[1].target, IpAddr::V4(second));
        assert_eq!(deltas[1].bytes, 60);
    }
}
