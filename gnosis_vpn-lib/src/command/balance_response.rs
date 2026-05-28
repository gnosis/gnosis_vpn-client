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
    serde_utils,
    ticket_stats::{self, TicketStats},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChannelOut {
    #[serde(with = "serde_utils::address")]
    pub address: Address,
    pub balance: ChannelBalance,
    pub matched_exit: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ChannelBalance {
    Unknown,
    Completed {
        #[serde(with = "serde_utils::balance")]
        amount: Balance<WxHOPR>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BalanceResponse {
    #[serde(with = "serde_utils::balance")]
    pub node: Balance<XDai>,
    #[serde(with = "serde_utils::balance")]
    pub safe: Balance<WxHOPR>,
    pub channels_out: Vec<ChannelOut>,
    pub info: Info,
    pub issues: Vec<FundingIssue>,
    #[serde(with = "serde_utils::balance")]
    pub ticket_price: Balance<WxHOPR>,
    pub winning_probability: f64,
}

impl BalanceResponse {
    pub fn try_build(
        info: &Info,
        balances: &balance::Balances,
        ticket_stats: &TicketStats,
        destinations: &HashMap<String, Destination>,
    ) -> Result<Self, ticket_stats::Error> {
        let node = balances.node_xdai;
        let safe = balances.safe_wxhopr;
        let channels_out = from_balances(balances.channels_out.iter(), destinations);

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
    destinations: &HashMap<String, Destination>,
) -> Vec<ChannelOut> {
    let addr_to_id: HashMap<Address, &str> = destinations
        .iter()
        .map(|(id, dest)| (dest.address, id.as_str()))
        .collect();
    channels_out
        .map(|(address, balance)| ChannelOut {
            address: *address,
            balance: ChannelBalance::Completed { amount: *balance },
            matched_exit: addr_to_id.get(address).map(|id| (*id).to_string()),
        })
        .collect()
}

impl Display for ChannelOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.matched_exit {
            Some(id) => write!(
                f,
                "Channel to {} (exit: {id}): {}",
                self.address.to_checksum(),
                self.balance
            ),
            None => write!(f, "Channel to {}: {}", self.address.to_checksum(), self.balance),
        }
    }
}

impl Display for ChannelBalance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelBalance::Unknown => write!(f, "unknown balance"),
            ChannelBalance::Completed { amount } => write!(f, "{amount}"),
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
    fn from_balances_sets_matched_exit_when_address_matches_destination() {
        let addr = address(1);
        let balance = Balance::<WxHOPR>::from(100u64);
        let mut destinations = HashMap::new();
        destinations.insert("dest-1".to_string(), destination("dest-1", addr));

        let result = from_balances(std::iter::once((&addr, &balance)), &destinations);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].address, addr);
        assert_eq!(result[0].matched_exit, Some("dest-1".to_string()));
        assert_eq!(result[0].balance, ChannelBalance::Completed { amount: balance });
    }

    #[test]
    fn from_balances_leaves_matched_exit_empty_for_unknown_address() {
        let addr = address(2);
        let balance = Balance::<WxHOPR>::from(50u64);

        let result = from_balances(std::iter::once((&addr, &balance)), &HashMap::new());

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].address, addr);
        assert_eq!(result[0].matched_exit, None);
        assert_eq!(result[0].balance, ChannelBalance::Completed { amount: balance });
    }
}
