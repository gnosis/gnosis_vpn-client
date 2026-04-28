use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt::{self, Display};

use crate::balance::{self, FundingIssue};
use crate::connection::destination::Destination;
use crate::info::Info;
use crate::ticket_stats::{self, TicketStats};

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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub fn try_build(
        info: &Info,
        balances: &balance::Balances,
        ticket_stats: &TicketStats,
        destinations: &HashMap<String, Destination>,
        ongoing_channel_fundings: &[&Address],
    ) -> Result<Self, ticket_stats::Error> {
        let node = balances.node_xdai;
        let safe = balances.safe_wxhopr;
        let mut channels_out = from_balances(balances.channels_out.iter());
        add_from_destinations(&mut channels_out, destinations.iter(), ongoing_channel_fundings);

        let ticket_value = ticket_stats.ticket_value()?;
        let issues: Vec<balance::FundingIssue> = balances.to_funding_issues(ticket_value);
        let info = info.clone();

        Ok(BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            info,
            ticket_price: ticket_stats.ticket_price,
            winning_probability: ticket_stats.winning_probability,
        })
    }
}

fn from_balances<'a>(
    channels_out: impl Iterator<Item = (&'a Address, &'a Balance<WxHOPR>)>,
) -> Vec<ChannelOut> {
    channels_out
        .map(|(address, balance)| ChannelOut {
            destination: ChannelDestination::Unconfigured(*address),
            balance: ChannelBalance::Completed(*balance),
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

