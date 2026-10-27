//! Gasless xDAI refuel: sells a little Safe wxHOPR on CoW Protocol for native xDAI paid to the node EOA.
//!
//! The node never sends a transaction. The order is owned by the EOA and carries two pre-hooks the
//! solver executes and pays for: an owner-signed Safe transaction that unwraps wxHOPR and moves the
//! xHOPR to the EOA, and an xHOPR permit for CoW's vault relayer.

pub(crate) mod api;
pub(crate) mod chain;
pub(crate) mod hooks;
pub(crate) mod order;

use edgli::hopr_lib::api::types::chain::exports::alloy;
use edgli::hopr_lib::api::types::primitive::prelude::{Address, XDaiBalance};
use edgli::hopr_lib::builder::Keypair;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time;

use alloy::primitives::{Address as EvmAddress, U256, address, keccak256, uint};
use alloy::signers::local::PrivateKeySigner;

use std::fmt::{self, Display};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::worker_params::{self, WorkerParams};
use crate::{balance, balance::FundingLevel};

pub const ENV_VAR_API_URL: &str = "GNOSISVPN_COW_API_URL";
pub const DEFAULT_API_URL: &str = "https://api.cow.fi/xdai/api/v1";

pub(crate) const CHAIN_ID: u64 = 100;
pub(crate) const SETTLEMENT: EvmAddress = address!("9008d19f58aabd9ed0d60971565aa8510560ab41");
pub(crate) const VAULT_RELAYER: EvmAddress = address!("c92e8bdf79f0507f65a392b0ab4667716bfe0110");
/// MultiSend 1.4.1 - the Safe delegatecalls it; every entry is a plain call.
pub(crate) const MULTISEND: EvmAddress = address!("38869bf66a61cf6bdb996a6ae40d5853fd43b526");
/// Unwraps wxHOPR it receives back into xHOPR for the sender; hard-wired to the two tokens below.
pub(crate) const HOPR_WRAPPER: EvmAddress = address!("097707143e01318734535676cfe2e5cf8b656ae8");
pub(crate) const WXHOPR_TOKEN: EvmAddress = address!("d4fdec44db9d44b8f2b6d529620f9c0c7066a2c1");
pub(crate) const XHOPR_TOKEN: EvmAddress = address!("d057604a14982fe8d88c5fc25aac3267ea142a08");
/// CoW's sentinel for buying the native token.
pub(crate) const BUY_NATIVE: EvmAddress = address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
pub(crate) const APP_CODE: &str = "gnosis_vpn";
/// Measured ≈ 290k inside the hook call (unwrap via the ERC777 receive hook dominates).
pub(crate) const HOOK_SAFE_GAS: u64 = 350_000;
/// Measured ≈ 64k for a first permit.
pub(crate) const HOOK_PERMIT_GAS: u64 = 100_000;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Indexing lag past validTo before an unobserved order counts as expired.
const EXPIRY_GRACE: Duration = Duration::from_secs(120);
/// Too close to validTo to be worth resuming.
const MIN_REMAINING_VALIDITY: u64 = 60;
const PERMIT_NONCE_PROBES: u32 = 3;
/// Tripwire against a corrupt quote: ~70x the 0.01 xDAI sell.
const MAX_SELL_WEI: U256 = uint!(50_000_000_000_000_000_000_U256);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RefuelConfig {
    pub enabled: bool,
    /// xDAI bought per refuel; lands an Empty EOA well above the Low line.
    pub target: XDaiBalance,
    /// Sell-side buffer as a fraction; unused buffer comes back as xDAI surplus.
    pub slippage: f64,
    /// Gap after a failed or expired attempt before the trigger may fire again; a fill does not wait.
    pub cooldown: Duration,
    /// validFor of the order; a stale order expires on its own, nothing cancels it.
    pub order_validity: Duration,
    /// Bound on one api.cow.fi request; the Linux killswitch drops silently.
    pub request_timeout: Duration,
}

impl Default for RefuelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target: "0.01 xDai".parse().expect("valid default refuel target"),
            slippage: 0.02,
            cooldown: Duration::from_secs(30 * 60),
            order_validity: Duration::from_secs(30 * 60),
            request_timeout: Duration::from_secs(20),
        }
    }
}

#[derive(Debug)]
pub(crate) enum State {
    Idle,
    InFlight,
    CoolingDown(SystemTime),
    /// A permanent error (wrong chain, our own encoding); stays until the worker restarts.
    Disabled,
}

impl State {
    pub(crate) fn cooldown_remaining(&self, cooldown: Duration, now: SystemTime) -> Option<Duration> {
        match self {
            State::CoolingDown(at) => cooldown.checked_sub(now.duration_since(*at).unwrap_or_default()),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) enum Outcome {
    Filled {
        uid: String,
    },
    /// Gas was no longer Empty when the run looked - a fill landed unnoticed.
    AlreadyRefueled,
}

impl Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Outcome::Filled { uid } => write!(f, "Filled({uid})"),
            Outcome::AlreadyRefueled => write!(f, "AlreadyRefueled"),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum Error {
    #[error(transparent)]
    WorkerParams(#[from] worker_params::Error),
    #[error("cow api unavailable: {0}")]
    CowUnavailable(String),
    #[error("cow api {status} {error_type}: {description}")]
    Api {
        status: u16,
        error_type: String,
        description: String,
    },
    #[error("blokli error: {0}")]
    Blokli(String),
    #[error("refuel only supports gnosis chain, blokli reports chain id {0}")]
    UnsupportedChain(u64),
    #[error("token deployment unknown to the wrapper: token {token}, xhopr {xhopr}")]
    UnexpectedTokens { token: EvmAddress, xhopr: EvmAddress },
    #[error("safe wxHOPR too low: need {needed} wei, have {available} wei")]
    InsufficientWxhopr { needed: U256, available: U256 },
    #[error("quote implies an implausible sell amount of {0} wei")]
    SellTooLarge(U256),
    #[error("order uid mismatch: local {local}, server {server}")]
    UidMismatch { local: String, server: String },
    #[error("permit nonce not found yet, next run probes from {next_nonce}")]
    PermitNonceUnknown { next_nonce: u64 },
    #[error("safe hook rejected by the orderbook simulation")]
    SafeHookRejected,
    #[error("order {0} expired unfilled")]
    Expired(String),
    #[error("signing error: {0}")]
    Signing(String),
}

impl Error {
    /// Errors a retry cannot fix: wrong deployment or our own encoding.
    pub(crate) fn is_permanent(&self) -> bool {
        match self {
            Error::UnsupportedChain(_) | Error::UnexpectedTokens { .. } | Error::UidMismatch { .. } => true,
            Error::Api { error_type, .. } => matches!(
                error_type.as_str(),
                "TooMuchGas"
                    | "InvalidAppData"
                    | "AppDataHashMismatch"
                    | "NonZeroFee"
                    | "InvalidSignature"
                    | "WrongOwner"
                    | "SameBuyAndSellToken"
                    | "ZeroAmount"
            ),
            _ => false,
        }
    }
}

fn evm(address: Address) -> EvmAddress {
    EvmAddress::from_slice(address.as_ref())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// One refuel attempt. `permit_nonce` is where the xHOPR permit nonce probe left off last time.
pub(crate) async fn refuel(
    worker_params: &WorkerParams,
    blokli_timeout: Duration,
    cfg: &RefuelConfig,
    safe: Address,
    eoa: Address,
    permit_nonce: u64,
) -> Result<Outcome, Error> {
    let chain_key = worker_params.calc_keys().await?.chain_key;
    let signer =
        PrivateKeySigner::from_slice(chain_key.secret().as_ref()).map_err(|e| Error::Signing(e.to_string()))?;
    let blokli = chain::Client::new(&worker_params.blokli_endpoint(blokli_timeout))?;
    let api = api::Client::new(worker_params.cow_api_url(), cfg.request_timeout)?;
    let (safe, eoa) = (evm(safe), evm(eoa));

    let mut snapshot = blokli.snapshot(safe, eoa).await?;
    if balance::gas_level(snapshot.eoa_native) != FundingLevel::Empty {
        tracing::info!(native = %snapshot.eoa_native, "node xDAI is no longer empty - skipping refuel");
        return Ok(Outcome::AlreadyRefueled);
    }

    let history = api.account_orders(eoa).await?;
    let permit_granted = api::has_filled_order(&history);
    if let Some(open) = api::live_order(&history, unix_now() + MIN_REMAINING_VALIDITY) {
        tracing::info!(uid = %open.uid, "resuming the open refuel order from a previous run");
        await_fill(&api, &open.uid, open.valid_to).await?;
        return Ok(Outcome::Filled { uid: open.uid.clone() });
    }

    let target = order::wei(cfg.target);
    let placeholders = hooks::placeholders(safe, !permit_granted);
    let quote = api
        .quote(eoa, target, cfg.order_validity, &order::app_data_json(&placeholders))
        .await?;
    let sell = order::sell_amount(quote.sell_amount, quote.fee_amount, order::slippage_bps(cfg.slippage));
    if sell > MAX_SELL_WEI {
        return Err(Error::SellTooLarge(sell));
    }
    let valid_to = quote.valid_to;

    let mut with_permit = !permit_granted;
    let mut permit_nonce = permit_nonce;
    let mut probes = 0;
    let mut safe_rebuilt = false;
    let uid = loop {
        let sizing = order::Sizing::new(sell, snapshot.eoa_xhopr, snapshot.safe_xhopr);
        if snapshot.safe_wxhopr < sizing.unwrap {
            return Err(Error::InsufficientWxhopr {
                needed: sizing.unwrap,
                available: snapshot.safe_wxhopr,
            });
        }
        let mut pre = Vec::new();
        if sizing.need > U256::ZERO {
            pre.push(hooks::safe_hook(
                &signer,
                safe,
                eoa,
                sizing.unwrap,
                sizing.need,
                snapshot.safe_nonce,
            )?);
        }
        if with_permit {
            pre.push(hooks::permit_hook(&signer, eoa, permit_nonce)?);
        }
        let app_data = order::app_data_json(&pre);
        let order = order::sell_order(eoa, sell, target, valid_to, keccak256(app_data.as_bytes()));
        let uid = order::order_uid(&order, eoa);
        let signature = order::sign_order(&order, &signer)?;
        match api.post_order(&order, eoa, &signature, &app_data).await {
            Ok(server_uid) if server_uid.eq_ignore_ascii_case(&uid) => break uid,
            Ok(server_uid) => {
                return Err(Error::UidMismatch {
                    local: uid,
                    server: server_uid,
                });
            }
            Err(Error::Api { error_type, .. }) if error_type == "DuplicatedOrder" => break uid,
            // No allowance in simulation: the permit was missing, or signed with a stale nonce.
            Err(Error::Api { error_type, .. }) if error_type == "InsufficientAllowance" => {
                if with_permit {
                    permit_nonce += 1;
                } else {
                    with_permit = true;
                }
                probes += 1;
                if probes > PERMIT_NONCE_PROBES {
                    return Err(Error::PermitNonceUnknown {
                        next_nonce: permit_nonce,
                    });
                }
                tracing::debug!(permit_nonce, with_permit, "probing the xHOPR permit nonce");
            }
            // The safe hook did not deliver the xHOPR in simulation: re-read what it was built from.
            Err(Error::Api { error_type, .. }) if error_type == "InsufficientBalance" => {
                if safe_rebuilt {
                    return Err(Error::SafeHookRejected);
                }
                safe_rebuilt = true;
                snapshot = blokli.snapshot(safe, eoa).await?;
                tracing::debug!(
                    safe_nonce = snapshot.safe_nonce,
                    "rebuilding the safe hook after a rejected simulation"
                );
            }
            Err(err) => return Err(err),
        }
    };
    tracing::info!(%uid, %sell, valid_to, with_permit, "placed gasless cow order");
    await_fill(&api, &uid, valid_to).await?;
    Ok(Outcome::Filled { uid })
}

async fn await_fill(api: &api::Client, uid: &str, valid_to: u32) -> Result<(), Error> {
    let deadline = u64::from(valid_to) + EXPIRY_GRACE.as_secs();
    loop {
        time::sleep(POLL_INTERVAL).await;
        match api.order_status(uid).await {
            Ok(api::OrderStatus::Fulfilled) => return Ok(()),
            Ok(api::OrderStatus::Expired | api::OrderStatus::Cancelled) => return Err(Error::Expired(uid.to_string())),
            Ok(_) => {}
            // Expected while a tunnel comes up; the order fills on-chain without us watching.
            Err(err) => tracing::warn!(?err, %uid, "refuel order poll failed"),
        }
        if unix_now() > deadline {
            return Err(Error::Expired(uid.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cooldown_remaining_only_while_cooling_down() {
        let now = SystemTime::now();
        let cooldown = Duration::from_secs(1800);
        assert_eq!(State::Idle.cooldown_remaining(cooldown, now), None);
        assert_eq!(State::InFlight.cooldown_remaining(cooldown, now), None);
        assert_eq!(State::Disabled.cooldown_remaining(cooldown, now), None);
        let remaining = State::CoolingDown(now - Duration::from_secs(600)).cooldown_remaining(cooldown, now);
        assert_eq!(remaining, Some(Duration::from_secs(1200)));
        assert_eq!(
            State::CoolingDown(now - Duration::from_secs(1801)).cooldown_remaining(cooldown, now),
            None
        );
    }

    #[test]
    fn permanent_errors_are_classified() {
        assert!(Error::UnsupportedChain(1).is_permanent());
        assert!(
            Error::Api {
                status: 400,
                error_type: "InvalidAppData".into(),
                description: String::new(),
            }
            .is_permanent()
        );
        assert!(
            !Error::Api {
                status: 400,
                error_type: "InsufficientBalance".into(),
                description: String::new(),
            }
            .is_permanent()
        );
        assert!(!Error::CowUnavailable("dns".into()).is_permanent());
        assert!(!Error::Expired("0x".into()).is_permanent());
    }
}
