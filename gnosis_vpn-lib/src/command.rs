//! What root forwards to the worker, and how worker state becomes protocol responses.
use edgli::EdgliInitState;
use edgli::hopr_lib::api::node::HoprState;
use edgli::hopr_lib::api::types::primitive::prelude::{Address, Balance, WxHOPR};
use gnosis_vpn_ipc as ipc;
use serde::{Deserialize, Serialize};

use std::collections::HashMap;
use std::fmt::{self, Display};

use crate::balance;
use crate::connection;
use crate::connection::destination::{Destination, Destinations};
use crate::hopr::types::SessionClientMetadata;
use crate::info::Info;
use crate::wire;

/// The requests root cannot answer itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerCommand {
    Status,
    NerdStats,
    Connect { destination: ipc::DestinationId },
    Disconnect,
    Balance,
    FundingTool { secret: ipc::FundingSecret },
    Telemetry,
    Destinations,
    Probe { destination: ipc::DestinationId },
    Unprobe,
    QuickProbe { destination: ipc::DestinationId },
}

/// Debug, not JSON: the funding secret must never reach a log line.
impl Display for WorkerCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

/// None for the requests root answers itself.
pub fn worker_command(request: &ipc::Request) -> Option<WorkerCommand> {
    let cmd = match request {
        ipc::Request::Status => WorkerCommand::Status,
        ipc::Request::NerdStats => WorkerCommand::NerdStats,
        ipc::Request::Connect { destination } => WorkerCommand::Connect {
            destination: destination.clone(),
        },
        ipc::Request::Disconnect => WorkerCommand::Disconnect,
        ipc::Request::Balance => WorkerCommand::Balance,
        ipc::Request::FundingTool { secret } => WorkerCommand::FundingTool { secret: secret.clone() },
        ipc::Request::Telemetry => WorkerCommand::Telemetry,
        ipc::Request::Destinations => WorkerCommand::Destinations,
        ipc::Request::Probe { destination } => WorkerCommand::Probe {
            destination: destination.clone(),
        },
        ipc::Request::Unprobe => WorkerCommand::Unprobe,
        ipc::Request::QuickProbe { destination } => WorkerCommand::QuickProbe {
            destination: destination.clone(),
        },
        ipc::Request::Ping | ipc::Request::Info | ipc::Request::StartClient { .. } | ipc::Request::StopClient => {
            return None;
        }
    };
    Some(cmd)
}

pub fn destination(dest: &Destination) -> Box<ipc::Destination> {
    Box::new(dest.into())
}

pub fn hopr_status(state: HoprState) -> ipc::HoprStatus {
    match state {
        HoprState::Uninitialized => ipc::HoprStatus::Uninitialized,
        HoprState::WaitingForFunds => ipc::HoprStatus::WaitingForFunds,
        HoprState::CheckingBalance => ipc::HoprStatus::CheckingBalance,
        HoprState::ValidatingNetworkConfig => ipc::HoprStatus::ValidatingNetworkConfig,
        HoprState::CheckingOnchainAddress => ipc::HoprStatus::CheckingOnchainAddress,
        HoprState::RegisteringSafe => ipc::HoprStatus::RegisteringSafe,
        HoprState::AnnouncingNode => ipc::HoprStatus::AnnouncingNode,
        HoprState::AwaitingKeyBinding => ipc::HoprStatus::AwaitingKeyBinding,
        HoprState::InitializingServices => ipc::HoprStatus::InitializingServices,
        HoprState::Running => ipc::HoprStatus::Running,
        HoprState::Terminated => ipc::HoprStatus::Terminated,
        HoprState::Degraded => ipc::HoprStatus::Degraded,
        HoprState::Failed => ipc::HoprStatus::Failed,
    }
}

/// The node's own state wins once it has one; before that, the edge client's start-up stage.
fn warmup_stage(edgli_init_state: Option<EdgliInitState>, hopr_state: Option<HoprState>) -> ipc::WarmupStage {
    if let Some(state) = hopr_state {
        return hopr_status(state).into();
    }
    match edgli_init_state {
        None => ipc::WarmupStage::Initializing,
        Some(EdgliInitState::ValidatingConfig) => ipc::WarmupStage::ValidatingConfig,
        Some(EdgliInitState::IdentifyingNode) => ipc::WarmupStage::IdentifyingNode,
        Some(EdgliInitState::ConnectingBlockchain) => ipc::WarmupStage::ConnectingBlockchain,
        Some(EdgliInitState::CreatingNode) => ipc::WarmupStage::CreatingNode,
        Some(EdgliInitState::StartingNode) => ipc::WarmupStage::StartingNode,
        Some(EdgliInitState::Ready) => ipc::WarmupStage::Ready,
    }
}

pub fn preparing_safe(
    node_address: Address,
    pre_safe: &Option<balance::PreSafe>,
    funding_tool: Option<String>,
    error: Option<String>,
    balance_recommendation: Option<balance::BalanceRecommendation>,
) -> ipc::RunMode {
    ipc::RunMode::PreparingSafe {
        node_address: wire::address(&node_address),
        node_xdai: wire::xdai(pre_safe.as_ref().map(|s| s.node_xdai).unwrap_or_default()),
        node_wxhopr: wire::wxhopr(pre_safe.as_ref().map(|s| s.node_wxhopr).unwrap_or_default()),
        funding_tool,
        error,
        balance_recommendation: balance_recommendation.as_ref().map(|r| Box::new(r.into())),
    }
}

pub fn deploying_safe(node_address: Address) -> ipc::RunMode {
    ipc::RunMode::DeployingSafe {
        node_address: wire::address(&node_address),
    }
}

pub fn warmup(
    edgli_init_state: Option<EdgliInitState>,
    hopr_state: Option<HoprState>,
    last_error: Option<String>,
) -> ipc::RunMode {
    ipc::RunMode::Warmup {
        stage: warmup_stage(edgli_init_state, hopr_state),
        last_error,
    }
}

pub fn running(
    hopr_state: Option<HoprState>,
    funding_status: Option<balance::FundingStatus>,
    channel_maintenance: ipc::ChannelMaintenance,
) -> ipc::RunMode {
    ipc::RunMode::Running {
        hopr_status: hopr_state.map(hopr_status),
        funding_status: funding_status.as_ref().map(Into::into),
        channel_maintenance,
    }
}

pub fn balances(
    info: &Info,
    balances: &balance::Balances,
    destinations: &Destinations,
    capacity_allocations: Option<&balance::CapacityAllocations>,
    ideal_balance: Option<balance::BalanceRecommendation>,
    funding_status: Option<balance::FundingStatus>,
) -> ipc::Balances {
    ipc::Balances {
        node: wire::xdai(balances.node_xdai),
        safe: wire::wxhopr(balances.safe_wxhopr),
        channels_out: channels_out(balances.channels_out.iter(), destinations),
        info: info.into(),
        capacity_allocations: capacity_allocations.map(Into::into),
        ideal_balance: ideal_balance.as_ref().map(Into::into),
        funding_status: funding_status.as_ref().map(Into::into),
    }
}

fn channels_out<'a>(
    channels: impl Iterator<Item = (&'a Address, &'a Balance<WxHOPR>)>,
    destinations: &Destinations,
) -> Vec<ipc::ChannelOut> {
    // Several destinations share one channel when they share an exit; the first id names it stably.
    let mut addr_to_id: HashMap<Address, &str> = HashMap::new();
    for dest in destinations.values() {
        let entry = addr_to_id.entry(dest.address).or_insert(&dest.connect_id);
        if dest.connect_id.as_str() < *entry {
            *entry = &dest.connect_id;
        }
    }
    channels
        .map(|(address, balance)| ipc::ChannelOut {
            address: wire::address(address),
            balance: Some(wire::wxhopr(*balance)),
            matched_exit: addr_to_id.get(address).map(|id| (*id).into()),
        })
        .collect()
}

/// Live SURB gauges for one session from hopr telemetry; none when the session reports none.
fn surb_stats(telemetry: &str, session_id: &str) -> Option<ipc::SurbStats> {
    use crate::hopr::metrics::{gauge_u64, gauge_value};
    let stats = ipc::SurbStats {
        buffer_estimate: gauge_u64(telemetry, "hopr_session_surb_buffer_estimate", session_id),
        target_buffer: gauge_u64(telemetry, "hopr_session_surb_target_buffer", session_id),
        rate_per_sec: gauge_value(telemetry, "hopr_session_surb_rate_per_sec", session_id),
        produced: gauge_u64(telemetry, "hopr_session_surb_produced_total", session_id),
        consumed: gauge_u64(telemetry, "hopr_session_surb_consumed_total", session_id),
    };
    let counters = [
        stats.buffer_estimate,
        stats.target_buffer,
        stats.produced,
        stats.consumed,
    ];
    let has_any = counters.iter().any(Option::is_some) || stats.rate_per_sec.is_some();
    has_any.then_some(stats)
}

fn active_session(kind: ipc::SessionKind, meta: &SessionClientMetadata) -> Option<ipc::ActiveSession> {
    let id = meta.active_clients.first()?.to_string();
    Some(ipc::ActiveSession {
        kind,
        id: id.into(),
        bound_host: meta.bound_host,
    })
}

/// `bridge` is the probe session the connection registered over, if it is still around.
pub fn conn_stats(
    conn: &connection::up::Up,
    node_address: Address,
    telemetry: Option<&str>,
    bridge: Option<&SessionClientMetadata>,
) -> ipc::ConnStats {
    use connection::up::SessionKind;
    let bridge_session = bridge.and_then(|meta| active_session(ipc::SessionKind::Bridge, meta));
    let main_session = conn.ping_session.as_ref().and_then(|(kind, meta)| {
        let kind = match kind {
            SessionKind::Ping => ipc::SessionKind::Ping,
            SessionKind::Main => ipc::SessionKind::Main,
        };
        active_session(kind, meta)
    });
    let telemetry = telemetry.unwrap_or_default();
    let bridge_surb = bridge_session
        .as_ref()
        .and_then(|s| surb_stats(telemetry, s.id.as_str()));
    let main_surb = main_session.as_ref().and_then(|s| surb_stats(telemetry, s.id.as_str()));
    let setpoint = |cfg: &edgli::hopr_lib::exports::transport::SurbBalancerConfig| ipc::SurbBalancerSetpoint {
        target_surb_buffer_size: cfg.target_surb_buffer_size,
        max_surbs_per_sec: cfg.max_surbs_per_sec,
    };
    let wg_key = |key: &str| {
        key.parse::<ipc::WgPublicKey>()
            .inspect_err(|err| tracing::warn!(%err, "WireGuard key is not a public key"))
            .ok()
    };
    ipc::ConnStats {
        node_address: wire::address(&node_address),
        destination: (&conn.destination).into(),
        wg_pubkey: conn.wireguard.as_ref().and_then(|wg| wg_key(&wg.key_pair.public_key)),
        wg_server_pubkey: conn
            .registration
            .as_ref()
            .and_then(|reg| wg_key(&reg.server_public_key())),
        wg_ip: conn.registration.as_ref().map(|reg| reg.ip()),
        bridge_session,
        main_session,
        wg_stats: (!conn.wg_stats.is_empty()).then(|| ipc::WgTunnelStats {
            current: conn.wg_stats.back().cloned(),
            history: conn.wg_stats.iter().cloned().collect(),
        }),
        bridge_surb,
        main_surb,
        surb_setpoint_applied: conn.surb_applied.as_ref().map(setpoint),
        surb_setpoint_target: conn.surb_target.as_ref().map(setpoint),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::destination::{DestinationSource, HopRouting, Meta};

    use std::time::Duration;

    fn address(byte: u8) -> Address {
        Address::from([byte; 20])
    }

    fn destination_at(connect_id: &str, byte: u8) -> Destination {
        Destination::new(
            connect_id.to_string(),
            address(byte),
            HopRouting::try_from(1).expect("conversion cannot fail"),
            Meta::default(),
            "172.30.0.1:8000".parse().unwrap(),
            "172.30.0.1:51820".parse().unwrap(),
            DestinationSource::Configured,
        )
    }

    /// Shell completion for `connect` lists these, so it must reach the merged map, not the config.
    #[test]
    fn listing_destinations_is_routed_to_the_worker() {
        assert_eq!(
            Some(WorkerCommand::Destinations),
            worker_command(&ipc::Request::Destinations)
        );
    }

    #[test]
    fn probe_commands_are_routed_to_the_worker() {
        let exit = ipc::DestinationId::from("exit");
        assert_eq!(
            Some(WorkerCommand::Probe {
                destination: exit.clone()
            }),
            worker_command(&ipc::Request::Probe { destination: exit })
        );
        assert_eq!(Some(WorkerCommand::Unprobe), worker_command(&ipc::Request::Unprobe));
    }

    #[test]
    fn requests_root_answers_itself_never_reach_the_worker() {
        for request in [
            ipc::Request::Info,
            ipc::Request::Ping,
            ipc::Request::StartClient {
                timeout: Duration::from_secs(1),
            },
            ipc::Request::StopClient,
        ] {
            assert_eq!(None, worker_command(&request));
        }
    }

    #[test]
    fn a_worker_command_never_prints_the_funding_secret() {
        let cmd = WorkerCommand::FundingTool {
            secret: ipc::FundingSecret::new("hunter2".to_string()),
        };
        assert!(!cmd.to_string().contains("hunter2"), "{cmd}");
    }

    #[test]
    fn warmup_prefers_the_node_state_over_the_edge_client_stage() {
        assert_eq!(warmup_stage(None, None), ipc::WarmupStage::Initializing);
        assert_eq!(
            warmup_stage(Some(EdgliInitState::CreatingNode), None),
            ipc::WarmupStage::CreatingNode
        );
        assert_eq!(
            warmup_stage(Some(EdgliInitState::Ready), Some(HoprState::WaitingForFunds)),
            ipc::WarmupStage::WaitingForFunds
        );
    }

    #[test]
    fn running_passes_through_the_hopr_status() {
        let mode = running(Some(HoprState::Running), None, ipc::ChannelMaintenance::Ok);
        assert!(matches!(
            mode,
            ipc::RunMode::Running {
                hopr_status: Some(ipc::HoprStatus::Running),
                ..
            }
        ));
    }

    #[test]
    fn a_channel_is_named_by_the_first_destination_owning_its_exit() {
        let destinations: Destinations = [destination_at("b-exit", 1), destination_at("a-exit", 1)]
            .into_iter()
            .collect();
        let stake = Balance::<WxHOPR>::zero();
        let channels = channels_out([(&address(1), &stake)].into_iter(), &destinations);
        assert_eq!(channels[0].matched_exit, Some(ipc::DestinationId::from("a-exit")));
        assert_eq!(channels[0].balance, Some(ipc::Balance::zero()));
    }

    #[test]
    fn a_channel_to_an_unknown_peer_names_no_exit() {
        let destinations: Destinations = [destination_at("a-exit", 1)].into_iter().collect();
        let stake = Balance::<WxHOPR>::zero();
        let channels = channels_out([(&address(9), &stake)].into_iter(), &destinations);
        assert_eq!(channels[0].matched_exit, None);
    }

    #[test]
    fn surb_stats_read_the_session_gauges() {
        let telemetry = concat!(
            "# TYPE hopr_session_surb_buffer_estimate gauge\n",
            "hopr_session_surb_buffer_estimate{session_id=\"aabbcc\"} 12345\n",
            "hopr_session_surb_target_buffer{session_id=\"aabbcc\"} 20000\n",
            "hopr_session_surb_rate_per_sec{session_id=\"aabbcc\"} 512.5\n",
            "hopr_session_surb_produced_total{session_id=\"aabbcc\"} 123456\n",
            "hopr_session_surb_consumed_total{session_id=\"aabbcc\"} 111111\n",
        );
        let stats = surb_stats(telemetry, "aabbcc").expect("gauges present");
        assert_eq!(stats.buffer_estimate, Some(12345));
        assert_eq!(stats.target_buffer, Some(20000));
        assert_eq!(stats.rate_per_sec, Some(512.5));
        assert_eq!(stats.produced, Some(123456));
        assert_eq!(stats.consumed, Some(111111));

        assert!(surb_stats(telemetry, "unknown").is_none());
        assert!(surb_stats("", "aabbcc").is_none());
    }
}
