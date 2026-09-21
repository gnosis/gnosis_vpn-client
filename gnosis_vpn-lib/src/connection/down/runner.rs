//! The runner module for `core::connection::down` struct.
//! It handles all state transitions and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backon::Retryable;
use edgli::FlowControlConfig;
use edgli::hopr_lib::HoprSessionClientConfig;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::Duration;

use crate::connection;
use crate::connection::options::Options;
use crate::connection::options::{SurbParams, surb_config_for};
use crate::core::runner::Results;
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::{gvpn_client, remote_data};

use super::{Error, Event};

/// hopr-lib's own 1-hop open_session budget; the tunnel is already gone, so a slower unregister is not worth waiting for.
const UNREGISTER_BUDGET: Duration = Duration::from_secs(15);

pub(crate) struct Runner {
    down: connection::down::Down,
    hopr: Arc<Hopr>,
    options: Options,
    /// The probe's session to unregister over; without it the runner opens and closes its own bridge.
    reuse: Option<SessionClientMetadata>,
}

impl Runner {
    pub(crate) fn new(
        down: connection::down::Down,
        hopr: Arc<Hopr>,
        options: Options,
        reuse: Option<SessionClientMetadata>,
    ) -> Self {
        Self {
            down,
            hopr,
            options,
            reuse,
        }
    }

    pub(crate) async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender
            .send(Results::DisconnectionResult {
                wg_public_key: self.down.wg_public_key.clone(),
                res,
            })
            .await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 0. disconnect wg tunnel done from root - already happens in spawning process

        // 1. reuse the probe session or open a bridge of our own
        let bridge_session = match &self.reuse {
            Some(session) => session.clone(),
            None => {
                let _ = results_sender
                    .send(Results::DisconnectionEvent {
                        wg_public_key: self.down.wg_public_key.clone(),
                        evt: Event::OpenBridge,
                    })
                    .await;
                let bridge_surb = surb_config_for(&self.options.surb_balancing.bridge)?;
                open_bridge_session(&self.hopr, &self.down, &self.options, bridge_surb).await?
            }
        };

        // 2. unregister wg public key
        let _ = results_sender
            .send(Results::DisconnectionEvent {
                wg_public_key: self.down.wg_public_key.clone(),
                evt: Event::UnregisterWg,
            })
            .await;
        // Only the unregister is bounded: the bridge session opened above must be closed on every path.
        let unregister_fut = unregister(&self.options, &bridge_session, self.down.wg_public_key.clone());
        let unregister_res = match tokio::time::timeout(UNREGISTER_BUDGET, unregister_fut).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(gvpn_client::Error::RegistrationNotFound)) => {
                tracing::warn!(wg_public_key = %self.down.wg_public_key, "trying to unregister already removed registration");
                Ok(())
            }
            Ok(Err(error)) => Err(Error::from(error)),
            Err(_elapsed) => Err(Error::Timeout(UNREGISTER_BUDGET)),
        };

        // 3. close only a bridge we opened ourselves - also after a failed unregister, so it does not linger
        let _ = results_sender
            .send(Results::DisconnectionEvent {
                wg_public_key: self.down.wg_public_key.clone(),
                evt: Event::CloseBridge,
            })
            .await;
        let close_res = match &self.reuse {
            Some(_) => Ok(()),
            None => close_bridge_session(&self.hopr, &bridge_session).await,
        };

        unregister_res?;
        Ok(close_res?)
    }
}

async fn open_bridge_session(
    hopr: &Hopr,
    down: &connection::down::Down,
    options: &Options,
    surb: SurbParams,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = HoprSessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path: down.destination.routing,
        return_path: down.destination.routing,
        always_max_out_surbs: surb.always_max_out_surbs,
        surb_management: surb.management,
        // Robust tail-tolerance profile for the down-direction data session.
        flow_control: Some(FlowControlConfig::robust()),
        ..Default::default()
    };
    let cfg = if options.pix.bridge.enabled {
        hopr.pix_aware_session_cfg(cfg)?
    } else {
        cfg
    };
    hopr.open_session(
        down.destination.address,
        down.destination.bridge_target(),
        Some(1),
        Some(1),
        cfg.clone(),
    )
    .await
}

async fn unregister(
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
    public_key: String,
) -> Result<(), gvpn_client::Error> {
    let input = gvpn_client::Input::new(public_key, session_client_metadata.bound_host, options.timeouts.http);
    (|| async {
        let client = reqwest::Client::new();
        gvpn_client::unregister(&client, &input).await
    })
    // One extra attempt only - UNREGISTER_BUDGET bounds the whole thing anyway.
    .retry(remote_data::backoff_expo_short_delay().with_max_times(1))
    .when(|err: &gvpn_client::Error| !matches!(err, gvpn_client::Error::RegistrationNotFound))
    .notify(|err: &gvpn_client::Error, dur: Duration| {
        tracing::warn!(error = ?err, "unregister wg pubkey failed - will retry after {:?}", dur);
    })
    .await
}

async fn close_bridge_session(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
    let res = hopr
        .close_session(session_client_metadata.bound_host, session_client_metadata.protocol)
        .await;
    match res {
        Ok(_) => Ok(()),
        Err(HoprError::SessionNotFound) => {
            tracing::warn!(bound_host = ?session_client_metadata.bound_host, "attempted to close bridge session but it was not found, possibly already closed");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

impl Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DisconnectionRunner for {}", self.down)
    }
}

impl Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::OpenBridge => write!(f, "OpenBridge"),
            Event::UnregisterWg => write!(f, "UnregisterWg"),
            Event::CloseBridge => write!(f, "CloseBridge"),
        }
    }
}
