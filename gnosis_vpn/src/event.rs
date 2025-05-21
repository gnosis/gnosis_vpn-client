use gnosis_vpn_lib::connection;
use std::fmt;

#[derive(Debug)]
pub enum Event {
    ConnectWg(connection::ConnectInfo),
    DropConnection,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::ConnectWg(..) => write!(f, "ConnectWg"),
            Event::DropConnection => write!(f, "DropConnection"),
        }
    }
}
