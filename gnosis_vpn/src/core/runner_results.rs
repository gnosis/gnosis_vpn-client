use thiserror::Error;

use gnosis_vpn_lib::balance::{self};
use gnosis_vpn_lib::ticket_stats::TicketStats;

use crate::core::{onboarding_runner, ticket_stats_runner};

#[derive(Debug)]
pub enum RunnerResults {
    TicketStats(Result<TicketStats, Error>),
    PreSafe(Result<balance::PreSafe, Error>),
}

#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    TicketStatsRunner(#[from] ticket_stats_runner::Error),
    #[error(transparent)]
    OnboardingRunner(#[from] onboarding_runner::Error),
}
