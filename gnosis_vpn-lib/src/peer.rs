use std::net::Ipv4Addr;

use crate::connection::destination::Address;

#[derive(Debug, Clone)]
pub struct Peer {
    pub address: Address,
    pub ipv4: Ipv4Addr,
}

impl Peer {
    pub fn new(address: Address, ipv4: Ipv4Addr) -> Self {
        Self { address, ipv4 }
    }
}
