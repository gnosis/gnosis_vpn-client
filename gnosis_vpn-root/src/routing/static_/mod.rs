use std::net::Ipv4Addr;

use gnosis_vpn_lib::event;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[derive(Debug)]
pub struct Static {
    wg_data: event::WireGuardData,
    peer_ips: Vec<Ipv4Addr>,
}

impl Static {
    pub fn new(wg_data: event::WireGuardData, peer_ips: Vec<Ipv4Addr>) -> Self {
        Self { wg_data, peer_ips }
    }
}
