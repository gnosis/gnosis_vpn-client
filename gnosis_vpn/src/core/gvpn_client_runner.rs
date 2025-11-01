use alloy::primitives::U256;
use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::ChainKeypair;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use rand::Rng;
use reqwest::Client;
use thiserror::Error;

use gnosis_vpn_lib::balance::{self};
use gnosis_vpn_lib::chain::client::GnosisRpcClient;
use gnosis_vpn_lib::chain::contracts::{NetworkSpecifications, SafeModuleDeploymentInputs, SafeModuleDeploymentResult};
use gnosis_vpn_lib::chain::errors::ChainError;
use gnosis_vpn_lib::gvpn_client::{self, Registration};

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct GvpnClientRunner {
    client: Client,
}

impl GvpnClientRunner {
    pub fn new() -> Self {
        Self { client: Client::new() }
    }

    pub async fn start_register(&self) -> Result<Registration, gvpn_client::Error> {
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
            token_amount,
        )
        .await
    }
}

async fn register(client: Client, input: Input) -> Result<Registration, Error> {
    retry(ExponentialBackoff::default(), || async {
        gvpn_client::register(&client, &input).await
    })
    .await
}
