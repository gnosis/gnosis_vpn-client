//! The runner module for `core::connection::up` struct.
//! It handles state transitions up until wg tunnel initiation and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backoff::ExponentialBackoff;
use backoff::future::retry;
use edgli::hopr_lib::SessionClientConfig;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;

use crate::connection::destination::Destination;
use crate::connection::options::Options;
use crate::core::runner::{self, Results};
use crate::gvpn_client::{self, Registration};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::wireguard::{self, WireGuard};

use super::{Error, Event, Progress, Setback};

pub struct Runner {
    destination: Destination,
    hopr: Arc<Hopr>,
    options: Options,
    wg_config: wireguard::Config,
}

impl Runner {
    pub fn new(destination: Destination, options: Options, wg_config: wireguard::Config, hopr: Arc<Hopr>) -> Self {
        Self {
            destination,
            hopr,
            options,
            wg_config,
        }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender.send(Results::ConnectionResultPreWg { res }).await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<SessionClientMetadata, Error> {
        // 0. generate wg keys
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::GenerateWg),
            })
            .await;
        let wg = WireGuard::from_config(self.wg_config.clone()).await?;
        let public_key = wg.key_pair.public_key.clone();

        // 1. open bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::OpenBridge(wg)),
            })
            .await;
        let bridge_session = open_bridge_session(&self.hopr, &self.destination, &self.options, &results_sender).await?;

        // 2. register wg public key
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::RegisterWg),
            })
            .await;
        let registration = register(&self.options, &bridge_session, public_key, &results_sender).await?;

        // 3. close bridge session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::CloseBridge(registration)),
            })
            .await;
        close_bridge_session(&self.hopr, &bridge_session).await?;

        // 4. open ping session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::OpenPing),
            })
            .await;
        let session = open_ping_session(&self.hopr, &self.destination, &self.options, &results_sender).await?;
        Ok(session)
    }
}

impl Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConnectionRunner pre WireGuard {{ {} }}", self.destination)
    }
}

#[tracing::instrument(
    skip(hopr, options, destination, results_sender),
    fields(
        address = %destination.address,
        routing = ?destination.routing,
    ),
    level = "debug",
    ret
)]
async fn open_bridge_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: Some(runner::to_surb_balancer_config(
            options.buffer_sizes.bridge,
            options.max_surb_upstream.bridge,
        )),
        ..Default::default()
    };
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(%destination,"attempting to open bridge session");
        let res = hopr
            .open_session(
                destination.address,
                options.sessions.bridge.target.clone(),
                Some(1),
                Some(1),
                cfg.clone(),
            )
            .await;
        if let Err(e) = &res {
            let _ = results_sender
                .send(Results::ConnectionEvent {
                    evt: setback(Setback::OpenBridge(e.to_string())),
                })
                .await;
        }
        let ret_val = res?;
        Ok(ret_val)
    })
    .await
}

async fn register(
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
    public_key: String,
    results_sender: &mpsc::Sender<Results>,
) -> Result<Registration, gvpn_client::Error> {
    let input = gvpn_client::Input::new(
        public_key,
        session_client_metadata.bound_host.port(),
        options.timeouts.http,
    );
    let client = reqwest::Client::new();
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(?input, "attempting to register gvpn client public key");
        let res = gvpn_client::register(&client, &input).await;
        if let Err(e) = &res {
            let _ = results_sender
                .send(Results::ConnectionEvent {
                    evt: setback(Setback::RegisterWg(e.to_string())),
                })
                .await;
        }
        let ret_val = res?;
        Ok(ret_val)
    })
    .await
}

async fn close_bridge_session(hopr: &Hopr, session_client_metadata: &SessionClientMetadata) -> Result<(), HoprError> {
    tracing::debug!(
        bound_host = ?session_client_metadata.bound_host,
        "closing bridge session"
    );
    let res = hopr
        .close_session(session_client_metadata.bound_host, session_client_metadata.protocol)
        .await;
    match res {
        Ok(_) => Ok(()),
        Err(HoprError::SessionNotFound) => {
            tracing::warn!("attempted to close bridge session but it was not found, possibly already closed");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

async fn open_ping_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.wg.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: Some(runner::to_surb_balancer_config(
            options.buffer_sizes.ping,
            options.max_surb_upstream.ping,
        )),
        ..Default::default()
    };
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(%destination, "attempting to open ping session");
        let res = hopr
            .open_session(
                destination.address,
                options.sessions.wg.target.clone(),
                None,
                None,
                cfg.clone(),
            )
            .await;
        if let Err(e) = &res {
            let _ = results_sender
                .send(Results::ConnectionEvent {
                    evt: setback(Setback::OpenPing(e.to_string())),
                })
                .await;
        }
        let ret_val = res?;
        Ok(ret_val)
    })
    .await
}

fn setback(setback: Setback) -> Event {
    Event::Setback(setback)
}

fn progress(progress: Progress) -> Event {
    Event::Progress(progress)
}
