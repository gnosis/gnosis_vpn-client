use edgli::hopr_utils_chain_connector::reexports::alloy::{
    contract::Error as ContractError,
    providers::{MulticallError, PendingTransactionError},
    signers::k256::ecdsa::Error as SigningKeyError,
    transports::{RpcError as AlloyRpcError, TransportErrorKind},
};

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error(transparent)]
    RpcUrlError(#[from] url::ParseError),

    #[error(transparent)]
    SigningKeyError(#[from] SigningKeyError),

    #[error(transparent)]
    AlloyRpcError(#[from] AlloyRpcError<TransportErrorKind>),

    #[error(transparent)]
    PendingTransactionError(#[from] PendingTransactionError),

    #[error(transparent)]
    ContractError(#[from] ContractError),

    #[error("Failed to decode event log: {0}")]
    DecodeEventError(String),

    #[error(transparent)]
    MulticallError(#[from] MulticallError),
}
