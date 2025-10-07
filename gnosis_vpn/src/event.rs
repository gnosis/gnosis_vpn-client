use std::fmt;

use gnosis_vpn_lib::{channel_funding, connection, metrics, node, onboarding, one_shot_tasks};

#[derive(Debug)]
pub enum Event {
    Connection(connection::Event),
    Node(node::Event),
    Onboarding(onboarding::Event),
    ChannelFunding(channel_funding::Event),
    Metrics(metrics::Event),
    OneShotTasks(one_shot_tasks::Event),
}

impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Event::Connection(event) => write!(f, "ConnectionEvent: {event}"),
            Event::Node(event) => write!(f, "NodeEvent: {event}"),
            Event::Onboarding(event) => write!(f, "OnboardingEvent: {event}"),
            Event::ChannelFunding(event) => write!(f, "ChannelFundingEvent: {event}"),
            Event::Metrics(event) => write!(f, "MetricsEvent: {event}"),
            Event::OneShotTasks(event) => write!(f, "OneShotTasksEvent: {event}"),
        }
    }
}
