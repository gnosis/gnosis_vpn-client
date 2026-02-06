/// This module helps identifiying the health of a destination's exit.
use edgli::hopr_lib::SessionClientConfig;
use edgli::hopr_lib::SurbBalancerConfig;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use crate::connection::destination::Destination;
use crate::connection::options::Options;
use crate::core::runner::SurbConfigError;
use crate::core::runner::{self, Results};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::{gvpn_client, log_output};

/// Health status of the exit and routing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DestinationHealth {
    pub slots: gvpn_client::Slots,
    pub load_avg: gvpn_client::LoadAvg,
    pub round_trip_time: Duration,
    pub checked_at: SystemTime,
}

pub struct Runner {
    destination: Destination,
    hopr: Arc<Hopr>,
    options: Options,
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Health error: {0}")]
    Health(String),
    #[error("Surb config error: {0}")]
    SurbConfig(#[from] SurbConfigError),
    #[error("Hopr error: {0}")]
    Hopr(#[from] HoprError),
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
        let measure = Instant::now();

        // 1. open health session
        let health_config =
            runner::to_surb_balancer_config(self.options.buffer_sizes.health, self.options.max_surb_upstream.health)?;
        let health_session = open_health_session(&self.hopr, &self.destination, &self.options, health_config).await?;

        // 2. request health
        let health_res = request_health(&self.options, &health_session).await;

        // 3. close bridge session
        close_health_session(&self.hopr, &health_session).await;

        // 4. record stats
        let round_trip_time = measure.elapsed();

        health_res
            .map(|health| DestinationHealth {
                slots: health.slots,
                load_avg: health.load_avg,
                round_trip_time,
                checked_at,
            })
            .map_err(|err| Error::Health(format!("Failed to request health status: {err}")))
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
        write!(
            f,
            "{}, {}, RoundTripTime: {:.2} s, {} ago",
            self.slots,
            self.load_avg,
            self.round_trip_time.as_secs_f64(),
            log_output::elapsed(&self.checked_at)
        )
    }
}
