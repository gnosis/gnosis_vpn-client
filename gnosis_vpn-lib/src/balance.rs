use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::ChainKeypair;
pub use edgli::hopr_lib::{Balance, GeneralError, WxHOPR, XDai};
use primitive_types::U256;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use std::fmt::{self, Display};

use crate::chain::client::GnosisRpcClient;
use crate::chain::contracts::{CheckBalanceInputs, CheckBalanceResult};
use crate::chain::errors::ChainError;

// in order of priority
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FundingIssue {
    Unfunded,           // cannot work at all - initial state
    ChannelsOutOfFunds, // less than 1 ticket (10 wxHOPR)
    SafeOutOfFunds,     // less than 1 ticket (10 wxHOPR) - cannot top up channels
    SafeLowOnFunds,     // lower than min_stake_threshold * channels
    NodeUnderfunded,    // lower than 0.0075 xDai
    NodeLowOnFunds,     // lower than 0.0075 xDai * channels
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FundingTool {
    NotStarted,
    InProgress,
    CompletedSuccess,
    CompletedError,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Chain(#[from] ChainError),
}

impl Display for FundingIssue {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            FundingIssue::Unfunded => "unfunded - nothing will work",
            FundingIssue::ChannelsOutOfFunds => "channels are out of funds - connections will not work",
            FundingIssue::SafeOutOfFunds => "safe is out of funds - connections will stop working",
            FundingIssue::SafeLowOnFunds => "safe is low on funds - connections will soon stop working",
            FundingIssue::NodeUnderfunded => "underfunded - cannot open new connection or keep existing ones",
            FundingIssue::NodeLowOnFunds => "low on funds - soon cannot open new connection or keep existing ones",
        };
        write!(f, "{s}")
    }
}

impl Display for FundingTool {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = match self {
            FundingTool::NotStarted => "funding tool not used",
            FundingTool::InProgress => "funding tool in progress",
            FundingTool::CompletedSuccess => "funding tool completed",
            FundingTool::CompletedError => "funding tool errored",
        };
        write!(f, "{s}")
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreSafe {
    pub node_xdai: Balance<XDai>,
    pub node_wxhopr: Balance<WxHOPR>,
}

impl PreSafe {
    pub async fn fetch(priv_key: &ChainKeypair, rpc_provider: &str, node_address: Address) -> Result<Self, Error> {
        let client = GnosisRpcClient::with_url(priv_key.clone(), rpc_provider)
            .await
            .map_err(Error::Chain)?;
        let check_balance_inputs = CheckBalanceInputs::new(node_address.into(), node_address.into());
        let res = check_balance_inputs
            .check(&client.provider)
            .await
            .map_err(Error::Chain)?;
        Ok(res.into())
    }
}

impl Default for PreSafe {
    fn default() -> Self {
        Self {
            node_xdai: Balance::<XDai>::zero(),
            node_wxhopr: Balance::<WxHOPR>::zero(),
        }
    }
}

impl Display for PreSafe {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "(node_xdai: {}, node_wxhopr: {})", self.node_xdai, self.node_wxhopr)
    }
}

impl From<CheckBalanceResult> for PreSafe {
    fn from(result: CheckBalanceResult) -> Self {
        let xdai_bytes: [u8; 32] = result.native_token_balance.to_be_bytes::<32>();
        let xdai_u256 = U256::from_big_endian(&xdai_bytes);
        let wxhopr_bytes: [u8; 32] = result.hopr_token_balance.to_be_bytes::<32>();
        let wxhopr_u256 = U256::from_big_endian(&wxhopr_bytes);
        Self {
            node_xdai: Balance::<XDai>::from(xdai_u256),
            node_wxhopr: Balance::<WxHOPR>::from(wxhopr_u256),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Balances {
    pub node_xdai: Balance<XDai>,
    pub safe_wxhopr: Balance<WxHOPR>,
    pub channels_out_wxhopr: Balance<WxHOPR>,
}

impl Display for Balances {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "Balances(node_xdai: {}, safe_wxhopr: {}, channels_out_wxhopr: {})",
            self.node_xdai, self.safe_wxhopr, self.channels_out_wxhopr
        )
    }
}

impl Balances {
    pub fn to_funding_issues(&self, channel_targets_len: usize, ticket_value: Balance<WxHOPR>) -> Vec<FundingIssue> {
        let mut issues = Vec::new();

        if self.node_xdai.is_zero() && self.safe_wxhopr.is_zero() {
            issues.push(FundingIssue::Unfunded);
            return issues;
        }

        if self.channels_out_wxhopr < min_stake_threshold(ticket_value) {
            issues.push(FundingIssue::ChannelsOutOfFunds);
        }

        if self.safe_wxhopr < min_stake_threshold(ticket_value) {
            issues.push(FundingIssue::SafeOutOfFunds);
        } else if self.safe_wxhopr < (min_stake_threshold(ticket_value) * channel_targets_len) {
            issues.push(FundingIssue::SafeLowOnFunds);
        }

        if self.node_xdai < min_funds_threshold() {
            issues.push(FundingIssue::NodeUnderfunded);
        } else if self.node_xdai < (min_funds_threshold() + channel_targets_len) {
            issues.push(FundingIssue::NodeLowOnFunds);
        }

        issues
    }
}

/// worth 1 more ticket than min_stake_threshold
//pub fn funding_amount(ticket_value: Balance<WxHOPR>) -> Balance<WxHOPR> {
pub fn funding_amount(ticket_value: Balance<WxHOPR>) -> Balance<WxHOPR> {
    min_stake_threshold(ticket_value) + ticket_value
}

/// imposed by 3hops. 3 times ticket_value at least are needed in a channel in case the 1st relayer wants to redeem a winning ticket
// pub fn min_stake_threshold(ticket_value: Balance<WxHOPR>) -> Balance<WxHOPR> {
pub fn min_stake_threshold(ticket_value: Balance<WxHOPR>) -> Balance<WxHOPR> {
    ticket_value * 3
}

/// Based on the fixed gas price we use (3gwei) and our average gas/tx consumption (250'000)
pub fn min_funds_threshold() -> Balance<XDai> {
    Balance::<XDai>::from(750000000000000_u64) // 0.00075 xDai = 3 gwei * 250'000 gas
}
