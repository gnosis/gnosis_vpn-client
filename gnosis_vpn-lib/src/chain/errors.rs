use alloy::{
    signers::k256::ecdsa::Error as SigningKeyError,
    transports::{RpcError as AlloyRpcError, TransportErrorKind},
};
use thiserror::Error;

#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error(transparent)]
    RpcUrlError(#[from] url::ParseError),

    #[error(transparent)]
    SigningKeyError(#[from] SigningKeyError),

    #[error(transparent)]
    AlloyRpcError(#[from] AlloyRpcError<TransportErrorKind>),
}
