use std::net::{Ipv4Addr, Ipv6Addr};

use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};

/// RFC-1918 private ranges + link-local + IPv6 ULA/link-local.
/// Traffic to/from these networks is allowed when lan_lockdown is false.
/// Mirrors Mullvad's ALLOWED_LAN_NETS.
pub(super) const LAN_NETS: [IpNetwork; 6] = [
    IpNetwork::V4(Ipv4Network::new_checked(Ipv4Addr::new(10, 0, 0, 0), 8).unwrap()),
    IpNetwork::V4(Ipv4Network::new_checked(Ipv4Addr::new(172, 16, 0, 0), 12).unwrap()),
    IpNetwork::V4(Ipv4Network::new_checked(Ipv4Addr::new(192, 168, 0, 0), 16).unwrap()),
    IpNetwork::V4(Ipv4Network::new_checked(Ipv4Addr::new(169, 254, 0, 0), 16).unwrap()),
    IpNetwork::V6(Ipv6Network::new_checked(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10).unwrap()),
    IpNetwork::V6(Ipv6Network::new_checked(Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7).unwrap()),
];

/// Multicast/broadcast ranges allowed alongside LAN traffic.
/// Mirrors Mullvad's ALLOWED_LAN_MULTICAST_NETS.
pub(super) const LAN_MULTICAST_NETS: [IpNetwork; 8] = [
    IpNetwork::V4(Ipv4Network::new_checked(Ipv4Addr::new(255, 255, 255, 255), 32).unwrap()),
    IpNetwork::V4(Ipv4Network::new_checked(Ipv4Addr::new(224, 0, 0, 0), 24).unwrap()),
    IpNetwork::V4(Ipv4Network::new_checked(Ipv4Addr::new(239, 0, 0, 0), 8).unwrap()),
    IpNetwork::V6(Ipv6Network::new_checked(Ipv6Addr::new(0xff01, 0, 0, 0, 0, 0, 0, 0), 16).unwrap()),
    IpNetwork::V6(Ipv6Network::new_checked(Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 0, 0), 16).unwrap()),
    IpNetwork::V6(Ipv6Network::new_checked(Ipv6Addr::new(0xff03, 0, 0, 0, 0, 0, 0, 0), 16).unwrap()),
    IpNetwork::V6(Ipv6Network::new_checked(Ipv6Addr::new(0xff04, 0, 0, 0, 0, 0, 0, 0), 16).unwrap()),
    IpNetwork::V6(Ipv6Network::new_checked(Ipv6Addr::new(0xff05, 0, 0, 0, 0, 0, 0, 0), 16).unwrap()),
];

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Error, Firewall};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{Error, Firewall};
