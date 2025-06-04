use gnosis_vpn_lib::connection;
use std::fmt;

#[derive(Debug)]
pub enum Event {
    ConnectWg(connection::ConnectInfo),
    /// Event indicating that the connection has been established and is ready for use.
    /// Boolean flag indicates if it has ever worked before, true meaning it has worked at least once.
    Disconnected(bool),
    DropConnection,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::ConnectWg(..) => write!(f, "ConnectWg"),
            Event::Disconnected(true) => write!(f, "Disconnected (ping has worked)"),
            Event::Disconnected(false) => write!(f, "Disconnected (ping never worked)"),
            Event::DropConnection => write!(f, "DropConnection"),
        }
    }
}
