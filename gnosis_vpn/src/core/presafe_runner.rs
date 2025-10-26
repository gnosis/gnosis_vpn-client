use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use thiserror::Error;

use gnosis_vpn_lib::balance::{self};

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct PreSafeRunner {
    hopr_params: HoprParams,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    PreSafe(#[from] balance::Error),
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
