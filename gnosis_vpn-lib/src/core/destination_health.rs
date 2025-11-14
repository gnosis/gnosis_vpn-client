use std::collections::HashSet;

use crate::connection::destination::{Address, Destination, NodeId, RoutingOptions};

#[derive(Debug, Clone)]
pub struct DestinationHealth {
    pub last_error: Option<String>,
    pub health: Health,
    need: Need,
}

#[derive(Clone, Debug)]
pub enum Need {
    Channel(Address),
    Peering(Address),
    AnyPeer,
    Nothing,
}

#[derive(Clone, Debug)]
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
            Need::Channel(_) | Need::Peering(_) | Need::AnyPeer => return true,
            Need::Nothing => (),
        }
    }
    false
}

impl DestinationHealth {
    pub fn from_destination(dest: &Destination, allow_insecure: bool) -> Self {
        match dest.routing.clone() {
            RoutingOptions::Hops(hops) if Into::<u8>::into(hops) == 0 => {
                if allow_insecure {
                    return Self {
                        last_error: None,
                        health: Health::NotPeered,
                        need: Need::Peering(dest.address),
                    };
                } else {
                    return Self {
                        last_error: None,
                        health: Health::NotAllowed,
                        need: Need::Nothing,
                    };
                }
            }
            RoutingOptions::Hops(_) => {
                return Self {
                    last_error: None,
                    health: Health::NotPeered,
                    need: Need::AnyPeer,
                };
            }
            RoutingOptions::IntermediatePath(nodes) => match nodes.into_iter().next() {
                Some(first) => match first {
                    NodeId::Chain(address) => Self {
                        last_error: None,
                        health: Health::MissingPeeredFundedChannel,
                        need: Need::Channel(address),
                    },
                    NodeId::Offchain(_) => {
                        return Self {
                            last_error: None,
                            health: Health::InvalidAddress,
                            need: Need::Nothing,
                        };
                    }
                },
                None => {
                    return Self {
                        last_error: None,
                        health: Health::InvalidPath,
                        need: Need::Nothing,
                    };
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

    pub fn peered(&self, addresses: &HashSet<Address>) -> Self {
        let health = match self.need {
            Need::Channel(addr) if addresses.contains(&addr) => match self.health {
                Health::MissingPeeredChannel => Health::ReadyToConnect,
                Health::MissingPeeredFundedChannel => Health::MissingFundedChannel,
                _ => self.health.clone(),
            },
            Need::Peering(addr) if addresses.contains(&addr) => Health::ReadyToConnect,
            Need::AnyPeer => Health::ReadyToConnect,
            _ => self.health.clone(),
        };
        Self {
            health,
            need: self.need.clone(),
            last_error: self.last_error.clone(),
        }
    }

    pub fn channel_funded(&self, addresses: &HashSet<Address>) -> Self {
        let health = match self.need {
            Need::Channel(addr) if addresses.contains(&addr) => match self.health {
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
}
