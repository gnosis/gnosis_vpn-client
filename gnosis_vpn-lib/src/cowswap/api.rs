//! Thin client for the CoW orderbook: quote, place, poll, and the account history used for recovery.

use backon::Retryable;
use edgli::hopr_lib::api::types::chain::exports::alloy;
use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::time;
use url::Url;

use alloy::primitives::{Address, U256, hex};

use std::time::Duration;

use super::order::Order;
use super::{APP_CODE, BUY_NATIVE, Error};
use crate::remote_data;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_AFTER_CAP: Duration = Duration::from_secs(60);
const ACCOUNT_ORDERS_LIMIT: u32 = 10;

pub(crate) struct Client {
    http: reqwest::Client,
    base: String,
    timeout: Duration,
}

#[derive(Debug)]
pub(crate) struct Quote {
    pub sell_amount: U256,
    pub fee_amount: U256,
    pub valid_to: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum OrderStatus {
    PresignaturePending,
    Open,
    Fulfilled,
    Cancelled,
    Expired,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OrderRecord {
    pub uid: String,
    pub status: OrderStatus,
    pub valid_to: u32,
    #[serde(default)]
    pub full_app_data: Option<String>,
    #[serde(default)]
    pub interactions: Interactions,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(crate) struct Interactions {
    #[serde(default)]
    pub pre: Vec<Interaction>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Interaction {
    pub target: Address,
}

impl OrderRecord {
    pub(crate) fn has_hook_target(&self, target: Address) -> bool {
        self.interactions.pre.iter().any(|i| i.target == target)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuoteResponse {
    quote: QuoteParams,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QuoteParams {
    sell_amount: String,
    fee_amount: String,
    valid_to: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ApiErrorBody {
    error_type: String,
    #[serde(default)]
    description: String,
}

/// Every hook carries an appData hash of its own, so "ours" is the appCode inside the stored document.
pub(crate) fn is_ours(order: &OrderRecord) -> bool {
    order
        .full_app_data
        .as_deref()
        .and_then(|doc| serde_json::from_str::<serde_json::Value>(doc).ok())
        .map(|doc| doc["appCode"] == APP_CODE)
        .unwrap_or(false)
}

/// Newest open order of ours with enough validity left to be worth resuming.
pub(crate) fn live_order(orders: &[OrderRecord], min_valid_to: u64) -> Option<&OrderRecord> {
    orders
        .iter()
        .find(|o| is_ours(o) && o.status == OrderStatus::Open && u64::from(o.valid_to) > min_valid_to)
}

/// A filled order of ours ran its permit hook, so the vault relayer allowance is unlimited already.
pub(crate) fn has_filled_order(orders: &[OrderRecord]) -> bool {
    orders.iter().any(|o| is_ours(o) && o.status == OrderStatus::Fulfilled)
}

fn parse_u256(decimal: &str) -> Result<U256, Error> {
    U256::from_str_radix(decimal, 10).map_err(|e| Error::Api {
        status: 200,
        error_type: "InvalidResponse".into(),
        description: format!("{decimal:?} is not a decimal amount: {e}"),
    })
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> Duration {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(2))
        .min(RETRY_AFTER_CAP)
}

impl Client {
    pub(crate) fn new(base: Url, timeout: Duration) -> Result<Self, Error> {
        // A LAN or env proxy would sit outside the tunnel; connects must fail fast under the killswitch.
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| Error::CowUnavailable(e.to_string()))?;
        Ok(Self {
            http,
            base: base.as_str().trim_end_matches('/').to_string(),
            timeout,
        })
    }

    pub(crate) async fn account_orders(&self, owner: Address) -> Result<Vec<OrderRecord>, Error> {
        let url = format!(
            "{}/account/{}/orders?offset=0&limit={ACCOUNT_ORDERS_LIMIT}",
            self.base,
            hex::encode_prefixed(owner)
        );
        self.send("account orders", || self.http.get(&url)).await
    }

    /// A buy-side quote for the target: the fee it returns already prices the hooks' gas.
    pub(crate) async fn quote(
        &self,
        sell_token: Address,
        eoa: Address,
        buy_amount: U256,
        validity: Duration,
        app_data: &str,
    ) -> Result<Quote, Error> {
        let body = json!({
            "sellToken": hex::encode_prefixed(sell_token),
            "buyToken": hex::encode_prefixed(BUY_NATIVE),
            "from": hex::encode_prefixed(eoa),
            "receiver": hex::encode_prefixed(eoa),
            "kind": "buy",
            "buyAmountAfterFee": buy_amount.to_string(),
            "validFor": validity.as_secs(),
            "appData": app_data,
            "signingScheme": "eip712",
            "onchainOrder": false,
            "priceQuality": "optimal",
        });
        let url = format!("{}/quote", self.base);
        let response: QuoteResponse = self.send("quote", || self.http.post(&url).json(&body)).await?;
        Ok(Quote {
            sell_amount: parse_u256(&response.quote.sell_amount)?,
            fee_amount: parse_u256(&response.quote.fee_amount)?,
            valid_to: response.quote.valid_to,
        })
    }

    /// Returns the uid the orderbook computed; the caller compares it with the local one.
    pub(crate) async fn post_order(
        &self,
        order: &Order,
        from: Address,
        signature: &[u8; 65],
        app_data: &str,
    ) -> Result<String, Error> {
        let body = json!({
            "sellToken": hex::encode_prefixed(order.sellToken),
            "buyToken": hex::encode_prefixed(order.buyToken),
            "receiver": hex::encode_prefixed(order.receiver),
            "sellAmount": order.sellAmount.to_string(),
            "buyAmount": order.buyAmount.to_string(),
            "validTo": order.validTo,
            "appData": app_data,
            "appDataHash": hex::encode_prefixed(order.appData),
            "feeAmount": order.feeAmount.to_string(),
            "kind": order.kind,
            "partiallyFillable": order.partiallyFillable,
            "sellTokenBalance": order.sellTokenBalance,
            "buyTokenBalance": order.buyTokenBalance,
            "signingScheme": "eip712",
            "signature": hex::encode_prefixed(signature),
            "from": hex::encode_prefixed(from),
            // Prove the hooks deliver the whole sell amount, not the default 1-100 atoms.
            "fullBalanceCheck": true,
        });
        let url = format!("{}/orders", self.base);
        self.send("place order", || self.http.post(&url).json(&body)).await
    }

    pub(crate) async fn order_status(&self, uid: &str) -> Result<OrderStatus, Error> {
        let url = format!("{}/orders/{uid}", self.base);
        let record: OrderRecord = self.send("order status", || self.http.get(&url)).await?;
        Ok(record.status)
    }

    /// Transport errors, 5xx, 403 and 429 mean "CoW is not available" and are retried briefly; other 4xx are final.
    async fn send<T: DeserializeOwned>(
        &self,
        what: &'static str,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<T, Error> {
        (|| self.once(&build))
            .retry(remote_data::backoff_expo_short_delay())
            .when(|err| matches!(err, Error::CowUnavailable(_)))
            .notify(|err, delay| tracing::warn!(?err, ?delay, what, "cow api request failed, retrying..."))
            .await
    }

    async fn once<T: DeserializeOwned>(&self, build: &impl Fn() -> reqwest::RequestBuilder) -> Result<T, Error> {
        let response = build()
            .timeout(self.timeout)
            .headers(remote_data::json_headers())
            .send()
            .await
            .map_err(|e| Error::CowUnavailable(e.to_string()))?;
        let status = response.status();
        let wait = retry_after(response.headers());
        let body = response
            .text()
            .await
            .map_err(|e| Error::CowUnavailable(e.to_string()))?;
        if status.is_success() {
            return serde_json::from_str(&body).map_err(|e| Error::Api {
                status: status.as_u16(),
                error_type: "InvalidResponse".into(),
                description: e.to_string(),
            });
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            time::sleep(wait).await;
            return Err(Error::CowUnavailable(format!("rate limited ({status})")));
        }
        if status.is_server_error() || status == StatusCode::FORBIDDEN {
            return Err(Error::CowUnavailable(format!("{status}: {body}")));
        }
        let (error_type, description) = serde_json::from_str::<ApiErrorBody>(&body)
            .map(|e| (e.error_type, e.description))
            .unwrap_or_else(|_| (String::new(), body));
        Err(Error::Api {
            status: status.as_u16(),
            error_type,
            description,
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::address;

    use super::*;

    fn record(status: OrderStatus, valid_to: u32, app_code: Option<&str>) -> OrderRecord {
        OrderRecord {
            uid: format!("0x{valid_to:x}"),
            status,
            valid_to,
            full_app_data: app_code.map(|c| format!(r#"{{"appCode":"{c}","metadata":{{}}}}"#)),
            interactions: Interactions::default(),
        }
    }

    #[test]
    fn live_order_prefers_an_open_order_of_ours() {
        let orders = vec![
            record(OrderStatus::Open, 2_000, Some("other-app")),
            record(OrderStatus::Open, 1_010, Some(APP_CODE)),
            record(OrderStatus::Expired, 2_000, Some(APP_CODE)),
            record(OrderStatus::Open, 2_000, Some(APP_CODE)),
            record(OrderStatus::Open, 3_000, None),
        ];
        assert_eq!(live_order(&orders, 1_060).unwrap().valid_to, 2_000);
        assert!(live_order(&orders, 2_000).is_none());
    }

    #[test]
    fn has_filled_order_detects_a_permit_that_ran() {
        assert!(!has_filled_order(&[record(
            OrderStatus::Fulfilled,
            1,
            Some("other-app")
        )]));
        assert!(!has_filled_order(&[record(OrderStatus::Expired, 1, Some(APP_CODE))]));
        assert!(has_filled_order(&[record(OrderStatus::Fulfilled, 1, Some(APP_CODE))]));
    }

    #[test]
    fn order_record_deserialises_the_orderbook_shape() {
        let json = r#"{"uid":"0xab","status":"presignaturePending","validTo":1790864944,
            "fullAppData":"{\"appCode\":\"gnosis_vpn\"}",
            "interactions":{"pre":[{"target":"0xD057604A14982FE8D88c5fC25Aac3267eA142a08","value":"0","callData":"0x"}],"post":[]},
            "owner":"0x0000000000000000000000000000000000000001","kind":"sell"}"#;
        let record: OrderRecord = serde_json::from_str(json).unwrap();
        assert_eq!(record.status, OrderStatus::PresignaturePending);
        assert!(is_ours(&record));
        assert!(record.has_hook_target(address!("d057604a14982fe8d88c5fc25aac3267ea142a08")));
        let unknown: OrderRecord = serde_json::from_str(r#"{"uid":"0x","status":"brandNew","validTo":1}"#).unwrap();
        assert_eq!(unknown.status, OrderStatus::Unknown);
    }

    #[test]
    fn quote_response_deserialises_the_captured_shape() {
        let json = r#"{"quote":{"sellToken":"0xd057604a14982fe8d88c5fc25aac3267ea142a08","buyToken":"0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "sellAmount":"734454209529933046","buyAmount":"10000000000000000","validTo":1790931183,"appData":"0x00",
            "feeAmount":"5840216480253","kind":"buy","partiallyFillable":false,"signingScheme":"eip712"},
            "from":"0x0000000000000000000000000000000000000001","expiration":"2026-10-02T06:36:33Z","id":213768498,"verified":false,"protocolFeeBps":"2"}"#;
        let response: QuoteResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            parse_u256(&response.quote.sell_amount).unwrap(),
            U256::from(734_454_209_529_933_046u64)
        );
        assert_eq!(
            parse_u256(&response.quote.fee_amount).unwrap(),
            U256::from(5_840_216_480_253u64)
        );
        assert_eq!(response.quote.valid_to, 1_790_931_183);
        assert!(parse_u256("12.5").is_err());
    }

    #[test]
    fn retry_after_is_parsed_and_capped() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(retry_after(&headers), Duration::from_secs(2));
        headers.insert(RETRY_AFTER, "7".parse().unwrap());
        assert_eq!(retry_after(&headers), Duration::from_secs(7));
        headers.insert(RETRY_AFTER, "600".parse().unwrap());
        assert_eq!(retry_after(&headers), RETRY_AFTER_CAP);
    }

    #[test]
    fn base_url_loses_its_trailing_slash() {
        let client = Client::new(
            "https://barn.api.cow.fi/xdai/api/v1/".parse().unwrap(),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(client.base, "https://barn.api.cow.fi/xdai/api/v1");
    }

    /// Read-only against the configured orderbook (`GNOSISVPN_COW_API_URL`, default production):
    /// the quoted fee must grow by the hook gas when the appData carries hooks.
    #[tokio::test]
    #[ignore = "talks to api.cow.fi"]
    async fn a_quote_with_hooks_includes_their_gas_in_the_fee() {
        use crate::cowswap::{DEFAULT_API_URL, ENV_VAR_API_URL, hooks};

        let base = std::env::var(ENV_VAR_API_URL).unwrap_or_else(|_| DEFAULT_API_URL.to_string());
        let client = Client::new(base.parse().unwrap(), Duration::from_secs(20)).unwrap();
        let xhopr = address!("d057604a14982fe8d88c5fc25aac3267ea142a08");
        let eoa = address!("0000000000000000000000000000000000000001");
        let target = U256::from(10_000_000_000_000_000u64);
        let validity = Duration::from_secs(1800);
        let plain = client
            .quote(xhopr, eoa, target, validity, &crate::cowswap::order::app_data_json(&[]))
            .await
            .unwrap();
        let hooked = client
            .quote(
                xhopr,
                eoa,
                target,
                validity,
                &crate::cowswap::order::app_data_json(&hooks::placeholders(eoa, xhopr, true)),
            )
            .await
            .unwrap();
        assert!(plain.sell_amount > U256::ZERO);
        assert!(
            hooked.fee_amount > plain.fee_amount,
            "hooks {hooked:?} vs plain {plain:?}"
        );
    }
}
