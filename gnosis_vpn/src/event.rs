use std::fmt;

use gnosis_vpn_lib::connection;

#[derive(Debug)]
pub enum Event {
    ConnectionEvent(connection::Event),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::ConnectionEvent(event) => write!(f, "ConnectionEvent: {}", event),
        }
    }
}
