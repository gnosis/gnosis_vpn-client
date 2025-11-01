use backoff::ExponentialBackoff;
use backoff::future::retry;
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
    input: gvpn_client::Input,
}

impl GvpnClientRunner {
    pub fn new(input: gvpn_client::Input) -> Self {
        Self {
            client: Client::new(),
            input,
        }
    }

    pub async fn start_register(&self) -> Result<Registration, gvpn_client::Error> {
        register(&self.client, &self.input).await
    }

    pub async fn start_unregister(&self) -> Result<(), gvpn_client::Error> {
        gvpn_client::unregister(&self.client, &self.input).await
    }
}

async fn register(client: &Client, input: &gvpn_client::Input) -> Result<Registration, gvpn_client::Error> {
    retry(ExponentialBackoff::default(), || async {
        gvpn_client::register(&client, &input).await
    })
    .await
}
