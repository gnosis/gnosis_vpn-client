use std::fmt;

use gnosis_vpn_lib::{connection, node};

#[derive(Debug)]
pub enum Event {
    ConnectionEvent(connection::Event),
    NodeEvent(node::Event),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::ConnectionEvent(event) => write!(f, "ConnectionEvent: {event}"),
            Event::NodeEvent(event) => write!(f, "NodeEvent: {event}"),
        }
    }
}
