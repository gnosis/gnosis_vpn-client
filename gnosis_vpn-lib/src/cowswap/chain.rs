//! Blokli reads for the refuel: deployment check, balances and the Safe nonce. Nothing here signs or sends.

use backon::Retryable;
use edgli::hopr_lib::api::types::chain::exports::alloy;
use edgli::hopr_lib::api::types::primitive::prelude::{Balance, Currency, WxHOPR, XDaiBalance, XHOPR};
use serde::Deserialize;
use serde_json::json;

use alloy::primitives::{Address, U256, hex};

use std::collections::HashMap;
use std::net::SocketAddr;
use std::str::FromStr;

use super::order::wei;
use super::{CHAIN_ID, Error, WXHOPR_TOKEN, XHOPR_TOKEN};
use crate::remote_data;

/// Schema version edgli's own Blokli client announces.
const SCHEMA_VERSION: &str = "1";

const SNAPSHOT_QUERY: &str = r#"query RefuelSnapshot($safe: String!, $eoa: String!) {
  chainInfo { __typename ... on ChainInfo { chainId contractAddresses } ... on QueryFailedError { message } }
  eoaNative: nativeBalance(address: $eoa) { __typename ... on NativeBalance { balance } ...NativeFailure }
  safeWxhopr: hoprBalance(address: $safe, token: HOPR) { ...TokenBalance }
  safeXhopr: hoprBalance(address: $safe, token: XHOPR) { ...TokenBalance }
  eoaXhopr: hoprBalance(address: $eoa, token: XHOPR) { ...TokenBalance }
  safeNonce: transactionCount(address: $safe) { __typename ... on TransactionCount { count } ...CountFailure }
}
fragment TokenBalance on HoprBalanceResult {
  __typename ... on HoprBalance { balance } ... on InvalidAddressError { message } ... on QueryFailedError { message }
}
fragment NativeFailure on NativeBalanceResult {
  ... on InvalidAddressError { message } ... on QueryFailedError { message }
}
fragment CountFailure on TransactionCountResult {
  ... on InvalidAddressError { message } ... on QueryFailedError { message }
}"#;

#[derive(Debug)]
pub(crate) struct Snapshot {
    pub eoa_native: XDaiBalance,
    pub safe_wxhopr: U256,
    pub safe_xhopr: U256,
    pub eoa_xhopr: U256,
    /// For a Safe address Blokli answers with the Safe's own `nonce()`, which only owner-path executions advance.
    pub safe_nonce: u64,
}

pub(crate) struct Client {
    http: reqwest::Client,
    url: String,
}

impl Client {
    /// Same endpoint edgli talks to, including the DNS pin that keeps Blokli reachable under the killswitch.
    pub(crate) fn new(endpoint: &edgli::BlokliEndpoint) -> Result<Self, Error> {
        // An env proxy would sit outside the pin; the killswitch exempts the pinned address only.
        let mut builder = reqwest::Client::builder().no_proxy().timeout(endpoint.request_timeout);
        if let Some(pin) = &endpoint.dns_override {
            let host = endpoint
                .url
                .host_str()
                .ok_or_else(|| Error::Blokli(format!("{} has no host to pin", endpoint.url)))?;
            let port = pin
                .port
                .or_else(|| endpoint.url.port_or_known_default())
                .ok_or_else(|| Error::Blokli(format!("{} has no port to pin", endpoint.url)))?;
            builder = builder.resolve(host, SocketAddr::new(pin.ip, port));
        }
        let http = builder.build().map_err(|e| Error::Blokli(e.to_string()))?;
        Ok(Self {
            http,
            // The endpoint path is a directory, as for edgli's client: `…/api` serves `…/api/graphql`.
            url: format!("{}/graphql", endpoint.url.as_str().trim_end_matches('/')),
        })
    }

    /// Transport and HTTP failures are retried briefly; an error member in the answer is final.
    pub(crate) async fn snapshot(&self, safe: Address, eoa: Address) -> Result<Snapshot, Error> {
        let body = json!({
            "query": SNAPSHOT_QUERY,
            "variables": { "safe": hex::encode(safe), "eoa": hex::encode(eoa) },
        });
        (|| self.post(&body))
            .retry(remote_data::backoff_expo_short_delay())
            .notify(|err, delay| tracing::warn!(?err, ?delay, "blokli request failed, retrying..."))
            .await?
            .into_snapshot()
    }

    async fn post(&self, body: &serde_json::Value) -> Result<Response, Error> {
        self.http
            .post(&self.url)
            .headers(remote_data::json_headers())
            .header("X-Blokli-Schema-Version", SCHEMA_VERSION)
            .json(body)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| Error::Blokli(e.to_string()))?
            .json()
            .await
            .map_err(|e| Error::Blokli(e.to_string()))
    }
}

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlError {
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    chain_info: Member,
    eoa_native: Member,
    safe_wxhopr: Member,
    safe_xhopr: Member,
    eoa_xhopr: Member,
    safe_nonce: Member,
}

/// One member of a Blokli result union: payload fields on success, `message` on an error member.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Member {
    #[serde(rename = "__typename")]
    typename: String,
    message: Option<String>,
    chain_id: Option<u64>,
    contract_addresses: Option<String>,
    balance: Option<String>,
    count: Option<String>,
}

impl Member {
    fn ok(self, alias: &str) -> Result<Self, Error> {
        match self.message {
            Some(message) => Err(Error::Blokli(format!("{alias}: {} {message}", self.typename))),
            None => Ok(self),
        }
    }
}

fn present<T>(value: Option<T>, name: &str) -> Result<T, Error> {
    value.ok_or_else(|| Error::Blokli(format!("{name} missing")))
}

impl Response {
    fn into_snapshot(self) -> Result<Snapshot, Error> {
        let Some(data) = self.data else {
            let messages: Vec<String> = self.errors.into_iter().map(|e| e.message).collect();
            return Err(Error::Blokli(format!("no data: {}", messages.join(", "))));
        };
        let info = data.chain_info.ok("chainInfo")?;
        check_deployment(
            present(info.chain_id, "chainInfo.chainId")?,
            &present(info.contract_addresses, "chainInfo.contractAddresses")?,
        )?;
        let native = present(data.eoa_native.ok("eoaNative")?.balance, "eoaNative.balance")?;
        let eoa_native = native
            .parse()
            .map_err(|e| Error::Blokli(format!("native balance {native:?}: {e}")))?;
        let count = present(data.safe_nonce.ok("safeNonce")?.count, "safeNonce.count")?;
        let safe_nonce = count
            .parse()
            .map_err(|e| Error::Blokli(format!("safe nonce {count:?}: {e}")))?;
        Ok(Snapshot {
            eoa_native,
            safe_wxhopr: token_wei::<WxHOPR>(data.safe_wxhopr, "safeWxhopr")?,
            safe_xhopr: token_wei::<XHOPR>(data.safe_xhopr, "safeXhopr")?,
            eoa_xhopr: token_wei::<XHOPR>(data.eoa_xhopr, "eoaXhopr")?,
            safe_nonce,
        })
    }
}

/// The wrapper is hard-wired to one token pair; any other deployment would make the unwrap hook revert.
fn check_deployment(chain_id: u64, contract_addresses: &str) -> Result<(), Error> {
    if chain_id != CHAIN_ID {
        return Err(Error::UnsupportedChain(chain_id));
    }
    let addresses: HashMap<String, String> =
        serde_json::from_str(contract_addresses).map_err(|e| Error::Blokli(format!("contract addresses: {e}")))?;
    let address = |key: &str| -> Result<Address, Error> {
        let raw = addresses
            .get(key)
            .ok_or_else(|| Error::Blokli(format!("contract addresses lack {key}")))?;
        Address::from_str(raw).map_err(|e| Error::Blokli(format!("contract address {key} {raw:?}: {e}")))
    };
    let (token, xhopr) = (address("token")?, address("xhopr_token")?);
    if token != WXHOPR_TOKEN || xhopr != XHOPR_TOKEN {
        return Err(Error::UnexpectedTokens { token, xhopr });
    }
    Ok(())
}

/// A parse failure must surface, never read as zero: it would size the unwrap wrong.
fn token_wei<C: Currency>(member: Member, alias: &str) -> Result<U256, Error> {
    let raw = present(member.ok(alias)?.balance, alias)?;
    raw.parse::<Balance<C>>()
        .map(wei)
        .map_err(|e| Error::Blokli(format!("{alias} {raw:?}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live piz-palu-dev answer, contract map trimmed to the two tokens.
    const CAPTURED: &str = r#"{"data":{
        "chainInfo":{"__typename":"ChainInfo","chainId":100,"contractAddresses":"{\"xhopr_token\":\"0xd057604a14982fe8d88c5fc25aac3267ea142a08\",\"token\":\"0xd4fdec44db9d44b8f2b6d529620f9c0c7066a2c1\"}"},
        "eoaNative":{"__typename":"NativeBalance","balance":"0.010200033532698918 xDai"},
        "eoaXhopr":{"__typename":"HoprBalance","balance":"0 xHOPR"},
        "safeNonce":{"__typename":"TransactionCount","count":"8"},
        "safeWxhopr":{"__typename":"HoprBalance","balance":"238.512179384556959849 wxHOPR"},
        "safeXhopr":{"__typename":"HoprBalance","balance":"0 xHOPR"}}}"#;

    fn snapshot(body: &str) -> Result<Snapshot, Error> {
        serde_json::from_str::<Response>(body).unwrap().into_snapshot()
    }

    #[test]
    fn a_captured_answer_is_read_in_full() {
        let snapshot = snapshot(CAPTURED).unwrap();
        assert_eq!(snapshot.eoa_native, "0.010200033532698918 xDai".parse().unwrap());
        assert_eq!(snapshot.safe_wxhopr, U256::from(238_512_179_384_556_959_849_u128));
        assert_eq!(snapshot.safe_xhopr, U256::ZERO);
        assert_eq!(snapshot.eoa_xhopr, U256::ZERO);
        assert_eq!(snapshot.safe_nonce, 8);
    }

    #[test]
    fn an_error_member_fails_the_snapshot() {
        let body = CAPTURED.replace(
            r#""safeNonce":{"__typename":"TransactionCount","count":"8"}"#,
            r#""safeNonce":{"__typename":"QueryFailedError","message":"rpc down"}"#,
        );
        let err = snapshot(&body).unwrap_err().to_string();
        assert!(err.contains("safeNonce") && err.contains("rpc down"), "{err}");
    }

    #[test]
    fn another_chain_is_unsupported() {
        let body = CAPTURED.replace(r#""chainId":100"#, r#""chainId":1"#);
        assert!(matches!(snapshot(&body), Err(Error::UnsupportedChain(1))));
    }

    #[test]
    fn graphql_errors_without_data_are_reported() {
        let err = snapshot(r#"{"data":null,"errors":[{"message":"unknown field"}]}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown field"), "{err}");
    }

    /// Covers what the captured answer cannot: the DNS pin, the schema header and unprefixed address variables.
    #[tokio::test]
    #[ignore = "talks to blokli"]
    async fn a_pinned_live_snapshot_is_read() {
        let url: url::Url = std::env::var(crate::hopr::ENV_VAR_BLOKLI_URL)
            .expect("blokli url")
            .parse()
            .unwrap();
        let ip = tokio::net::lookup_host((url.host_str().unwrap(), 443))
            .await
            .unwrap()
            .find(SocketAddr::is_ipv4)
            .unwrap()
            .ip();
        let endpoint = edgli::BlokliEndpoint::new(url)
            .with_request_timeout(std::time::Duration::from_secs(20))
            .with_dns_override(edgli::BlokliDnsOverride::new(ip, None));
        let any = Address::repeat_byte(1);
        Client::new(&endpoint).unwrap().snapshot(any, any).await.unwrap();
    }
}
