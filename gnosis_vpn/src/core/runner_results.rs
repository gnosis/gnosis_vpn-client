use thiserror::Error;

use gnosis_vpn_lib::balance::{self};
use gnosis_vpn_lib::chain::contracts::SafeModuleDeploymentResult;
use gnosis_vpn_lib::ticket_stats::TicketStats;

use crate::core::{funding_runner, hopr_runner, presafe_runner, safe_deployment_runner, ticket_stats_runner};

#[derive(Debug)]
pub enum RunnerResults {
    TicketStats(Result<TicketStats, Error>),
    PreSafe(Result<balance::PreSafe, Error>),
    SafeDeployment(Result<SafeModuleDeploymentResult, Error>),
    Hopr(Result<(), Error>),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    TicketStats(#[from] ticket_stats_runner::Error),
    #[error(transparent)]
    PreSafe(#[from] presafe_runner::Error),
    #[error(transparent)]
    SafeDeployment(#[from] safe_deployment_runner::Error),
    #[error(transparent)]
    Hopr(#[from] hopr_runner::Error),
}
