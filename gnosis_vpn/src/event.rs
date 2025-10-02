use std::fmt;

use gnosis_vpn_lib::{connection, node, onboarding};

#[derive(Debug)]
pub enum Event {
    Connection(connection::Event),
    Node(node::Event),
    Onboarding(onboarding::Event),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::Connection(event) => write!(f, "ConnectionEvent: {event}"),
            Event::Node(event) => write!(f, "NodeEvent: {event}"),
            Event::Onboarding(event) => write!(f, "OnboardingEvent: {event}"),
        }
    }
}
