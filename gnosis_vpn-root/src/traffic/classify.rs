//! Turns captured link-layer frames into direction-tagged, IP-sized observations.
//!
//! Only the IP headers are inspected. Payload bytes are never copied, decoded or
//! retained: everything downstream of [`observe`] sees an address, a direction and
//! a length.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use gnosis_vpn_lib::traffic::{Class, Delta, Direction};

/// Ethernet, plus up to two stacked VLAN tags (802.1Q / 802.1ad).
const ETHERNET_HEADER_LEN: usize = 14;
const VLAN_TAG_LEN: usize = 4;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_IPV6: u16 = 0x86dd;
const ETHERTYPE_VLAN: [u16; 3] = [0x8100, 0x88a8, 0x9100];
const IPV4_HEADER_LEN: usize = 20;
const IPV6_HEADER_LEN: usize = 40;

/// A packet that involves the captured interface exactly once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    /// The remote endpoint. Private to the model; never rendered.
    pub target: IpAddr,
    pub direction: Direction,
    /// Complete IP-layer size: IP header plus IP payload.
    pub length: u32,
}

/// Classifies one captured frame, or returns `None` when it is not IP traffic
/// involving exactly one local endpoint.
pub fn observe(frame: &[u8], local: &HashSet<IpAddr>) -> Option<Observation> {
    let ip = ip_layer(frame)?;
    let (source, destination, length) = match ip[0] >> 4 {
        4 => ipv4_endpoints(ip)?,
        6 => ipv6_endpoints(ip)?,
        _ => return None,
    };

    let source_is_local = local.contains(&source);
    if source_is_local == local.contains(&destination) {
        return None;
    }

    let (target, direction) = if source_is_local {
        (destination, Direction::Upstream)
    } else {
        (source, Direction::Downstream)
    };
    if is_unusable_target(target) {
        return None;
    }

    Some(Observation {
        target,
        direction,
        length,
    })
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

fn is_unusable_target(target: IpAddr) -> bool {
    match target {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_multicast() || v4.is_unspecified() || v4 == Ipv4Addr::BROADCAST,
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_multicast() || v6.is_unspecified(),
    }
}

/// Per-second aggregates keyed by target, class and direction.
#[derive(Default)]
pub struct Aggregates {
    entries: HashMap<(u64, IpAddr, Class, Direction), Aggregate>,
}

#[derive(Default)]
struct Aggregate {
    packets: u32,
    bytes: u64,
    largest_packet: u32,
}

impl Aggregates {
    pub fn record(&mut self, second: u64, observation: Observation, class: Class) {
        let entry = self
            .entries
            .entry((second, observation.target, class, observation.direction))
            .or_default();
        entry.packets = entry.packets.saturating_add(1);
        entry.bytes = entry.bytes.saturating_add(u64::from(observation.length));
        entry.largest_packet = entry.largest_packet.max(observation.length);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drains everything collected since the last flush.
    pub fn take(&mut self) -> Vec<Delta> {
        self.entries
            .drain()
            .map(|((second, target, class, direction), aggregate)| Delta {
                second,
                target,
                class,
                direction,
                packets: aggregate.packets,
                bytes: aggregate.bytes,
                largest_packet: aggregate.largest_packet,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCAL_V4: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 20);

    fn local() -> HashSet<IpAddr> {
        [IpAddr::V4(LOCAL_V4)].into_iter().collect()
    }

    fn ethernet(ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; 12];
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

    #[test]
    fn ipv6_uses_payload_length_plus_fixed_header() {
        let remote: Ipv6Addr = "2001:db8::7".parse().expect("ip");
        // version/traffic class/flow label, payload length, next header, hop limit
        let mut header = vec![0x60u8, 0, 0, 0];
        header.extend_from_slice(&64u16.to_be_bytes());
        header.push(17);
        header.push(64);
        header.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        header.extend_from_slice(&remote.octets());
        let frame = ethernet(ETHERTYPE_IPV6, &header);
        let mut local = local();
        local.insert(IpAddr::V6(Ipv6Addr::LOCALHOST));
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
    fn aggregates_combine_packets_of_one_second_and_direction() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let mut aggregates = Aggregates::default();
        for len in [100u16, 300, 200] {
            let frame = ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, remote, len));
            let observation = observe(&frame, &local()).expect("observed");
            aggregates.record(4, observation, Class::Other);
        }
        let deltas = aggregates.take();
        assert_eq!(deltas.len(), 1);
        let delta = &deltas[0];
        assert_eq!(delta.second, 4);
        assert_eq!(delta.packets, 3);
        assert_eq!(delta.bytes, (120 + 320 + 220) as u64);
        assert_eq!(delta.largest_packet, 320);
        assert!(aggregates.is_empty());
    }

    #[test]
    fn aggregates_keep_classes_and_directions_apart() {
        let remote = Ipv4Addr::new(203, 0, 113, 9);
        let mut aggregates = Aggregates::default();
        let out = observe(&ethernet(ETHERTYPE_IPV4, &ipv4(LOCAL_V4, remote, 10)), &local()).expect("observed");
        let back = observe(&ethernet(ETHERTYPE_IPV4, &ipv4(remote, LOCAL_V4, 10)), &local()).expect("observed");
        aggregates.record(1, out, Class::HoprAssociated);
        aggregates.record(1, back, Class::HoprAssociated);
        aggregates.record(1, out, Class::Other);
        assert_eq!(aggregates.take().len(), 3);
    }
}
