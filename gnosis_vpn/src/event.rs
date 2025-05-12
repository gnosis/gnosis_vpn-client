use gnosis_vpn_lib::connection;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug)]
pub enum Event {
    ConnectWg(connection::ConnectInfo),
    DisconnectWg,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ListSessionsEntry {
    target: String,
    protocol: String,
    ip: String,
    port: u16,
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::ConnectWg(..) => write!(f, "ConnectWg"),
            Event::DisconnectWg => write!(f, "DisconnectWg"),
        }
    }
}
