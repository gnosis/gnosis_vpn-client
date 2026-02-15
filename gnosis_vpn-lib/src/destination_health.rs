/// This module helps identifying the health of a destination's exit.
use edgli::hopr_lib::SessionClientConfig;
use edgli::hopr_lib::SurbBalancerConfig;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::connection::destination::Destination;
use crate::connection::options::Options;
use crate::core::runner::{self, Results};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::{gvpn_client, log_output};

const MAX_INTERVAL_BETWEEN_FAILURES: Duration = Duration::from_mins(5);
const FAILURE_INTERVAL: Duration = Duration::from_secs(30);
const CONNECTED_INTERVAL: Duration = Duration::from_mins(3);
const DISCONNECTED_INTERVAL: Duration = Duration::from_secs(90);

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub enum DestinationHealth {
    #[default]
    Init,
    Running {
        since: SystemTime,
    },
    Failure {
        checked_at: SystemTime,
        error: String,
        previous_failures: u32,
    },
    Success {
        checked_at: SystemTime,
        health: gvpn_client::Health,
        total_time: Duration,
        round_trip_time: Duration,
    },
}

pub struct Container {
    instructions_sender: mpsc::Sender<Instruction>,
    health: DestinationHealth,
}

/// This event marks an actionable moment to core module
pub enum HealthCheckEvent {
    SetupFailed(String),
    Ready,
    Dismantled
}

/// core module triggering next step
pub enum Instruction {
    Check,
    Shutdown,
}

pub enum Connected {
    Yes,
    No,
}

pub struct Runner {
    destination: Destination,
    hopr: Arc<Hopr>,
    options: Options,
    results_sender: mpsc::Sender<Results>,
    instructions_receiver: mpsc::Receiver<Instruction>,
}

impl Runner {
    pub fn new(destination: Destination, options: Options, hopr: Arc<Hopr>, results_sender: mpsc::Sender<Results>, instructions_receiver: mpsc::Receiver<Instruction>) -> Self {
        Self {
            destination,
            hopr,
            options,
            results_sender,
            instructions_receiver,
        }
    }

    pub async fn start(self) {
        // setup: session config
        let res_config = runner::to_surb_balancer_config(self.options.buffer_sizes.health, self.options.max_surb_upstream.health);
        let config = match res_config {
            Ok(config) => config,
            Err(err) => {
                self.send_event(HealthCheckEvent::SetupFailed(format!("Surb balancer config error: {err}"))),
                return;
            }
        };

        // setup: open session
        let res_session = open_health_session(&self.hopr, &self.destination, &self.options, config).await;
        let session = match res_session {
            Ok(session) => session,
            Err(err) => {
                self.send_event(HealthCheckEvent::SetupFailed(format!("Surb balancer config error: {err}"))),
                return;
            }
        };

        // waiting for instructions
        self.send_event(HealthCheckEvent::Ready).await;
        loop {
            tokio::select! {
                Some(instr) = self.instructions_receiver.recv() => {
                }
                else => {
                    tracing::warn!("instructions receiver closed");
                    break;
                }
            }
        }

        // dismantle: close session
        close_health_session(&self.hopr, &session).await;
    }

    pub async check(&mut self) {
        let checked_at: SystemTime::now();
        self.send_health(DestinationHealth::Runner { since: checked_at }).await;

        let measure_rtt = Instant::now();
        let res_health = request_health(&self.options, &self.session).await;
        let rtt = measure_rtt.elapsed();

        match res_health {
            Ok(health) => {
                self.send_health(DestinationHealth::Success {
                checked_at,
                health,
                round_trip_time: rtt,
                });
                self.failure_count = 0;
            },
            Err(err) => {
                self.send_health(DestinationHealth::Failure {
                checked_at,
                error: format!("Health request error: {err}"),
                previous_failures: self.failure_count,
            });
                self.failure_count ++;
            },
        }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let _ = results_sender
            .send(Results::HealthCheck {
                id: self.destination.id.clone(),
                health: DestinationHealth::Running {
                    since: SystemTime::now(),
                },
            })
            .await;
        let new_health = self.run().await;
        let health = match (self.old_health.clone(), new_health) {
            // increment failure count if the health check failed again
            (
                DestinationHealth::Failure { previous_failures, .. },
                DestinationHealth::Failure { error, checked_at, .. },
            ) => DestinationHealth::Failure {
                checked_at,
                error,
                previous_failures: previous_failures + 1,
            },
            (_, h) => h,
        };

        let _ = results_sender
            .send(Results::HealthCheck {
                id: self.destination.id.clone(),
                health,
            })
            .await;
    }

    async fn run(&self) -> DestinationHealth {
        let checked_at = SystemTime::now();

        // 1. calc health session config
        let measure_total = Instant::now();
        let res_config =
            runner::to_surb_balancer_config(self.options.buffer_sizes.health, self.options.max_surb_upstream.health);
        let config = match res_config {
            Ok(config) => config,
            Err(err) => {
                return DestinationHealth::Failure {
                    checked_at,
                    error: format!("Surb balancer config error: {err}"),
                    previous_failures: 0,
                };
            }
        };

        // 2. open health session
        let res_session = open_health_session(&self.hopr, &self.destination, &self.options, config).await;
        let session = match res_session {
            Ok(session) => session,
            Err(err) => {
                return DestinationHealth::Failure {
                    checked_at,
                    error: format!("Session creation error: {err}"),
                    previous_failures: 0,
                };
            }
        };

        // 3. request health
        let measure_rtt = Instant::now();
        let res_health = request_health(&self.options, &session).await;
        let rtt = measure_rtt.elapsed();

        // 4. close health session
        close_health_session(&self.hopr, &session).await;

        let measure_total = measure_total.elapsed();
        match res_health {
            Ok(health) => DestinationHealth::Success {
                checked_at,
                health,
                total_time: measure_total,
                round_trip_time: rtt,
            },
            Err(err) => DestinationHealth::Failure {
                checked_at,
                error: format!("Health request error: {err}"),
                previous_failures: 0,
            },
        }
    }
async fn send_health(&self, health: DestinationHealth) {
        let _ = self.results_sender.send(Results::HealthCheck {
                id: self.destination.id.clone(),
                health,
            })
            .await;
}

async fn send_event(&self, evt: HealthCheckEvent) {
        let _ = self.results_sender.send(Results::HealthCheckEvent {
                id: self.destination.id.clone(),
                evt,
            })
            .await;
}

}

async fn open_health_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    surb_management: SurbBalancerConfig,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.health.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: Some(surb_management),
        ..Default::default()
    };
    tracing::debug!(%destination, "attempting to open health check session");
    hopr.open_session(
        destination.address,
        options.sessions.health.target.clone(),
        Some(1),
        Some(1),
        cfg.clone(),
    )
    .await
}

async fn request_health(
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
) -> Result<gvpn_client::Health, gvpn_client::Error> {
    let client = reqwest::Client::new();
    let socket_addr = session_client_metadata.bound_host;
    let timeout = options.timeouts.http;
    tracing::debug!(?socket_addr, ?timeout, "requesting health status from exit");
    gvpn_client::health(&client, socket_addr, timeout).await
}

async fn close_health_session(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) {
    tracing::debug!( bound_host = ?session_client_metadata.bound_host, "closing health check session");
    let _ = hopr
        .close_session(session_client_metadata.bound_host, session_client_metadata.protocol)
        .await
        .map_err(|err| {
            tracing::warn!(error = ?err, "failed to close health session after health check");
            err
        });
}


impl DestinationHealth {
    pub fn next_interval(&self, connected: Connected) -> Option<Duration> {
        match self {
            DestinationHealth::Failure { previous_failures, .. } => {
                // increment by failure amount up to max interval
                let i =
                    (FAILURE_INTERVAL + (FAILURE_INTERVAL * (*previous_failures))).min(MAX_INTERVAL_BETWEEN_FAILURES);
                Some(i)
            }
            DestinationHealth::Running { .. } => None,
            DestinationHealth::Init => None,
            DestinationHealth::Success { .. } => match connected {
                Connected::Yes => Some(CONNECTED_INTERVAL),
                Connected::No => Some(DISCONNECTED_INTERVAL),
            },
        }
    }
}
impl From<bool> for Connected {
    fn from(value: bool) -> Self {
        if value { Connected::Yes } else { Connected::No }
    }
}

impl Display for DestinationHealth {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DestinationHealth::Init => write!(f, "waiting for connection"),
            DestinationHealth::Running { since } => write!(f, "running since {}", log_output::elapsed(since)),
            DestinationHealth::Failure {
                checked_at,
                error,
                previous_failures,
            } if *previous_failures > 0 => write!(
                f,
                "failed {} times in a row {} ago: {}",
                previous_failures + 1,
                log_output::elapsed(checked_at),
                error,
            ),
            DestinationHealth::Failure {
                checked_at,
                error,
                previous_failures: _,
            } => write!(f, "failed {} ago: {}", log_output::elapsed(checked_at), error),
            DestinationHealth::Success {
                checked_at,
                health,
                total_time,
                round_trip_time,
            } => write!(
                f,
                "{} ago: total time {:.2} s, round trip: {:.2} s, {}",
                log_output::elapsed(checked_at),
                total_time.as_secs_f32(),
                round_trip_time.as_secs_f32(),
                health,
            ),
        }
    }
}
