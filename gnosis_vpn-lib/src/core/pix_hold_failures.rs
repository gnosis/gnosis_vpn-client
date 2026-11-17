use crate::connection::destination::ExitKey;
use crate::hopr::pix_chunks::Deposit;

/// Survives a reconnect to the same destination so repeated failures do not speed up the loop.
#[derive(Default)]
pub(super) struct PixHoldFailures {
    destination: Option<ExitKey>,
    count: u32,
}

impl PixHoldFailures {
    pub(super) fn move_to(&mut self, destination: ExitKey) {
        if self.destination != Some(destination) {
            self.destination = Some(destination);
            self.count = 0;
        }
    }

    pub(super) fn may_end_hold(&self, destination: &ExitKey, reason: Deposit) -> bool {
        reason != Deposit::Refused && (self.destination != Some(*destination) || self.count == 0)
    }

    pub(super) fn record_failed(&mut self, destination: ExitKey) {
        self.move_to(destination);
        self.count = self.count.saturating_add(1);
    }

    pub(super) fn hold_resumed(&mut self, destination: ExitKey) {
        if self.destination == Some(destination) {
            self.count = 0;
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
        assert!(!failures.may_end_hold(&destination(1), Deposit::Refused));
    }

    #[test]
    fn a_second_failed_hold_on_the_same_destination_does_not_end_early() {
        for reason in [Deposit::Failed, Deposit::Deferred] {
            let destination = destination(1);
            let mut failures = PixHoldFailures::default();
            failures.move_to(destination);
            assert!(failures.may_end_hold(&destination, reason));
            assert!(failures.may_end_hold(&destination, reason));
            failures.record_failed(destination);
            failures.move_to(destination);
            assert!(!failures.may_end_hold(&destination, reason));
            assert!(!failures.may_end_hold(&destination, reason));
        }
    }

    #[test]
    fn a_resumed_hold_resets_the_count() {
        let destination = destination(1);
        let mut failures = PixHoldFailures::default();
        failures.record_failed(destination);
        assert!(!failures.may_end_hold(&destination, Deposit::Failed));
        failures.hold_resumed(destination);
        assert!(failures.may_end_hold(&destination, Deposit::Failed));
        assert!(failures.may_end_hold(&destination, Deposit::Deferred));
        assert!(!failures.may_end_hold(&destination, Deposit::Refused));
    }

    #[test]
    fn another_destination_starts_with_no_failures() {
        let first = destination(1);
        let second = destination(2);
        let mut failures = PixHoldFailures::default();
        failures.record_failed(first);
        assert!(!failures.may_end_hold(&first, Deposit::Failed));
        failures.hold_resumed(second);
        assert!(!failures.may_end_hold(&first, Deposit::Failed));
        failures.move_to(second);
        assert!(failures.may_end_hold(&second, Deposit::Failed));
        failures.move_to(first);
        assert!(failures.may_end_hold(&first, Deposit::Failed));
    }
}
