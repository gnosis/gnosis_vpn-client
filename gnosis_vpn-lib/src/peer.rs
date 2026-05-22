use serde::{Deserialize, Serialize};

use std::net::Ipv4Addr;

use crate::connection::destination::Address;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub address: Address,
    pub ips: Vec<Ipv4Addr>,
}

impl Peer {
    pub fn new(address: Address, ips: Vec<Ipv4Addr>) -> Self {
        Self { address, ips }
    }
}
