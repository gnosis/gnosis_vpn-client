/// This module helps identifiying the health of a destination's exit.
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

pub const MAX_INTERVAL_BETWEEN_FAILURES: Duration = Duration::from_mins(5);
pub const FAILURE_INTERVAL: Duration = Duration::from_secs(30);
pub const CONNECTED_INTERVAL: Duration = Duration::from_mins(3);
pub const DISCONNECTED_INTERVAL: Duration = Duration::from_secs(90);

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub enum DestinationHealth {
    #[default]
    Init,
    Running {
        since: SystemTime,
        amount_of_failures: u32,
    },
    Failure {
        checked_at: SystemTime,
        last_error: String,
        amount_of_failures: u32,
    },
    Success {
        checked_at: SystemTime,
        health: gvpn_client::Health,
        total_time: Duration,
        round_trip_time: Duration,
    },
}

pub struct Runner {
    destination: Destination,
    hopr: Arc<Hopr>,
    options: Options,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub message: String,
    pub checked_at: SystemTime,
}

impl Runner {
    pub fn new(destination: Destination, options: Options, hopr: Arc<Hopr>) -> Self {
        Self {
            destination,
            hopr,
            options,
        }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run().await;
        let _ = results_sender
            .send(Results::HealthCheck {
                id: self.destination.id.clone(),
                res,
            })
            .await;
    }

    async fn run(&self) -> Result<DestinationHealth, Error> {
        let checked_at = SystemTime::now();

        // 1. open health session
        let measure_total = Instant::now();
        let health_config =
            runner::to_surb_balancer_config(self.options.buffer_sizes.health, self.options.max_surb_upstream.health)
                .map_err(|err| Error {
                    message: format!("Surb balancer config error: {err}"),
                    checked_at,
                })?;
        let health_session = open_health_session(&self.hopr, &self.destination, &self.options, health_config)
            .await
            .map_err(|err| Error {
                message: format!("Session creation error: {err}"),
                checked_at,
            })?;

        // 2. request health
        let measure_rtt = Instant::now();
        let health_res = request_health(&self.options, &health_session).await;
        let rtt = measure_rtt.elapsed();

        // 3. close bridge session
        close_health_session(&self.hopr, &health_session).await;

        let measure_total = measure_total.elapsed();
        health_res
            .map(|health| DestinationHealth::Success {
                checked_at,
                health,
                total_time: measure_total,
                round_trip_time: rtt,
            })
            .map_err(|err| Error {
                message: format!("Health request error: {err}"),
                checked_at,
            })
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
    tracing::debug!(%destination, "attempting to open bridge session for health check");
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
    let port = session_client_metadata.bound_host.port();
    let timeout = options.timeouts.http;

    tracing::debug!(%port, ?timeout, "requesting health status from exit");
    let client = reqwest::Client::new();
    gvpn_client::health(&client, port, timeout).await
}

async fn close_health_session(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) {
    tracing::debug!( bound_host = ?session_client_metadata.bound_host, "closing bridge session from health check");
    let _ = hopr
        .close_session(session_client_metadata.bound_host, session_client_metadata.protocol)
        .await
        .map_err(|err| {
            tracing::warn!(error = ?err, "failed to close health session after health check");
            err
        });
}

impl Display for DestinationHealth {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            DestinationHealth::Init => write!(f, "waiting for connection"),
            DestinationHealth::Running {
                since,
                amount_of_failures: _,
            } => write!(f, "running since {}", log_output::elapsed(since)),
            DestinationHealth::Failure {
                checked_at,
                last_error,
                amount_of_failures,
            } if *amount_of_failures > 1 => write!(
                f,
                "failed {} times in a row {} ago: {}",
                amount_of_failures,
                log_output::elapsed(checked_at),
                last_error
            ),
            DestinationHealth::Failure {
                checked_at,
                last_error,
                amount_of_failures: _,
            } => write!(f, "failed {} ago: {}", log_output::elapsed(checked_at), last_error),
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
