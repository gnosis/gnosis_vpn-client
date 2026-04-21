use edgli::hopr_lib::{Address, Balance, NodeId, RoutingOptions, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt::{self, Display};

use crate::balance::{self, FundingIssue};
use crate::connection::destination::Destination;
use crate::info::Info;
use crate::ticket_stats::TicketStats;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelOut {
    pub destination: ChannelDestination,
    pub balance: ChannelBalance,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ChannelDestination {
    Unconfigured(Address),
    Configured((String, Address)),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum ChannelBalance {
    Unknown,
    FundingOngoing,
    Completed(Balance<WxHOPR>),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BalanceResponse {
    pub node: Balance<XDai>,
    pub safe: Balance<WxHOPR>,
    pub channels_out: Vec<ChannelOut>,
    pub info: Info,
    pub issues: Vec<FundingIssue>,
    pub ticket_price: Balance<WxHOPR>,
    pub winning_probability: f64,
}

impl BalanceResponse {
    pub fn new(
        info: &Info,
        balances: &balance::Balances,
        ticket_stats: &TicketStats,
        destinations: &HashMap<String, Destination>,
        ongoing_channel_fundings: &[&Address],
    ) -> Self {
        let node = balances.node_xdai;
        let safe = balances.safe_wxhopr;
        let mut channels_out = from_balances(balances.channels_out.iter(), destinations.iter());
        add_from_destinations(&mut channels_out, destinations.iter(), ongoing_channel_fundings);

        // ticket_value() is only used internally to determine funding issues
        let ticket_value = ticket_stats.ticket_value().unwrap_or_default();
        let issues: Vec<balance::FundingIssue> = balances.to_funding_issues(ticket_value);
        let info = info.clone();

        BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            info,
            ticket_price: ticket_stats.ticket_price,
            winning_probability: ticket_stats.winning_probability,
        }
    }
}

fn from_balances<'a, 'b>(
    channels_out: impl Iterator<Item = (&'a Address, &'a Balance<WxHOPR>)>,
    destinations: impl Iterator<Item = (&'b String, &'b Destination)>,
) -> Vec<ChannelOut> {
    let destinations_by_relay_address: HashMap<Address, String> = destinations
        .filter_map(|(id, dest)| match dest.routing.clone() {
            RoutingOptions::Hops(_) => None,
            RoutingOptions::IntermediatePath(nodes) => nodes.into_iter().next().and_then(|node_id| match node_id {
                NodeId::Chain(addr) => Some((addr, id.clone())),
                NodeId::Offchain(_) => None,
            }),
        })
        .collect();

    channels_out
        .map(|(address, balance)| {
            let destination = if let Some(id) = destinations_by_relay_address.get(address) {
                ChannelDestination::Configured((id.clone(), *address))
            } else {
                ChannelDestination::Unconfigured(*address)
            };
            let balance = ChannelBalance::Completed(*balance);
            ChannelOut { destination, balance }
        })
        .collect()
}

fn add_from_destinations<'a>(
    channels_out: &mut Vec<ChannelOut>,
    destinations: impl Iterator<Item = (&'a String, &'a Destination)>,
    ongoing_channel_fundings: &[&Address],
) {
    for (id, dest) in destinations {
        let already_present = channels_out.iter().any(|channel| match &channel.destination {
            ChannelDestination::Configured((existing_id, _)) => existing_id == id,
            ChannelDestination::Unconfigured(_) => false,
        });
        if already_present {
            continue;
        }

        // ongoing_channel_fundings contains relay addresses, so match
        // against the relay in the routing path, not the exit address
        let relay_is_funding = ongoing_channel_fundings
            .iter()
            .find(|&&addr| dest.has_intermediate_channel(*addr));
        if let Some(&&relay_addr) = relay_is_funding {
            channels_out.push(ChannelOut {
                destination: ChannelDestination::Configured((id.clone(), relay_addr)),
                balance: ChannelBalance::FundingOngoing,
            });
        }
    }
}

impl Display for ChannelOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Channel to {dest}: {bal}",
            dest = self.destination,
            bal = self.balance
        )
    }
}

impl Display for ChannelBalance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelBalance::Unknown => write!(f, "unknown balance"),
            ChannelBalance::FundingOngoing => write!(f, "funding ongoing"),
            ChannelBalance::Completed(balance) => write!(f, "{balance}"),
        }
    }
}

impl Display for ChannelDestination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelDestination::Unconfigured(addr) => write!(f, "{} (unconfigured)", addr.to_checksum()),
            ChannelDestination::Configured((id, addr)) => write!(f, "{checksum} ({id})", checksum = addr.to_checksum()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    #[test]
    fn ongoing_funding_detected_for_relay_in_intermediate_path() {
        let relay = address(0xAA);
        let dest = Destination::new(
            "d1".to_string(),
            address(0xBB),
            RoutingOptions::IntermediatePath([NodeId::Chain(relay)].into_iter().collect()),
            HashMap::new(),
        );
        let destinations = HashMap::from([("d1".to_string(), dest)]);
        let ongoing = vec![&relay];

        let mut channels_out = Vec::new();
        add_from_destinations(&mut channels_out, destinations.iter(), &ongoing);

        assert_eq!(
            channels_out,
            vec![ChannelOut {
                destination: ChannelDestination::Configured(("d1".to_string(), relay)),
                balance: ChannelBalance::FundingOngoing,
            }]
        );
    }
}
