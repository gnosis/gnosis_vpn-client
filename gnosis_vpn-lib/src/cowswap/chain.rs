//! Blokli reads for the refuel: token addresses, balances and the Safe nonce. Nothing here signs or sends.

use edgli::hopr_lib::api::types::chain::exports::alloy;
use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR, XDaiBalance, XHOPR};
use hopr_chain_connector::blokli_client::types::Token;
use hopr_chain_connector::blokli_client::{BlokliClient, BlokliQueryClient};
use hopr_chain_connector::{HoprBlokliClientConfig, create_blokli_client};

use alloy::primitives::{Address as EvmAddress, U256};

use std::collections::HashMap;
use std::str::FromStr;

use super::order::wei;
use super::{CHAIN_ID, Error, WRAPPER_WXHOPR, WRAPPER_XHOPR};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tokens {
    pub wxhopr: EvmAddress,
    pub xhopr: EvmAddress,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Balances {
    pub safe_wxhopr: U256,
    pub safe_xhopr: U256,
    pub eoa_xhopr: U256,
}

/// Same client edgli builds for itself, including the DNS pin that keeps Blokli reachable under the killswitch.
pub(crate) fn blokli_client(endpoint: edgli::BlokliEndpoint) -> BlokliClient {
    create_blokli_client(HoprBlokliClientConfig {
        url: endpoint.url,
        dns_override: endpoint.dns_override.map(|o| (o.ip, o.port)),
        request_timeout: endpoint.request_timeout,
    })
}

/// The wrapper is hard-wired to one token pair; any other deployment would make the unwrap hook revert.
pub(crate) async fn tokens(client: &BlokliClient) -> Result<Tokens, Error> {
    let info = client
        .query_chain_info()
        .await
        .map_err(|e| Error::Blokli(e.to_string()))?;
    let chain_id = u64::try_from(info.chain_id).unwrap_or_default();
    if chain_id != CHAIN_ID {
        return Err(Error::UnsupportedChain(chain_id));
    }
    let addresses: HashMap<String, String> = serde_json::from_str(&info.contract_addresses.0)
        .map_err(|e| Error::Blokli(format!("contract addresses: {e}")))?;
    let address = |key: &str| -> Result<EvmAddress, Error> {
        let raw = addresses
            .get(key)
            .ok_or_else(|| Error::Blokli(format!("contract addresses lack {key}")))?;
        EvmAddress::from_str(raw).map_err(|e| Error::Blokli(format!("contract address {key} {raw:?}: {e}")))
    };
    let tokens = Tokens {
        wxhopr: address("token")?,
        xhopr: address("xhopr_token")?,
    };
    if tokens.wxhopr != WRAPPER_WXHOPR || tokens.xhopr != WRAPPER_XHOPR {
        return Err(Error::UnexpectedTokens {
            token: tokens.wxhopr.to_string(),
            xhopr: tokens.xhopr.to_string(),
        });
    }
    Ok(tokens)
}

pub(crate) async fn native_balance(client: &BlokliClient, address: Address) -> Result<XDaiBalance, Error> {
    let balance = client
        .query_native_balance(&<[u8; 20]>::from(address))
        .await
        .map_err(|e| Error::Blokli(e.to_string()))?;
    balance
        .balance
        .0
        .parse()
        .map_err(|e| Error::Blokli(format!("native balance {:?}: {e}", balance.balance.0)))
}

async fn token_wei(client: &BlokliClient, address: Address, token: Token) -> Result<U256, Error> {
    let balance = client
        .query_token_balance(&<[u8; 20]>::from(address), token)
        .await
        .map_err(|e| Error::Blokli(e.to_string()))?;
    let raw = balance.balance.0;
    // A parse failure must surface, never read as zero: it would size the unwrap wrong.
    let amount = match token {
        Token::XHOPR => raw.parse::<Balance<XHOPR>>().map(wei),
        _ => raw.parse::<Balance<WxHOPR>>().map(wei),
    };
    amount.map_err(|e| Error::Blokli(format!("token balance {raw:?}: {e}")))
}

impl Balances {
    pub(crate) async fn read(client: &BlokliClient, safe: Address, eoa: Address) -> Result<Self, Error> {
        Ok(Self {
            safe_wxhopr: token_wei(client, safe, Token::WxHOPR).await?,
            safe_xhopr: token_wei(client, safe, Token::XHOPR).await?,
            eoa_xhopr: token_wei(client, eoa, Token::XHOPR).await?,
        })
    }
}

/// For a Safe address Blokli answers with the Safe's own `nonce()`, which only owner-path executions advance.
pub(crate) async fn safe_nonce(client: &BlokliClient, safe: Address) -> Result<u64, Error> {
    client
        .query_transaction_count(&<[u8; 20]>::from(safe))
        .await
        .map_err(|e| Error::Blokli(e.to_string()))
}
