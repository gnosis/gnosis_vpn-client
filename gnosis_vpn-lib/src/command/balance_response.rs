use edgli::hopr_lib::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};

use std::collections::HashMap;

use crate::balance::{self, FundingIssue};
use crate::connection::destination::Destination;
use crate::connectivity_health::{self, ConnectivityHealth};
use crate::info::Info;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
struct ChannelOut {
    destination: ChannelDestination,
    balance: ChannelBalance,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
enum ChannelDestination {
    Unconfigured(Address),
    Configured((String, Address)),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
enum ChannelBalance {
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
}

impl BalanceResponse {
    pub fn new(
        info: &Info,
        balances: &balance::Balances,
        ticket_value: &Balance<WxHOPR>,
        destinations: &HashMap<String, Destination>,
        connectivity_health: &[&ConnectivityHealth],
        ongoing_channel_fundings: &[&Address],
    ) -> Self {
        let node = balances.node_xdai;
        let safe = balances.safe_wxhopr;
        let mut channels_out = from_balances(balances.channels_out.iter(), destinations.iter());
        add_from_destinations(&mut channels_out, destinations.iter(), ongoing_channel_fundings);

        let min_channel_count = connectivity_health::count_distinct_channels(connectivity_health);
        let issues: Vec<balance::FundingIssue> = balances.to_funding_issues(min_channel_count, *ticket_value);
        let info = info.clone();

        BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            info,
        }
    }
}

fn from_balances<'a, 'b>(
    channels_out: impl Iterator<Item = (&'a Address, &'a Balance<WxHOPR>)>,
    destinations: impl Iterator<Item = (&'b String, &'b Destination)>,
) -> Vec<ChannelOut> {
    let destinations_by_address: HashMap<Address, String> =
        destinations.map(|(id, dest)| (dest.address, id.clone())).collect();
    channels_out
        .map(|(address, balance)| {
            let destination = if let Some(id) = destinations_by_address.get(address) {
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
        if !channels_out.iter().any(|channel| match &channel.destination {
            ChannelDestination::Configured((existing_id, _)) => existing_id == id,
            ChannelDestination::Unconfigured(_) => false,
        }) {
            let destination = ChannelDestination::Configured((id.clone(), dest.address));
            let balance = if ongoing_channel_fundings.contains(&&dest.address) {
                ChannelBalance::FundingOngoing
            } else {
                ChannelBalance::Unknown
            };
            channels_out.push(ChannelOut { destination, balance });
        }
    }
}
