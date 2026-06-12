use exitcode::{self, ExitCode};

use std::fmt;
use std::process;
use std::time::Duration;

use gnosis_vpn_lib::balance;
use gnosis_vpn_lib::check_update;
use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::connection::destination::{NodeId, RoutingOptions};
use gnosis_vpn_lib::socket;

mod cli;

use cli::OutputFormat;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() {
    let args = cli::parse();
    let format = args.output.unwrap_or(OutputFormat::Plain);

    if let cli::Command::CheckUpdate { force } = args.command {
        let exit = run_check_update(format, &args.socket_path, force).await;
        process::exit(exit);
    }

    let cmd: Command = args.command.into();
    let resp = match socket::root::process_cmd(&args.socket_path, &cmd).await {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("Error processing {cmd}: {e}");
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

async fn run_check_update(format: OutputFormat, socket_path: &std::path::Path, force: bool) -> ExitCode {
    let client = match reqwest::Client::builder().timeout(Duration::from_secs(30)).build() {
        Ok(c) => c,
        Err(e) => return emit_check_update_error(format, CheckUpdateErrorKind::Internal, &e.to_string()),
    };
    let gate = (!force).then_some(socket_path);
    match check_update::download(&client, gate).await {
        Ok(manifest) => {
            match format {
                OutputFormat::Json => match serde_json::to_string_pretty(&manifest) {
                    Ok(s) => println!("{s}"),
                    Err(e) => {
                        return emit_check_update_error(
                            OutputFormat::Json,
                            CheckUpdateErrorKind::Internal,
                            &e.to_string(),
                        );
                    }
                },
                OutputFormat::Yaml => match serde_saphyr::to_string(&manifest) {
                    Ok(s) => print!("{s}"),
                    Err(e) => {
                        return emit_check_update_error(
                            OutputFormat::Yaml,
                            CheckUpdateErrorKind::Internal,
                            &e.to_string(),
                        );
                    }
                },
                OutputFormat::Plain => {
                    if let Some(stable) = &manifest.channels.stable {
                        println!(
                            "Stable: {}, published at {}, download at: {}",
                            stable.version, stable.published_at, stable.download_url
                        );
                    }
                    if let Some(snapshot) = &manifest.channels.snapshot {
                        println!(
                            "Latest Snapshot: {}, published at {}, download at: {}",
                            snapshot.version, snapshot.published_at, snapshot.download_url
                        );
                    }
                }
            }
            exitcode::OK
        }
        Err(check_update::Error::VpnNotConnected) => emit_check_update_error(
            format,
            CheckUpdateErrorKind::VpnNotConnected,
            "pass -f/--force to bypass the VPN connection check",
        ),
        Err(e @ check_update::Error::Integrity(_)) => {
            emit_check_update_error(format, CheckUpdateErrorKind::IntegrityError, &e.to_string())
        }
        Err(e) => emit_check_update_error(format, CheckUpdateErrorKind::Unavailable, &e.to_string()),
    }
}

#[derive(Clone, Copy, Debug)]
enum CheckUpdateErrorKind {
    Unavailable,
    IntegrityError,
    Internal,
    VpnNotConnected,
}

impl CheckUpdateErrorKind {
    fn slug(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::IntegrityError => "integrity_error",
            Self::Internal => "internal",
            Self::VpnNotConnected => "vpn_not_connected",
        }
    }

    fn exit_code(self) -> ExitCode {
        match self {
            Self::Unavailable => exitcode::UNAVAILABLE,
            Self::VpnNotConnected => exitcode::NOPERM,
            Self::IntegrityError | Self::Internal => exitcode::SOFTWARE,
        }
    }
}

impl fmt::Display for CheckUpdateErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Unavailable => "Update manifest unavailable",
            Self::IntegrityError => "Update manifest integrity check failed",
            Self::Internal => "Internal error",
            Self::VpnNotConnected => "VPN not connected",
        };
        f.write_str(label)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CheckUpdateErrorPayload {
    r#type: String,
    error: String,
}

fn emit_check_update_error(format: OutputFormat, kind: CheckUpdateErrorKind, message: &str) -> ExitCode {
    let payload = CheckUpdateErrorPayload {
        r#type: kind.slug().to_string(),
        error: message.to_string(),
    };
    match format {
        OutputFormat::Json => {
            eprintln!("{}", serde_json::to_string_pretty(&payload).unwrap_or_default());
        }
        OutputFormat::Yaml => {
            eprintln!("{}", serde_saphyr::to_string(&payload).unwrap_or_default());
        }
        OutputFormat::Plain => {
            eprintln!("{kind}: {message}");
        }
    }
    kind.exit_code()
}

fn json_print(resp: &Response) {
    match serde_json::to_string_pretty(resp) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("Error serializing response to JSON: {e}"),
    }
}

fn yaml_print(resp: &Response) {
    match serde_saphyr::to_string(resp) {
        Ok(s) => print!("{s}"),
        Err(e) => eprintln!("Error serializing response to YAML: {e}"),
    }
}

fn pretty_print(resp: &Response) {
    match resp {
        Response::Connect(command::ConnectResponse::AlreadyConnected(dest)) => {
            println!("Already connected to {dest}");
        }
        Response::Connect(command::ConnectResponse::Connecting(dest)) => {
            println!("Connecting to {dest}");
        }
        Response::Connect(command::ConnectResponse::WaitingToConnect(dest, route_health)) => {
            println!("Waiting to connect to {dest} once possible: {route_health}")
        }
        Response::Connect(command::ConnectResponse::UnableToConnect(dest, route_health)) => {
            eprintln!("Unable to connect to {dest}: {route_health}");
        }
        Response::Connect(command::ConnectResponse::DestinationNotFound) => {
            eprintln!("Destination not found");
        }
        Response::Disconnect(command::DisconnectResponse::Disconnecting(dest)) => {
            println!("Disconnecting from {dest}");
        }
        Response::Disconnect(command::DisconnectResponse::NotConnected) => {
            eprintln!("Currently not connected to any destination");
        }
        Response::Telemetry(Some(metrics)) => {
            println!("{metrics}");
        }
        Response::Telemetry(None) => {
            println!("No telemetry information available.");
        }
        Response::Status(command::StatusResponse {
            run_mode,
            destinations,
            target_destination,
            connecting,
            connected,
            disconnecting,
        }) => {
            let mut str_resp = format!("{run_mode}\n");
            if let Some(id) = target_destination {
                let is_active = connecting.as_ref().is_some_and(|c| c.destination_id == *id)
                    || connected.as_ref().is_some_and(|c| c.destination_id == *id);
                if !is_active {
                    str_resp.push_str(&format!("---\nWaiting to connect to {id}\n"));
                }
            }
            if let Some(info) = connecting {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            if let Some(info) = connected {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            for info in disconnecting {
                str_resp.push_str(&format!("---\n{info}\n"));
            }
            for dest_state in destinations {
                str_resp.push_str(&format!("---\n{}\n", dest_state.destination));
                if let Some(rh) = &dest_state.route_health {
                    str_resp.push_str(&format!("{} Route health: {}\n", dest_state.destination.id, rh,));
                }
            }
            println!("{str_resp}");
        }
        Response::Balance(Some(command::BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            info,
            ticket_price,
            winning_probability,
        })) => {
            let mut str_resp = String::new();
            str_resp.push_str(&format!(
                "Node Address: {}\nNode Peer ID: {}\nSafe Address: {}\n",
                info.node_address.to_checksum(),
                info.node_peer_id,
                info.safe_address.to_checksum()
            ));
            let safe_sci = balance::wxhopr_scientific(*safe)
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            let price_sci = balance::wxhopr_scientific(*ticket_price)
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            str_resp.push_str(&format!(
                "---\nNode Balance: {node}\nSafe Balance: {safe}{safe_sci}\nTicket Price: {ticket_price}{price_sci}\nWinning Probability: {}\n",
                format_probability(*winning_probability)
            ));
            if channels_out.is_empty() {
                str_resp.push_str("---\nNo outgoing channels.\n");
            } else {
                str_resp.push_str("---\n");
            }
            for ch in channels_out {
                str_resp.push_str(&format!("{ch}\n"));
            }
            if !issues.is_empty() {
                str_resp.push_str("---\nFunding Issues:\n");
                for issue in issues {
                    str_resp.push_str(&format!("  - {issue}\n"));
                }
            }
            println!("{str_resp}");
        }
        Response::Balance(None) => {
            println!("No balance information available.");
        }
        Response::Pong => {
            println!("Pong");
        }
        Response::NerdStats(command::NerdStatsResponse::NoInfo) => {
            eprintln!("No extra stats available. Try connecting to a destination first.");
        }
        Response::NerdStats(command::NerdStatsResponse::Connecting(stats)) => {
            print_connecting_stats(stats);
        }
        Response::NerdStats(command::NerdStatsResponse::Connected(stats)) => {
            print_connected_stats(stats);
        }
        Response::FundingTool(command::FundingToolResponse::WrongPhase) => {
            eprintln!("Already past potential funding phase - no longer possible to fund");
        }
        Response::FundingTool(command::FundingToolResponse::Started) => {
            println!("Started funding");
        }
        Response::FundingTool(command::FundingToolResponse::InProgress) => {
            println!("Funding in progress");
        }
        Response::FundingTool(command::FundingToolResponse::Done) => {
            println!("Funding complete");
        }
        Response::RefreshNodeTriggered => {
            println!("Node balance check triggered");
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
        Response::StartClient(command::StartClientResponse::Started) => {
            println!("Worker client started");
        }
        Response::StartClient(command::StartClientResponse::AlreadyRunning) => {
            eprintln!("Worker client already running");
        }
        Response::StopClient(command::StopClientResponse::Stopped) => {
            println!("Worker client stopped");
        }
        Response::StopClient(command::StopClientResponse::NotRunning) => {
            eprintln!("Worker client not running");
        }
        Response::WorkerOffline => {
            eprintln!("Worker client is currently offline - use command `start-client` to start it");
        }
        Response::ForceReconnectAcknowledged => {}
    }
}

fn format_probability(p: f64) -> String {
    let s = format!("{:.8}", p);
    let trimmed = s.trim_end_matches('0');
    trimmed.trim_end_matches('.').to_string()
}

fn determine_exitcode(resp: &Response) -> ExitCode {
    match resp {
        Response::Connect(command::ConnectResponse::AlreadyConnected(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::Connecting(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::DestinationNotFound) => exitcode::UNAVAILABLE,
        Response::Connect(command::ConnectResponse::WaitingToConnect(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::UnableToConnect(..)) => exitcode::UNAVAILABLE,
        Response::Disconnect(command::DisconnectResponse::Disconnecting(..)) => exitcode::OK,
        Response::Disconnect(command::DisconnectResponse::NotConnected) => exitcode::PROTOCOL,
        Response::Status(..) => exitcode::OK,
        Response::Balance(..) => exitcode::OK,
        Response::Pong => exitcode::OK,
        Response::Telemetry(Some(_)) => exitcode::OK,
        Response::Telemetry(None) => exitcode::UNAVAILABLE,
        Response::NerdStats(command::NerdStatsResponse::NoInfo) => exitcode::UNAVAILABLE,
        Response::NerdStats(command::NerdStatsResponse::Connecting(_)) => exitcode::OK,
        Response::NerdStats(command::NerdStatsResponse::Connected(_)) => exitcode::OK,
        Response::FundingTool(command::FundingToolResponse::WrongPhase) => exitcode::UNAVAILABLE,
        Response::FundingTool(command::FundingToolResponse::Started) => exitcode::OK,
        Response::FundingTool(command::FundingToolResponse::InProgress) => exitcode::OK,
        Response::FundingTool(command::FundingToolResponse::Done) => exitcode::OK,
        Response::RefreshNodeTriggered => exitcode::OK,
        Response::Info(..) => exitcode::OK,
        Response::StartClient(command::StartClientResponse::Started) => exitcode::OK,
        Response::StartClient(command::StartClientResponse::AlreadyRunning) => exitcode::PROTOCOL,
        Response::StopClient(command::StopClientResponse::Stopped) => exitcode::OK,
        Response::StopClient(command::StopClientResponse::NotRunning) => exitcode::PROTOCOL,
        Response::WorkerOffline => exitcode::UNAVAILABLE,
        Response::ForceReconnectAcknowledged => exitcode::PROTOCOL,
    }
}

fn print_connecting_stats(stats: &command::ConnStats) {
    let mut str_resp = print_conn_stats_routing(stats, "-CONNECTING-");
    str_resp.push_str("---\n");
    str_resp.push_str(
        format!(
            "WireGuard Public Key: {}\n",
            stats.wg_pubkey.clone().unwrap_or("--pending generation--".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Assigned WireGuard IP: {}\n",
            stats.wg_ip.clone().unwrap_or("--pending registration--".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(&print_active_session(
        &stats.active_session,
        "--pending session creation--",
    ));
    str_resp.push_str(
        format!(
            "---\nExit WireGuard Public Key: {}\n",
            stats
                .wg_server_pubkey
                .clone()
                .unwrap_or("--pending registration--".to_string())
        )
        .as_str(),
    );
    println!("{str_resp}");
}

fn print_connected_stats(stats: &command::ConnStats) {
    let mut str_resp = print_conn_stats_routing(stats, "-o-");
    str_resp.push_str("---\n");
    if let Some(ref wg_pubkey) = stats.wg_pubkey {
        str_resp.push_str(format!("WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    if let Some(ref ip) = stats.wg_ip {
        str_resp.push_str(format!("Assigned WireGuard IP: {ip}\n").as_str());
    }
    str_resp.push_str(&print_active_session(&stats.active_session, "--none--"));
    if let Some(ref wg_pubkey) = stats.wg_server_pubkey {
        str_resp.push_str(format!("---\nExit WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    println!("{str_resp}");
}

fn print_active_session(session: &Option<command::ActiveSession>, pending: &str) -> String {
    use command::ActiveSession;
    match session {
        Some(ActiveSession::Bridge { bound_host, id }) => {
            format!("Bridge Session entry: {bound_host}\nBridge Session ID: {id}\n")
        }
        Some(ActiveSession::Ping { bound_host, id }) => {
            format!("Ping Session entry: {bound_host}\nPing Session ID: {id}\n")
        }
        Some(ActiveSession::Main { bound_host, id }) => {
            format!("Main Session entry: {bound_host}\nMain Session ID: {id}\n")
        }
        None => format!("Session entry: {pending}\nSession ID: {pending}\n"),
    }
}

fn print_conn_stats_routing(stats: &command::ConnStats, title: &str) -> String {
    let mut str_resp = String::new();
    match stats.destination.routing {
        RoutingOptions::IntermediatePath(ref nodes) => {
            str_resp.push_str(&format!(
                "{node_addr}(me) -{title}-VIA-->",
                node_addr = stats.node_address.to_checksum()
            ));
            for n in nodes.clone() {
                let formatted = match n {
                    NodeId::Chain(addr) => addr.to_checksum(),
                    NodeId::Offchain(peer_id) => peer_id.to_string(),
                };
                str_resp.push_str(&format!(" {formatted} --VIA-->"));
            }
            // safe to truncate as nodes cannot be empty - ensured by type definition
            str_resp.truncate(str_resp.len() - 8);
            str_resp.push_str(&format!(
                "--TO--> {addr}(exit)\n",
                addr = stats.destination.address.to_checksum()
            ));
        }
        RoutingOptions::Hops(nr) => {
            let nr_val: usize = nr.into();
            match nr_val {
                0 => {
                    str_resp.push_str(&format!(
                        "{node_addr}(me) -{title}-DIRECTLY--> {addr}({exit})\n",
                        node_addr = stats.node_address.to_checksum(),
                        addr = stats.destination.address.to_checksum(),
                        exit = stats.destination.id,
                    ));
                }
                1 => {
                    str_resp.push_str(&format!(
                        "{node_addr}(me) -{title}-VIA--1HOP--> {addr}({exit})\n",
                        node_addr = stats.node_address.to_checksum(),
                        addr = stats.destination.address.to_checksum(),
                        exit = stats.destination.id,
                    ));
                }
                _ => {
                    str_resp.push_str(&format!(
                        "{node_addr}(me) -{title}-VIA--{nr}HOPS--> {addr}({exit})\n",
                        node_addr = stats.node_address.to_checksum(),
                        addr = stats.destination.address.to_checksum(),
                        nr = nr_val,
                        exit = stats.destination.id,
                    ));
                }
            }
        }
    };
    str_resp
}
