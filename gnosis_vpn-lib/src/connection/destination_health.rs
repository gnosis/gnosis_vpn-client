use serde::{Deserialize, Serialize};

use std::collections::HashSet;
use std::fmt::{self, Display};

use crate::connection::destination::{Address, Destination, NodeId, RoutingOptions};
use crate::log_output;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestinationHealth {
    pub last_error: Option<String>,
    pub health: Health,
    need: Need,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Need {
    Channel(Address),
    AnyChannel,
    Peering(Address),
    Nothing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Health {
    ReadyToConnect,
    MissingPeeredFundedChannel,
    MissingPeeredChannel,
    MissingFundedChannel,
    NotPeered,
    NotAllowed,
    InvalidAddress,
    InvalidPath,
}

pub fn needs_peers(dest_healths: &[&DestinationHealth]) -> bool {
    for dh in dest_healths {
        match dh.need {
            Need::Channel(_) | Need::Peering(_) | Need::AnyChannel => return true,
            Need::Nothing => (),
        }
    }
    false
}

pub fn count_distinct_channels(dest_healths: &[&DestinationHealth]) -> usize {
    let mut addresses = HashSet::new();
    for dh in dest_healths {
        if let Need::Channel(addr) = dh.need {
            addresses.insert(addr);
        }
    }
    let count = addresses.len();
    if count == 0
        && dest_healths.iter().any(|h| matches!(h.need, Need::AnyChannel)) {
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
            RoutingOptions::Hops(_) => {
                Self {
                    last_error: None,
                    health: Health::MissingPeeredFundedChannel,
                    need: Need::AnyChannel,
                }
            }
            RoutingOptions::IntermediatePath(nodes) => match nodes.into_iter().next() {
                Some(first) => match first {
                    NodeId::Chain(address) => Self {
                        last_error: None,
                        health: Health::MissingPeeredFundedChannel,
                        need: Need::Channel(address),
                    },
                    NodeId::Offchain(_) => {
                        Self {
                            last_error: None,
                            health: Health::InvalidAddress,
                            need: Need::Nothing,
                        }
                    }
                },
                None => {
                    Self {
                        last_error: None,
                        health: Health::InvalidPath,
                        need: Need::Nothing,
                    }
                }
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

    pub fn peers(&self, addresses: &HashSet<Address>) -> Self {
        let health = match self.need {
            // channel address becomes peered
            Need::Channel(addr) if addresses.contains(&addr) => match self.health {
                Health::MissingPeeredChannel => Health::ReadyToConnect,
                Health::MissingPeeredFundedChannel => Health::MissingFundedChannel,
                _ => self.health.clone(),
            },
            // channel address lost its peer
            Need::Channel(_) => match self.health {
                Health::ReadyToConnect => Health::MissingPeeredChannel,
                Health::MissingFundedChannel => Health::MissingPeeredFundedChannel,
                _ => self.health.clone(),
            },
            // desired peer address becomes peered
            Need::Peering(addr) if addresses.contains(&addr) => Health::ReadyToConnect,
            // peered address lost its peer
            Need::Peering(_) => Health::NotPeered,
            // no peer available, even any channel lost its peer
            Need::AnyChannel if addresses.is_empty() => match self.health {
                Health::ReadyToConnect => Health::MissingPeeredChannel,
                Health::MissingFundedChannel => Health::MissingPeeredFundedChannel,
                _ => self.health.clone(),
            },
            // any peer will suffice for any channel need
            Need::AnyChannel => match self.health {
                Health::MissingPeeredChannel => Health::ReadyToConnect,
                Health::MissingPeeredFundedChannel => Health::MissingFundedChannel,
                _ => self.health.clone(),
            },
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
                Health::MissingFundedChannel => Health::ReadyToConnect,
                Health::MissingPeeredFundedChannel => Health::MissingPeeredChannel,
                _ => self.health.clone(),
            },
            // any channel becomes funded
            Need::AnyChannel => match self.health {
                Health::MissingFundedChannel => Health::ReadyToConnect,
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

    pub fn needs_channel_funding(&self) -> Option<Address> {
        match self.need {
            Need::Channel(addr) => match self.health {
                Health::MissingFundedChannel | Health::MissingPeeredFundedChannel => Some(addr),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn is_ready_to_connect(&self) -> bool {
        matches!(self.health, Health::ReadyToConnect)
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
        write!(
            f,
            "{error}{health:?},{need}",
            health = self.health,
            need = self.need
        )
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
