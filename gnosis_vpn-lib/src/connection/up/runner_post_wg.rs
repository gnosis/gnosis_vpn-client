//! The runner module for `core::connection::up` struct.
//! It handles state transitions after wg tunnel established and  forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backoff::ExponentialBackoff;
use backoff::future::retry;
use tokio::sync::mpsc;

use std::fmt::{self, Display};
use std::sync::Arc;

use crate::connection::destination::Destination;
use crate::connection::options::Options;
use crate::core::runner::{self, Results};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::ping;

use super::{Error, Event, Progress, Setback};

pub struct Runner {
    destination: Destination,
    hopr: Arc<Hopr>,
    options: Options,
    ping_session: SessionClientMetadata,
}

impl Runner {
    pub fn new(
        destination: Destination,
        options: Options,
        ping_session: SessionClientMetadata,
        hopr: Arc<Hopr>,
    ) -> Self {
        Self {
            destination,
            hopr,
            options,
            ping_session,
        }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender.send(Results::ConnectionResult { res }).await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // check ping
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::Ping),
            })
            .await;
        ping(&self.options).await?;

        // adjust to main session
        let _ = results_sender
            .send(Results::ConnectionEvent {
                evt: progress(Progress::AdjustToMain),
            })
            .await;
        adjust_to_main_session(&self.hopr, &self.options, &self.ping_session).await?;
        Ok(())
    }
}

impl Display for Runner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ConnectionRunner post WireGuard {{ {} }}", self.destination)
    }
}

/*
async fn wg_tunnel(
    registration: &Registration,
    session_client_metadata: &SessionClientMetadata,
    wg: &wireguard::WireGuard,
) -> Result<(), wireguard::Error> {
    // run wg-quick down once to ensure no dangling state
    // _ = wg_tooling::down().await;
    unimplemented!();

    let interface_info = wireguard::InterfaceInfo {
        address: registration.address(),
        mtu: session_client_metadata.hopr_mtu,
    };

    let peer_info = wireguard::PeerInfo {
        public_key: registration.server_public_key(),
        endpoint: format!("127.0.0.1:{}", session_client_metadata.bound_host.port()),
    };

    tracing::debug!(%registration, "establishing wg tunnel");
    // wg.up(&interface_info, &peer_info).await
    unimplemented!()
}
*/

async fn ping(options: &Options) -> Result<(), ping::Error> {
    retry(ExponentialBackoff::default(), || async {
        tracing::debug!(?options, "attempting to ping through wg tunnel");
        ping::ping(&options.ping_options)?;
        Ok(())
    })
    .await
}

async fn adjust_to_main_session(
    hopr: &Hopr,
    options: &Options,
    session_client_metadata: &SessionClientMetadata,
) -> Result<(), HoprError> {
    let active_client = match session_client_metadata.active_clients.as_slice() {
        [] => return Err(HoprError::SessionNotFound),
        [client] => client.clone(),
        _ => return Err(HoprError::SessionAmbiguousClient),
    };
    tracing::debug!(bound_host = ?session_client_metadata.bound_host, "adjusting to main session");
    let surb_management = runner::to_surb_balancer_config(options.buffer_sizes.main, options.max_surb_upstream.main);
    hopr.adjust_session(surb_management, active_client).await
}

fn setback(setback: Setback) -> Event {
    Event::Setback(setback)
}

fn progress(progress: Progress) -> Event {
    Event::Progress(progress)
}
