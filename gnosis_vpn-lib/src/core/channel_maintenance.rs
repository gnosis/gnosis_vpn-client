//! Smooths the channel-lifecycle strategy's per-tick verdict into a health the status can show.

use edgli::StrategyState;

use std::time::{Duration, SystemTime};

use crate::command::ChannelMaintenance;

pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
const FAILED_SAMPLES_TO_UNAVAILABLE: u32 = 3;

#[derive(Default)]
pub(crate) struct Supervisor {
    failed_samples_in_row: u32,
    trouble_since: Option<SystemTime>,
    last_logged: Option<StrategyState>,
}

impl Supervisor {
    pub fn on_sample(&mut self, sample: Option<StrategyState>, now: SystemTime) {
        let Some(state) = sample else { return };
        match state {
            StrategyState::Failed => {
                self.failed_samples_in_row += 1;
                self.trouble_since.get_or_insert(now);
            }
            StrategyState::Degraded | StrategyState::Running => {
                self.failed_samples_in_row = 0;
                self.trouble_since = None;
            }
        }
        self.log_change(state);
    }

    pub fn health(&self) -> ChannelMaintenance {
        let unavailable = self.failed_samples_in_row >= FAILED_SAMPLES_TO_UNAVAILABLE;
        match (unavailable, self.trouble_since) {
            (true, Some(since)) => ChannelMaintenance::Unavailable { since },
            _ => ChannelMaintenance::Ok,
        }
    }

    fn log_change(&mut self, state: StrategyState) {
        if self.last_logged == Some(state) {
            return;
        }
        match state {
            StrategyState::Running => tracing::info!(%state, "channel maintenance back to running"),
            _ => tracing::warn!(%state, prev = ?self.last_logged, "channel maintenance degraded"),
        }
        self.last_logged = Some(state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn three_failed_samples_report_unavailable_since_the_first() {
        let mut s = Supervisor::default();
        for secs in [60, 120] {
            s.on_sample(Some(StrategyState::Failed), at(secs));
            assert_eq!(s.health(), ChannelMaintenance::Ok);
        }
        s.on_sample(Some(StrategyState::Failed), at(180));
        assert_eq!(s.health(), ChannelMaintenance::Unavailable { since: at(60) });
    }

    #[test]
    fn a_healthy_sample_clears_the_failed_run() {
        for healthy in [StrategyState::Running, StrategyState::Degraded] {
            let mut s = Supervisor::default();
            for secs in [60, 120, 180] {
                s.on_sample(Some(StrategyState::Failed), at(secs));
            }
            s.on_sample(Some(healthy), at(240));
            assert_eq!(s.health(), ChannelMaintenance::Ok);
            // The run restarts from scratch, not from the old count.
            s.on_sample(Some(StrategyState::Failed), at(300));
            assert_eq!(s.health(), ChannelMaintenance::Ok);
        }
    }

    #[test]
    fn interrupted_failed_runs_do_not_add_up() {
        let mut s = Supervisor::default();
        s.on_sample(Some(StrategyState::Failed), at(60));
        s.on_sample(Some(StrategyState::Failed), at(120));
        s.on_sample(Some(StrategyState::Degraded), at(180));
        s.on_sample(Some(StrategyState::Failed), at(240));
        s.on_sample(Some(StrategyState::Failed), at(300));
        assert_eq!(s.health(), ChannelMaintenance::Ok);
    }

    #[test]
    fn no_reactor_means_no_sample() {
        let mut s = Supervisor::default();
        s.on_sample(Some(StrategyState::Failed), at(60));
        s.on_sample(None, at(120));
        s.on_sample(None, at(180));
        assert_eq!(s.health(), ChannelMaintenance::Ok);
    }
}
