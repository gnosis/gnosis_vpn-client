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
    ChannelFunding,
    /// Usually switches to this need, once channel funding is established
    ChannelPeering,
    Peering,
    AnyPeer,
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
            Need::ChannelFunding | Need::ChannelPeering | Need::Peering | Need::AnyPeer => return true,
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
                        need: Need::Peering,
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
                    NodeId::Chain(_) => Self {
                        last_error: None,
                        health: Health::MissingChannel,
                        need: Need::ChannelFunding,
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

    pub fn channel_funded(&self) -> Self {
        Self {
            health: Health::ReadyToConnect,
            need: Need::ChannelPeering,
            last_error: self.last_error.clone(),
        }
    }
}
