//! The runner module for `core::connection::up` struct.
//! It handles state transitions up until wg tunnel initiation and forwards transition events though its channel.
//! This allows keeping the source of truth for data in `core` and avoiding structs duplication.
use backon::{FibonacciBuilder, Retryable};
use edgli::hopr_lib::{HoprSessionClientConfig, api::types::internal::protocol::HoprPseudonym};
use ipnetwork::IpNetwork;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use std::fmt::{self, Display};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use crate::connection::destination::Destination;
use crate::connection::options::{Options, SurbParams, surb_config_for};
use crate::core::runner::Results;
use crate::event::RunnerToRoot;
use crate::gvpn_client::{self, Registration};
use crate::hopr::types::{SessionClientMetadata, SplicedWgSession};
use crate::hopr::{self, Hopr, HoprError};
use crate::wireguard::{self, WireGuard};
use crate::worker_params::WorkerParams;
use crate::{ping, remote_data, wg_tunnel};

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
    /// Cancelled when the connection is torn down; scopes the lifetime of the
    /// spawned NepTUN pump so it stops (dropping the TUN fd and its network
    /// endpoint) on disconnect or reconnect.
    cancel: CancellationToken,
    /// Tracks the spawned pump task so core can wait for it to finish (and thus
    /// for the TUN fd to close) before asking root to tear down routing.
    pump_tasks: TaskTracker,
}

impl Runner {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        destination: Destination,
        options: Options,
        wg_config: wireguard::Config,
        hopr: Arc<Hopr>,
        worker_params: WorkerParams,
        prev_conn: PreviousConnection,
        cancel: CancellationToken,
        pump_tasks: TaskTracker,
    ) -> Self {
        Self {
            destination,
            hopr,
            options,
            wg_config,
            worker_params,
            prev_conn,
            cancel,
            pump_tasks,
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

        // 6. open the wg session (also carries the in-tunnel verification ping).
        //    With the udp-bridge data plane this binds a local loopback listener;
        //    with the spliced data plane the raw session is handed to the pump.
        let data_plane = wg_tunnel::data_plane();
        let ping_surb = surb_config_for(&self.options.surb_balancing.ping)?;
        let (session, spliced) = match data_plane {
            wg_tunnel::DataPlane::UdpBridge => {
                let session = open_ping_session(
                    &self.hopr,
                    &self.destination,
                    &self.options,
                    ping_surb,
                    self.prev_conn.pseudonym,
                    &results_sender,
                )
                .await?;
                (session, None)
            }
            wg_tunnel::DataPlane::Splice => {
                let spliced = open_spliced_wg_session(
                    &self.hopr,
                    &self.destination,
                    &self.options,
                    ping_surb,
                    self.prev_conn.pseudonym,
                    &results_sender,
                )
                .await?;
                (spliced.metadata.clone(), Some((spliced.session, spliced.configurator)))
            }
        };

        // 7. gather ips of all announced peers
        let _ = results_sender.send(progress(Progress::PeerIps)).await;
        let mut peer_ips = gather_peer_ips(&self.hopr).await?;
        // blokli must be in the initial snapshot so it becomes part of the permanent
        // firewall floor and stays reachable for the duration of the connection.
        peer_ips.extend(blokli_ips);

        // 8. set up the NepTUN data plane — root provisions the TUN device + routing
        //    and returns the resolved interface name; the worker then receives the
        //    TUN fd out-of-band and starts the pump. The pump's network side is
        //    either a loopback UDP socket dialing the session bridge port or the
        //    spliced session itself, per the data-plane selection above.
        let _ = results_sender
            .send(progress(Progress::StaticWgTunnel(session.clone())))
            .await;
        let allowed_ips = parse_allowed_ips(self.wg_config.allowed_ips.as_deref());
        let interface = request_setup_tunnel(&registration, &self.wg_config, peer_ips.clone(), &results_sender).await?;
        let (engine, tun_reader, tun_writer) = prepare_pump(&wg, &registration, allowed_ips).await?;
        let configurator = match spliced {
            None => {
                let udp = tokio::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                    .await
                    .map_err(|e| Error::Runtime(format!("failed to bind wg udp socket: {e}")))?;
                udp.connect(session.bound_host).await.map_err(|e| {
                    Error::Runtime(format!(
                        "failed to connect wg udp socket to {}: {e}",
                        session.bound_host
                    ))
                })?;
                let (net_tx, net_rx) = wg_tunnel::udp_endpoints(udp);
                self.spawn_pump_task(engine, net_tx, net_rx, tun_writer, tun_reader, &results_sender);
                None
            }
            Some((hopr_session, configurator)) => {
                let (read_half, write_half) = tokio::io::split(hopr_session);
                let net_tx = wg_tunnel::SessionSender::new(write_half);
                let net_rx = wg_tunnel::SessionReceiver::new(read_half);
                self.spawn_pump_task(engine, net_tx, net_rx, tun_writer, tun_reader, &results_sender);
                Some(configurator)
            }
        };

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
            match configurator {
                // A spliced session is not in the listener registry, so the SURB
                // balancer is adjusted through its configurator handle directly.
                Some(configurator) => {
                    tracing::debug!("adjusting spliced wg session to main session");
                    configurator
                        .update_surb_balancer_config(main_config)
                        .await
                        .map_err(|e| HoprError::SessionNotAdjusted(e.to_string()))?;
                }
                None => {
                    let active_client = match session.active_clients.as_slice() {
                        [] => return Err(HoprError::SessionNotFound.into()),
                        [client] => client.clone(),
                        _ => return Err(HoprError::SessionAmbiguousClient.into()),
                    };
                    tracing::debug!(bound_host = ?session.bound_host, "adjusting to main session");
                    self.hopr.adjust_session(main_config, active_client).await?;
                }
            }
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

/// Splice-mode sibling of [`open_ping_session`]: same capabilities, target, and
/// retry behavior, but returns the raw session for the pump instead of binding a
/// local listener.
async fn open_spliced_wg_session(
    hopr: &Hopr,
    destination: &Destination,
    options: &Options,
    surb: SurbParams,
    pseudonym: Option<HoprPseudonym>,
    results_sender: &mpsc::Sender<Results>,
) -> Result<SplicedWgSession, HoprError> {
    let cfg = HoprSessionClientConfig {
        capabilities: options.sessions.wg.capabilities,
        forward_path: destination.routing,
        return_path: destination.routing,
        always_max_out_surbs: surb.always_max_out_surbs,
        surb_management: surb.management,
        pseudonym,
    };
    (|| async {
        tracing::debug!(%destination, "attempting to open spliced wg session");
        hopr.open_wg_session(destination.address, options.sessions.wg.target.clone(), cfg.clone())
            .await
    })
    .retry(remote_data::backoff_expo_short_delay())
    .notify(|err: &HoprError, dur: Duration| {
        tracing::warn!(error = ?err, "error opening spliced wg session - will retry after {:?}", dur);
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

/// Ask root to provision the TUN device + split-tunnel routing and return the
/// resolved interface name. No key material is sent: the WireGuard keys stay in
/// the worker, where the `WgTunnel` runs.
async fn request_setup_tunnel(
    registration: &Registration,
    wg_config: &wireguard::Config,
    peer_ips: Vec<Ipv4Addr>,
    results_sender: &mpsc::Sender<Results>,
) -> Result<String, Error> {
    let (tx, rx) = oneshot::channel();
    let _ = results_sender
        .send(Results::ConnectionRequestToRoot(RunnerToRoot::SetupTunnel {
            interface_address: registration.address(),
            mtu: wireguard::WG_MTU,
            dns: wg_config.dns.clone(),
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

/// Receive the TUN fd from root and build the pump's engine and TUN endpoints.
/// The network endpoint is supplied by the caller per the data-plane selection.
async fn prepare_pump(
    wg: &WireGuard,
    registration: &Registration,
    allowed_ips: Vec<IpNetwork>,
) -> Result<(wg_tunnel::WgTunnel, wg_tunnel::TunReader, wg_tunnel::TunWriter), Error> {
    // The TUN fd is delivered out-of-band by root over the dedicated fd-passing
    // socket; block for it off the async runtime.
    let tun_fd = tokio::task::spawn_blocking(crate::socket::worker::recv_tun_fd)
        .await
        .map_err(|e| Error::Runtime(format!("tun fd receive task failed: {e}")))?
        .map_err(|e| Error::Runtime(format!("failed to receive tun fd from root: {e}")))?;
    let (tun_reader, tun_writer) = wg_tunnel::tun_endpoints(tun_fd, wg_tunnel::PLATFORM_TUN_HEADER_LEN)
        .map_err(|e| Error::Runtime(format!("failed to wrap tun fd: {e}")))?;

    let preshared = registration.preshared_key();
    let preshared = if preshared.is_empty() { None } else { Some(preshared) };
    let engine = wg_tunnel::WgTunnel::new(
        &wg.key_pair.priv_key,
        &registration.server_public_key(),
        preshared.as_deref(),
        &allowed_ips,
    )
    .map_err(|e| Error::Runtime(format!("failed to build wg tunnel: {e}")))?;

    Ok((engine, tun_reader, tun_writer))
}

impl Runner {
    /// Spawn the NepTUN pump onto the connection's task tracker. The task runs
    /// until `cancel` fires (disconnect/reconnect), at which point the TUN fd and
    /// network endpoint are dropped; core waits on the tracker before asking root
    /// to tear down routing. A pump that exits on its own (WG session expiry, an
    /// endpoint closing, or a pump error) reports through the results channel so
    /// core reconnects immediately instead of waiting for a ping failure.
    fn spawn_pump_task<NS, NR>(
        &self,
        engine: wg_tunnel::WgTunnel,
        net_tx: NS,
        net_rx: NR,
        tun_writer: wg_tunnel::TunWriter,
        tun_reader: wg_tunnel::TunReader,
        results_sender: &mpsc::Sender<Results>,
    ) where
        NS: wg_tunnel::NetworkSender + 'static,
        NR: wg_tunnel::NetworkReceiver + 'static,
    {
        let cancel = self.cancel.clone();
        let results_sender = results_sender.clone();
        self.pump_tasks.spawn(async move {
            match cancel
                .run_until_cancelled(wg_tunnel::run(engine, net_tx, net_rx, tun_writer, tun_reader))
                .await
            {
                None => tracing::debug!("wg pump stopped (connection cancelled)"),
                Some(Ok(exit)) => {
                    tracing::warn!(?exit, "wg pump exited - requesting reconnect");
                    let _ = results_sender
                        .send(Results::WgPumpExited {
                            reason: format!("{exit:?}"),
                        })
                        .await;
                }
                Some(Err(e)) => {
                    tracing::warn!(error = %e, "wg pump error - requesting reconnect");
                    let _ = results_sender
                        .send(Results::WgPumpExited { reason: e.to_string() })
                        .await;
                }
            }
        });
    }
}

/// Parse the peer's allowed-IPs (comma-separated CIDRs) used for INGRESS filtering
/// in the `WgTunnel`. Defaults to `0.0.0.0/0` (accept all from the single server
/// peer), matching the client-side config that was replaced. Unparseable entries
/// are skipped with a warning; an empty result falls back to the default.
fn parse_allowed_ips(allowed: Option<&str>) -> Vec<IpNetwork> {
    let default: IpNetwork = "0.0.0.0/0".parse().expect("valid default cidr");
    let nets: Vec<IpNetwork> = allowed
        .unwrap_or("0.0.0.0/0")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| match s.parse::<IpNetwork>() {
            Ok(net) => Some(net),
            Err(e) => {
                tracing::warn!(entry = %s, %e, "ignoring unparseable allowed-ip");
                None
            }
        })
        .collect();
    if nets.is_empty() { vec![default] } else { nets }
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
            let input = gvpn_client::Input::new(old_key, bridge_session.bound_host, options.timeouts.http);
            let client = reqwest::Client::new();
            match gvpn_client::unregister(&client, &input).await {
                Ok(()) => tracing::debug!("unregistered old wg public key"),
                Err(gvpn_client::Error::RegistrationNotFound) => {
                    tracing::warn!(wg_public_key = %input.public_key(), "old wg key not found during unregister, possibly already removed");
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

#[cfg(test)]
mod tests {
    use super::*;

    fn nets(s: &[&str]) -> Vec<IpNetwork> {
        s.iter().map(|n| n.parse().expect("valid cidr")).collect()
    }

    #[test]
    fn allowed_ips_default_to_accept_all_when_unset() {
        assert_eq!(parse_allowed_ips(None), nets(&["0.0.0.0/0"]));
    }

    #[test]
    fn allowed_ips_parse_a_comma_separated_list() {
        assert_eq!(
            parse_allowed_ips(Some("10.128.0.0/9,192.168.0.1/32")),
            nets(&["10.128.0.0/9", "192.168.0.1/32"])
        );
    }

    #[test]
    fn allowed_ips_tolerate_whitespace_and_empty_entries() {
        assert_eq!(
            parse_allowed_ips(Some(" 10.128.0.0/9 , ,192.168.0.1/32 ")),
            nets(&["10.128.0.0/9", "192.168.0.1/32"])
        );
    }

    #[test]
    fn allowed_ips_skip_unparseable_entries() {
        assert_eq!(
            parse_allowed_ips(Some("10.128.0.0/9,not-a-cidr")),
            nets(&["10.128.0.0/9"])
        );
    }

    #[test]
    fn allowed_ips_fall_back_to_default_when_nothing_parses() {
        assert_eq!(parse_allowed_ips(Some("not-a-cidr,also/bad")), nets(&["0.0.0.0/0"]));
        assert_eq!(parse_allowed_ips(Some("")), nets(&["0.0.0.0/0"]));
    }
}
