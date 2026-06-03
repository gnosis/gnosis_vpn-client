use bytesize::ByteSize;
use edgli::{BlockchainConnectorConfig, EdgeNodeApi, EdgliInitState};
use edgli::{
    Edgli,
    hopr_lib::{
        HopRouting, HoprSessionClientConfig, HoprSessionClientExplicitPathConfig,
        api::{
            chain::ChainKeyOperations,
            node::{HasChainApi, HasTransportApi},
            types::{internal::channels::ChannelStatus, primitive::prelude::Address},
        },
        exports::{
            network::types::types::IpProtocol,
            transport::{SESSION_MTU, SURB_SIZE, SessionId, SessionTarget, SurbBalancerConfig},
        },
    },
};
use futures_util::future::AbortHandle;
use hopr_utils_session::{
    ExplicitPathSessionFactory, HopSessionFactory, ListenerId, ListenerJoinHandles, SessionTargetSpec,
    create_tcp_client_binding, create_udp_client_binding,
};
use multiaddr::Protocol;
use tokio::task::JoinSet;
use tracing::instrument;

use std::collections::{BTreeSet, HashMap};
use std::{
    fmt::{self, Display},
    str::FromStr,
};
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

use crate::connection::destination::RoutingMode;
use crate::peer::Peer;
use crate::{
    balance::{self, Balances},
    hopr::{HoprError, types::SessionClientMetadata},
    info::Info,
};

pub struct Hopr {
    edgli: Arc<edgli::Edgli>,
    open_listeners: Arc<ListenerJoinHandles>,
}

impl Hopr {
    #[instrument(skip_all, level = "debug", err)]
    pub async fn new(
        cfg: edgli::hopr_lib::config::HoprLibConfig,
        keys: edgli::hopr_lib::HoprKeys,
        blokli_url: Option<url::Url>,
        blokli_config: BlockchainConnectorConfig,
        init_visitor: impl Fn(EdgliInitState) + Send + 'static,
    ) -> Result<Self, HoprError> {
        tracing::debug!("running hopr edge node");
        let edge_node = Edgli::new(
            cfg,
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

    // --- session management ---

    /// Open a local port and return the configuration
    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    async fn open_session(
        &self,
        destination: Address,
        target: SessionTarget,
        session_pool: Option<usize>,
        max_client_sessions: Option<usize>,
        cfg: HoprSessionClientConfig,
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
                HopSessionFactory::new(self.edgli.as_hopr()),
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
                HopSessionFactory::new(self.edgli.as_hopr()),
                open_listeners.clone(),
                destination,
                session_target_spec.clone(),
                cfg.clone(),
            )
            .await
            .map_err(|e| HoprError::Construction(format!("failed to create UDP client binding: {e}")))?,
        };

        let max_surb_upstream = cfg.surb_management.as_ref().map(|v| {
            human_bandwidth::parse_bandwidth(format!("{} bps", v.max_surbs_per_sec * SURB_SIZE as u64 * 8).as_ref())
                .expect("config value extract that cannot fail")
        });

        let response_buffer: Option<bytesize::ByteSize> = cfg
            .surb_management
            .as_ref()
            .map(|v| ByteSize::b(v.target_surb_buffer_size * SESSION_MTU as u64));

        Ok(SessionClientMetadata {
            protocol,
            bound_host,
            target: session_target_spec.to_string(),
            destination,
            forward_path: cfg.forward_path.into(),
            return_path: cfg.return_path.into(),
            hopr_mtu: SESSION_MTU,
            surb_len: SURB_SIZE,
            active_clients: udp_session_id.into_iter().map(|s| s.to_string()).collect(),
            max_client_sessions: max_clients,
            max_surb_upstream,
            response_buffer,
            session_pool,
        })
    }

    /// Open a local port using an explicit ordered list of intermediate relayer nodes.
    ///
    /// Note: the underlying hopr-lib explicit-path API is deprecated and may be removed
    /// in a future hoprnet release. The same relay nodes are used for both forward and
    /// return paths.
    #[allow(deprecated)]
    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    async fn open_session_explicit_path(
        &self,
        destination: Address,
        target: SessionTarget,
        intermediates: Vec<Address>,
        session_pool: Option<usize>,
        max_client_sessions: Option<usize>,
        base_cfg: HoprSessionClientConfig,
    ) -> Result<SessionClientMetadata, HoprError> {
        tracing::debug!("open hopr session with explicit intermediate path");
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

        // Resolve each intermediate chain address to a packet key (NodeId).
        let node_ids: Vec<_> = intermediates
            .iter()
            .map(|addr| -> Result<_, HoprError> {
                let key = self
                    .edgli
                    .chain_api()
                    .chain_key_to_packet_key(addr)
                    .map_err(|e| HoprError::Construction(e.to_string()))?
                    .ok_or_else(|| HoprError::Construction(format!("intermediate path node not found: {addr}")))?;
                Ok(key.into())
            })
            .collect::<Result<_, _>>()?;

        let explicit_cfg = HoprSessionClientExplicitPathConfig {
            forward_path: node_ids.clone(),
            return_path: node_ids,
            capabilities: base_cfg.capabilities,
            surb_management: base_cfg.surb_management,
            pseudonym: base_cfg.pseudonym,
            always_max_out_surbs: base_cfg.always_max_out_surbs,
            ..Default::default()
        };

        let listener_id = ListenerId(protocol, bind_host);
        let open_listeners = self.open_listeners.clone();
        if bind_host.port() > 0 && open_listeners.as_ref().0.contains_key(&listener_id) {
            return Err(HoprError::Construction("listener already exists".into()));
        }

        let port_range = std::env::var("GNOSISVPN_CLIENT_SESSION_PORT_RANGE").ok();

        let max_surb_upstream = explicit_cfg.surb_management.as_ref().map(|v| {
            human_bandwidth::parse_bandwidth(format!("{} bps", v.max_surbs_per_sec * SURB_SIZE as u64 * 8).as_ref())
                .expect("config value extract that cannot fail")
        });
        let response_buffer = explicit_cfg
            .surb_management
            .as_ref()
            .map(|v| ByteSize::b(v.target_surb_buffer_size * SESSION_MTU as u64));

        let (bound_host, udp_session_id, max_clients) = match protocol {
            IpProtocol::TCP => create_tcp_client_binding(
                bind_host,
                port_range,
                ExplicitPathSessionFactory::new(self.edgli.as_hopr()),
                open_listeners.clone(),
                destination,
                session_target_spec.clone(),
                explicit_cfg,
                session_pool,
                max_client_sessions,
            )
            .await
            .map_err(|e| HoprError::Construction(format!("failed to create TCP client binding: {e}")))?,
            IpProtocol::UDP => create_udp_client_binding(
                bind_host,
                port_range,
                ExplicitPathSessionFactory::new(self.edgli.as_hopr()),
                open_listeners.clone(),
                destination,
                session_target_spec.clone(),
                explicit_cfg,
            )
            .await
            .map_err(|e| HoprError::Construction(format!("failed to create UDP client binding: {e}")))?,
        };

        // Use hop count as an approximation for display in the returned metadata.
        // The accurate routing is stored in the listener entry and reflected by list_sessions.
        let hop_routing = HopRouting::try_from(intermediates.len()).unwrap_or_default();

        Ok(SessionClientMetadata {
            protocol,
            bound_host,
            target: session_target_spec.to_string(),
            destination,
            forward_path: hop_routing.into(),
            return_path: hop_routing.into(),
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
    pub async fn open_session_with_routing(
        &self,
        destination: Address,
        target: SessionTarget,
        routing: &RoutingMode,
        session_pool: Option<usize>,
        max_client_sessions: Option<usize>,
        base_cfg: HoprSessionClientConfig,
    ) -> Result<SessionClientMetadata, HoprError> {
        match routing {
            RoutingMode::HopBased(hop_routing) => {
                let cfg = HoprSessionClientConfig {
                    forward_path: *hop_routing,
                    return_path: *hop_routing,
                    ..base_cfg
                };
                self.open_session(destination, target, session_pool, max_client_sessions, cfg)
                    .await
            }
            RoutingMode::ExplicitPath(nodes) => {
                self.open_session_explicit_path(
                    destination,
                    target,
                    nodes.clone(),
                    session_pool,
                    max_client_sessions,
                    base_cfg,
                )
                .await
            }
        }
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
    pub async fn list_sessions(&self, protocol: IpProtocol) -> Vec<SessionClientMetadata> {
        tracing::debug!("list hopr sessions");
        self.open_listeners
            .as_ref()
            .0
            .iter()
            .filter(|content| content.key().0 == protocol)
            .map(|content| {
                let key = content.key();
                let entry = content.value();
                SessionClientMetadata {
                    protocol,
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

        // NOTE: the live SURB balancer is updated via the configurator below, but the
        // cached `max_surb_upstream` and `response_buffer` snapshots stored in
        // `open_listeners` (computed once at session creation) are not refreshed —
        // so list_sessions continues to report the originally-configured values for
        // adjusted sessions.
        self.open_listeners
            .find_configurator(&session_id)
            .ok_or(HoprError::SessionNotFound)?
            .update_surb_balancer_config(balancer_cfg)
            .await
            .map_err(|e| HoprError::SessionNotAdjusted(e.to_string()))
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub fn info(&self) -> Info {
        tracing::debug!("query hopr info");
        Info {
            node_address: self.edgli.me_onchain(),
            safe_address: self.edgli.safe_address(),
        }
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn balances(&self) -> Result<Balances, HoprError> {
        tracing::debug!("query hopr balances");
        let node_balances = self.edgli.balances().await.map_err(HoprError::HoprLib)?;
        let channels_out = self
            .edgli
            .my_outgoing_channels()
            .await
            .map_err(HoprError::HoprLib)?
            .into_iter()
            .filter_map(|ch| {
                if matches!(ch.status, ChannelStatus::Open) || matches!(ch.status, ChannelStatus::PendingToClose(_)) {
                    Some((ch.destination, ch.balance))
                } else {
                    None
                }
            })
            .collect();
        Ok(Balances {
            node_xdai: node_balances.node_xdai,
            safe_wxhopr: node_balances.safe_wxhopr,
            channels_out,
        })
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub fn status(&self) -> edgli::hopr_lib::api::node::HoprState {
        tracing::debug!("query hopr status");
        self.edgli.status()
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn connected_peers(&self) -> Result<Vec<Address>, HoprError> {
        tracing::debug!("query hopr connected peers");
        self.edgli.connected_peer_addresses().await.map_err(HoprError::HoprLib)
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub async fn start_telemetry_reactor(
        &self,
        sizing: edgli::strategy::IncentiveConfiguration,
    ) -> Result<AbortHandle, HoprError> {
        let cfg = edgli::strategy::default_strategy_cfg(&self.edgli, &sizing)
            .await
            .map_err(|e| HoprError::TelemetryReactorStart(e.to_string()))?;
        self.edgli
            .run_reactor_from_cfg(cfg)
            .map_err(|e| HoprError::TelemetryReactorStart(e.to_string()))
    }

    #[tracing::instrument(skip(self), level = "debug", ret)]
    pub async fn announced_peers(&self) -> Result<HashMap<Address, Peer>, HoprError> {
        tracing::debug!("query hopr connected peers");
        let offchain_keys = self
            .edgli
            .transport()
            .network_connected_peers()
            .await
            .map_err(|e| HoprError::HoprLib(e.into()))?;
        let mut set: JoinSet<Option<Peer>> = JoinSet::new();
        for key in offchain_keys {
            let address = match self.edgli.chain_api().packet_key_to_chain_key(&key) {
                Ok(Some(address)) => address,
                Ok(None) => {
                    tracing::warn!(%key, "no chain address for offchain key");
                    continue;
                }
                Err(err) => {
                    tracing::error!(%key, ?err, "failed to get chain address for offchain key");
                    continue;
                }
            };
            let hopr = self.edgli.clone();
            set.spawn(async move {
                let observed = hopr.transport().network_observed_multiaddresses(&key).await;
                let ipv4_addrs: Vec<Ipv4Addr> = observed
                    .iter()
                    .flat_map(|addr| {
                        let mut addr = addr.clone();
                        let mut found = vec![];
                        while let Some(protocol) = addr.pop() {
                            if let Protocol::Ip4(ipv4) = protocol {
                                found.push(ipv4);
                            }
                        }
                        found
                    })
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                if ipv4_addrs.is_empty() {
                    None
                } else {
                    Some(Peer::new(address, ipv4_addrs))
                }
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

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn ideal_balance_recommendation(
        &self,
        cfg: &edgli::strategy::IncentiveConfiguration,
    ) -> Result<balance::BalanceRecommendation, HoprError> {
        let rec = self
            .edgli
            .ideal_balance_recommendation(cfg)
            .await
            .map_err(|e| HoprError::Strategy(e.to_string()))?;
        Ok(balance::BalanceRecommendation {
            wxhopr: rec.wxhopr,
            xdai: rec.xdai,
        })
    }

    #[tracing::instrument(skip(self), level = "debug", ret, err)]
    pub async fn capacity_allocations(
        &self,
    ) -> Result<HashMap<balance::CapacityAllocator, balance::Capacity>, HoprError> {
        let raw = self
            .edgli
            .describe_current_capacity_allocations()
            .await
            .map_err(|e| HoprError::Strategy(e.to_string()))?;
        Ok(raw.into_iter().map(|(k, v)| (k.into(), v.into())).collect())
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
