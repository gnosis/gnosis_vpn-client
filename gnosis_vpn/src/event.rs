use gnosis_vpn_lib::connection;
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::remote_data;

#[derive(Debug)]
pub enum Event {
    FetchAddresses(remote_data::Event),
    FetchOpenSession(remote_data::Event),
    FetchListSessions(remote_data::Event),
    FetchCloseSession(remote_data::Event),
    CheckSession,
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
            Event::FetchAddresses(evt) => write!(f, "FetchAddresses: {}", evt),
            Event::FetchOpenSession(evt) => write!(f, "FetchOpenSessions: {}", evt),
            Event::FetchListSessions(evt) => write!(f, "FetchListSessions: {}", evt),
            Event::FetchCloseSession(evt) => write!(f, "FetchCloseSession: {}", evt),
            Event::CheckSession => write!(f, "CheckSession"),
            Event::ConnectWg(..) => write!(f, "ConnectWg"),
            Event::DisconnectWg => write!(f, "DisconnectWg"),
        }
    }
}
