//! The runner module for `core::connection::up` struct.
//! It handles state transitions up until wg tunnel initiation and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backon::{FibonacciBuilder, Retryable};
use edgli::hopr_lib::{HoprSessionClientConfig, api::types::internal::protocol::HoprPseudonym};
use tokio::sync::{mpsc, oneshot};

use std::fmt::{self, Display};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use crate::connection::destination::Destination;
use crate::connection::options::{Options, SurbParams, surb_config_for};
use crate::core::runner::Results;
use crate::event::{self, RunnerToRoot};
use crate::gvpn_client::{self, Registration};
use crate::hopr::types::SessionClientMetadata;
use crate::hopr::{self, Hopr, HoprError};
use crate::wireguard::{self, WireGuard};
use crate::worker_params::WorkerParams;
use crate::{ping, remote_data};

use super::{Error, Event, Progress, Setback};

/// State carried over from a previous connection attempt.
pub(crate) struct PreviousConnection {
    /// Blokli IPs resolved during the previous connection (reused when killswitch blocks DNS).
    pub blokli_ips: Vec<Ipv4Addr>,
    /// Session pseudonym from the previous connection (reused to avoid re-registration churn).
    pub pseudonym: Option<HoprPseudonym>,
    /// WireGuard public key from the previous connection to unregister during bridge cleanup.
    pub wg_public_key: Option<String>,
}

pub(crate) struct Runner {
    destination: Destination,
    hopr: Arc<Hopr>,
    options: Options,
    wg_config: wireguard::Config,
    worker_params: WorkerParams,
    prev_conn: PreviousConnection,
}

impl Runner {
    pub(crate) fn new(
        destination: Destination,
        options: Options,
        wg_config: wireguard::Config,
        hopr: Arc<Hopr>,
        worker_params: WorkerParams,
        prev_conn: PreviousConnection,
    ) -> Self {
        Self {
            destination,
            hopr,
            options,
            wg_config,
            worker_params,
            prev_conn,
        }
    }

    pub(crate) async fn start(&self, results_sender: mpsc::Sender<Results>) {
        let res = self.run(results_sender.clone()).await;
        let _ = results_sender.send(Results::ConnectionResult { res }).await;
    }

    async fn run(&self, results_sender: mpsc::Sender<Results>) -> Result<SessionClientMetadata, Error> {
        // 1. resolve blokli ips — use cached IPs when killswitch is active (DNS unreachable)
        let _ = results_sender.send(progress(Progress::ResolveBlokliIps)).await;
        let blokli_url = hopr::blokli_url(self.worker_params.blokli_url());
        let blokli_ips = if self.prev_conn.blokli_ips.is_empty() {
            remote_data::resolve_ips(&blokli_url).await?
        } else {
            self.prev_conn.blokli_ips.clone()
        };

        // 2. generate wg keys
        let _ = results_sender
            .send(progress(Progress::GenerateWg(blokli_ips.clone())))
            .await;
        let wg = WireGuard::from_config(self.wg_config.clone()).await?;
        let public_key = wg.key_pair.public_key.clone();

        // 3. open bridge session
        let _ = results_sender.send(progress(Progress::OpenBridge(wg.clone()))).await;
        let bridge_surb = surb_config_for(&self.options.surb_balancing.bridge)?;
        let bridge_session = open_bridge_session(
            &self.hopr,
            &self.destination,
            &self.options,
            bridge_surb,
            &results_sender,
        )
        .await?;
        let _ = results_sender
            .send(progress(Progress::BridgeOpened(bridge_session.clone())))
            .await;

        // 4. register wg public key
        let _ = results_sender.send(progress(Progress::RegisterWg)).await;
        let registration = register(&self.options, &bridge_session, public_key, &results_sender).await?;

        // 5. signal ping phase (carries registration) and close bridge in background
        let _ = results_sender
            .send(progress(Progress::OpenPing(registration.clone())))
            .await;
        spawn_background_bridge_cleanup(
            self.hopr.clone(),
            bridge_session,
            self.options.clone(),
            self.prev_conn.wg_public_key.clone(),
            results_sender.clone(),
        );

        // 6. open ping session
        let ping_surb = surb_config_for(&self.options.surb_balancing.ping)?;
        let session = open_ping_session(
            &self.hopr,
            &self.destination,
            &self.options,
            ping_surb,
            self.prev_conn.pseudonym,
            &results_sender,
        )
        .await?;

        // 7. gather ips of all announced peers
        let _ = results_sender.send(progress(Progress::PeerIps)).await;
        let mut peer_ips = gather_peer_ips(&self.hopr).await?;
        // blokli must be in the initial snapshot so it becomes part of the permanent
        // firewall floor and stays reachable for the duration of the connection.
        peer_ips.extend(blokli_ips);

        // 8. setup static wg tunnel — returns the resolved WireGuard interface name
        let _ = results_sender
            .send(progress(Progress::StaticWgTunnel(session.clone())))
            .await;
        let interface =
            request_static_wg_tunnel(&wg, &registration, &session, peer_ips.clone(), &results_sender).await?;

        // 9. activate killswitch now that the interface name is known
        let _ = results_sender.send(progress(Progress::KillswitchLockdown)).await;
        request_killswitch_lockdown(peer_ips, interface, &results_sender).await?;

        // 10. verify tunnel with ping — give it some leeway with 5 retries
        let _ = results_sender.send(progress(Progress::Ping)).await;
        let round_trip_time = request_ping(&self.options.ping_options, 5, &results_sender).await?;

        // 11. adjust to main session
        let _ = results_sender
            .send(progress(Progress::AdjustToMain(round_trip_time)))
            .await;
        let main_surb = surb_config_for(&self.options.surb_balancing.main)?;
        if let Some(main_config) = main_surb.management {
            let active_client = match session.active_clients.as_slice() {
                [] => return Err(HoprError::SessionNotFound.into()),
                [client] => client.clone(),
                _ => return Err(HoprError::SessionAmbiguousClient.into()),
            };
            tracing::debug!(bound_host = ?session.bound_host, "adjusting to main session");
            self.hopr.adjust_session(main_config, active_client).await?;
        }

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
    surb: SurbParams,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = HoprSessionClientConfig {
        capabilities: options.sessions.bridge.capabilities,
        forward_path: destination.routing,
        return_path: destination.routing,
        always_max_out_surbs: surb.always_max_out_surbs,
        surb_management: surb.management,
        ..Default::default()
    };
    // Each open_session attempt times out after `initiation_timeout_base × (forward_hops + return_hops + 2)`,
    // where initiation_timeout_base defaults to 500 ms. hopr-lib retries 3× with 2 s delays before giving up:
    //   1-hop: ~2 s/attempt, ~15 s total
    //   2-hop: ~3 s/attempt, ~19 s total
    //   3-hop: ~4 s/attempt, ~23 s total
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
    .retry(remote_data::backoff_expo_short_delay_bridge())
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
    let input = gvpn_client::Input::new(public_key, session_client_metadata.bound_host, options.timeouts.http);
    (|| async {
        tracing::debug!(?input, "attempting to register gvpn client public key");
        let client = reqwest::Client::new();
        gvpn_client::register(&client, &input).await
    })
    .retry(remote_data::backoff_expo_short_delay())
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
            tracing::warn!(bound_host = ?session_client_metadata.bound_host, "attempted to close bridge session but it was not found, possibly already closed");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

async fn open_ping_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    surb: SurbParams,
    pseudonym: Option<HoprPseudonym>,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SessionClientMetadata, HoprError> {
    let cfg = HoprSessionClientConfig {
        capabilities: options.sessions.wg.capabilities,
        forward_path: destination.routing,
        return_path: destination.routing,
        always_max_out_surbs: surb.always_max_out_surbs,
        surb_management: surb.management,
        pseudonym,
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
    .retry(remote_data::backoff_expo_short_delay())
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

async fn request_killswitch_lockdown(
    peer_ips: Vec<Ipv4Addr>,
    interface: String,
    results_sender: &mpsc::Sender<Results>,
) -> Result<(), Error> {
    let (tx, rx) = oneshot::channel();
    let _ = results_sender
        .send(Results::ConnectionRequestToRoot(RunnerToRoot::KillswitchLockdown {
            peer_ips,
            interface,
            resp: tx,
        }))
        .await;

    tokio::select!(
        res = rx => match res {
            Ok(Ok(interface)) => Ok(interface),
            Ok(Err(e)) => Err(Error::Routing(e)),
            Err(reason) => Err(Error::Runtime(format!("Channel closed unexpectedly: {reason}"))),
        },
        _ = tokio::time::sleep(Duration::from_secs(20)) => {
            Err(Error::Runtime("Timed out waiting for killswitch lockdown".to_string()))
        }
    )
}

async fn request_static_wg_tunnel(
    wg: &WireGuard,
    registration: &Registration,
    session: &SessionClientMetadata,
    peer_ips: Vec<Ipv4Addr>,
    results_sender: &mpsc::Sender<Results>,
) -> Result<String, Error> {
    let (tx, rx) = oneshot::channel();
    let interface_info = wireguard::InterfaceInfo {
        address: registration.address(),
    };
    let peer_info = wireguard::PeerInfo {
        public_key: registration.server_public_key(),
        preshared_key: registration.preshared_key(),
        endpoint: format!(
            "{host}:{port}",
            host = session.bound_host.ip(),
            port = session.bound_host.port()
        ),
    };
    let wg_data = event::WireGuardData {
        wg: wg.clone(),
        peer_info,
        interface_info,
    };
    let _ = results_sender
        .send(Results::ConnectionRequestToRoot(RunnerToRoot::StaticWgRouting {
            wg_data,
            peer_ips,
            resp: tx,
        }))
        .await;

    tokio::select!(
        res = rx => match res {
            Ok(Ok(interface)) => Ok(interface),
            Ok(Err(e)) => Err(Error::Routing(e)),
            Err(reason) => Err(Error::Runtime(format!("Channel closed unexpectedly: {}", reason))),
        },
        _ = tokio::time::sleep(Duration::from_secs(20)) => {
            Err(Error::Runtime("Timed out waiting for response".to_string()))
        }
    )
}

async fn gather_peer_ips(hopr: &Hopr) -> Result<Vec<Ipv4Addr>, HoprError> {
    let peers = hopr.announced_peers().await?;
    let mut peer_ips: Vec<Ipv4Addr> = peers.into_values().flat_map(|p| p.ipv4_addrs).collect();
    peer_ips.sort_unstable();
    peer_ips.dedup();
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
            .send(Results::ConnectionRequestToRoot(RunnerToRoot::Ping {
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

fn spawn_background_bridge_cleanup(
    hopr: Arc<Hopr>,
    bridge_session: SessionClientMetadata,
    options: Options,
    prev_public_key: Option<String>,
    results_sender: mpsc::Sender<Results>,
) {
    tokio::spawn(async move {
        if let Some(old_key) = prev_public_key {
            let input = gvpn_client::Input::new(old_key.clone(), bridge_session.bound_host, options.timeouts.http);
            let client = reqwest::Client::new();
            match gvpn_client::unregister(&client, &input).await {
                Ok(()) => tracing::debug!("unregistered old wg public key"),
                Err(gvpn_client::Error::RegistrationNotFound) => {
                    tracing::warn!(wg_public_key = %old_key, "old wg key not found during unregister, possibly already removed");
                }
                Err(err) => {
                    tracing::warn!(%err, "failed to unregister old wg public key");
                }
            }
        }
        if let Err(err) = close_bridge_session(&hopr, &bridge_session).await {
            tracing::warn!(%err, "failed to close bridge session in background");
        }
        let _ = results_sender.send(progress(Progress::BridgeClosed)).await;
    });
}

fn setback(setback: Setback) -> Results {
    Results::ConnectionEvent(Event::Setback(Box::new(setback)))
}

fn progress(progress: Progress) -> Results {
    Results::ConnectionEvent(Event::Progress(Box::new(progress)))
}
