use bytesize::ByteSize;
use exitcode::{self, ExitCode};
use gnosis_vpn_ipc::{self as ipc, CallError, Request, Response, ServiceError};

use std::process;
use std::time::Duration;

mod cli;

use cli::OutputFormat;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() {
    let args = cli::parse();
    let format = args.output.unwrap_or(OutputFormat::Plain);

    if let cli::Command::Completions { shell } = args.command {
        cli::generate_completions(shell);
        process::exit(exitcode::OK);
    }

    let request: Request = args.command.into();
    let resp = match ipc::call(&args.socket_path, request.clone()) {
        Ok(resp) => resp,
        Err(CallError::Service(error)) => {
            match format {
                OutputFormat::Json => json_print(&error),
                OutputFormat::Yaml => yaml_print(&error),
                OutputFormat::Plain => eprintln!("{}", service_error_text(&error)),
            };
            process::exit(service_error_exitcode(&error));
        }
        Err(e) => {
            eprintln!("Error processing {request:?}: {e}");
            process::exit(exitcode::UNAVAILABLE);
        }
    };

    match format {
        OutputFormat::Json => json_print(&resp),
        OutputFormat::Yaml => yaml_print(&resp),
        OutputFormat::Plain => pretty_print(&resp),
    };

    let exit = determine_exitcode(&resp);
    process::exit(exit);
}

fn json_print<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("Error serializing response to JSON: {e}"),
    }
}

fn yaml_print<T: serde::Serialize>(value: &T) {
    match serde_saphyr::to_string(value) {
        Ok(s) => print!("{s}"),
        Err(e) => eprintln!("Error serializing response to YAML: {e}"),
    }
}

fn service_error_text(error: &ServiceError) -> String {
    match error {
        ServiceError::WorkerOffline => {
            "Worker client is currently offline - use command `start-client` to start it".to_string()
        }
        ServiceError::WorkerRestarting => "Worker client is restarting - try again shortly".to_string(),
        ServiceError::ProtocolMismatch { .. } | ServiceError::InvalidRequest { .. } => {
            format!("Service refused the request: {error}")
        }
    }
}

fn service_error_exitcode(error: &ServiceError) -> ExitCode {
    match error {
        ServiceError::WorkerOffline => exitcode::UNAVAILABLE,
        ServiceError::WorkerRestarting => exitcode::TEMPFAIL,
        ServiceError::ProtocolMismatch { .. } => exitcode::PROTOCOL,
        ServiceError::InvalidRequest { .. } => exitcode::SOFTWARE,
    }
}

fn pretty_print(resp: &Response) {
    match resp {
        Response::Connect(ipc::ConnectResponse::AlreadyConnected { destination: dest }) => {
            println!("Already connected to {dest}");
        }
        Response::Connect(ipc::ConnectResponse::Connecting { destination: dest }) => {
            println!("Connecting to {dest}");
        }
        Response::Connect(ipc::ConnectResponse::WaitingToConnect {
            destination: dest,
            route_health,
        }) => {
            println!("Waiting to connect to {dest} once possible: {route_health}")
        }
        Response::Connect(ipc::ConnectResponse::UnableToConnect {
            destination: dest,
            route_health,
        }) => {
            eprintln!("Unable to connect to {dest}: {route_health}");
        }
        Response::Connect(ipc::ConnectResponse::DestinationNotFound) => {
            eprintln!("Destination not found");
        }
        Response::Connect(ipc::ConnectResponse::DestinationAmbiguous { candidates: ids }) => {
            eprintln!(
                "That exit is reachable by several paths - connect to one of: {}",
                ipc::join(ids)
            );
        }
        Response::Disconnect(ipc::DisconnectResponse::Disconnecting { destination: dest }) => {
            println!("Disconnecting from {dest}");
        }
        Response::Disconnect(ipc::DisconnectResponse::NotConnected) => {
            eprintln!("Currently not connected to any destination");
        }
        Response::Probe(ipc::ProbeResponse::Probing { destination: dest }) => {
            println!("Probing {dest}");
        }
        Response::Probe(ipc::ProbeResponse::Replaced {
            destination: dest,
            previous,
        }) => {
            println!("Probing {dest} (closed probe of {previous})");
        }
        Response::Probe(ipc::ProbeResponse::AlreadyProbing { destination: dest }) => {
            println!("Already probing {dest}");
        }
        Response::Probe(ipc::ProbeResponse::InUse { destination: dest }) => {
            eprintln!("Probe of {dest} is still in use by a connection - retry once connected");
        }
        Response::Probe(ipc::ProbeResponse::UnableToProbe {
            destination: dest,
            route_health,
        }) => {
            eprintln!("Unable to probe {dest}: {route_health}");
        }
        Response::Probe(ipc::ProbeResponse::NotReady) => {
            eprintln!("Edge client not running yet - try again later");
        }
        Response::Probe(ipc::ProbeResponse::DestinationNotFound) => {
            eprintln!("Destination not found");
        }
        Response::Probe(ipc::ProbeResponse::DestinationAmbiguous { candidates: ids }) => {
            eprintln!(
                "That exit is reachable by several paths - probe one of: {}",
                ipc::join(ids)
            );
        }
        Response::Unprobe(ipc::UnprobeResponse::Closing { destination: dest }) => {
            println!("Closing probe of {dest}");
        }
        Response::Unprobe(ipc::UnprobeResponse::InUse { destination: dest }) => {
            eprintln!("Probe of {dest} is still in use by a connection - retry once connected");
        }
        Response::Unprobe(ipc::UnprobeResponse::NotProbing) => {
            eprintln!("Currently not probing any destination");
        }
        Response::QuickProbe(
            res @ (ipc::QuickProbeResponse::Checking { .. } | ipc::QuickProbeResponse::AlreadyChecking { .. }),
        ) => {
            println!("{res}");
        }
        Response::QuickProbe(res) => {
            eprintln!("{res}");
        }
        Response::Telemetry(Some(metrics)) => {
            println!("{metrics}");
        }
        Response::Telemetry(None) => {
            println!("No telemetry information available.");
        }
        Response::Status(status) => {
            let ipc::Status {
                run_mode,
                destinations,
                target,
                connection,
                disconnecting,
                probe,
            } = status.as_ref();
            let mut str_resp = format!("{run_mode}\n");
            if let Some(id) = target {
                let is_active = connection.destination() == Some(id);
                if !is_active {
                    str_resp.push_str(&format!("---\nWaiting to connect to {id}\n"));
                }
            }
            if *connection != ipc::Connection::Disconnected {
                str_resp.push_str(&format!("---\n{connection}\n"));
            }
            for info in disconnecting {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            if let Some(probe) = probe {
                str_resp.push_str(&format!("---\n{probe}\n"));
            }
            if !destinations.is_empty() {
                str_resp.push_str("---\nDestinations - [c] configured, [d] discovered, (c) value from config\n");
            }
            for dest_state in destinations {
                let dest = &dest_state.destination;
                str_resp.push_str(&format!("---\n{dest}\n"));
                if let Some(rh) = &dest_state.route_health {
                    str_resp.push_str(&format!("{} Route health: {}\n", dest.id, rh));
                    if let Some(walk) = &rh.walk {
                        str_resp.push_str(&format!("{} Route walk: {}\n", dest.id, walk));
                    }
                    if let Some(quick) = &rh.quick_probe {
                        str_resp.push_str(&format!("{} Quick check: {}\n", dest.id, quick));
                    }
                }
            }
            println!("{str_resp}");
        }
        Response::Balance(ipc::BalanceResponse::Available(balances)) => {
            let ipc::Balances {
                node,
                safe,
                channels_out,
                info,
                capacity_allocations,
                ideal_balance: _,
                funding_status,
            } = balances.as_ref();
            let mut str_resp = String::new();
            str_resp.push_str(&format!(
                "Node Address: {}\nSafe Address: {}\n",
                info.node_address, info.safe_address
            ));
            let safe_sci = ipc::scientific_suffix(safe);
            str_resp.push_str(&format!("---\nNode Balance: {node}\nSafe Balance: {safe}{safe_sci}\n"));
            let allocations = capacity_allocations.as_ref();
            if let Some(nc) = allocations.map(|a| a.node).filter(|c| !c.stake.is_zero()) {
                str_resp.push_str(&format!(
                    "Node wxHOPR (not yet in Safe): {}{}{}\n",
                    nc.stake,
                    ipc::scientific_suffix(&nc.stake),
                    format_capacity(Some(&nc))
                ));
            }
            if channels_out.is_empty() {
                str_resp.push_str("---\nNo outgoing channels.\n");
            } else {
                let safe_cap = allocations.map(|a| &a.safe);
                str_resp.push_str(&format!("Safe: {safe}{safe_sci}{}\n", format_capacity(safe_cap)));
                for ch in channels_out {
                    let ch_cap = allocations.and_then(|a| a.peer_allocations.get(&ch.address));
                    str_resp.push_str(&format!("{ch}{}\n", format_capacity(ch_cap)));
                }
            }
            match &funding_status {
                None => str_resp.push_str("---\nWaiting for funding calculations\n"),
                Some(status) => str_resp.push_str(&format!("---\n{}", format_funding_status(status))),
            }
            println!("{str_resp}");
        }
        Response::Balance(ipc::BalanceResponse::Unavailable { reason }) => {
            eprintln!("Balance error: {reason}");
        }
        Response::Pong => {
            println!("Pong");
        }
        Response::NerdStats(nerd_stats) => {
            print_nerd_stats(nerd_stats);
        }
        Response::FundingTool(ipc::FundingToolResponse::WrongPhase) => {
            eprintln!("Already past potential funding phase - no longer possible to fund");
        }
        Response::FundingTool(ipc::FundingToolResponse::Started) => {
            println!("Started funding");
        }
        Response::FundingTool(ipc::FundingToolResponse::InProgress) => {
            println!("Funding in progress");
        }
        Response::FundingTool(ipc::FundingToolResponse::Done) => {
            println!("Funding complete");
        }
        Response::FundingTool(ipc::FundingToolResponse::Cooldown { remaining }) => {
            println!(
                "Funding tool already ran successfully, still in cooldown - try again in {}",
                humantime::format_duration(Duration::from_secs(remaining.as_secs()))
            );
        }
        Response::Info(info) => {
            println!(
                "Gnosis VPN: client service version: {}, package version: {}{}",
                info.version,
                info.package_version.as_deref().unwrap_or("not available"),
                info.log_file
                    .as_ref()
                    .map(|f| format!("\nLog file: {}", f.display()))
                    .unwrap_or_default(),
            );
        }
        Response::StartClient(ipc::StartClientResponse::Started) => {
            println!("Worker client started");
        }
        Response::StartClient(ipc::StartClientResponse::AlreadyRunning) => {
            eprintln!("Worker client already running");
        }
        Response::StopClient(ipc::StopClientResponse::Stopped) => {
            println!("Worker client stopped");
        }
        Response::StopClient(ipc::StopClientResponse::NotRunning) => {
            eprintln!("Worker client not running");
        }
        Response::Destinations(ids) => {
            for id in ids {
                println!("{id}");
            }
        }
    }
}

fn format_probability(p: f64) -> String {
    let s = format!("{:.8}", p);
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

fn format_funding_status(status: &ipc::FundingStatus) -> String {
    // A deficit (not yet funded) takes precedence over the Safe's shortfall for the next top-up pass.
    let wxhopr_note = match (status.wxhopr_deficit, status.refill_shortfall) {
        (Some(deficit), _) => format!(" (top up {deficit} recommended)"),
        (None, Some(shortfall)) => format!(" (top up {shortfall})"),
        (None, None) => String::new(),
    };
    let xdai_deficit = status
        .xdai_deficit
        .map(|d| format!(" (top up {d} recommended)"))
        .unwrap_or_default();
    format!(
        "Traffic: {}{wxhopr_note}\nGas: {}{xdai_deficit}\n",
        status.traffic, status.gas
    )
}

fn format_capacity(capacity: Option<&ipc::Capacity>) -> String {
    match capacity {
        None => String::new(),
        Some(c) => format!(
            " [{} msgs, {}]",
            human_msgs(c.expected_messages),
            ByteSize(c.byte_capacity)
        ),
    }
}

fn human_msgs(msgs: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000 * K;
    const G: u64 = 1_000 * M;
    match msgs {
        m if m >= G => format!("{:.1}B", m as f64 / G as f64),
        m if m >= M => format!("{:.1}M", m as f64 / M as f64),
        m if m >= K => format!("{:.1}K", m as f64 / K as f64),
        m => format!("{m}"),
    }
}

fn determine_exitcode(resp: &Response) -> ExitCode {
    match resp {
        Response::Connect(ipc::ConnectResponse::AlreadyConnected { .. }) => exitcode::OK,
        Response::Connect(ipc::ConnectResponse::Connecting { .. }) => exitcode::OK,
        Response::Connect(ipc::ConnectResponse::DestinationNotFound) => exitcode::UNAVAILABLE,
        Response::Connect(ipc::ConnectResponse::DestinationAmbiguous { .. }) => exitcode::USAGE,
        Response::Connect(ipc::ConnectResponse::WaitingToConnect { .. }) => exitcode::OK,
        Response::Connect(ipc::ConnectResponse::UnableToConnect { .. }) => exitcode::UNAVAILABLE,
        Response::Disconnect(ipc::DisconnectResponse::Disconnecting { .. }) => exitcode::OK,
        Response::Disconnect(ipc::DisconnectResponse::NotConnected) => exitcode::PROTOCOL,
        Response::Probe(ipc::ProbeResponse::Probing { .. }) => exitcode::OK,
        Response::Probe(ipc::ProbeResponse::Replaced { .. }) => exitcode::OK,
        Response::Probe(ipc::ProbeResponse::AlreadyProbing { .. }) => exitcode::OK,
        Response::Probe(ipc::ProbeResponse::InUse { .. }) => exitcode::UNAVAILABLE,
        Response::Probe(ipc::ProbeResponse::UnableToProbe { .. }) => exitcode::UNAVAILABLE,
        Response::Probe(ipc::ProbeResponse::NotReady) => exitcode::UNAVAILABLE,
        Response::Probe(ipc::ProbeResponse::DestinationNotFound) => exitcode::UNAVAILABLE,
        Response::Probe(ipc::ProbeResponse::DestinationAmbiguous { .. }) => exitcode::USAGE,
        Response::QuickProbe(ipc::QuickProbeResponse::Checking { .. }) => exitcode::OK,
        Response::QuickProbe(ipc::QuickProbeResponse::AlreadyChecking { .. }) => exitcode::OK,
        Response::QuickProbe(ipc::QuickProbeResponse::DestinationAmbiguous { .. }) => exitcode::USAGE,
        Response::QuickProbe(..) => exitcode::UNAVAILABLE,
        Response::Unprobe(ipc::UnprobeResponse::Closing { .. }) => exitcode::OK,
        Response::Unprobe(ipc::UnprobeResponse::InUse { .. }) => exitcode::PROTOCOL,
        Response::Unprobe(ipc::UnprobeResponse::NotProbing) => exitcode::PROTOCOL,
        Response::Status(..) => exitcode::OK,
        Response::Balance(ipc::BalanceResponse::Available(..)) => exitcode::OK,
        Response::Balance(ipc::BalanceResponse::Unavailable { .. }) => exitcode::SOFTWARE,
        Response::Pong => exitcode::OK,
        Response::Telemetry(Some(_)) => exitcode::OK,
        Response::Telemetry(None) => exitcode::UNAVAILABLE,
        Response::NerdStats(stats) => match stats.as_ref() {
            ipc::NerdStats::NoInfo {
                tickets: ipc::TicketStatsStatus::Available(_),
            } => exitcode::OK,
            ipc::NerdStats::NoInfo {
                tickets: ipc::TicketStatsStatus::Waiting,
            } => exitcode::UNAVAILABLE,
            ipc::NerdStats::NoInfo {
                tickets: ipc::TicketStatsStatus::Error { .. },
            } => exitcode::SOFTWARE,
            ipc::NerdStats::Connecting { .. } => exitcode::OK,
            ipc::NerdStats::Connected { .. } => exitcode::OK,
        },
        Response::FundingTool(ipc::FundingToolResponse::WrongPhase) => exitcode::UNAVAILABLE,
        Response::FundingTool(ipc::FundingToolResponse::Started) => exitcode::OK,
        Response::FundingTool(ipc::FundingToolResponse::InProgress) => exitcode::OK,
        Response::FundingTool(ipc::FundingToolResponse::Done) => exitcode::OK,
        Response::FundingTool(ipc::FundingToolResponse::Cooldown { .. }) => exitcode::UNAVAILABLE,
        Response::Info(..) => exitcode::OK,
        Response::StartClient(ipc::StartClientResponse::Started) => exitcode::OK,
        Response::StartClient(ipc::StartClientResponse::AlreadyRunning) => exitcode::PROTOCOL,
        Response::StopClient(ipc::StopClientResponse::Stopped) => exitcode::OK,
        Response::StopClient(ipc::StopClientResponse::NotRunning) => exitcode::PROTOCOL,
        Response::Destinations(..) => exitcode::OK,
    }
}

fn print_ticket_stats_status(status: &ipc::TicketStatsStatus) {
    match status {
        ipc::TicketStatsStatus::Available(ts) => {
            let sci = ipc::scientific_suffix(&ts.ticket_price);
            println!(
                "Ticket Price: {}{}\nWinning Probability: {}",
                ts.ticket_price,
                sci,
                format_probability(ts.winning_probability)
            )
        }
        ipc::TicketStatsStatus::Waiting => {
            println!("waiting for incentive operations to become available")
        }
        ipc::TicketStatsStatus::Error { message } => eprintln!("Error fetching ticket stats: {message}"),
    }
}

fn print_nerd_stats(nerd_stats: &ipc::NerdStats) {
    match nerd_stats {
        ipc::NerdStats::NoInfo { tickets } => {
            print_ticket_stats_status(tickets);
            println!("(connect to a destination to see more stats)");
        }
        ipc::NerdStats::Connecting { tickets, conn } => {
            print_ticket_stats_status(tickets);
            println!("---");
            print_connecting_stats(conn);
        }
        ipc::NerdStats::Connected { tickets, conn } => {
            print_ticket_stats_status(tickets);
            println!("---");
            print_connected_stats(conn);
        }
    }
}

fn print_connecting_stats(stats: &ipc::ConnStats) {
    let mut str_resp = print_conn_stats_routing(stats, "-CONNECTING-");
    str_resp.push_str("---\n");
    str_resp.push_str(
        format!(
            "WireGuard Public Key: {}\n",
            stats
                .wg_pubkey
                .map(|k| k.to_string())
                .unwrap_or("--pending generation--".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Assigned WireGuard IP: {}\n",
            stats
                .wg_ip
                .map(|ip| format!("{ip}/32"))
                .unwrap_or("--pending registration--".to_string())
        )
        .as_str(),
    );
    if let Some(ref session) = stats.bridge_session {
        str_resp.push_str(&print_session(session));
    }
    str_resp.push_str(&print_session_or_pending(
        &stats.main_session,
        "--pending session creation--",
    ));
    str_resp.push_str(
        format!(
            "---\nExit WireGuard Public Key: {}\n",
            stats
                .wg_server_pubkey
                .map(|k| k.to_string())
                .unwrap_or("--pending registration--".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(&print_wg_tunnel_stats(stats));
    str_resp.push_str(&print_surb_stats(stats));
    println!("{str_resp}");
}

fn print_connected_stats(stats: &ipc::ConnStats) {
    let mut str_resp = print_conn_stats_routing(stats, "-o-");
    str_resp.push_str("---\n");
    if let Some(ref wg_pubkey) = stats.wg_pubkey {
        str_resp.push_str(format!("WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    if let Some(ref ip) = stats.wg_ip {
        str_resp.push_str(format!("Assigned WireGuard IP: {ip}/32\n").as_str());
    }
    if let Some(ref session) = stats.bridge_session {
        str_resp.push_str(&print_session(session));
    }
    str_resp.push_str(&print_session_or_pending(&stats.main_session, "--none--"));
    if let Some(ref wg_pubkey) = stats.wg_server_pubkey {
        str_resp.push_str(format!("---\nExit WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    str_resp.push_str(&print_wg_tunnel_stats(stats));
    str_resp.push_str(&print_surb_stats(stats));
    println!("{str_resp}");
}

fn format_handshake_age(d: Duration) -> String {
    // drop sub-second precision so humantime output skips ms/us
    let secs = Duration::from_secs(d.as_secs());
    // 3 decimals: NepTUN's tick granularity (pump.rs TIMER_PERIOD)
    format!("{:.3}s ({})", d.as_secs_f64(), humantime::format_duration(secs))
}

/// Render the last WireGuard tunnel stats sample, if the pump has started and
/// recorded one. `rtt`/handshake age are labeled "as of last handshake", not
/// "current" - WireGuard only updates them on a handshake, which recurs every
/// ~120s on an active tunnel, so they can be meaningfully stale.
fn print_wg_tunnel_stats(stats: &ipc::ConnStats) -> String {
    let Some(current) = stats.wg_stats.as_ref().and_then(|s| s.current.as_ref()) else {
        return String::new();
    };
    let rtt = current.rtt_ms.map(|ms| format!("{ms}ms")).unwrap_or("--".to_string());
    let handshake_age = current
        .time_since_last_handshake
        .map(format_handshake_age)
        .unwrap_or("--".to_string());
    format!(
        "---\nTunnel RTT (as of last handshake): {rtt}\nLast handshake: {handshake_age} ago\nTx bytes: {} ({})\nRx bytes: {} ({})\nEstimated loss: {:.1}%\n",
        current.tx_bytes,
        ByteSize(current.tx_bytes),
        current.rx_bytes,
        ByteSize(current.rx_bytes),
        current.estimated_loss * 100.0,
    )
}

/// Render SURB balancer insights, if any session has them; gauges can be stale on idle tunnels.
fn print_surb_stats(stats: &ipc::ConnStats) -> String {
    let mut lines = String::new();
    if let (Some(session), Some(surb)) = (&stats.bridge_session, &stats.bridge_surb) {
        lines.push_str(&print_session_surb(session_label(session), surb));
    }
    if let (Some(session), Some(surb)) = (&stats.main_session, &stats.main_surb) {
        lines.push_str(&print_session_surb(session_label(session), surb));
    }
    let has_setpoint = stats.surb_setpoint_applied.is_some() || stats.surb_setpoint_target.is_some();
    if has_setpoint {
        let fmt = |sp: Option<ipc::SurbBalancerSetpoint>| {
            sp.map(|s| format!("{} @ {}/s", s.target_surb_buffer_size, s.max_surbs_per_sec))
                .unwrap_or("--".to_string())
        };
        lines.push_str(&format!(
            "SURB setpoint: applied {} -> target {}\n",
            fmt(stats.surb_setpoint_applied),
            fmt(stats.surb_setpoint_target),
        ));
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("---\n{lines}")
}

fn print_session_surb(label: &str, surb: &ipc::SurbStats) -> String {
    let fmt = |v: Option<u64>| v.map(|v| v.to_string()).unwrap_or("--".to_string());
    let rate = surb
        .rate_per_sec
        .map(|r| format!("{r:.1}/s"))
        .unwrap_or("--".to_string());
    format!(
        "{label} SURB buffer (est. at exit): {} / target {}\n{label} SURB upstream rate: {rate}\n{label} SURBs produced: {}, consumed: {}\n",
        fmt(surb.buffer_estimate),
        fmt(surb.target_buffer),
        fmt(surb.produced),
        fmt(surb.consumed),
    )
}

fn session_label(session: &ipc::ActiveSession) -> &'static str {
    match session.kind {
        ipc::SessionKind::Bridge => "Bridge",
        ipc::SessionKind::Ping => "Ping",
        ipc::SessionKind::Main => "Main",
    }
}

fn print_session(session: &ipc::ActiveSession) -> String {
    let label = session_label(session);
    format!(
        "{label} Session entry: {}\n{label} Session ID: {}\n",
        session.bound_host, session.id
    )
}

fn print_session_or_pending(session: &Option<ipc::ActiveSession>, pending: &str) -> String {
    session
        .as_ref()
        .map(print_session)
        .unwrap_or_else(|| format!("Session entry: {pending}\nSession ID: {pending}\n"))
}

fn print_conn_stats_routing(stats: &ipc::ConnStats, title: &str) -> String {
    let route = match stats.destination.hops.count() {
        0 => "DIRECTLY".to_string(),
        1 => "VIA--1HOP".to_string(),
        nr => format!("VIA--{nr}HOPS"),
    };
    format!(
        "{node_addr}(me) -{title}-{route}--> {addr}({exit})\n",
        node_addr = stats.node_address,
        addr = stats.destination.address,
        exit = stats.destination.id,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipc::{Balance, FundingLevel, FundingStatus};

    fn good() -> FundingStatus {
        FundingStatus {
            traffic: FundingLevel::Good,
            gas: FundingLevel::Good,
            wxhopr_deficit: None,
            xdai_deficit: None,
            refill_shortfall: None,
        }
    }

    #[test]
    fn renders_refill_shortfall_when_no_deficit() {
        let status = FundingStatus {
            refill_shortfall: Some(Balance::from_wei(5)),
            ..good()
        };
        let out = format_funding_status(&status);
        assert!(out.contains("top up"), "shortfall must render, not blank: {out}");
        // a deficit would read "recommended"; a bare shortfall must not.
        assert!(!out.contains("recommended"), "{out}");
    }

    #[test]
    fn deficit_takes_precedence_over_refill_shortfall() {
        let status = FundingStatus {
            wxhopr_deficit: Some(Balance::from_wei(9)),
            refill_shortfall: Some(Balance::from_wei(5)),
            ..good()
        };
        assert!(format_funding_status(&status).contains("recommended"));
    }
}
