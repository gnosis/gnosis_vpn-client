use crate::core::ticket_stats_runner;

#[derive(Debug)]
pub enum RunnerResults {
    TicketStatsRunner(ticket_stats_runner::Results),
}

impl From<ticket_stats_runner::Results> for RunnerResults {
    fn from(result: ticket_stats_runner::Results) -> Self {
        RunnerResults::TicketStatsRunner(result)
    }
}
