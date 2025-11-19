/// This module keeps track of a destination's health indicating wether a connection can be
/// successful.
/// **last_error** and **health** are dynamic values depending on connected hopr peers and attempted
/// connections.
/// The **need** field indicates what is required to make the destination healthy in general.
use rand::seq::IteratorRandom;
use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use std::fmt::{self, Display};

use crate::connection::destination::{Address, Destination, NodeId, RoutingOptions};
use crate::log_output;
use crate::peer::Peer;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestinationHealth {
    pub last_error: Option<String>,
    pub health: Health,
    pub need: Need,
}

/// Requirements to be able to connect to this destination
/// This is statically derived at construction time from a destination's routing options.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Need {
    Channel(Address),
    AnyChannel,
    Peering(Address),
    Nothing,
}

/// Potential problems or final health states of a destination
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Health {
    ReadyToConnect(Peer),
    MissingPeeredFundedChannel,
    MissingPeeredChannel,
    MissingFundedChannel(Peer),
    NotPeered,
    // final - not allowed to connect to this destination
    NotAllowed,
    // final - destination address is invalid - should be impossible due to config deserialization
    InvalidAddress,
    // final - destination path is invalid - should be impossible due to config deserialization
    InvalidPath,
}

// Determine if any destination needs peers
pub fn needs_peers(dest_healths: &[&DestinationHealth]) -> bool {
    dest_healths
        .iter()
        .any(|v| matches!(v.need, Need::Channel(_) | Need::Peering(_) | Need::AnyChannel))
}

pub fn count_distinct_channels(dest_healths: &[&DestinationHealth]) -> usize {
    let mut addresses = HashSet::new();
    for dh in dest_healths {
        if let Need::Channel(addr) = dh.need {
            addresses.insert(addr);
        }
    }
    let count = addresses.len();
    if count == 0 && dest_healths.iter().any(|h| matches!(h.need, Need::AnyChannel)) {
        return 1;
    }
    count
}

impl DestinationHealth {
    pub fn from_destination(dest: &Destination, allow_insecure: bool) -> Self {
        match dest.routing.clone() {
            RoutingOptions::Hops(hops) if Into::<u8>::into(hops) == 0 => {
                if allow_insecure {
                    Self {
                        last_error: None,
                        health: Health::NotPeered,
                        need: Need::Peering(dest.address),
                    }
                } else {
                    Self {
                        last_error: None,
                        health: Health::NotAllowed,
                        need: Need::Nothing,
                    }
                }
            }
            RoutingOptions::Hops(_) => Self {
                last_error: None,
                health: Health::MissingPeeredFundedChannel,
                need: Need::AnyChannel,
            },
            RoutingOptions::IntermediatePath(nodes) => match nodes.into_iter().next() {
                Some(first) => match first {
                    NodeId::Chain(address) => Self {
                        last_error: None,
                        health: Health::MissingPeeredFundedChannel,
                        need: Need::Channel(address),
                    },
                    NodeId::Offchain(_) => Self {
                        last_error: None,
                        health: Health::InvalidAddress,
                        need: Need::Nothing,
                    },
                },
                None => Self {
                    last_error: None,
                    health: Health::InvalidPath,
                    need: Need::Nothing,
                },
            },
        }
    }

    pub fn with_error(&self, err: String) -> Self {
        Self {
            health: self.health.clone(),
            need: self.need.clone(),
            last_error: Some(err),
        }
    }

    pub fn no_error(&self) -> Self {
        Self {
            health: self.health.clone(),
            need: self.need.clone(),
            last_error: None,
        }
    }

    pub fn peers(&self, peers: &HashMap<Address, Peer>) -> Self {
        let health = match self.need {
            Need::Channel(addr) => {
                if let Some(peer) = peers.get(&addr) {
                    // channel address becomes peered
                    match self.health {
                        Health::MissingPeeredChannel => Health::ReadyToConnect(peer.clone()),
                        Health::MissingPeeredFundedChannel => Health::MissingFundedChannel(peer.clone()),
                        _ => self.health.clone(),
                    }
                } else {
                    // channel address lost its peer
                    match self.health {
                        Health::ReadyToConnect(_) => Health::MissingPeeredChannel,
                        Health::MissingFundedChannel(_) => Health::MissingPeeredFundedChannel,
                        _ => self.health.clone(),
                    }
                }
            }
            Need::Peering(addr) => {
                if let Some(peer) = peers.get(&addr) {
                    // desired peer address becomes peered
                    Health::ReadyToConnect(peer.clone())
                } else {
                    // peered address lost its peer
                    Health::NotPeered
                }
            }
            Need::AnyChannel => {
                let mut rng = rand::rng();
                if let Some((_, peer)) = peers.iter().choose(&mut rng) {
                    // any peer will suffice for any channel need
                    match self.health {
                        Health::MissingPeeredChannel => Health::ReadyToConnect(peer.clone()),
                        Health::MissingPeeredFundedChannel => Health::MissingFundedChannel(peer.clone()),
                        _ => self.health.clone(),
                    }
                } else {
                    // no peer available, even any channel lost its peer
                    match self.health {
                        Health::ReadyToConnect(_) => Health::MissingPeeredChannel,
                        Health::MissingFundedChannel(_) => Health::MissingPeeredFundedChannel,
                        _ => self.health.clone(),
                    }
                }
            }

            Need::Nothing => self.health.clone(),
        };
        Self {
            health,
            need: self.need.clone(),
            last_error: self.last_error.clone(),
        }
    }

    pub fn channel_funded(&self, address: Address) -> Self {
        let health = match self.need {
            // needed channel becomes funded
            Need::Channel(addr) if addr == address => match self.health {
                Health::MissingFundedChannel(ref peer) => Health::ReadyToConnect(peer.clone()),
                Health::MissingPeeredFundedChannel => Health::MissingPeeredChannel,
                _ => self.health.clone(),
            },
            // any channel becomes funded
            Need::AnyChannel => match self.health {
                Health::MissingFundedChannel(ref peer) => Health::ReadyToConnect(peer.clone()),
                Health::MissingPeeredFundedChannel => Health::MissingPeeredChannel,
                _ => self.health.clone(),
            },
            _ => self.health.clone(),
        };
        Self {
            health,
            need: self.need.clone(),
            last_error: self.last_error.clone(),
        }
    }

    pub fn needs_peer(&self) -> bool {
        match self.need {
            Need::Channel(_) | Need::AnyChannel => matches!(
                self.health,
                Health::MissingPeeredChannel | Health::MissingPeeredFundedChannel | Health::NotPeered
            ),
            Need::Peering(_) => matches!(self.health, Health::NotPeered),
            Need::Nothing => false,
        }
    }

    pub fn needs_channel_funding(&self) -> Option<Address> {
        match self.need {
            Need::Channel(addr) => match self.health {
                Health::MissingFundedChannel(_) | Health::MissingPeeredFundedChannel => Some(addr),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn is_ready_to_connect(&self) -> Option<Peer> {
        match self.health {
            Health::ReadyToConnect(ref peer) => Some(peer.clone()),
            _ => None,
        }
    }

    pub fn is_unrecoverable(&self) -> bool {
        matches!(
            self.health,
            Health::NotAllowed | Health::InvalidAddress | Health::InvalidPath
        )
    }
}

impl Display for DestinationHealth {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let error = if let Some(err) = self.last_error.as_ref() {
            format!("Last error: {}, ", err)
        } else {
            String::new()
        };
        let need = if matches!(self.health, Health::ReadyToConnect(_)) {
            String::new()
        } else {
            format!(", {}", self.need)
        };
        write!(f, "{error}{health:?}{need}", health = self.health)
    }
}

impl Display for Need {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Need::Channel(addr) => write!(f, "needs channel to {}", log_output::address(addr)),
            Need::AnyChannel => write!(f, "needs any channel"),
            Need::Peering(addr) => write!(f, "needs to see peer {}", log_output::address(addr)),
            Need::Nothing => write!(f, "unable to connect"),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_count_distinct_channels() -> anyhow::Result<()> {
        use super::*;

        let addr_1 = "5aaeb6053f3e94c9b9a09f33669435e7ef1beaed".parse()?;
        let addr_2 = "fb6916095ca1df60bb79ce92ce3ea74c37c5d359".parse()?;

        let dh1 = DestinationHealth {
            last_error: None,
            health: Health::MissingPeeredFundedChannel,
            need: Need::Channel(addr_1),
        };
        let dh2 = DestinationHealth {
            last_error: None,
            health: Health::MissingPeeredFundedChannel,
            need: Need::Channel(addr_2),
        };
        let dh3 = DestinationHealth {
            last_error: None,
            health: Health::MissingPeeredFundedChannel,
            need: Need::Channel(addr_1),
        };
        let dh4 = DestinationHealth {
            last_error: None,
            health: Health::MissingPeeredFundedChannel,
            need: Need::AnyChannel,
        };
        let dh5 = DestinationHealth {
            last_error: None,
            health: Health::MissingPeeredFundedChannel,
            need: Need::Peering(addr_1),
        };

        let dest_healths = vec![&dh1, &dh2, &dh3, &dh4, &dh5];
        assert_eq!(count_distinct_channels(&dest_healths), 2);

        let dest_healths_any = vec![&dh4, &dh5];
        assert_eq!(count_distinct_channels(&dest_healths_any), 1);

        let dest_healths_mixed = vec![&dh5];
        assert_eq!(count_distinct_channels(&dest_healths_mixed), 0);
        Ok(())
    }
}
