use crate::connection::destination::ExitKey;
use crate::hopr::pix_chunks::Deposit;

/// Survives a reconnect to the same destination so repeated failures do not speed up the loop.
#[derive(Default)]
pub(super) struct PixHoldFailures {
    destination: Option<ExitKey>,
    count: u32,
    failure_logged: bool,
}

impl PixHoldFailures {
    pub(super) fn move_to(&mut self, destination: ExitKey) {
        if self.destination != Some(destination) {
            self.destination = Some(destination);
            self.count = 0;
            self.failure_logged = false;
        }
    }

    /// `unpaid` and `has_refused` must come from the same PixChunks snapshot.
    pub(super) fn end_hold_for(
        &self,
        destination: &ExitKey,
        unpaid: Option<Deposit>,
        has_refused: bool,
    ) -> Option<Deposit> {
        unpaid.filter(|_| !has_refused && (self.destination != Some(*destination) || self.count == 0))
    }

    pub(super) fn hold_started(&mut self) {
        self.failure_logged = false;
    }

    /// Returns true only for the first retained failure in this hold.
    pub(super) fn note_retained_failure(&mut self) -> bool {
        !std::mem::replace(&mut self.failure_logged, true)
    }

    pub(super) fn record_failed(&mut self, destination: ExitKey) {
        self.move_to(destination);
        self.count = self.count.saturating_add(1);
    }

    pub(super) fn hold_resumed(&mut self, destination: ExitKey) {
        if self.destination == Some(destination) {
            self.count = 0;
            self.failure_logged = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{Address, HopRouting};

    fn destination(address: u8) -> ExitKey {
        ExitKey {
            address: Address::from([address; 20]),
            routing: HopRouting::try_from(1).expect("valid hop count"),
        }
    }

    #[test]
    fn a_refused_deposit_does_not_end_the_hold_early() {
        let failures = PixHoldFailures::default();
        for reason in [Deposit::Failed, Deposit::Deferred, Deposit::Refused] {
            assert_eq!(failures.end_hold_for(&destination(1), Some(reason), true), None);
        }
    }

    #[test]
    fn a_second_failed_hold_on_the_same_destination_does_not_end_early() {
        for reason in [Deposit::Failed, Deposit::Deferred] {
            let destination = destination(1);
            let mut failures = PixHoldFailures::default();
            failures.move_to(destination);
            assert_eq!(failures.end_hold_for(&destination, Some(reason), false), Some(reason));
            assert_eq!(failures.end_hold_for(&destination, Some(reason), false), Some(reason));
            failures.record_failed(destination);
            failures.move_to(destination);
            assert_eq!(failures.end_hold_for(&destination, Some(reason), false), None);
            assert_eq!(failures.end_hold_for(&destination, Some(reason), false), None);
        }
    }

    #[test]
    fn a_resumed_hold_resets_the_count() {
        let destination = destination(1);
        let mut failures = PixHoldFailures::default();
        failures.record_failed(destination);
        assert_eq!(failures.end_hold_for(&destination, Some(Deposit::Failed), false), None);
        failures.hold_resumed(destination);
        assert_eq!(
            failures.end_hold_for(&destination, Some(Deposit::Failed), false),
            Some(Deposit::Failed)
        );
        assert_eq!(
            failures.end_hold_for(&destination, Some(Deposit::Deferred), false),
            Some(Deposit::Deferred)
        );
        assert_eq!(failures.end_hold_for(&destination, Some(Deposit::Refused), true), None);
    }

    #[test]
    fn another_destination_starts_with_no_failures() {
        let first = destination(1);
        let second = destination(2);
        let mut failures = PixHoldFailures::default();
        failures.record_failed(first);
        assert_eq!(failures.end_hold_for(&first, Some(Deposit::Failed), false), None);
        failures.hold_resumed(second);
        assert_eq!(failures.end_hold_for(&first, Some(Deposit::Failed), false), None);
        failures.move_to(second);
        assert_eq!(
            failures.end_hold_for(&second, Some(Deposit::Failed), false),
            Some(Deposit::Failed)
        );
        failures.move_to(first);
        assert_eq!(
            failures.end_hold_for(&first, Some(Deposit::Failed), false),
            Some(Deposit::Failed)
        );
    }

    #[test]
    fn an_unknown_outcome_does_not_end_the_hold() {
        let failures = PixHoldFailures::default();
        assert_eq!(failures.end_hold_for(&destination(1), None, false), None);
    }

    #[test]
    fn a_retained_failure_is_logged_once_per_hold() {
        let mut failures = PixHoldFailures::default();
        let destination = destination(1);
        failures.record_failed(destination);
        for _ in 0..2 {
            failures.hold_started();
            assert!(failures.note_retained_failure());
            assert!(!failures.note_retained_failure());
            // A new hold clears the warning, but must not grant another early reconnect.
            assert_eq!(failures.end_hold_for(&destination, Some(Deposit::Failed), false), None);
        }
    }
}
