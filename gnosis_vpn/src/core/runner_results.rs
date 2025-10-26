use crate::core::{onboarding_runner, ticket_stats_runner};

#[derive(Debug)]
pub enum RunnerResults {
    TicketStatsRunner(ticket_stats_runner::Results),
    OnboardingRunner(onboarding_runner::Results),
}

impl From<ticket_stats_runner::Results> for RunnerResults {
    fn from(result: ticket_stats_runner::Results) -> Self {
        RunnerResults::TicketStatsRunner(result)
    }
}

impl From<onboarding_runner::Results> for RunnerResults {
    fn from(result: onboarding_runner::Results) -> Self {
        RunnerResults::OnboardingRunner(result)
    }
}
