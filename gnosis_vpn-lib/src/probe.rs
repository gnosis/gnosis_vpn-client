//! One long-lived TCP bridge session to a destination, probed on three independent cadences:
//! API version, ping latency and exit load. The same session carries WireGuard registration.
//!
//! [`Probe`] is Core's mirror of the background task: pure state fed by [`Event`]s, plus the
//! task's cancellation handle. The task itself never stops on its own; after
//! `REOPEN_AFTER_FAILURES` consecutive failed checks it closes the session and opens a new one.
use edgli::FlowControlConfig;
use edgli::hopr_lib::HoprSessionClientConfig;
use rand::prelude::*;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio::time;
use tokio_util::sync::CancellationToken;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::command;
use crate::connection::destination::{Destination, ExitKey};
use crate::connection::options::{HealthCheckIntervals, Options, surb_config_for};
use crate::core::runner::Results;
use crate::gvpn_client;
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};

pub use crate::gvpn_client::{Health, LoadAvg, Slots, Versions};

const REOPEN_AFTER_FAILURES: u32 = 3;
const REOPEN_BACKOFF_STEP: Duration = Duration::from_secs(5);
const REOPEN_BACKOFF_MAX: Duration = Duration::from_secs(60);

/// Add ±25 % random jitter to `base`; zero stays zero so immediate triggers are not delayed.
pub(crate) fn jitter(base: Duration) -> Duration {
    if base.is_zero() {
        return base;
    }
    let factor = rand::rng().random::<f64>() * 0.5 - 0.25;
    let jitter_secs = base.as_secs_f64() * factor;
    if jitter_secs >= 0.0 {
        base.checked_add(Duration::from_secs_f64(jitter_secs))
            .unwrap_or(Duration::MAX)
    } else {
        base.saturating_sub(Duration::from_secs_f64(-jitter_secs))
    }
}

/// The single place mapping exit API versions to client modules; only v1 (`gvpn_client`) exists.
pub(crate) fn select_api_version(server_versions: &[String]) -> Option<&'static str> {
    const SUPPORTED: &[&str] = &["v1"];
    SUPPORTED
        .iter()
        .copied()
        .find(|&v| server_versions.iter().any(|sv| sv == v))
}

fn reopen_backoff(attempt: u32) -> Duration {
    REOPEN_BACKOFF_STEP
        .saturating_mul(attempt.max(1))
        .min(REOPEN_BACKOFF_MAX)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq)]
pub enum Check {
    Version,
    Ping,
    Load,
}

/// Wire type shown by the CLI, so variant names are user-visible.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state")]
pub enum ProbeState {
    /// No session yet, or the last open attempt failed and a retry is pending.
    Opening,
    /// Session open, waiting for the first result of every check.
    Checking,
    /// Every check has succeeded at least once on this session.
    Ready,
    /// Too many consecutive failures; the session was closed and a new one is being opened.
    Reopening,
}

#[derive(Debug)]
pub(crate) enum Event {
    Opened {
        session: SessionClientMetadata,
        since: SystemTime,
    },
    OpenFailed {
        error: String,
    },
    Version {
        checked_at: SystemTime,
        versions: Versions,
    },
    Ping {
        checked_at: SystemTime,
        rtt: Duration,
    },
    Load {
        checked_at: SystemTime,
        health: Health,
    },
    Failed {
        check: Check,
        error: String,
    },
    Reopening {
        error: String,
    },
}

pub(crate) struct Probe {
    destination: Destination,
    generation: u64,
    state: ProbeState,
    session: Option<SessionClientMetadata>,
    since: Option<SystemTime>,
    versions: Option<Versions>,
    ping_rtt: Option<Duration>,
    load: Option<Health>,
    checked_at: Option<SystemTime>,
    consecutive_failures: u32,
    last_error: Option<String>,
    cancel: CancellationToken,
}

impl Probe {
    /// Spawns the probe task; `generation` lets Core drop events of a probe it already replaced.
    pub(crate) fn start(
        dest: &Destination,
        generation: u64,
        hopr: Arc<Hopr>,
        options: Options,
        cancel_on_shutdown: &CancellationToken,
        sender: &mpsc::Sender<Results>,
    ) -> Self {
        let cancel = cancel_on_shutdown.child_token();
        let task_cancel = cancel.clone();
        let destination = dest.clone();
        let sender = sender.clone();
        tokio::spawn(async move {
            task_cancel
                .run_until_cancelled(run_probe(hopr, destination, options, generation, sender))
                .await;
        });
        Self {
            destination: dest.clone(),
            generation,
            state: ProbeState::Opening,
            session: None,
            since: None,
            versions: None,
            ping_rtt: None,
            load: None,
            checked_at: None,
            consecutive_failures: 0,
            last_error: None,
            cancel,
        }
    }

    pub(crate) fn key(&self) -> ExitKey {
        self.destination.key()
    }

    pub(crate) fn destination(&self) -> &Destination {
        &self.destination
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// The session once every check has passed on it; what a connection may register over.
    pub(crate) fn ready_session(&self) -> Option<&SessionClientMetadata> {
        match self.state {
            ProbeState::Ready => self.session.as_ref(),
            _ => None,
        }
    }

    /// Whatever session is open right now, ready or not; good enough for a best-effort unregister.
    pub(crate) fn any_session(&self) -> Option<&SessionClientMetadata> {
        self.session.as_ref()
    }

    pub(crate) fn apply(&mut self, event: Event) {
        match event {
            Event::Opened { session, since } => {
                self.state = ProbeState::Checking;
                self.session = Some(session);
                self.since = Some(since);
                self.consecutive_failures = 0;
            }
            Event::OpenFailed { error } => {
                self.state = ProbeState::Opening;
                self.last_error = Some(error);
            }
            Event::Version { checked_at, versions } => {
                self.versions = Some(versions);
                self.succeeded(checked_at);
            }
            Event::Ping { checked_at, rtt } => {
                self.ping_rtt = Some(rtt);
                self.succeeded(checked_at);
            }
            Event::Load { checked_at, health } => {
                self.load = Some(health);
                self.succeeded(checked_at);
            }
            Event::Failed { check, error } => {
                self.consecutive_failures += 1;
                self.last_error = Some(format!("{check:?} check failed: {error}"));
            }
            Event::Reopening { error } => {
                self.state = ProbeState::Reopening;
                self.session = None;
                self.since = None;
                self.last_error = Some(error);
            }
        }
    }

    fn succeeded(&mut self, checked_at: SystemTime) {
        self.checked_at = Some(checked_at);
        self.consecutive_failures = 0;
        self.last_error = None;
        let all_checked = self.versions.is_some() && self.ping_rtt.is_some() && self.load.is_some();
        if self.state == ProbeState::Checking && all_checked {
            tracing::info!(destination = %self.destination, "probe ready");
            self.state = ProbeState::Ready;
        }
    }

    pub(crate) fn view(&self) -> command::ProbeView {
        command::ProbeView {
            destination_id: self.destination.connect_id.clone(),
            state: self.state.clone(),
            session_since: self.since,
            versions: self.versions.clone(),
            api_version: self
                .versions
                .as_ref()
                .and_then(|v| select_api_version(&v.versions))
                .map(str::to_owned),
            ping_rtt: self.ping_rtt,
            load: self.load.clone(),
            checked_at: self.checked_at,
            consecutive_failures: self.consecutive_failures,
            last_error: self.last_error.clone(),
        }
    }
}

/// A replaced probe must not keep its session or hand stale events to its successor.
impl Drop for Probe {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

// ---------------------------------------------------------------------------
// Background task
// ---------------------------------------------------------------------------

async fn run_probe(
    hopr: Arc<Hopr>,
    destination: Destination,
    options: Options,
    generation: u64,
    sender: mpsc::Sender<Results>,
) {
    let send = |event: Event| {
        let sender = sender.clone();
        async move {
            let _ = sender.send(Results::Probe { generation, event }).await;
        }
    };
    let mut open_attempt = 0;
    loop {
        let session = match ProbeSession::open(hopr.clone(), &destination, &options).await {
            Ok(session) => session,
            Err(err) => {
                open_attempt += 1;
                let delay = reopen_backoff(open_attempt);
                tracing::warn!(%destination, ?err, ?delay, "opening probe session failed - retrying");
                send(Event::OpenFailed { error: err.to_string() }).await;
                time::sleep(delay).await;
                continue;
            }
        };
        open_attempt = 0;
        send(Event::Opened {
            session: session.meta.clone(),
            since: SystemTime::now(),
        })
        .await;

        let checker = Checker {
            client: reqwest::Client::new(),
            bound_host: session.meta.bound_host,
            timeout: options.timeouts.http,
            sender: sender.clone(),
            generation,
        };
        let error = checker.run_until_broken(&options.health_check_intervals).await;
        tracing::warn!(%destination, %error, "probe session broken - reopening");
        send(Event::Reopening { error }).await;
        session.close().await;
    }
}

/// Runs the checks over one session and reports each result.
struct Checker {
    client: reqwest::Client,
    bound_host: std::net::SocketAddr,
    timeout: Duration,
    sender: mpsc::Sender<Results>,
    generation: u64,
}

impl Checker {
    /// Initial version → ping → load, then each check on its own jittered timer.
    /// Returns the error that broke the session once failures pile up.
    async fn run_until_broken(&self, intervals: &HealthCheckIntervals) -> String {
        let mut failures = 0;
        for check in [Check::Version, Check::Ping, Check::Load] {
            if let Err(error) = self.run(check).await {
                failures += 1;
                if failures >= REOPEN_AFTER_FAILURES {
                    return error;
                }
            }
        }

        let mut next_version = Box::pin(time::sleep(jitter(intervals.version)));
        let mut next_ping = Box::pin(time::sleep(jitter(intervals.ping)));
        let mut next_load = Box::pin(time::sleep(jitter(intervals.load)));
        loop {
            let check = tokio::select! {
                _ = &mut next_version => {
                    next_version.as_mut().reset(time::Instant::now() + jitter(intervals.version));
                    Check::Version
                }
                _ = &mut next_ping => {
                    next_ping.as_mut().reset(time::Instant::now() + jitter(intervals.ping));
                    Check::Ping
                }
                _ = &mut next_load => {
                    next_load.as_mut().reset(time::Instant::now() + jitter(intervals.load));
                    Check::Load
                }
            };
            match self.run(check).await {
                Ok(()) => failures = 0,
                Err(error) => {
                    failures += 1;
                    if failures >= REOPEN_AFTER_FAILURES {
                        return error;
                    }
                }
            }
        }
    }

    async fn run(&self, check: Check) -> Result<(), String> {
        let checked_at = SystemTime::now();
        let outcome = match check {
            Check::Version => gvpn_client::versions(&self.client, self.bound_host, self.timeout)
                .await
                .map(|versions| Event::Version { checked_at, versions }),
            Check::Ping => {
                let started = Instant::now();
                gvpn_client::ping(&self.client, self.bound_host, self.timeout)
                    .await
                    .map(|()| Event::Ping {
                        checked_at,
                        rtt: started.elapsed(),
                    })
            }
            Check::Load => gvpn_client::health(&self.client, self.bound_host, self.timeout)
                .await
                .map(|health| Event::Load { checked_at, health }),
        };
        let (event, result) = match outcome {
            Ok(event) => {
                tracing::debug!(?check, "probe check passed");
                (event, Ok(()))
            }
            Err(err) => {
                tracing::warn!(?check, ?err, "probe check failed");
                let error = err.to_string();
                (
                    Event::Failed {
                        check,
                        error: error.clone(),
                    },
                    Err(error),
                )
            }
        };
        let _ = self
            .sender
            .send(Results::Probe {
                generation: self.generation,
                event,
            })
            .await;
        result
    }
}

/// RAII guard around the probe's bridge session.
///
/// The happy path awaits [`ProbeSession::close`]; cancellation of the surrounding task falls
/// through to `Drop`, which detaches a close task so the exit port is not leaked.
struct ProbeSession {
    hopr: Arc<Hopr>,
    meta: SessionClientMetadata,
    closed: bool,
}

impl ProbeSession {
    /// Opened with the bridge settings because WireGuard registration runs over this very session.
    async fn open(hopr: Arc<Hopr>, destination: &Destination, options: &Options) -> Result<Self, HoprError> {
        let surb = surb_config_for(&options.surb_balancing.bridge).map_err(|e| HoprError::Session(e.to_string()))?;
        let cfg = HoprSessionClientConfig {
            capabilities: options.sessions.bridge.capabilities,
            forward_path: destination.routing,
            return_path: destination.routing,
            always_max_out_surbs: surb.always_max_out_surbs,
            surb_management: surb.management,
            flow_control: Some(FlowControlConfig::robust()),
            ..Default::default()
        };
        let cfg = if options.pix.bridge.enabled {
            hopr.pix_aware_session_cfg(cfg)?
        } else {
            cfg
        };
        tracing::debug!(%destination, "opening probe session");
        let meta = hopr
            .open_session(destination.address, destination.bridge_target(), None, None, cfg)
            .await?;
        Ok(Self {
            hopr,
            meta,
            closed: false,
        })
    }

    async fn close(mut self) {
        close_probe_session(&self.hopr, &self.meta).await;
        self.closed = true;
    }
}

impl Drop for ProbeSession {
    fn drop(&mut self) {
        if self.closed {
            return;
        }
        let hopr = self.hopr.clone();
        let meta = self.meta.clone();
        tokio::spawn(async move {
            close_probe_session(&hopr, &meta).await;
        });
    }
}

/// Errors are logged and swallowed; a leaked session does not justify failing the probe.
async fn close_probe_session(hopr: &Hopr, session: &SessionClientMetadata) {
    tracing::debug!(bound_host = ?session.bound_host, "closing probe session");
    if let Err(err) = hopr.close_session(session.bound_host, session.protocol).await {
        tracing::warn!(?err, "failed to close probe session");
    }
}

impl Display for ProbeState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ProbeState::Opening => write!(f, "opening session"),
            ProbeState::Checking => write!(f, "running initial checks"),
            ProbeState::Ready => write!(f, "ready"),
            ProbeState::Reopening => write!(f, "reopening session"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{Address, DestinationSource, HopRouting};

    fn destination() -> Destination {
        Destination::new(
            "test".to_string(),
            Address::from([1u8; 20]),
            HopRouting::try_from(1).unwrap(),
            Default::default(),
            "172.30.0.1:8000".parse().unwrap(),
            "172.30.0.1:51820".parse().unwrap(),
            DestinationSource::Configured,
        )
    }

    fn session() -> SessionClientMetadata {
        SessionClientMetadata {
            target: "172.30.0.1:8000".to_string(),
            destination: Address::from([1u8; 20]),
            forward_path: HopRouting::try_from(1).unwrap(),
            return_path: HopRouting::try_from(1).unwrap(),
            protocol: edgli::hopr_lib::exports::network::types::types::IpProtocol::TCP,
            bound_host: "127.0.0.1:4000".parse().unwrap(),
            hopr_mtu: 1000,
            surb_len: 200,
            active_clients: vec![],
            max_client_sessions: 1,
            max_surb_upstream: None,
            response_buffer: None,
            session_pool: None,
        }
    }

    fn versions() -> Versions {
        Versions {
            versions: vec!["v1".to_string()],
            latest: "v1".to_string(),
        }
    }

    fn health() -> Health {
        Health {
            slots: Slots {
                total: 16,
                available: 10,
                connected: 1,
            },
            load_avg: LoadAvg {
                one: 0.1,
                five: 0.2,
                fifteen: 0.3,
                nproc: 4,
            },
        }
    }

    /// A probe whose task is a no-op, so `apply` can be driven by hand.
    fn probe() -> Probe {
        Probe {
            destination: destination(),
            generation: 1,
            state: ProbeState::Opening,
            session: None,
            since: None,
            versions: None,
            ping_rtt: None,
            load: None,
            checked_at: None,
            consecutive_failures: 0,
            last_error: None,
            cancel: CancellationToken::new(),
        }
    }

    fn opened() -> Probe {
        let mut p = probe();
        p.apply(Event::Opened {
            session: session(),
            since: SystemTime::UNIX_EPOCH,
        });
        p
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    #[test]
    fn ready_only_once_every_check_passed() {
        let mut p = opened();
        assert!(p.ready_session().is_none());
        p.apply(Event::Version {
            checked_at: now(),
            versions: versions(),
        });
        p.apply(Event::Ping {
            checked_at: now(),
            rtt: Duration::from_millis(100),
        });
        assert!(p.ready_session().is_none(), "load still missing");
        p.apply(Event::Load {
            checked_at: now(),
            health: health(),
        });
        assert!(p.ready_session().is_some());
        assert_eq!(p.state, ProbeState::Ready);
    }

    #[test]
    fn any_session_is_available_before_ready() {
        let p = opened();
        assert!(p.any_session().is_some());
        assert!(p.ready_session().is_none());
    }

    #[test]
    fn failures_count_until_a_success_resets_them() {
        let mut p = opened();
        p.apply(Event::Failed {
            check: Check::Ping,
            error: "timeout".to_string(),
        });
        p.apply(Event::Failed {
            check: Check::Load,
            error: "timeout".to_string(),
        });
        assert_eq!(p.consecutive_failures, 2);
        assert!(p.last_error.as_deref().unwrap().contains("Load"));
        p.apply(Event::Ping {
            checked_at: now(),
            rtt: Duration::from_millis(10),
        });
        assert_eq!(p.consecutive_failures, 0);
        assert!(p.last_error.is_none());
    }

    #[test]
    fn reopening_drops_the_session_but_keeps_the_last_samples() {
        let mut p = opened();
        p.apply(Event::Ping {
            checked_at: now(),
            rtt: Duration::from_millis(10),
        });
        p.apply(Event::Reopening {
            error: "broken".to_string(),
        });
        assert_eq!(p.state, ProbeState::Reopening);
        assert!(p.any_session().is_none());
        assert!(p.ping_rtt.is_some());
        assert_eq!(p.last_error.as_deref(), Some("broken"));
    }

    #[test]
    fn an_incompatible_version_is_still_a_successful_check() {
        let mut p = opened();
        p.apply(Event::Version {
            checked_at: now(),
            versions: Versions {
                versions: vec!["v99".to_string()],
                latest: "v99".to_string(),
            },
        });
        assert_eq!(p.consecutive_failures, 0);
        assert!(p.view().api_version.is_none());
    }

    #[test]
    fn dropping_a_probe_cancels_its_task() {
        let p = probe();
        let token = p.cancel.clone();
        assert!(!token.is_cancelled());
        drop(p);
        assert!(token.is_cancelled());
    }

    #[test]
    fn reopen_backoff_grows_and_clamps() {
        assert_eq!(reopen_backoff(0), REOPEN_BACKOFF_STEP);
        assert_eq!(reopen_backoff(1), REOPEN_BACKOFF_STEP);
        assert_eq!(reopen_backoff(2), REOPEN_BACKOFF_STEP * 2);
        assert_eq!(reopen_backoff(u32::MAX), REOPEN_BACKOFF_MAX);
    }

    #[test]
    fn select_api_version_finds_v1() {
        assert_eq!(select_api_version(&["v1".to_string()]), Some("v1"));
    }

    #[test]
    fn select_api_version_returns_none_when_no_match() {
        assert_eq!(select_api_version(&[]), None);
        assert_eq!(select_api_version(&["v2".to_string(), "v99".to_string()]), None);
    }

    #[test]
    fn jitter_zero_returns_zero() {
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
    }
}
