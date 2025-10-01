use std::{collections::HashMap, net::SocketAddr, str::FromStr, sync::Arc};

use bytesize::ByteSize;
use edgli::{
    EdgliProcesses,
    hopr_lib::{
        Address, HoprSessionId, IpProtocol, SESSION_MTU, SURB_SIZE, SessionClientConfig, SessionTarget,
        SurbBalancerConfig,
        utils::session::{
            ListenerId, ListenerJoinHandles, SessionTargetSpec, create_tcp_client_binding, create_udp_client_binding,
        },
    },
    run_hopr_edge_node,
};

use crate::{
    balance::Balances,
    hopr::{HoprError, types::SessionClientMetadata},
    info::Info,
};

pub struct Hopr {
    hopr: Arc<edgli::hopr_lib::Hopr>,
    rt: tokio::runtime::Runtime,
    // processes: Vec<EdgliProcesses>,      // TODO: add processes once the app is async
    open_listeners: ListenerJoinHandles,
}

impl Hopr {
    pub fn new(
        cfg: edgli::hopr_lib::config::HoprLibConfig,
        keys: edgli::hopr_lib::HoprKeys,
        notifier: crossbeam_channel::Sender<
            std::result::Result<Vec<EdgliProcesses>, edgli::hopr_lib::errors::HoprLibError>,
        >,
    ) -> std::result::Result<Self, HoprError> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| HoprError::Construction(e.to_string()))?;

        let (hopr, processes) = rt
            .block_on(run_hopr_edge_node(cfg, keys))
            .map_err(|e| HoprError::Construction(e.to_string()))?;

        rt.spawn(async move {
            let result = processes.await;
            if let Err(e) = notifier.send(result) {
                panic!("failed to notify HOPR process startup: {e}")
            }
        });

        let open_listeners = Arc::new(async_lock::RwLock::new(HashMap::new()));

        Ok(Self {
            hopr,
            rt,
            //
            open_listeners,
        })
    }

    // --- session management ---

    /// Open a local port and return the configuration
    pub fn open_session(
        &self,
        destination: Address,
        target: SessionTarget,
        session_pool: Option<usize>,
        max_client_sessions: Option<usize>,
        cfg: SessionClientConfig,
    ) -> std::result::Result<SessionClientMetadata, HoprError> {
        let bind_host: std::net::SocketAddr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0).into();

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

        let max_surb_upstream = cfg.surb_management.map(|v| {
            human_bandwidth::parse_bandwidth(format!("{} bps", v.max_surbs_per_sec * SURB_SIZE as u64).as_ref())
                .expect("config value extract that cannot fail")
        });

        let response_buffer: Option<bytesize::ByteSize> = cfg
            .surb_management
            .map(|v| ByteSize::kib(v.target_surb_buffer_size * SESSION_MTU as u64));

        let listener_id = ListenerId(protocol, bind_host);

        let hopr = self.hopr.clone();
        let open_listeners = self.open_listeners.clone();
        self.rt.block_on(async move {
            if bind_host.port() > 0 && open_listeners.read_arc().await.contains_key(&listener_id) {
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
                    hopr.clone(),
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
                    hopr.clone(),
                    open_listeners.clone(),
                    destination,
                    session_target_spec.clone(),
                    cfg.clone(),
                )
                .await
                .map_err(|e| HoprError::Construction(format!("failed to create UDP client binding: {e}")))?,
            };

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
        })
    }

    pub fn close_session(&self, bound_session: SocketAddr, protocol: IpProtocol) -> std::result::Result<(), HoprError> {
        let unspecified: std::net::SocketAddr = std::net::SocketAddrV4::new(std::net::Ipv4Addr::UNSPECIFIED, 0).into();

        self.rt.block_on(async move {
            let mut open_listeners = self.open_listeners.write_arc().await;

            // Find all listeners with protocol, listening IP and optionally port number (if > 0)
            let to_remove = open_listeners
                .iter()
                .filter_map(|(ListenerId(proto, addr), _)| {
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
                let entry = open_listeners.remove(&bound_addr).ok_or(HoprError::SessionNotFound)?;

                entry.abort_handle.abort();
            }

            Ok(())
        })
    }

    pub fn list_sessions(&self, protocol: IpProtocol) -> Vec<SessionClientMetadata> {
        let open_listeners = self.open_listeners.clone();

        self.rt.block_on(async move {
            open_listeners
                .read_arc()
                .await
                .iter()
                .filter(|(id, _)| id.0 == protocol)
                .map(|(id, entry)| SessionClientMetadata {
                    protocol,
                    bound_host: id.1,
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
                })
                .collect::<Vec<_>>()
        })
    }

    pub fn adjust_session(
        &self,
        balancer_cfg: SurbBalancerConfig,
        client: String,
    ) -> std::result::Result<(), HoprError> {
        self.rt.block_on(async move {
            let session_id =
                HoprSessionId::from_str(&client).map_err(|e| HoprError::SessionNotAdjusted(e.to_string()))?;

            self.hopr
                .update_session_surb_balancer_config(&session_id, balancer_cfg)
                .await
                .map_err(|e| HoprError::SessionNotAdjusted(e.to_string()))
        })
    }

    pub fn as_hopr_ref(&self) -> &edgli::hopr_lib::Hopr {
        self.hopr.as_ref()
    }

    pub fn as_hopr(&self) -> Arc<edgli::hopr_lib::Hopr> {
        self.hopr.clone()
    }

    pub fn info(&self) -> Info {
        Info {
            node_address: self.hopr.me_onchain(),
            safe_address: self.hopr.get_safe_config().safe_address,
            network: self.hopr.network(),
        }
    }

    pub fn balances(&self) -> Result<Balances, HoprError> {
        let node = self.hopr.clone();
        self.rt.block_on(async move {
            Ok(Balances {
                node_xdai: node.get_balance().await?,
                safe_wxhopr: node.get_safe_balance().await?,
                channels_out_wxhopr: node
                    .channels_from(&node.me_onchain())
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
        })
    }
}

impl Drop for Hopr {
    fn drop(&mut self) {
        // for process in &mut self.processes {
        //     tracing::info!("shutting down HOPR process: {process}");
        //     match process {
        //         EdgliProcesses::HoprLib(_process, handle) => handle.abort(),
        //         EdgliProcesses::Hopr(handle) => handle.abort(),
        //     }
        // }

        let open_listeners = self.open_listeners.clone();

        self.rt.block_on(async {
            let open_listeners = open_listeners.write_arc().await;
            for process in open_listeners.iter() {
                tracing::info!("shutting down session listener: {:?}", process.0);
                process.1.abort_handle.abort();
            }
        })
    }
}
