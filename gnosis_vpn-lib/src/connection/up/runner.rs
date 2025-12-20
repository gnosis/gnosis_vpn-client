//! The runner module for `core::connection::up` struct.
//! It handles state transitions up until wg tunnel initiation and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backon::{FibonacciBuilder, ExponentialBuilder, Retryable};
use edgli::hopr_lib::SessionClientConfig;
use tokio::sync::{oneshot, mpsc};


use std::fmt::{self, Display};
use std::sync::Arc;
use std::time::Duration;

use crate::connection::destination::Destination;
use crate::connection::options::Options;
use crate::core::runner::{self, Results};
use crate::gvpn_client::{self, Registration};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::wireguard::{self, WireGuard};
use crate::event::RespondableRequestToRoot;

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
        let _ = results_sender.send(Results::ConnectionResult { res }).await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 0. generate wg keys
        let _ = results_sender
            .send(progress(Progress::GenerateWg))
            .await;
        let wg = WireGuard::from_config(self.wg_config.clone()).await?;
        let public_key = wg.key_pair.public_key.clone();

        // 1. open bridge session
        let _ = results_sender
            .send(progress(Progress::OpenBridge(wg)))
            .await;
        let bridge_session = open_bridge_session(&self.hopr, &self.destination, &self.options, &results_sender).await?;

        // 2. register wg public key
        let _ = results_sender
            .send(progress(Progress::RegisterWg))
            .await;
        let registration = register(&self.options, &bridge_session, public_key, &results_sender).await?;

        // 3. close bridge session
        let _ = results_sender
            .send(progress(Progress::CloseBridge(registration)))
            .await;
        close_bridge_session(&self.hopr, &bridge_session).await?;

        // 4. open ping session
        let _ = results_sender
            .send(progress(Progress::OpenPing))
            .await;
        let session = open_ping_session(&self.hopr, &self.destination, &self.options, &results_sender).await?;

        // 5. establish wg tunnel
        let _ = results_sender
            .send(progress(Progress::WgTunnel(session)))
            .await;

        // 5a. request dynamic wg tunnel from root
        let (tx, rx) = oneshot::channel();
                            let interface_info = wireguard::InterfaceInfo { address: reg.address() };
                            let peer_info = wireguard::PeerInfo {
                                public_key: reg.server_public_key(),
                                endpoint: format!("127.0.0.1:{}", session.bound_host.port()),
                            };
                            let wg_data = event::WgData {
                                wg,
                                peer_info,
                                interface_info,
                            };
        let _ = results_sender.send(Results::ConnectionRequestToRoot(RespondableRequestToRoot::DynamicWgRouting { wg_data, resp: tx, })).await;
        let res = await_with_timeout(rx, Duration::from_secs(60)).await?;

        match res {
            Ok(()) => {
                self.run_after_wg_tunnel_established(&results_sender).await
            },
                Err(err) => {
                        tracing::error!(error = ?err, "failed to establishment dynamically routed WireGuard tunnel");
                        self.run_fallback_to_static_wg_tunnel(&results_sender).await
            },
        }
    }

    async fn run_fallback_to_static_wg_tunnel(&self, results_sender: &mpsc::Sender<Results>) -> Result<(), Error> {
        // 5b. gather announced peer ids
        peer_ips = self.peers().await?;
        // 5c. request static wg tunnel from root
        let (tx, rx) = oneshot::channel();
        let _ = results_sender.send(Results::RequestStaticWgTunnel { wg_data, peer_ips, resp: tx, }).await;
        await_with_timeout(rx, Duration::from_secs(60)).await?;

        self.run_after_wg_tunnel_established(&results_sender).await
    }

    async fn run_after_wg_tunnel_established(&self, results_sender: mpsc::Sender<Results>) -> Result<(), Error> {
        // 6. check ping
        let _ = results_sender.send(Results::ConnectionEvent {evt: progress(Progress::Ping) }).await;

        // 6a. request ping from root
        let round_trip_time = self.ping().await?;
        /*
    }).retry(FibonacciBuilder::default())
        .when(|err: &Error| err.is_ping_error())
            .notify(|err: &Error, dur: Duration| {
                let _ = results_sender.send(Results::ConnectionEvent { evt: setback(Setback::Ping(err.to_string())), }).await;
                tracing::debug!("retrying ping after {:?}", dur);
            })
        .await;
        */
        // let round_trip_time = ping(&self.options).await?;

        tracing::info!(?round_trip_time, "ping successful");

        // 7. adjust to main session
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
    (|| async {
        tracing::debug!(%destination, "attempting to open bridge session");
        hopr
            .open_session(
                destination.address,
                options.sessions.bridge.target.clone(),
                Some(1),
                Some(1),
                cfg.clone(),
            )
            .await
    }).retry(ExponentialBuilder::default())
    .when(|err: &HoprError>| {
        tracing::error!(error = ?err, "when on open");
        true
    })
    .notify(|err: &HoprError, dur: Duration| {
            let _ = results_sender.send(Results::ConnectionEvent { evt: setback(Setback::OpenBridge(err.to_string())), }).await;
            tracing::debug!("retrying open bridge session after {:?}", dur);
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
    (|| async {
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
    .retry(ExponentialBuilder::default())
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
    (|| async {
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
    .retry(ExponentialBuilder::default())
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

fn progress(progress: Progress) -> Results {
    Results::ConnectionEvent(Event::Progress(progress))
}

async fn await_with_timeout<T>(rx: tokio::sync::oneshot::Receiver<T>, duration: Duration) -> Result<T, Error> {
        tokio::select!(
            res = rx => res.map_err(|_| Error::Runtime("Channel closed unexpectedly".to_string())),
            _ = tokio::time::sleep(duration) => {
                Err(Error::Runtime("Timed out waiting for response".to_string()))
            }
        )
}
/*
            Results::ConnectionResult { res } => {
                tracing::debug!(?res, "handling pre wg connection runner result");
                match (res, self.phase.clone()) {
                    (Ok(session), Phase::Connecting(mut conn)) => {
                        if let (Some(wg), Some(reg)) = (conn.wireguard.clone(), conn.registration.clone()) {
                            let interface_info = wireguard::InterfaceInfo { address: reg.address() };
                            let peer_info = wireguard::PeerInfo {
                                public_key: reg.server_public_key(),
                                endpoint: format!("127.0.0.1:{}", session.bound_host.port()),
                            };
                            let wg_data = event::WgData {
                                wg,
                                peer_info,
                                interface_info,
                            };
                            self.outgoing_sender
                                .send(OutgoingCore::WgUp(wg_data))
                                .await
                                .expect("worker outgoing channel closed - shutting down");
                            let evt = connection::up::Progress::WgTunnel(session);
                            conn.connect_progress(evt);
                            self.phase = Phase::Connecting(conn);
                        } else {
                            tracing::error!(%conn, "missing WireGuard or registration data for connection - disconnecting");
                            self.target_destination = None;
                            self.act_on_target(results_sender);
                        }
                    }
                    (Err(err), Phase::Connecting(conn)) => {
                        tracing::error!(%conn, %err, "Opening ping session failed - disconnecting");
                        self.update_health(conn.destination.address, |h| h.with_error(err.to_string()));
                        self.target_destination = None;
                        self.act_on_target(results_sender);
                    }
                    (Ok(_), phase) => {
                        tracing::warn!(?phase, "unawaited opening ping session succeeded");
                    }
                    (Err(err), phase) => {
                        tracing::warn!(?phase, %err, "connection failed in unexpecting state");
                    }
                }
            }
*/
