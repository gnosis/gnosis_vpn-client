//! The runner module for `core::connection::down` struct.
//! It handles all state transitions and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backon::Retryable;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::Duration;

use crate::connection;
use crate::connection::bridge;
use crate::connection::options::Options;
use crate::connection::options::surb_config_for;
use crate::core::runner::Results;
use crate::hopr::Hopr;
use crate::hopr::types::SessionClientMetadata;
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
        let (mut own_bridge, mut bridge_session) = match self.reuse.clone() {
            Some(reused) => (None, reused),
            None => {
                let _ = results_sender
                    .send(Results::DisconnectionEvent {
                        wg_public_key: self.down.wg_public_key.clone(),
                        evt: Event::OpenBridge,
                    })
                    .await;
                let own = self.open_bridge().await?;
                let session = own.session().clone();
                (Some(own), session)
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
        let mut unregister_res = self.unregister_over(&bridge_session).await;
        // The probe's listener can be gone (the same failure the connect side works around); the
        // exit never saw the request, so retrying over a bridge of our own is still one unregister.
        if let Err(Error::GvpnClient(err)) = &unregister_res
            && err.is_local_connect()
            && err.local_cause() != gvpn_client::LocalConnectCause::AddressUnavailable
            && own_bridge.is_none()
        {
            tracing::warn!(error = ?err, "cannot reach the probe bridge session listener - unregistering over a bridge session of our own");
            match self.open_bridge().await {
                Ok(own) => {
                    bridge_session = own.session().clone();
                    own_bridge = Some(own);
                    unregister_res = self.unregister_over(&bridge_session).await;
                }
                Err(open_error) => {
                    tracing::warn!(%open_error, "cannot open a bridge session of our own - keeping the original failure");
                }
            }
        }

        // 3. close only a bridge we opened ourselves - also after a failed unregister, so it does not linger
        let _ = results_sender
            .send(Results::DisconnectionEvent {
                wg_public_key: self.down.wg_public_key.clone(),
                evt: Event::CloseBridge,
            })
            .await;
        let close_res = match &mut own_bridge {
            Some(own) => own.close().await.map_err(Error::from),
            None => Ok(()),
        };

        unregister_res?;
        close_res
    }

    async fn open_bridge(&self) -> Result<bridge::Owned, Error> {
        let bridge_surb = surb_config_for(&self.options.surb_balancing.bridge)?;
        Ok(bridge::Owned::open(self.hopr.clone(), &self.down.destination, &self.options, bridge_surb).await?)
    }

    /// One bounded unregister over `bridge_session`.
    async fn unregister_over(&self, bridge_session: &SessionClientMetadata) -> Result<(), Error> {
        let unregister_fut = unregister(&self.options, bridge_session, self.down.wg_public_key.clone());
        match tokio::time::timeout(UNREGISTER_BUDGET, unregister_fut).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(gvpn_client::Error::RegistrationNotFound)) => {
                tracing::warn!(wg_public_key = %self.down.wg_public_key, "trying to unregister already removed registration");
                Ok(())
            }
            Ok(Err(error)) => Err(Error::from(error)),
            Err(_elapsed) => Err(Error::Timeout(UNREGISTER_BUDGET)),
        }
    }
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
    // A local connect failure is not the exit's fault: retrying over the same listener cannot help.
    .when(|err: &gvpn_client::Error| {
        !matches!(err, gvpn_client::Error::RegistrationNotFound) && !err.is_local_connect()
    })
    .notify(|err: &gvpn_client::Error, dur: Duration| {
        tracing::warn!(error = ?err, "unregister wg pubkey failed - will retry after {:?}", dur);
    })
    .await
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
