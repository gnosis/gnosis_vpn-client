use std::fmt::{self, Display};

use gnosis_vpn_lib::chain::errors::ChainError;
use gnosis_vpn_lib::ticket_stats::TicketStats;
use gnosis_vpn_lib::{channel_funding, connection, metrics, node, onboarding};

#[derive(Debug)]
pub enum Event {
    Connection(connection::Event),
    Node(node::Event),
    Onboarding(onboarding::Event),
    ChannelFunding(channel_funding::Event),
    Metrics(metrics::Event),
    TicketStats(Result<TicketStats, ChainError>),
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::Connection(event) => write!(f, "ConnectionEvent: {event}"),
            Event::Node(event) => write!(f, "NodeEvent: {event}"),
            Event::Onboarding(event) => write!(f, "OnboardingEvent: {event}"),
            Event::ChannelFunding(event) => write!(f, "ChannelFundingEvent: {event}"),
            Event::Metrics(event) => write!(f, "MetricsEvent: {event}"),
            Event::TicketStats(event) => match event {
                Ok(stats) => write!(f, "TicketStatsEvent: {stats}"),
                Err(err) => write!(f, "TicketStatsEvent Error: {err}"),
            },
        }
    }
}
