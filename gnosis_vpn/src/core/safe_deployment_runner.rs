use alloy::primitives::U256;
use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::ChainKeypair;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use thiserror::Error;

use gnosis_vpn_lib::balance::{self};
use gnosis_vpn_lib::chain::client::GnosisRpcClient;
use gnosis_vpn_lib::chain::contracts::{
    CheckBalanceInputs, CheckBalanceResult, NetworkSpecifications, SafeModuleDeploymentInputs,
    SafeModuleDeploymentResult,
};
use gnosis_vpn_lib::chain::errors::ChainError;
use gnosis_vpn_lib::network::Network;

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct PreSafeRunner {
    hopr_params: HoprParams,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Chain(#[from] ChainError),
}

impl PreSafeRunner {
    pub fn new(hopr_params: HoprParams) -> Self {
        Self { hopr_params }
    }

    pub async fn start(&self) -> Result<balance::PreSafe, Error> {
        let keys = self.hopr_params.calc_keys()?;
        let private_key = keys.chain_key.clone();
        let rpc_provider = self.hopr_params.rpc_provider.clone();
        let node_address = keys.chain_key.public().to_address();
        let res = balance::PreSafe::fetch(&private_key, rpc_provider.as_str(), node_address).await?;
        Ok(res)
    }
}

async fn safe_module_deployment(
    network_specs: NetworkSpecifications,
    priv_key: ChainKeypair,
    rpc_provider: String,
    node_address: Address,
    nonce: U256,
    token_amount: U256,
) -> Result<SafeModuleDeploymentResult, Error> {
    retry(ExponentialBackoff::default(), || async {
        let client = GnosisRpcClient::with_url(priv_key.clone(), rpc_provider.as_str())
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
