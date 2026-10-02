//! GPv2 order typing, EIP-712 digest and uid, appData document, amount math. Pure.

use edgli::hopr_lib::api::types::chain::exports::alloy;
use edgli::hopr_lib::api::types::primitive::prelude::{Balance, Currency};
use serde::Serialize;

use alloy::primitives::{Address, B256, U256, keccak256};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::{Eip712Domain, SolStruct, eip712_domain};

use super::hooks::Hook;
use super::{APP_CODE, BUY_NATIVE, CHAIN_ID, Error, SETTLEMENT};

const APP_DATA_VERSION: &str = "1.6.0";

sol! {
    // String fields on purpose: GPv2Order hashes `kind`/balances as strings, so the type hash matches.
    struct Order {
        address sellToken;
        address buyToken;
        address receiver;
        uint256 sellAmount;
        uint256 buyAmount;
        uint32 validTo;
        bytes32 appData;
        uint256 feeAmount;
        string kind;
        bool partiallyFillable;
        string sellTokenBalance;
        string buyTokenBalance;
    }
}

pub(crate) fn domain() -> Eip712Domain {
    eip712_domain! {
        name: "Gnosis Protocol",
        version: "v2",
        chain_id: CHAIN_ID,
        verifying_contract: SETTLEMENT,
    }
}

#[derive(Serialize)]
struct AppData<'a> {
    #[serde(rename = "appCode")]
    app_code: &'a str,
    metadata: Metadata<'a>,
    version: &'a str,
}

#[derive(Serialize)]
struct Metadata<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    hooks: Option<Hooks<'a>>,
}

#[derive(Serialize)]
struct Hooks<'a> {
    pre: &'a [Hook],
}

/// The exact bytes posted with the order; their keccak is the signed `appData`.
pub(crate) fn app_data_json(pre: &[Hook]) -> String {
    let doc = AppData {
        app_code: APP_CODE,
        metadata: Metadata {
            hooks: (!pre.is_empty()).then_some(Hooks { pre }),
        },
        version: APP_DATA_VERSION,
    };
    serde_json::to_string(&doc).expect("app data serialises")
}

pub(crate) fn app_data_hash(json: &str) -> B256 {
    keccak256(json.as_bytes())
}

pub(crate) fn sell_order(
    sell_token: Address,
    receiver: Address,
    sell_amount: U256,
    buy_amount: U256,
    valid_to: u32,
    app_data: B256,
) -> Order {
    Order {
        sellToken: sell_token,
        buyToken: BUY_NATIVE,
        receiver,
        sellAmount: sell_amount,
        buyAmount: buy_amount,
        validTo: valid_to,
        appData: app_data,
        feeAmount: U256::ZERO,
        kind: "sell".into(),
        partiallyFillable: false,
        sellTokenBalance: "erc20".into(),
        buyTokenBalance: "erc20".into(),
    }
}

/// `digest ‖ owner ‖ validTo` - what the orderbook returns on placement and what hooks reference.
pub(crate) fn order_uid(order: &Order, owner: Address) -> [u8; 56] {
    let mut uid = [0u8; 56];
    uid[..32].copy_from_slice(order.eip712_signing_hash(&domain()).as_slice());
    uid[32..52].copy_from_slice(owner.as_slice());
    uid[52..].copy_from_slice(&order.validTo.to_be_bytes());
    uid
}

pub(crate) fn uid_hex(uid: &[u8; 56]) -> String {
    alloy::primitives::hex::encode_prefixed(uid)
}

pub(crate) fn sign_order(order: &Order, signer: &PrivateKeySigner) -> Result<[u8; 65], Error> {
    let signature = signer
        .sign_hash_sync(&order.eip712_signing_hash(&domain()))
        .map_err(|e| Error::Signing(e.to_string()))?;
    Ok(signature.as_bytes())
}

pub(crate) fn slippage_bps(fraction: f64) -> u32 {
    (fraction * 10_000.0).round() as u32
}

/// Quote amounts are pre-fee; the signed fee is zero, so the fee and the buffer go into the sell side.
pub(crate) fn sell_amount(quote_sell: U256, quote_fee: U256, slippage_bps: u32) -> U256 {
    ((quote_sell + quote_fee) * U256::from(10_000 + slippage_bps)).div_ceil(U256::from(10_000))
}

/// Two primitive-types versions are locked, so the conversion goes through the decimal string.
pub(crate) fn wei<C: Currency>(balance: Balance<C>) -> U256 {
    U256::from_str_radix(&balance.amount().to_string(), 10).expect("balance amount is a decimal integer")
}

/// How much xHOPR the Safe must send and how much wxHOPR it must unwrap for that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sizing {
    pub need: U256,
    pub unwrap: U256,
}

impl Sizing {
    /// xHOPR already on the EOA (a hook someone executed early) is sold first, then Safe xHOPR left by an expired order.
    pub(crate) fn new(sell: U256, eoa_xhopr: U256, safe_xhopr: U256) -> Self {
        let need = sell.saturating_sub(eoa_xhopr);
        Self {
            need,
            unwrap: need.saturating_sub(safe_xhopr),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy::primitives::{address, b256};
    use edgli::hopr_lib::api::types::primitive::prelude::XDaiBalance;

    use super::*;
    use crate::cowswap::hooks::tests::test_signer;

    const OWNER: Address = address!("c364f6b404d94e510a95064cfb0557708b181140");
    const XHOPR: Address = address!("d057604a14982fe8d88c5fc25aac3267ea142a08");

    #[test]
    fn domain_separator_matches_the_settlement_contract() {
        assert_eq!(
            domain().separator(),
            b256!("8f05589c4b810bc2f706854508d66d447cd971f8354a4bb0b3471ceb0a466bc7")
        );
    }

    #[test]
    fn order_type_hash_matches_gpv2() {
        assert_eq!(
            keccak256(Order::eip712_encode_type().as_bytes()),
            b256!("d5a25ba2e97094ad7d83dc28a6572da797d6b3e7fc6663bd93efb789fc17e489")
        );
    }

    #[test]
    fn order_uid_is_digest_owner_valid_to() {
        let order = sell_order(
            XHOPR,
            OWNER,
            U256::from(1u64),
            U256::from(2u64),
            1_790_864_944,
            B256::ZERO,
        );
        let uid = order_uid(&order, OWNER);
        assert_eq!(&uid[..32], order.eip712_signing_hash(&domain()).as_slice());
        assert_eq!(&uid[32..52], OWNER.as_slice());
        assert_eq!(&uid[52..], &1_790_864_944u32.to_be_bytes());
        assert_eq!(uid_hex(&uid).len(), 2 + 112);
    }

    #[test]
    fn order_signature_recovers_the_owner() {
        let signer = test_signer();
        let order = sell_order(
            XHOPR,
            signer.address(),
            U256::from(1u64),
            U256::from(2u64),
            1,
            B256::ZERO,
        );
        let bytes = sign_order(&order, &signer).unwrap();
        assert!(bytes[64] == 27 || bytes[64] == 28);
        let signature = alloy::primitives::Signature::from_raw(&bytes).unwrap();
        let recovered = signature
            .recover_address_from_prehash(&order.eip712_signing_hash(&domain()))
            .unwrap();
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn app_data_json_is_byte_stable_and_hashes_consistently() {
        assert_eq!(
            app_data_json(&[]),
            r#"{"appCode":"gnosis_vpn","metadata":{},"version":"1.6.0"}"#
        );
        let hook = Hook {
            target: XHOPR,
            call_data: vec![0xd5, 0x05, 0xac, 0xcf],
            gas_limit: 100_000,
        };
        let json = app_data_json(&[hook]);
        assert_eq!(
            json,
            r#"{"appCode":"gnosis_vpn","metadata":{"hooks":{"pre":[{"target":"0xd057604a14982fe8d88c5fc25aac3267ea142a08","callData":"0xd505accf","gasLimit":"100000"}]}},"version":"1.6.0"}"#
        );
        assert_eq!(app_data_hash(&json), keccak256(json.as_bytes()));
        assert_eq!(
            app_data_hash("{}"),
            b256!("b48d38f93eaa084033fc5970bf96e559c33c4cdc07d889ab00b4d63f9590739d")
        );
    }

    #[test]
    fn sell_amount_adds_fee_then_slippage() {
        let sell = U256::from(714_798_325_619_277_984u64);
        let fee = U256::from(1_922_012_763_219u64);
        assert_eq!(sell_amount(sell, fee, 200), U256::from(729_096_252_584_682_028u64));
        assert_eq!(sell_amount(sell, fee, 0), sell + fee);
        // Rounds up: 1 wei at 1 bps must not vanish.
        assert_eq!(sell_amount(U256::from(1u64), U256::ZERO, 1), U256::from(2u64));
    }

    #[test]
    fn slippage_fraction_converts_to_whole_bps() {
        assert_eq!(slippage_bps(0.02), 200);
        assert_eq!(slippage_bps(0.005), 50);
        assert_eq!(slippage_bps(0.0), 0);
    }

    #[test]
    fn sizing_sells_eoa_xhopr_first_then_safe_xhopr_then_unwraps() {
        let sell = U256::from(100u64);
        assert_eq!(
            Sizing::new(sell, U256::ZERO, U256::ZERO),
            Sizing {
                need: sell,
                unwrap: sell
            }
        );
        assert_eq!(
            Sizing::new(sell, U256::ZERO, U256::from(30u64)),
            Sizing {
                need: sell,
                unwrap: U256::from(70u64)
            }
        );
        assert_eq!(
            Sizing::new(sell, U256::from(40u64), U256::from(30u64)),
            Sizing {
                need: U256::from(60u64),
                unwrap: U256::from(30u64)
            }
        );
        assert_eq!(
            Sizing::new(sell, U256::from(150u64), U256::ZERO),
            Sizing {
                need: U256::ZERO,
                unwrap: U256::ZERO
            }
        );
    }

    #[test]
    fn wei_converts_balances_exactly() {
        let target: XDaiBalance = "0.01 xDai".parse().unwrap();
        assert_eq!(wei(target), U256::from(10_000_000_000_000_000u64));
    }
}
