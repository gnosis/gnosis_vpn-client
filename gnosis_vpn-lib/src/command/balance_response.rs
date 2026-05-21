use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDai};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fmt::{self, Display},
};

use crate::{
    balance::{self, FundingIssue},
    connection::destination::Destination,
    info::Info,
    ticket_stats::{self, TicketStats},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelOut {
    pub address: Address,
    pub balance: ChannelBalance,
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

fn from_balances<'a>(channels_out: impl Iterator<Item = (&'a Address, &'a Balance<WxHOPR>)>) -> Vec<ChannelOut> {
    channels_out
        .map(|(address, balance)| ChannelOut {
            address: *address,
            balance: ChannelBalance::Completed(*balance),
        })
        .collect()
}

fn add_from_destinations<'a>(
    channels_out: &mut Vec<ChannelOut>,
    destinations: impl Iterator<Item = (&'a String, &'a Destination)>,
    ongoing_channel_fundings: &[&Address],
) {
    for (_, dest) in destinations {
        let already_present = channels_out.iter().any(|channel| channel.address == dest.address);
        if already_present {
            continue;
        }

        let is_funding = ongoing_channel_fundings.iter().any(|&&addr| addr == dest.address);
        if is_funding {
            channels_out.push(ChannelOut {
                address: dest.address,
                balance: ChannelBalance::FundingOngoing,
            });
        }
    }
}

impl Display for ChannelOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Channel to {}: {}", self.address.to_checksum(), self.balance)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{Destination, HopRouting};

    fn address(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn destination(id: &str, addr: Address) -> Destination {
        Destination::new(
            id.to_string(),
            addr,
            HopRouting::try_from(1).expect("conversion cannot fail"),
            HashMap::new(),
        )
    }

    #[test]
    fn from_balances_maps_address_and_balance() {
        let addr = address(1);
        let balance = Balance::<WxHOPR>::from(100u64);

        let result = from_balances(std::iter::once((&addr, &balance)));

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].address, addr);
        assert_eq!(result[0].balance, ChannelBalance::Completed(balance));
    }

    #[test]
    fn add_from_destinations_adds_funding_ongoing_for_funded_exit() {
        let addr = address(3);
        let mut channels_out = Vec::new();
        let mut destinations = HashMap::new();
        destinations.insert("dest-2".to_string(), destination("dest-2", addr));

        add_from_destinations(&mut channels_out, destinations.iter(), &[&addr]);

        assert_eq!(channels_out.len(), 1);
        assert_eq!(channels_out[0].address, addr);
        assert_eq!(channels_out[0].balance, ChannelBalance::FundingOngoing);
    }

    #[test]
    fn add_from_destinations_skips_when_not_funding() {
        let addr = address(4);
        let mut channels_out = Vec::new();
        let mut destinations = HashMap::new();
        destinations.insert("dest-3".to_string(), destination("dest-3", addr));

        add_from_destinations(&mut channels_out, destinations.iter(), &[]);

        assert!(channels_out.is_empty());
    }
}
