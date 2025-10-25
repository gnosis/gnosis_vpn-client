use tokio::sync::mpsc;

use std::fmt::{self, Display};

use gnosis_vpn_lib::chain::contracts::NetworkSpecifications;
use gnosis_vpn_lib::hopr::{Hopr, HoprError, api::HoprTelemetry, config as hopr_config, identity};
use gnosis_vpn_lib::ticket_stats::{self, TicketStats};

use crate::hopr_params::{self, HoprParams};

pub struct TicketStatsRunner {
    hopr_params: HoprParams,
}

pub enum Results {
  HoprParamsError(hopr_params::Error),
  TicketStatsError(ticket_stats::Error),
  Sucess(TicketStats)
}

impl TicketStatsRunner {
    pub fn new(hopr_params: HoprParams) -> Self {
        Self { hopr_params }
    }

    pub async fn start(&self, sender: mpsc::Sender<Results>) {
            let keys = match self.hopr_params.calc_keys() {
                Ok(keys) => keys,
                Err(e) => {
                    let _ = sender.send(Results::HoprKeysError(e)).await;
                    return;
                }
            };
            let private_key = keys.chain_key;
            let rpc_provider = self.hopr_params.rpc_provider.clone();
            let network = self.hopr_params.network.clone();
            let res = TicketStats::fetch(
                &private_key,
                rpc_provider.as_str(),
                &NetworkSpecifications::from_network(&network),
            )
            .await;
            match res {
                Ok(stats) => {
                    let _ = sender.send(Results::Success(stats)).await;
                }
                Err(e) => {
                    let _ = sender.send(Results::TicketStatsError(e)).await;
                }

            }
    }
}

impl Display for Results {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Results::Foobar => write!(f, "Foobar Result"),
        }
    }
}
