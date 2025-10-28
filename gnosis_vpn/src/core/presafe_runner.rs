use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use thiserror::Error;
use tokio::time;

use std::time::Duration;

use gnosis_vpn_lib::balance::{self};

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct PreSafeRunner {
    hopr_params: HoprParams,
    delay: Duration,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    PreSafe(#[from] balance::Error),
}

impl PreSafeRunner {
    pub fn new(hopr_params: HoprParams, delay: Duration) -> Self {
        Self { hopr_params, delay }
    }

    pub async fn start(&self) -> Result<balance::PreSafe, Error> {
        time::sleep(self.delay).await;
        let keys = self.hopr_params.calc_keys()?;
        let private_key = keys.chain_key.clone();
        let rpc_provider = self.hopr_params.rpc_provider.clone();
        let node_address = keys.chain_key.public().to_address();
        let res = balance::PreSafe::fetch(&private_key, rpc_provider.as_str(), node_address).await?;
        Ok(res)
    }
}
