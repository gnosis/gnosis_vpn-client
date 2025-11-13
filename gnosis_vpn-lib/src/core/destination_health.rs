use crate::connection::destination::{Address, Destination, NodeId, RoutingOptions};
use thiserror::Error;

pub struct DestinationHealth {
    pub last_error: Option<String>,
    pub health: Health,
    needs_channel: Option<Address>,
}

pub enum Health {
    ReadyToConnect,
    NeedsPeeredChannel,
    NeedsFundedChannel,
    NotPeered,
    NotAllowed,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid address in destination")]
    InvalidAddress,
    #[error("Invalid path in destination")]
    InvalidPath,
}

impl DestinationHealth {
    pub fn init_from_destination(dest: &Destination, allow_insecure: bool) -> Result<Self, Error> {
        match dest.routing.clone() {
            RoutingOptions::Hops(hops) if Into::<u8>::into(hops) == 0 => {
                if allow_insecure {
                    return Ok(Self {
                        last_error: None,
                        health: Health::NotPeered,
                        needs_channel: None,
                    });
                } else {
                    return Ok(Self {
                        last_error: None,
                        health: Health::NotAllowed,
                        needs_channel: None,
                    });
                }
            }
            RoutingOptions::Hops(_) => {
                return Ok(Self {
                    last_error: None,
                    health: Health::ReadyToConnect,
                    needs_channel: None,
                });
            }
            RoutingOptions::IntermediatePath(nodes) => {
                let first = nodes.into_iter().next().ok_or(Error::InvalidPath)?;
                let address = match first {
                    NodeId::Chain(address) => address,
                    NodeId::Offchain(_) => {
                        return Err(Error::InvalidAddress);
                    }
                };

                return Ok(Self {
                    last_error: None,
                    health: Health::NeedsPeeredChannel,
                    needs_channel: Some(address),
                });
            }
        }
    }
}
