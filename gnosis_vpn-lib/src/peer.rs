use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;

use crate::connection::destination::Address;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Peer {
    pub address: Address,
    pub ipv4_addrs: Vec<Ipv4Addr>,
}

impl Peer {
    pub fn new(address: Address, ipv4_addrs: Vec<Ipv4Addr>) -> Self {
        Self { address, ipv4_addrs }
    }
}

/// Two independent sources: on-chain announcements drive the killswitch allowlist, live transport connections routability.
#[derive(Debug, Clone)]
pub struct Peers {
    pub announced: HashMap<Address, Peer>,
    pub connected: HashSet<Address>,
}
