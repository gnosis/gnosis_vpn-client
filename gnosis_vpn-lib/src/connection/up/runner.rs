//! The runner module for `core::connection::up` struct.
//! It handles state transitions up until wg tunnel initiation and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backon::{ExponentialBuilder, FibonacciBuilder, Retryable};
use edgli::hopr_lib::SessionClientConfig;
use edgli::hopr_lib::SurbBalancerConfig;
use tokio::sync::{mpsc, oneshot};

use std::fmt::{self, Display};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use crate::connection::destination::Destination;
use crate::connection::options::Options;
use crate::core::runner::{self, Results};
use crate::event::{self, RespondableRequestToRoot};
use crate::gvpn_client::{self, Registration};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{Hopr, HoprError};
use crate::hopr_params::HoprParams;
use crate::ping;
use crate::wireguard::{self, WireGuard};

use super::{Error, Event, Progress, Setback};

pub struct Runner {
    destination: Destination,
    hopr: Arc<Hopr>,
    options: Options,
    wg_config: wireguard::Config,
    hopr_params: HoprParams,
}

impl Runner {
    pub fn new(
        destination: Destination,
        options: Options,
        wg_config: wireguard::Config,
        hopr: Arc<Hopr>,
        hopr_params: HoprParams,
    ) -> Self {
        Self {
            destination,
            hopr,
            options,
            wg_config,
            hopr_params,
        }
    }

    pub async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender.send(Results::ConnectionResult { res }).await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<SessionClientMetadata, Error> {
        // 0. generate wg keys
        let _ = results_sender.send(progress(Progress::GenerateWg)).await;
        let wg = WireGuard::from_config(self.wg_config.clone()).await?;
        let public_key = wg.key_pair.public_key.clone();

        // 1. open bridge session
        let _ = results_sender.send(progress(Progress::OpenBridge(wg.clone()))).await;
        let bridge_config =
            runner::to_surb_balancer_config(self.options.buffer_sizes.bridge, self.options.max_surb_upstream.bridge)?;
        let bridge_session = open_bridge_session(
            &self.hopr,
            &self.destination,
            &self.options,
            bridge_config,
            &results_sender,
        )
        .await?;

        // 2. register wg public key
        let _ = results_sender.send(progress(Progress::RegisterWg)).await;
        let registration = register(&self.options, &bridge_session, public_key, &results_sender).await?;

        // 3. close bridge session
        let _ = results_sender
            .send(progress(Progress::CloseBridge(registration.clone())))
            .await;
        close_bridge_session(&self.hopr, &bridge_session).await?;

        // 4. open ping session
        let _ = results_sender.send(progress(Progress::OpenPing)).await;
        let ping_config =
            runner::to_surb_balancer_config(self.options.buffer_sizes.ping, self.options.max_surb_upstream.ping)?;
        let session = open_ping_session(
            &self.hopr,
            &self.destination,
            &self.options,
            ping_config,
            &results_sender,
        )
        .await?;

        // 5a. request dynamic wg tunnel from root
        let _ = results_sender
            .send(progress(Progress::DynamicWgTunnel(session.clone())))
            .await;
        // dynamic routing might block all outgoing communication
        // this leads to loosing peers and thus falling back to static routing might break because of that
        // gather peers before we start any routing attempt to ensure static routing might still work
        let peer_ips = gather_peer_ips(&self.hopr, self.options.announced_peer_minimum_score).await?;
        if self.hopr_params.force_static_routing() {
            tracing::info!("force static routing enabled - skipping dynamic routing attempt");
            self.run_fallback_to_static_wg_tunnel(&wg, &registration, &session, peer_ips, &results_sender)
                .await
        } else {
            let res = request_dynamic_wg_tunnel(&wg, &registration, &session, &results_sender).await;
            match res {
                Ok(()) => {
                    self.run_check_dynamic_routing(&wg, &registration, &session, peer_ips, &results_sender)
                        .await
                }
                Err(err) => {
                    tracing::warn!(error = ?err, "failed to establishment dynamically routed WireGuard tunnel - fallback to static routing");
                    self.run_fallback_to_static_wg_tunnel(&wg, &registration, &session, peer_ips, &results_sender)
                        .await
                }
            }
        }
    }

    async fn run_fallback_to_static_wg_tunnel(
        &self,
        wg: &WireGuard,
        registration: &Registration,
        session: &SessionClientMetadata,
        peer_ips: Vec<Ipv4Addr>,
        results_sender: &mpsc::Sender<Results>,
    ) -> Result<SessionClientMetadata, Error> {
        // 5b. gather ips of all announced peers
        let _ = results_sender.send(progress(Progress::PeerIps)).await;

        // 5c. request static wg tunnel from root
        let _ = results_sender
            .send(progress(Progress::StaticWgTunnel(peer_ips.len())))
            .await;
        request_static_wg_tunnel(wg, registration, session, peer_ips, results_sender).await?;
        self.run_check_static_routing(session, results_sender).await
    }

    async fn run_check_dynamic_routing(
        &self,
        wg: &WireGuard,
        registration: &Registration,
        session: &SessionClientMetadata,
        peer_ips: Vec<Ipv4Addr>,
        results_sender: &mpsc::Sender<Results>,
    ) -> Result<SessionClientMetadata, Error> {
        // 6a. request ping from root to check if dynamic routing works
        let _ = results_sender.send(progress(Progress::Ping)).await;
        // only one retry to avoid long fallback time in case dynamic routing doesn't work
        let res = request_ping(&self.options.ping_options, 1, results_sender).await;
        match res {
            Ok(round_trip_time) => {
                self.run_after_verified_working(round_trip_time, session, results_sender)
                    .await
            }
            Err(err) => {
                tracing::warn!(error = ?err, "ping over dynamically routed WireGuard tunnel failed - fallback to static routing");
                self.run_fallback_to_static_wg_tunnel(wg, registration, session, peer_ips, results_sender)
                    .await
            }
        }
    }

    async fn run_check_static_routing(
        &self,
        session: &SessionClientMetadata,
        results_sender: &mpsc::Sender<Results>,
    ) -> Result<SessionClientMetadata, Error> {
        // 6b. request ping from root to check if static routing works
        let _ = results_sender.send(progress(Progress::Ping)).await;
        // this is our last chance - give it some leeway with 5 retries
        let round_trip_time = request_ping(&self.options.ping_options, 5, results_sender).await?;
        self.run_after_verified_working(round_trip_time, session, results_sender)
            .await
    }

    async fn run_after_verified_working(
        &self,
        round_trip_time: Duration,
        session: &SessionClientMetadata,
        results_sender: &mpsc::Sender<Results>,
    ) -> Result<SessionClientMetadata, Error> {
        // 7. adjust to main session
        let _ = results_sender
            .send(progress(Progress::AdjustToMain(round_trip_time)))
            .await;
        let main_config =
            runner::to_surb_balancer_config(self.options.buffer_sizes.main, self.options.max_surb_upstream.main)?;

        let active_client = match session.active_clients.as_slice() {
            [] => return Err(HoprError::SessionNotFound.into()),
            [client] => client.clone(),
            _ => return Err(HoprError::SessionAmbiguousClient.into()),
        };
        tracing::debug!(bound_host = ?session.bound_host, "adjusting to main session");
        self.hopr.adjust_session(main_config, active_client).await?;

        Ok(session.clone())
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
    surb_management: SurbBalancerConfig,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: Some(surb_management),
        ..Default::default()
    };
    (|| async {
        tracing::debug!(%destination, "attempting to open bridge session");
        hopr.open_session(
            destination.address,
            options.sessions.bridge.target.clone(),
            Some(1),
            Some(1),
            cfg.clone(),
        )
        .await
    })
    .retry(ExponentialBuilder::default())
    .notify(|err: &HoprError, dur: Duration| {
        tracing::warn!(error = ?err, "error opening bridge session - will retry after {:?}", dur);
        let tx = results_sender.clone();
        let payload = setback(Setback::OpenBridge(err.to_string()));
        tokio::spawn(async move {
            let _ = tx.send(payload).await;
        });
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
    (|| async {
        tracing::debug!(?input, "attempting to register gvpn client public key");
        let client = reqwest::Client::new();
        gvpn_client::register(&client, &input).await
    })
    .retry(ExponentialBuilder::default())
    .notify(|err: &gvpn_client::Error, dur: Duration| {
        tracing::warn!(error = ?err, "register wg pubkey failed - will retry after {:?}", dur);
        let tx = results_sender.clone();
        let payload = setback(Setback::RegisterWg(err.to_string()));
        tokio::spawn(async move {
            let _ = tx.send(payload).await;
        });
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
    surb_management: SurbBalancerConfig,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = SessionClientConfig {
        capabilities: options.sessions.wg.capabilities,
        forward_path_options: destination.routing.clone(),
        return_path_options: destination.routing.clone(),
        surb_management: Some(surb_management),
        ..Default::default()
    };
    (|| async {
        tracing::debug!(%destination, "attempting to open ping session");
        hopr.open_session(
            destination.address,
            options.sessions.wg.target.clone(),
            None,
            None,
            cfg.clone(),
        )
        .await
    })
    .retry(ExponentialBuilder::default())
    .notify(|err: &HoprError, dur: Duration| {
        tracing::warn!(error = ?err, "error opening ping session - will retry after {:?}", dur);
        let tx = results_sender.clone();
        let payload = setback(Setback::OpenPing(err.to_string()));
        tokio::spawn(async move {
            let _ = tx.send(payload).await;
        });
    })
    .await
}

async fn request_dynamic_wg_tunnel(
    wg: &WireGuard,
    registration: &Registration,
    session: &SessionClientMetadata,
    results_sender: &mpsc::Sender<Results>,
) -> Result<(), Error> {
    let (tx, rx) = oneshot::channel();
    let interface_info = wireguard::InterfaceInfo {
        address: registration.address(),
    };
    let peer_info = wireguard::PeerInfo {
        public_key: registration.server_public_key(),
        endpoint: format!("127.0.0.1:{}", session.bound_host.port()),
    };
    let wg_data = event::WireGuardData {
        wg: wg.clone(),
        peer_info,
        interface_info,
    };
    let _ = results_sender
        .send(Results::ConnectionRequestToRoot(
            RespondableRequestToRoot::DynamicWgRouting { wg_data, resp: tx },
        ))
        .await;

    tokio::select!(
        res = rx => match res {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(Error::Routing(e)),
            Err(reason) => Err(Error::Runtime(format!("Channel closed unexpectedly: {}", reason))),
        },
        _ = tokio::time::sleep(Duration::from_secs(20)) => {
            Err(Error::Runtime("Timed out waiting for response".to_string()))
        }
    )
}

async fn request_static_wg_tunnel(
    wg: &WireGuard,
    registration: &Registration,
    session: &SessionClientMetadata,
    peer_ips: Vec<Ipv4Addr>,
    results_sender: &mpsc::Sender<Results>,
) -> Result<(), Error> {
    let (tx, rx) = oneshot::channel();
    let interface_info = wireguard::InterfaceInfo {
        address: registration.address(),
    };
    let peer_info = wireguard::PeerInfo {
        public_key: registration.server_public_key(),
        endpoint: format!("127.0.0.1:{}", session.bound_host.port()),
    };
    let wg_data = event::WireGuardData {
        wg: wg.clone(),
        peer_info,
        interface_info,
    };
    let _ = results_sender
        .send(Results::ConnectionRequestToRoot(
            RespondableRequestToRoot::StaticWgRouting {
                wg_data,
                peer_ips,
                resp: tx,
            },
        ))
        .await;

    tokio::select!(
        res = rx => match res {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(Error::Routing(e)),
            Err(reason) => Err(Error::Runtime(format!("Channel closed unexpectedly: {}", reason))),
        },
        _ = tokio::time::sleep(Duration::from_secs(20)) => {
            Err(Error::Runtime("Timed out waiting for response".to_string()))
        }
    )
}

async fn gather_peer_ips(hopr: &Hopr, minimum_score: f64) -> Result<Vec<Ipv4Addr>, HoprError> {
    let peers = hopr.announced_peers(minimum_score).await?;
    let peer_ips = peers.iter().map(|p| p.1.ipv4).collect();
    Ok(peer_ips)
}

async fn request_ping(
    options: &ping::Options,
    max_backoff: usize,
    results_sender: &mpsc::Sender<Results>,
) -> Result<Duration, Error> {
    (|| async {
        let (tx, rx) = oneshot::channel();
        let _ = results_sender
            .send(Results::ConnectionRequestToRoot(RespondableRequestToRoot::Ping {
                options: options.clone(),
                resp: tx,
            }))
            .await;
        tokio::select!(
            res = rx => match res {
                Ok(Ok(duration)) => Ok(duration),
                Ok(Err(e)) => Err(Error::Ping(e)),
                Err(reason) => Err(Error::Runtime(format!("Channel closed unexpectedly: {}", reason))),
            },
            _ = tokio::time::sleep(options.timeout + Duration::from_secs(20)) => {
                Err(Error::Runtime("Timed out waiting for response".to_string()))
            }
        )
    })
    .retry(FibonacciBuilder::new().with_jitter().with_max_times(max_backoff))
    .when(|err: &Error| err.is_ping_error())
    .notify(|err: &Error, dur: Duration| {
        tracing::warn!(error = ?err, "ping request failed - will retry after {:?}", dur);
        let tx = results_sender.clone();
        let payload = setback(Setback::Ping(err.to_string()));
        tokio::spawn(async move {
            let _ = tx.send(payload).await;
        });
    })
    .await
}

fn setback(setback: Setback) -> Results {
    Results::ConnectionEvent(Event::Setback(setback))
}

fn progress(progress: Progress) -> Results {
    Results::ConnectionEvent(Event::Progress(progress))
}
