use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::{ChainKeypair, Keypair};
use tokio::sync::mpsc;

use gnosis_vpn_lib::balance::{self};

use crate::core::runner_results::RunnerResults;
use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct OnboardingRunner {
    hopr_params: HoprParams,
}

#[derive(Debug)]
pub enum Results {
    HoprParamsError(hopr_params::Error),
    PreSafeError(balance::Error),
    Success(balance::PreSafe),
}

impl OnboardingRunner {
    pub fn new(hopr_params: HoprParams) -> Self {
        Self { hopr_params }
    }

    pub async fn start(&self, sender: mpsc::Sender<RunnerResults>) {
        let keys = match self.hopr_params.calc_keys() {
            Ok(keys) => keys,
            Err(e) => {
                let _ = sender.send(Results::HoprParamsError(e).into()).await;
                return;
            }
        };
        let private_key = keys.chain_key.clone();
        let rpc_provider = self.hopr_params.rpc_provider.clone();
        let node_address = keys.chain_key.public().to_address();
        let res = balance::PreSafe::fetch(&private_key, rpc_provider.as_str(), node_address).await;
        match res {
            Ok(presafe) => {
                let _ = sender.send(Results::Success(presafe).into()).await;
            }
            Err(e) => {
                let _ = sender.send(Results::PreSafeError(e).into()).await;
            }
        }
    }
}
