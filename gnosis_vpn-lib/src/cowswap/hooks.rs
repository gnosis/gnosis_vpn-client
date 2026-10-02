//! The two pre-hooks the solver executes for us: the owner-signed Safe unwrap and the xHOPR permit. Pure.

use edgli::hopr_lib::api::types::chain::exports::alloy;
use serde::{Serialize, Serializer};

use alloy::primitives::{Address, B256, Bytes, U256, hex};
use alloy::signers::SignerSync;
use alloy::signers::local::PrivateKeySigner;
use alloy::sol;
use alloy::sol_types::{Eip712Domain, SolCall, SolStruct, eip712_domain};

use super::chain::Tokens;
use super::{CHAIN_ID, Error, HOOK_PERMIT_GAS, HOOK_SAFE_GAS, HOPR_WRAPPER, MULTISEND, VAULT_RELAYER};

sol! {
    function execTransaction(
        address to,
        uint256 value,
        bytes data,
        uint8 operation,
        uint256 safeTxGas,
        uint256 baseGas,
        uint256 gasPrice,
        address gasToken,
        address refundReceiver,
        bytes signatures
    ) returns (bool);
    function multiSend(bytes transactions);
    function transfer(address to, uint256 amount) returns (bool);
    function permit(address owner, address spender, uint256 value, uint256 deadline, uint8 v, bytes32 r, bytes32 s);
}

mod safe_tx {
    super::sol! {
        struct SafeTx {
            address to;
            uint256 value;
            bytes data;
            uint8 operation;
            uint256 safeTxGas;
            uint256 baseGas;
            uint256 gasPrice;
            address gasToken;
            address refundReceiver;
            uint256 nonce;
        }
    }
}

mod permit_message {
    super::sol! {
        struct Permit {
            address owner;
            address spender;
            uint256 value;
            uint256 nonce;
            uint256 deadline;
        }
    }
}

const DELEGATE_CALL: u8 = 1;

/// One `metadata.hooks.pre` entry of the CoW appData document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Hook {
    pub target: Address,
    pub call_data: Vec<u8>,
    pub gas_limit: u64,
}

impl Serialize for Hook {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            target: String,
            call_data: String,
            // The appData schema types gasLimit as a decimal string.
            gas_limit: &'a str,
        }
        Wire {
            target: hex::encode_prefixed(self.target),
            call_data: hex::encode_prefixed(&self.call_data),
            gas_limit: &self.gas_limit.to_string(),
        }
        .serialize(serializer)
    }
}

/// Safe 1.4.1 domains have no name or version.
fn safe_domain(safe: Address) -> Eip712Domain {
    eip712_domain! {
        chain_id: CHAIN_ID,
        verifying_contract: safe,
    }
}

/// The verifying contract is the token proxy, not its PermittableToken implementation.
fn permit_domain(xhopr: Address) -> Eip712Domain {
    eip712_domain! {
        name: "HOPR Token on xDai",
        version: "1",
        chain_id: CHAIN_ID,
        verifying_contract: xhopr,
    }
}

/// `op ‖ to ‖ value ‖ len ‖ data`, the MultiSend packing.
pub(crate) fn multisend_entry(to: Address, data: &[u8]) -> Vec<u8> {
    let mut entry = Vec::with_capacity(85 + data.len());
    entry.push(0);
    entry.extend_from_slice(to.as_slice());
    entry.extend_from_slice(&[0u8; 32]);
    entry.extend_from_slice(&U256::from(data.len()).to_be_bytes::<32>());
    entry.extend_from_slice(data);
    entry
}

/// Hooks with the real gas limits but empty calldata, so a quote prices the hook gas before the amounts are known.
pub(crate) fn placeholders(safe: Address, xhopr: Address, with_permit: bool) -> Vec<Hook> {
    let mut pre = vec![Hook {
        target: safe,
        call_data: Vec::new(),
        gas_limit: HOOK_SAFE_GAS,
    }];
    if with_permit {
        pre.push(Hook {
            target: xhopr,
            call_data: Vec::new(),
            gas_limit: HOOK_PERMIT_GAS,
        });
    }
    pre
}

/// `Safe.execTransaction` → MultiSend[unwrap `unwrap` wxHOPR, send `need` xHOPR to the EOA], valid for anyone to submit.
pub(crate) fn safe_hook(
    signer: &PrivateKeySigner,
    safe: Address,
    eoa: Address,
    tokens: Tokens,
    unwrap: U256,
    need: U256,
    nonce: u64,
) -> Result<Hook, Error> {
    let mut transactions = Vec::new();
    if unwrap > U256::ZERO {
        let unwrap_call = transferCall {
            to: HOPR_WRAPPER,
            amount: unwrap,
        };
        transactions.extend(multisend_entry(tokens.wxhopr, &unwrap_call.abi_encode()));
    }
    let send_call = transferCall { to: eoa, amount: need };
    transactions.extend(multisend_entry(tokens.xhopr, &send_call.abi_encode()));
    let data = multiSendCall {
        transactions: transactions.into(),
    }
    .abi_encode();

    let tx = safe_tx::SafeTx {
        to: MULTISEND,
        value: U256::ZERO,
        data: data.clone().into(),
        operation: DELEGATE_CALL,
        safeTxGas: U256::ZERO,
        baseGas: U256::ZERO,
        gasPrice: U256::ZERO,
        gasToken: Address::ZERO,
        refundReceiver: Address::ZERO,
        nonce: U256::from(nonce),
    };
    let signature = signer
        .sign_hash_sync(&tx.eip712_signing_hash(&safe_domain(safe)))
        .map_err(|e| Error::Signing(e.to_string()))?;
    let call = execTransactionCall {
        to: MULTISEND,
        value: U256::ZERO,
        data: data.into(),
        operation: DELEGATE_CALL,
        safeTxGas: U256::ZERO,
        baseGas: U256::ZERO,
        gasPrice: U256::ZERO,
        gasToken: Address::ZERO,
        refundReceiver: Address::ZERO,
        signatures: Bytes::copy_from_slice(&signature.as_bytes()),
    };
    Ok(Hook {
        target: safe,
        call_data: call.abi_encode(),
        gas_limit: HOOK_SAFE_GAS,
    })
}

/// EIP-2612 permit for the vault relayer; max value never decrements, so one permit serves every later order.
pub(crate) fn permit_hook(signer: &PrivateKeySigner, eoa: Address, xhopr: Address, nonce: u64) -> Result<Hook, Error> {
    let message = permit_message::Permit {
        owner: eoa,
        spender: VAULT_RELAYER,
        value: U256::MAX,
        nonce: U256::from(nonce),
        deadline: U256::MAX,
    };
    let signature = signer
        .sign_hash_sync(&message.eip712_signing_hash(&permit_domain(xhopr)))
        .map_err(|e| Error::Signing(e.to_string()))?;
    let bytes = signature.as_bytes();
    let call = permitCall {
        owner: eoa,
        spender: VAULT_RELAYER,
        value: U256::MAX,
        deadline: U256::MAX,
        v: bytes[64],
        r: B256::from_slice(&bytes[..32]),
        s: B256::from_slice(&bytes[32..64]),
    };
    Ok(Hook {
        target: xhopr,
        call_data: call.abi_encode(),
        gas_limit: HOOK_PERMIT_GAS,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use alloy::primitives::{Signature, address, b256, keccak256};
    use alloy::sol_types::SolValue;

    use super::*;

    const SAFE: Address = address!("6bc16d6a3cb94fcb63fb5d56e09039c984185d0a");
    const OWNER: Address = address!("c364f6b404d94e510a95064cfb0557708b181140");
    const WXHOPR: Address = address!("d4fdec44db9d44b8f2b6d529620f9c0c7066a2c1");
    const XHOPR: Address = address!("d057604a14982fe8d88c5fc25aac3267ea142a08");

    /// The well-known key `0x…01`, address 0x7E5F4552091A69125d5DfCb7b8C2659029395Bdf.
    pub(crate) fn test_signer() -> PrivateKeySigner {
        let mut key = [0u8; 32];
        key[31] = 1;
        PrivateKeySigner::from_slice(&key).unwrap()
    }

    fn tokens() -> Tokens {
        Tokens {
            wxhopr: WXHOPR,
            xhopr: XHOPR,
        }
    }

    #[test]
    fn safe_domain_type_is_chain_id_and_contract_only() {
        assert_eq!(
            safe_domain(SAFE).encode_type(),
            "EIP712Domain(uint256 chainId,address verifyingContract)"
        );
        assert_eq!(
            safe_domain(SAFE).type_hash(),
            b256!("47e79534a245952e8b16893a336b85a3d9ea9fa8c573f3d803afb92a79469218")
        );
    }

    #[test]
    fn safe_tx_type_hash_matches_v141() {
        assert_eq!(
            keccak256(safe_tx::SafeTx::eip712_encode_type().as_bytes()),
            b256!("bb8310d486368db6bd6f849402fdd73ad53d316b5a4b2644ad6efe0f941286d8")
        );
    }

    /// Vector taken from the live jura-prod Safe at nonce 5: 0.72 wxHOPR unwrapped, 0.72 xHOPR to the owner.
    #[test]
    fn the_jura_safe_tx_vector_is_reproduced() {
        let amount = U256::from(720_000_000_000_000_000u64);
        let hook = safe_hook(&test_signer(), SAFE, OWNER, tokens(), amount, amount, 5).unwrap();
        let call = execTransactionCall::abi_decode(&hook.call_data).unwrap();
        let tx = safe_tx::SafeTx {
            to: call.to,
            value: call.value,
            data: call.data.clone(),
            operation: call.operation,
            safeTxGas: call.safeTxGas,
            baseGas: call.baseGas,
            gasPrice: call.gasPrice,
            gasToken: call.gasToken,
            refundReceiver: call.refundReceiver,
            nonce: U256::from(5u64),
        };
        assert_eq!(
            tx.eip712_hash_struct(),
            b256!("c4cbe3448cba065c5a3fc6530f25ec285c42b9f98435a40bc4b7d82e2fa74f0e")
        );
        let signed_hash = tx.eip712_signing_hash(&safe_domain(SAFE));
        assert_eq!(
            signed_hash,
            b256!("2cd50fe89500edd19038fc160f91842d2f3af5ef286ec08fdeb868586cd18252")
        );
        let signature = Signature::from_raw(&call.signatures).unwrap();
        assert_eq!(call.signatures[64], 0x1b);
        assert_eq!(
            signature.recover_address_from_prehash(&signed_hash).unwrap(),
            address!("7e5f4552091a69125d5dfcb7b8c2659029395bdf")
        );
    }

    #[test]
    fn safe_hook_calldata_decodes_back() {
        let hook = safe_hook(
            &test_signer(),
            SAFE,
            OWNER,
            tokens(),
            U256::from(3u64),
            U256::from(5u64),
            7,
        )
        .unwrap();
        assert_eq!(hook.target, SAFE);
        assert_eq!(hook.gas_limit, HOOK_SAFE_GAS);
        let call = execTransactionCall::abi_decode(&hook.call_data).unwrap();
        assert_eq!(call.to, MULTISEND);
        assert_eq!(call.operation, DELEGATE_CALL);
        assert!(call.value.is_zero() && call.safeTxGas.is_zero() && call.gasPrice.is_zero());
        assert_eq!(call.signatures.len(), 65);
        assert!(matches!(call.signatures[64], 27 | 28));
        let batch = multiSendCall::abi_decode(&call.data).unwrap().transactions;
        let unwrap_data = transferCall {
            to: HOPR_WRAPPER,
            amount: U256::from(3u64),
        }
        .abi_encode();
        let send_data = transferCall {
            to: OWNER,
            amount: U256::from(5u64),
        }
        .abi_encode();
        let mut expected = multisend_entry(WXHOPR, &unwrap_data);
        expected.extend(multisend_entry(XHOPR, &send_data));
        assert_eq!(batch.to_vec(), expected);
    }

    #[test]
    fn safe_hook_without_unwrap_only_sends_xhopr() {
        let hook = safe_hook(&test_signer(), SAFE, OWNER, tokens(), U256::ZERO, U256::from(5u64), 7).unwrap();
        let call = execTransactionCall::abi_decode(&hook.call_data).unwrap();
        let batch = multiSendCall::abi_decode(&call.data).unwrap().transactions;
        assert_eq!(batch.len(), 85 + 68);
        assert_eq!(&batch[1..21], XHOPR.as_slice());
    }

    #[test]
    fn multisend_entry_packs_op_to_value_len_data() {
        let entry = multisend_entry(XHOPR, &[1, 2, 3]);
        assert_eq!(entry.len(), 88);
        assert_eq!(entry[0], 0);
        assert_eq!(&entry[1..21], XHOPR.as_slice());
        assert!(entry[21..53].iter().all(|b| *b == 0));
        assert_eq!(entry[53..85], U256::from(3u64).to_be_bytes::<32>());
        assert_eq!(&entry[85..], &[1, 2, 3]);
    }

    #[test]
    fn permit_domain_separator_matches_the_token() {
        assert_eq!(
            permit_domain(XHOPR).separator(),
            b256!("9fa511986d5bb98924889e00c2c64692150095e26c511cd279ffe79f60499d1f")
        );
    }

    #[test]
    fn permit_type_hash_matches_eip2612() {
        assert_eq!(
            keccak256(permit_message::Permit::eip712_encode_type().as_bytes()),
            b256!("6e71edae12b1b97f4d1f60370fef10105fa2faae0126114a169c64845d6126c9")
        );
    }

    #[test]
    fn permit_hook_signature_recovers_the_owner() {
        let signer = test_signer();
        let hook = permit_hook(&signer, signer.address(), XHOPR, 0).unwrap();
        assert_eq!(hook.target, XHOPR);
        assert_eq!(hook.gas_limit, HOOK_PERMIT_GAS);
        let call = permitCall::abi_decode(&hook.call_data).unwrap();
        assert_eq!((call.owner, call.spender), (signer.address(), VAULT_RELAYER));
        assert_eq!((call.value, call.deadline), (U256::MAX, U256::MAX));
        let message = permit_message::Permit {
            owner: signer.address(),
            spender: VAULT_RELAYER,
            value: U256::MAX,
            nonce: U256::ZERO,
            deadline: U256::MAX,
        };
        let mut raw = [0u8; 65];
        raw[..32].copy_from_slice(call.r.as_slice());
        raw[32..64].copy_from_slice(call.s.as_slice());
        raw[64] = call.v;
        let recovered = Signature::from_raw(&raw)
            .unwrap()
            .recover_address_from_prehash(&message.eip712_signing_hash(&permit_domain(XHOPR)))
            .unwrap();
        assert_eq!(recovered, signer.address());
    }

    #[test]
    fn selectors_match_the_safe_cow_and_token_abis() {
        assert_eq!(execTransactionCall::SELECTOR, [0x6a, 0x76, 0x12, 0x02]);
        assert_eq!(multiSendCall::SELECTOR, [0x8d, 0x80, 0xff, 0x0a]);
        assert_eq!(transferCall::SELECTOR, [0xa9, 0x05, 0x9c, 0xbb]);
        assert_eq!(permitCall::SELECTOR, [0xd5, 0x05, 0xac, 0xcf]);
    }

    #[test]
    fn hooks_serialise_to_the_app_data_shape() {
        let hook = Hook {
            target: XHOPR,
            call_data: vec![0xab, 0xcd],
            gas_limit: 100_000,
        };
        assert_eq!(
            serde_json::to_string(&hook).unwrap(),
            r#"{"target":"0xd057604a14982fe8d88c5fc25aac3267ea142a08","callData":"0xabcd","gasLimit":"100000"}"#
        );
        assert_eq!(placeholders(SAFE, XHOPR, true).len(), 2);
        assert_eq!(placeholders(SAFE, XHOPR, false).len(), 1);
    }

    #[test]
    fn transfer_amount_round_trips_through_abi() {
        let data = transferCall {
            to: OWNER,
            amount: U256::from(42u64),
        }
        .abi_encode();
        assert_eq!(data.len(), 68);
        assert_eq!(U256::abi_decode(&data[36..]).unwrap(), U256::from(42u64));
    }
}
