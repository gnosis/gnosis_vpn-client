//! Startup failures the sync screen explains to the user.
use edgli::FailureKind;
use gnosis_vpn_ipc::{StartupIssue, StartupIssueKind};

use std::time::SystemTime;

use super::runner;

pub(super) fn failed(previous: Option<&StartupIssue>, err: &runner::Error) -> StartupIssue {
    continued(previous, err.startup_issue_kind(), err.to_string())
}

/// The step failed again: keep the streak's `since` so the app can tell a blip from a blocker.
pub fn continued(previous: Option<&StartupIssue>, kind: StartupIssueKind, message: String) -> StartupIssue {
    StartupIssue {
        kind,
        message,
        since: previous.map_or_else(SystemTime::now, |issue| issue.since),
    }
}

/// The issue that has blocked startup longest.
pub(super) fn longest<'a>(issues: impl IntoIterator<Item = Option<&'a StartupIssue>>) -> Option<StartupIssue> {
    issues.into_iter().flatten().min_by_key(|issue| issue.since).cloned()
}

pub(crate) fn kind(failure: FailureKind) -> StartupIssueKind {
    match failure {
        FailureKind::Unreachable => StartupIssueKind::UNREACHABLE,
        FailureKind::ServiceUnavailable => StartupIssueKind::ServiceUnavailable,
        FailureKind::Incompatible => StartupIssueKind::Incompatible,
        FailureKind::Other => StartupIssueKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::hopr::config as hopr_config;
    use std::time::Duration;

    fn issue(since_secs: u64) -> StartupIssue {
        StartupIssue {
            kind: StartupIssueKind::Other,
            message: "boom".to_string(),
            since: SystemTime::UNIX_EPOCH + Duration::from_secs(since_secs),
        }
    }

    #[test]
    fn repeated_failure_keeps_the_first_since() {
        let first = issue(100);
        let err = runner::Error::HoprConfig(hopr_config::Error::Output("still failing".to_string()));

        let again = failed(Some(&first), &err);

        assert_eq!(again.since, first.since);
        assert!(again.message.contains("still failing"));
    }

    #[test]
    fn first_failure_starts_a_streak_now() {
        let before = SystemTime::now();
        let err = runner::Error::HoprConfig(hopr_config::Error::Output("down".to_string()));

        let issue = failed(None, &err);

        assert!(issue.since >= before);
        assert_eq!(issue.kind, StartupIssueKind::Other);
    }

    #[test]
    fn longest_picks_the_oldest_issue() {
        let older = issue(10);
        let newer = issue(20);

        let picked = longest([Some(&newer), None, Some(&older)]);

        assert_eq!(picked, Some(older));
    }
}
