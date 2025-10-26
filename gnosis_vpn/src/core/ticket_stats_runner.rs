use thiserror::Error;

use gnosis_vpn_lib::chain::contracts::NetworkSpecifications;
use gnosis_vpn_lib::ticket_stats::{self, TicketStats};

use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub struct TicketStatsRunner {
    hopr_params: HoprParams,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    TicketStats(#[from] ticket_stats::Error),
}

impl TicketStatsRunner {
    pub fn new(hopr_params: HoprParams) -> Self {
        Self { hopr_params }
    }

    pub async fn start(&self) -> Result<TicketStats, Error> {
        let keys = self.hopr_params.calc_keys()?;
        let private_key = keys.chain_key;
        let rpc_provider = self.hopr_params.rpc_provider.clone();
        let network = self.hopr_params.network.clone();
        let res = TicketStats::fetch(
            &private_key,
            rpc_provider.as_str(),
            &NetworkSpecifications::from_network(&network),
        )
        .await?;
        Ok(res)
    }
}
