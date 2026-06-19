use bytesize::ByteSize;
use edgli::{BlockchainConnectorConfig, EdgeNodeApi, EdgliInitState};
use edgli::{
    Edgli,
    hopr_lib::{
        HoprSessionClientConfig,
        api::{
            chain::{AccountSelector, ChainReadAccountOperations},
            node::HasChainApi,
            types::{internal::channels::ChannelStatus, primitive::prelude::Address},
        },
        errors::HoprLibError,
        exports::{
            network::types::types::IpProtocol,
            transport::{SESSION_MTU, SURB_SIZE, SessionId, SessionTarget, SurbBalancerConfig},
        },
    },
};
use futures_util::{StreamExt, future::AbortHandle};
use hopr_utils_session::{
    HopSessionFactory, ListenerId, ListenerJoinHandles, SessionTargetSpec, create_tcp_client_binding,
    create_udp_client_binding,
};
use multiaddr::Protocol;
use tracing::instrument;

use std::collections::{BTreeSet, HashMap};
use std::str::FromStr;
use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
};

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
    pub async fn open_session(
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

        let open_listeners = self.open_listeners.clone();

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
            forward_path: cfg.forward_path,
            return_path: cfg.return_path,
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
                    forward_path: entry.forward_path,
                    return_path: entry.return_path,
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
            node_peer_id: self.edgli.me_peer_id().to_string(),
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
        tracing::debug!("query hopr announced peers");
        let selector = AccountSelector::default().with_public_only(true);
        let mut stream = self
            .edgli
            .chain_api()
            .stream_accounts(selector)
            .map_err(|e| HoprError::HoprLib(HoprLibError::GeneralError(e.to_string())))?;

        let mut peers = HashMap::new();
        while let Some(entry) = stream.next().await {
            let ipv4_addrs = extract_ipv4_addrs(entry.get_multiaddrs());
            if !ipv4_addrs.is_empty() {
                peers.insert(entry.chain_addr, Peer::new(entry.chain_addr, ipv4_addrs));
            }
        }
        tracing::debug!(
            peers = %peers.iter()
                .map(|(addr, p)| format!("{addr}:{:?}", p.ipv4_addrs))
                .collect::<Vec<_>>()
                .join(" "),
            "announced peers"
        );
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

/// Extract all unique IPv4 addresses from a list of multiaddrs.
///
/// Walks each multiaddr from right to left (via `pop`), collecting
/// `Protocol::Ip4` components. The result is deduplicated and sorted
/// via `BTreeSet`, then returned as a `Vec`.
fn extract_ipv4_addrs(multiaddrs: &[multiaddr::Multiaddr]) -> Vec<Ipv4Addr> {
    multiaddrs
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
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn multiaddr(s: &str) -> multiaddr::Multiaddr {
        multiaddr::Multiaddr::from_str(s).expect("valid multiaddr")
    }

    #[test]
    fn extract_ipv4_single_address() {
        let addrs = vec![multiaddr("/ip4/1.2.3.4/tcp/9091")];
        let result = extract_ipv4_addrs(&addrs);
        assert_eq!(result, vec!["1.2.3.4".parse::<Ipv4Addr>().unwrap()]);
    }

    #[test]
    fn extract_ipv4_deduplicates_same_ip_across_multiaddrs() {
        let addrs = vec![multiaddr("/ip4/1.2.3.4/tcp/9091"), multiaddr("/ip4/1.2.3.4/udp/9092")];
        let result = extract_ipv4_addrs(&addrs);
        assert_eq!(result, vec!["1.2.3.4".parse::<Ipv4Addr>().unwrap()]);
    }

    #[test]
    fn extract_ipv4_ignores_non_ipv4_protocols() {
        let addrs = vec![multiaddr("/dns4/example.com/tcp/9091")];
        let result = extract_ipv4_addrs(&addrs);
        assert!(result.is_empty());
    }

    #[test]
    fn extract_ipv4_empty_input_returns_empty() {
        assert!(extract_ipv4_addrs(&[]).is_empty());
    }
}
