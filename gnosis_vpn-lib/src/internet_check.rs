//! Tells an offline user from a Blokli outage by asking public resolvers whether the internet answers.
use gnosis_vpn_ipc::{Internet, RunMode, StartupIssue, StartupIssueKind};
use rand::seq::SliceRandom;
use tokio::net::TcpStream;

use std::net::{IpAddr, Ipv4Addr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::ping;

/// One resolver per operator, so a single operator's outage or block cannot read as offline.
const TARGETS: [Ipv4Addr; 5] = [
    Ipv4Addr::new(1, 1, 1, 1),        // Cloudflare
    Ipv4Addr::new(8, 8, 8, 8),        // Google
    Ipv4Addr::new(9, 9, 9, 9),        // Quad9
    Ipv4Addr::new(208, 67, 222, 222), // OpenDNS
    Ipv4Addr::new(94, 140, 14, 14),   // AdGuard
];
const TARGETS_PER_PROBE: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(2);
/// What the system `ping` sends unasked on Linux and macOS, so the echo looks like anyone's.
const SYSTEM_TTL: u32 = 64;
/// All targets serve DNS over HTTPS here, so a bare connect looks like ordinary traffic.
const HTTPS_PORT: u16 = 443;
const REPROBE_AFTER: Duration = Duration::from_secs(30);
const REPROBE_JITTER_MS: u64 = 10_000;

/// Cached answer shared by status calls; probes only while an unreachable issue is reported.
#[derive(Clone, Default)]
pub struct InternetCheck {
    state: Arc<Mutex<State>>,
}

#[derive(Default)]
struct State {
    answer: Option<Answer>,
    probing: bool,
}

#[derive(Clone, Copy)]
struct Answer {
    internet: Internet,
    stale_at: Instant,
}

impl InternetCheck {
    /// Fills in `internet` on an unreachable startup issue, probing in the background once the last answer is stale.
    pub fn annotate(&self, run_mode: &mut RunMode, killswitch_armed: bool) {
        let Some(StartupIssueKind::Unreachable { internet }) = startup_issue_mut(run_mode).map(|issue| &mut issue.kind)
        else {
            // A later streak must not inherit this one's answer.
            self.lock().answer = None;
            return;
        };
        // The killswitch drops traffic to the targets, so a probe would always say down.
        if !killswitch_armed {
            *internet = self.current();
        }
    }

    fn current(&self) -> Internet {
        let mut state = self.lock();
        let stale = state.answer.is_none_or(|answer| Instant::now() >= answer.stale_at);
        if stale && !state.probing {
            state.probing = true;
            let shared = Arc::clone(&self.state);
            tokio::spawn(async move {
                let internet = probe().await;
                let jitter = Duration::from_millis(rand::random_range(0..REPROBE_JITTER_MS));
                let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
                state.probing = false;
                state.answer = Some(Answer {
                    internet,
                    stale_at: Instant::now() + REPROBE_AFTER + jitter,
                });
            });
        }
        state.answer.map_or(Internet::Unknown, |answer| answer.internet)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn startup_issue_mut(run_mode: &mut RunMode) -> Option<&mut StartupIssue> {
    match run_mode {
        RunMode::Init { issue } | RunMode::PreparingSafe { issue, .. } | RunMode::Warmup { issue, .. } => {
            issue.as_mut()
        }
        _ => None,
    }
}

#[tracing::instrument(name = "internet_check", ret)]
async fn probe() -> Internet {
    let mut targets = TARGETS;
    targets.shuffle(&mut rand::rng());
    for target in targets.into_iter().take(TARGETS_PER_PROBE) {
        if answers(target).await {
            return Internet::Up;
        }
    }
    Internet::Down
}

async fn answers(target: Ipv4Addr) -> bool {
    let opts = ping::Options {
        address: IpAddr::V4(target),
        timeout: TIMEOUT,
        ttl: SYSTEM_TTL,
        seq_count: 1,
    };
    if ping::ping(&opts).await.is_ok() {
        return true;
    }
    // Some networks drop ICMP but still pass HTTPS.
    matches!(
        tokio::time::timeout(TIMEOUT, TcpStream::connect((target, HTTPS_PORT))).await,
        Ok(Ok(_))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::time::SystemTime;

    fn init(kind: StartupIssueKind) -> RunMode {
        RunMode::Init {
            issue: Some(StartupIssue {
                kind,
                message: "boom".to_string(),
                since: SystemTime::UNIX_EPOCH,
            }),
        }
    }

    fn kind(run_mode: &mut RunMode) -> StartupIssueKind {
        startup_issue_mut(run_mode).expect("issue").kind
    }

    fn answered(internet: Internet) -> InternetCheck {
        let check = InternetCheck::default();
        check.lock().answer = Some(Answer {
            internet,
            stale_at: Instant::now() + Duration::from_secs(60),
        });
        check
    }

    #[tokio::test]
    async fn a_fresh_answer_fills_in_an_unreachable_issue() {
        let mut run_mode = init(StartupIssueKind::UNREACHABLE);

        answered(Internet::Down).annotate(&mut run_mode, false);

        assert_eq!(
            kind(&mut run_mode),
            StartupIssueKind::Unreachable {
                internet: Internet::Down
            }
        );
    }

    #[tokio::test]
    async fn an_armed_killswitch_keeps_it_unknown_and_never_probes() {
        let check = InternetCheck::default();
        let mut run_mode = init(StartupIssueKind::UNREACHABLE);

        check.annotate(&mut run_mode, true);

        assert_eq!(kind(&mut run_mode), StartupIssueKind::UNREACHABLE);
        assert!(!check.lock().probing);
    }

    #[tokio::test]
    async fn other_issues_are_left_alone_and_clear_the_answer() {
        let check = answered(Internet::Up);
        let mut run_mode = init(StartupIssueKind::ServiceUnavailable);

        check.annotate(&mut run_mode, false);

        assert_eq!(kind(&mut run_mode), StartupIssueKind::ServiceUnavailable);
        assert!(check.lock().answer.is_none());
        assert!(!check.lock().probing);
    }
}
