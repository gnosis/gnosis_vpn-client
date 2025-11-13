use crate::connection::destination::{Address, Destination, NodeId, RoutingOptions};

#[derive(Debug, Clone)]
pub struct DestinationHealth {
    pub last_error: Option<String>,
    pub health: Health,
    need: Need,
}

#[derive(Clone, Debug)]
pub enum Need {
    /// This implies channel peering as well
    ChannelFunded(Address),
    /// Usually switches to this need, once channel funding is established
    ChannelPeered(Address),
    Peer(Address),
    SomePeers,
    Nothing,
}

#[derive(Clone, Debug)]
pub enum Health {
    ReadyToConnect,
    MissingChannel,
    NotPeered,
    NotAllowed,
    InvalidAddress,
    InvalidPath,
}

pub fn needs_peers(dest_healths: &[&DestinationHealth]) -> bool {
    for dh in dest_healths {
        match dh.need {
            Need::ChannelFunded(_) | Need::ChannelPeered(_) | Need::Peer(_) | Need::SomePeers => return true,
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
                        need: Need::Peer(dest.address),
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
                    health: Health::ReadyToConnect,
                    need: Need::SomePeers,
                };
            }
            RoutingOptions::IntermediatePath(nodes) => match nodes.into_iter().next() {
                Some(first) => match first {
                    NodeId::Chain(address) => Self {
                        last_error: None,
                        health: Health::MissingChannel,
                        need: Need::ChannelFunded(address),
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

    pub fn with_error(&self, err: Option<String>) -> Self {
        Self {
            health: self.health.clone(),
            need: self.need.clone(),
            last_error: err,
        }
    }
}
