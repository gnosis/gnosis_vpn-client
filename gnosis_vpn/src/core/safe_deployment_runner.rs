use alloy::primitives::U256;
use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::ChainKeypair;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use rand::Rng;
use thiserror::Error;

use gnosis_vpn_lib::balance::{self};
use gnosis_vpn_lib::chain::client::GnosisRpcClient;
use gnosis_vpn_lib::chain::contracts::{NetworkSpecifications, SafeModuleDeploymentInputs, SafeModuleDeploymentResult};
use gnosis_vpn_lib::chain::errors::ChainError;

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct SafeDeploymentRunner {
    hopr_params: HoprParams,
    nonce: U256,
    presafe: balance::PreSafe,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
}

impl SafeDeploymentRunner {
    pub fn new(hopr_params: HoprParams, presafe: balance::PreSafe) -> Self {
        let nonce = U256::from(rand::rng().random_range(u128::MIN..u128::MAX));
        Self {
            hopr_params,
            nonce,
            presafe,
        }
    }

    pub async fn start(&self) -> Result<SafeModuleDeploymentResult, Error> {
        let keys = self.hopr_params.calc_keys()?;
        let private_key = keys.chain_key.clone();
        let rpc_provider = self.hopr_params.rpc_provider.clone();
        let node_address = keys.chain_key.public().to_address();
        let token_u256 = self.presafe.node_wxhopr.amount();
        let token_bytes: [u8; 32] = token_u256.to_big_endian();
        let token_amount: U256 = U256::from_be_bytes::<32>(token_bytes);
        safe_module_deployment(
            NetworkSpecifications::from_network(&self.hopr_params.network),
            private_key,
            rpc_provider.as_str(),
            node_address,
            self.nonce,
            token_amount,
        )
        .await
    }
}

async fn safe_module_deployment(
    network_specs: NetworkSpecifications,
    priv_key: ChainKeypair,
    rpc_provider: &str,
    node_address: Address,
    nonce: U256,
    token_amount: U256,
) -> Result<SafeModuleDeploymentResult, Error> {
    retry(ExponentialBackoff::default(), || async {
        let client = GnosisRpcClient::with_url(priv_key.clone(), rpc_provider)
            .await
            .map_err(Error::from)?;
        let safe_module_deployment_inputs =
            SafeModuleDeploymentInputs::new(nonce, token_amount, vec![node_address.into()]);
        let res = safe_module_deployment_inputs
            .deploy(&client.provider, network_specs.network.clone())
            .await
            .map_err(Error::from)?;

        Ok(res)
    })
    .await
}
