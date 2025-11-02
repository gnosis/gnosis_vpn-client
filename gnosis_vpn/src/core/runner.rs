use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::Address;
use edgli::hopr_lib::exports::crypto::types::prelude::Keypair;
use edgli::hopr_lib::state::HoprState;
use edgli::hopr_lib::{Balance, WxHOPR};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::fmt::{self, Display};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gnosis_vpn_lib::chain::contracts::NetworkSpecifications;
use gnosis_vpn_lib::channel_funding::{self, ChannelFunding};
use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::config::{self, Config};
use gnosis_vpn_lib::connection::{self, Connection as LibCon, destination::Destination};
use gnosis_vpn_lib::hopr::{Hopr, HoprError, api as hopr_api, config as hopr_config, types::SessionClientMetadata};
use gnosis_vpn_lib::metrics::{self, Metrics};
use gnosis_vpn_lib::node::{self, Node};
use gnosis_vpn_lib::onboarding::{self, Onboarding};
use gnosis_vpn_lib::ticket_stats::{self, TicketStats};
use gnosis_vpn_lib::{balance, info, wg_tooling};

use crate::event::Event;
use crate::hopr_params::{self, HoprParams};

#[derive(Debug)]
pub enum Results {
    FundChannel {
        address: Address,
        res: Result<(), hopr_api::ChannelError>,
    },
    PreSafe {
        res: Result<balance::PreSafe, Error>,
    },
    TicketStats {
        res: Result<ticket_stats::TicketStats, Error>,
    },
}

#[derive(Debug, Error)]
enum Error {
    #[error(transparent)]
    HoprParams(#[from] hopr_params::Error),
    #[error(transparent)]
    PreSafe(#[from] balance::Error),
    #[error(transparent)]
    TicketStats(#[from] ticket_stats::Error),
}

pub async fn presafe(hopr_params: HoprParams, results_sender: mpsc::Sender<Results>) {
    let res = run_presafe(hopr_params).await;
    let _ = results_sender.send(Results::PreSafe { res }).await;
}

pub async fn ticket_stats(hopr_params: HoprParams, results_sender: mpsc::Sender<Results>) {
    let res = run_ticket_stats(hopr_params).await;
    let _ = results_sender.send(Results::TicketStats { res }).await;
}

async fn run_presafe(hopr_params: HoprParams) -> Result<balance::PreSafe, Error> {
    tracing::debug!("starting presafe balance runner");
    let keys = hopr_params.calc_keys()?;
    let private_key = keys.chain_key.clone();
    let rpc_provider = hopr_params.rpc_provider.clone();
    let node_address = keys.chain_key.public().to_address();
    retry(ExponentialBackoff::default(), || async {
        let presafe = balance::PreSafe::fetch(&private_key, rpc_provider.as_str(), node_address)
            .await
            .map_err(Error::from)?;
        Ok(presafe)
    })
    .await
}

async fn run_ticket_stats(hopr_params: HoprParams) -> Result<ticket_stats::TicketStats, Error> {
    tracing::debug!("starting ticket stats runner");
    let keys = hopr_params.calc_keys()?;
    let private_key = keys.chain_key;
    let rpc_provider = hopr_params.rpc_provider.clone();
    let network = hopr_params.network.clone();
    retry(ExponentialBackoff::default(), || async {
        let stats = TicketStats::fetch(
            &private_key,
            rpc_provider.as_str(),
            &NetworkSpecifications::from_network(&network),
        )
        .await
        .map_err(Error::from)?;
        Ok(stats)
    })
    .await
}
