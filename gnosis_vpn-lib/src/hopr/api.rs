use bytesize::ByteSize;
use edgli::{BlockchainConnectorConfig, EdgliInitState};
use edgli::{
    Edgli,
    hopr_lib::{
        Address, HoprBalance, IpProtocol, SESSION_MTU, SURB_SIZE, SessionClientConfig, SessionId, SessionTarget,
        SurbBalancerConfig, errors::HoprLibError,
    },
};
use futures_util::future::AbortHandle;
use hopr_utils_session::{
    ListenerId, ListenerJoinHandles, SessionTargetSpec, create_tcp_client_binding, create_udp_client_binding,
};
use multiaddr::Protocol;
use regex::Regex;
use thiserror::Error;
use tokio::task::JoinSet;
use tracing::instrument;

use std::collections::HashMap;
use std::{
    fmt::{self, Display},
    str::FromStr,
};
use std::{net::SocketAddr, sync::Arc};

use crate::peer::Peer;
use crate::{
    balance::{self, Balances},
    hopr::{HoprError, types::SessionClientMetadata},
    info::Info,
    ticket_stats::TicketStats,
};

#[derive(Debug, Error)]
pub enum ChannelError {
    #[error("failed to fund channel: {0}")]
    Fund(HoprError),
    #[error("channel is pending to close")]
    PendingToClose,
    #[error("failed to open channel: {0}")]
    Open(HoprError),
    #[error("HOPR library error: {0}")]
    HoprLibError(#[from] HoprLibError),
}

pub struct Hopr {
    edgli: Arc<edgli::Edgli>,
    open_listeners: Arc<ListenerJoinHandles>,
}

impl Hopr {
    #[instrument(skip_all, level = "debug", err)]
    pub async fn new(
        cfg: edgli::hopr_lib::config::HoprLibConfig,
        db_data_dir: &std::path::Path,
        keys: edgli::hopr_lib::HoprKeys,
        blokli_url: Option<url::Url>,
        blokli_config: BlockchainConnectorConfig,
        init_visitor: impl Fn(EdgliInitState) + Send + 'static,
    ) -> Result<Self, HoprError> {
        tracing::debug!("running hopr edge node");
        let edge_node = Edgli::new(
            cfg,
            db_data_dir,
            keys,
            blokli_url.map(|u| u.to_string()),
            Some(blokli_config),
            init_visitor,
        )
        .await
        .map_err(|e| HoprError::Construction(e.to_string()))?;

        tracing::debug!("hopr edge node finished setup");
        Ok(Self {
            edgli: Arc::new(edge_node),
            open_listeners: Default::default(),
        })
    }

    // --- channel management ---
    /// Ensure a channel to the specified target is open and funded with the specified amount.
    ///
    /// This API assumes that hopr object imlements 2 strategies to avoid edge scenarios and race conditions:
    /// 1. ClosureFinalizer to make sure that every PendingToClose channel is eventually closed
    /// 2. AutoFunding making sure that once a channel is open, it will stay funded
    #[instrument(skip(self), level = "debug", ret, err)]
    pub async fn ensure_channel_open_and_funded(
        &self,
        target: Address,
        amount: edgli::hopr_lib::Balance<edgli::hopr_lib::WxHOPR>,
        threshold: edgli::hopr_lib::Balance<edgli::hopr_lib::WxHOPR>,
    ) -> Result<(), ChannelError> {
        tracing::debug!("ensure hopr channel funding");
        let channels_from_me = self.edgli.channels_from(&self.edgli.me_onchain()).await?;

        if let Some(channel) = channels_from_me.iter().find(|ch| ch.destination == target) {
            match channel.status {
                edgli::hopr_lib::ChannelStatus::Open => {
                    // mirror AutoFunding strategy behaviour on startup (fund if less than or equal to threshold)
                    if channel.balance.le(&threshold) {
                        tracing::debug!(destination = %target, %amount, channel = %channel.get_id(), "funding existing channel");
                        self.edgli
                            .fund_channel(channel.get_id(), amount)
                            .await
                            .map(|_| ())
                            .map_err(HoprError::HoprLib)
                            .map_err(ChannelError::Fund)
                    } else {
                        Ok(())
                    }
                }
                edgli::hopr_lib::ChannelStatus::PendingToClose(_) => {
                    tracing::debug!(destination = %target, %amount, channel = %channel.get_id(), "channel is pending to close, cannot fund or open a new one");
                    Err(ChannelError::PendingToClose)
                }
                edgli::hopr_lib::ChannelStatus::Closed => {
                    tracing::debug!(destination = %target, %amount, channel = %channel.get_id(), "channel is closed, opening a new one");
                    self.edgli
                        .open_channel(&target, amount)
                        .await
                        .map(|_| ())
                        .map_err(HoprError::HoprLib)
                        .map_err(ChannelError::Open)
                }
            }
        } else {
            tracing::debug!(destination = %target, %amount, "no existing channel found, opening a new one");
            self.edgli
                .open_channel(&target, amount)
                .await
                .map(|_| ())
                .map_err(HoprError::HoprLib)
                .map_err(ChannelError::Open)
        }
    }

    // --- session management ---

    /// Open a local port and return the configuration
    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn open_session(
        &self,
        destination: Address,
        target: SessionTarget,
        session_pool: Option<usize>,
        max_client_sessions: Option<usize>,
        cfg: SessionClientConfig,
    ) -> Result<SessionClientMetadata, HoprError> {
        tracing::debug!("open hopr session");
        let bind_host: std::net::SocketAddr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 0).into();

        let protocol = match target {
            SessionTarget::TcpStream(_) => IpProtocol::TCP,
            SessionTarget::UdpStream(_) => IpProtocol::UDP,
            SessionTarget::ExitNode(_) => {
                return Err(HoprError::Construction(
                    "cannot open session for exit node target".into(),
                ));
            }
        };

        let session_target_spec = match target {
            SessionTarget::TcpStream(addr) => match addr {
                edgli::hopr_lib::exports::transport::session::SealedHost::Plain(ip_or_host) => {
                    SessionTargetSpec::Plain(ip_or_host.to_string())
                }
                edgli::hopr_lib::exports::transport::session::SealedHost::Sealed(items) => {
                    SessionTargetSpec::Sealed(items.into())
                }
            },
            SessionTarget::UdpStream(addr) => match addr {
                edgli::hopr_lib::exports::transport::session::SealedHost::Plain(ip_or_host) => {
                    SessionTargetSpec::Plain(ip_or_host.to_string())
                }
                edgli::hopr_lib::exports::transport::session::SealedHost::Sealed(items) => {
                    SessionTargetSpec::Sealed(items.into())
                }
            },
            SessionTarget::ExitNode(_) => {
                return Err(HoprError::Construction(
                    "cannot open session for exit node target".into(),
                ));
            }
        };

        let listener_id = ListenerId(protocol, bind_host);

        let open_listeners = self.open_listeners.clone();
        if bind_host.port() > 0 && open_listeners.as_ref().0.contains_key(&listener_id) {
            return Err(HoprError::Construction("listener already exists".into()));
        }

        let port_range = std::env::var("GNOSISVPN_CLIENT_SESSION_PORT_RANGE").ok();
        tracing::debug!(
            "binding {protocol} session listening socket to {bind_host} (port range limitations: {port_range:?})"
        );

        let (bound_host, udp_session_id, max_clients) = match protocol {
            IpProtocol::TCP => create_tcp_client_binding(
                bind_host,
                port_range,
                self.edgli.as_hopr(),
                open_listeners.clone(),
                destination,
                session_target_spec.clone(),
                cfg.clone(),
                session_pool,
                max_client_sessions,
            )
            .await
            .map_err(|e| HoprError::Construction(format!("failed to create TCP client binding: {e}")))?,
            IpProtocol::UDP => create_udp_client_binding(
                bind_host,
                port_range,
                self.edgli.as_hopr(),
                open_listeners.clone(),
                destination,
                session_target_spec.clone(),
                cfg.clone(),
            )
            .await
            .map_err(|e| HoprError::Construction(format!("failed to create UDP client binding: {e}")))?,
        };

        let max_surb_upstream = cfg.surb_management.map(|v| {
            human_bandwidth::parse_bandwidth(format!("{} bps", v.max_surbs_per_sec * SURB_SIZE as u64 * 8).as_ref())
                .expect("config value extract that cannot fail")
        });

        let response_buffer: Option<bytesize::ByteSize> = cfg
            .surb_management
            .map(|v| ByteSize::b(v.target_surb_buffer_size * SESSION_MTU as u64));

        Ok(SessionClientMetadata {
            protocol,
            bound_host,
            target: session_target_spec.to_string(),
            destination,
            forward_path: cfg.forward_path_options,
            return_path: cfg.return_path_options,
            hopr_mtu: SESSION_MTU,
            surb_len: SURB_SIZE,
            active_clients: udp_session_id.into_iter().map(|s| s.to_string()).collect(),
            max_client_sessions: max_clients,
            max_surb_upstream,
            response_buffer,
            session_pool,
        })
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn close_session(
        &self,
        bound_session: SocketAddr,
        protocol: IpProtocol,
    ) -> std::result::Result<(), HoprError> {
        tracing::debug!("close hopr session");
        let unspecified: std::net::SocketAddr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 0).into();

        // Find all listeners with protocol, listening IP and optionally port number (if > 0)
        let to_remove = self
            .open_listeners
            .as_ref()
            .0
            .iter()
            .filter_map(|record| {
                let ListenerId(proto, addr) = record.key();
                if protocol == *proto && (addr == &bound_session || addr == &unspecified) {
                    Some(ListenerId(*proto, *addr))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if to_remove.is_empty() {
            return Err(HoprError::SessionNotFound);
        }

        for bound_addr in to_remove {
            let entry = self
                .open_listeners
                .as_ref()
                .0
                .remove(&bound_addr)
                .ok_or(HoprError::SessionNotFound)?;

            entry.1.abort_handle.abort();
        }

        Ok(())
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub async fn list_sessions(&self) -> Vec<SessionClientMetadata> {
        tracing::debug!("list hopr sessions");
        self.open_listeners
            .as_ref()
            .0
            .iter()
            .map(|content| {
                let key = content.key();
                let entry = content.value();
                SessionClientMetadata {
                    protocol: key.0,
                    bound_host: key.1,
                    target: entry.target.to_string(),
                    forward_path: entry.forward_path.clone(),
                    return_path: entry.return_path.clone(),
                    destination: entry.destination,
                    hopr_mtu: SESSION_MTU,
                    surb_len: SURB_SIZE,
                    active_clients: entry.get_clients().iter().map(|e| e.key().to_string()).collect(),
                    max_client_sessions: entry.max_client_sessions,
                    max_surb_upstream: entry.max_surb_upstream,
                    response_buffer: entry.response_buffer,
                    session_pool: entry.session_pool,
                }
            })
            .collect::<Vec<_>>()
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub async fn adjust_session(&self, balancer_cfg: SurbBalancerConfig, client: String) -> Result<(), HoprError> {
        tracing::debug!("adjust hopr session");
        let session_id = SessionId::from_str(&client).map_err(|e| HoprError::SessionNotAdjusted(e.to_string()))?;

        // NOTE: known bug: adjust session does not update self.open_listeners which leads to
        // outdated info being reported by list_sessions
        self.edgli
            .update_session_surb_balancer_config(&session_id, balancer_cfg)
            .await
            .map_err(|e| HoprError::SessionNotAdjusted(e.to_string()))
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub fn info(&self) -> Info {
        tracing::debug!("query hopr info");
        Info {
            node_address: self.edgli.me_onchain(),
            node_peer_id: self.edgli.me_peer_id().to_string(),
            safe_address: self.edgli.get_safe_config().safe_address,
        }
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn balances(&self) -> Result<Balances, HoprError> {
        tracing::debug!("query hopr balances");
        Ok(Balances {
            node_xdai: self.edgli.get_balance().await?,
            safe_wxhopr: self.edgli.get_safe_balance().await?,
            channels_out_wxhopr: self
                .edgli
                .channels_from(&self.edgli.me_onchain())
                .await?
                .into_iter()
                .filter_map(|ch| {
                    if matches!(ch.status, edgli::hopr_lib::ChannelStatus::Open)
                        || matches!(ch.status, edgli::hopr_lib::ChannelStatus::PendingToClose(_))
                    {
                        Some(ch.balance)
                    } else {
                        None
                    }
                })
                .reduce(|acc, x| acc + x)
                .unwrap_or(edgli::hopr_lib::Balance::<edgli::hopr_lib::WxHOPR>::zero()),
        })
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub fn get_telemetry(&self) -> Result<HoprTelemetry, HoprError> {
        tracing::debug!("query hopr telemetry");
        // Regex to match: hopr_indexer_sync_progress followed by optional labels and a float value
        // Handles cases like:
        // hopr_indexer_sync_progress 0.85
        // hopr_indexer_sync_progress{label="value"} 0.85
        // hopr_indexer_sync_progress{label1="value1",label2="value2"} 0.85
        let re = Regex::new(r"hopr_indexer_sync_progress(?:\{[^}]*\})?\s+([0-9]*\.?[0-9]+(?:[eE][-+]?[0-9]+)?)")
            .expect("the sync extraction regex is constructible");

        edgli::hopr_lib::Hopr::<bool, bool>::collect_hopr_metrics()
            .map(move |prometheus_values| {
                tracing::debug!("prometheus metrics: {}", prometheus_values);
                let sync_percentage = re
                    .captures(prometheus_values.as_ref())
                    .and_then(|caps| caps.get(1))
                    .and_then(|m| m.as_str().parse::<f32>().ok())
                    .unwrap_or_default();

                HoprTelemetry { sync_percentage }
            })
            .map_err(|e| HoprError::Telemetry(e.to_string()))
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn get_ticket_stats(&self) -> Result<TicketStats, HoprError> {
        tracing::debug!("query hopr ticket price");
        let ticket_price = self.edgli.get_ticket_price().await?;
        let winning_probability = self.edgli.get_minimum_incoming_ticket_win_probability().await?;
        Ok(TicketStats::new(ticket_price, winning_probability.into()))
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub fn status(&self) -> edgli::hopr_lib::state::HoprState {
        tracing::debug!("query hopr status");
        self.edgli.status()
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn connected_peers(&self) -> Result<Vec<Address>, HoprError> {
        tracing::debug!("query hopr connected peers");
        let peer_ids = self.edgli.network_connected_peers().await?;
        let mut set = JoinSet::new();
        for p in peer_ids {
            let hopr = self.edgli.clone();
            set.spawn(async move { hopr.peerid_to_chain_key(&p).await });
        }

        let mut addresses = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Ok(Some(address))) = res {
                addresses.push(address);
            }
        }
        Ok(addresses)
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub fn start_telemetry_reactor(&self, ticket_value: HoprBalance) -> Result<AbortHandle, HoprError> {
        let cfg = edgli::strategy::default_edge_client_telemetry_reactor_cfg(
            balance::min_stake_threshold(ticket_value),
            balance::funding_amount(ticket_value),
        );
        self.edgli
            .run_reactor_from_cfg(cfg)
            .map_err(|e| HoprError::TelemetryReactorStart(e.to_string()))
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub async fn announced_peers(&self, minimum_score: f64) -> Result<HashMap<Address, Peer>, HoprError> {
        tracing::debug!("query hopr connected peers");
        let peer_ids = self.edgli.network_connected_peers().await?;
        let mut set = JoinSet::new();
        for peer_id in peer_ids {
            let hopr = self.edgli.clone();
            set.spawn(async move {
                let address = match hopr.peerid_to_chain_key(&peer_id).await {
                    Ok(Some(address)) => address,
                    Ok(None) => {
                        tracing::warn!(%peer_id, "no address for peer id");
                        return None;
                    }
                    Err(err) => {
                        tracing::error!(%peer_id, ?err, "failed to get address for peer id");
                        return None;
                    }
                };
                let observed = hopr.network_observed_multiaddresses(&peer_id).await;
                for addr in observed.clone().iter_mut() {
                    while let Some(protocol) = addr.pop() {
                        if let Protocol::Ip4(ipv4) = protocol {
                            return Some(Peer::new(address, ipv4));
                        }
                    }
                }
                None
            });
        }

        let mut peers = HashMap::new();
        while let Some(res) = set.join_next().await {
            if let Ok(Some(peer)) = res {
                peers.insert(peer.address, peer);
            }
        }
        Ok(peers)
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub async fn shutdown(&self) {
        tracing::debug!("shutdown hopr session listeners");

        for process in self.open_listeners.as_ref().0.iter() {
            tracing::info!("shutting down session listener: {:?}", process.key());
            process.value().abort_handle.abort();
        }
    }
}

#[derive(Debug, Clone)]
pub struct HoprTelemetry {
    pub sync_percentage: f32,
}

impl Display for HoprTelemetry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "HoprTelemetry(sync_percentage: {:.2}%)",
            self.sync_percentage * 100.0
        )
    }
}
