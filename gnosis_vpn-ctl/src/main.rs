use exitcode::{self, ExitCode};

use std::process;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::connection::destination::{NodeId, RoutingOptions};
use gnosis_vpn_lib::check_update;
use gnosis_vpn_lib::socket;

mod cli;

// Avoid musl's default allocator due to degraded performance
// https://nickb.dev/blog/default-musl-allocator-considered-harmful-to-performance
#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() {
    let args = cli::parse();

    if let cli::Command::CheckUpdate {} = args.command {
        let client = reqwest::Client::new();
        match check_update::download(&client).await {
            Ok(manifest) => {
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&manifest).unwrap_or_default());
                } else {
                    let channels = &manifest["channels"];
                    if let Some(stable) = channels["stable"].as_object() {
                        let version = stable["version"].as_str().unwrap_or("unknown");
                        let published_at = stable["published_at"].as_str().unwrap_or("unknown");
                        let url = stable["download_url"].as_str().unwrap_or("unknown");
                        println!("Stable: {version}, published at {published_at}, download at: {url}");
                    }
                    if let Some(nightly) = channels["nightly"].as_object() {
                        let version = nightly["version"].as_str().unwrap_or("unknown");
                        let published_at = nightly["published_at"].as_str().unwrap_or("unknown");
                        let url = nightly["download_url"].as_str().unwrap_or("unknown");
                        println!("Latest Nightly: {version}, published at {published_at}, download at: {url}");
                    }
                }
                process::exit(exitcode::OK);
            }
            Err(e) => {
                eprintln!("Error fetching latest version: {e}");
                process::exit(exitcode::UNAVAILABLE);
            }
        }
    }

    let cmd: Command = args.command.into();
    let resp = match socket::root::process_cmd(&args.socket_path, &cmd).await {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("Error processing {cmd}: {e}");
            process::exit(exitcode::UNAVAILABLE);
        }
    };

    if args.json {
        json_print(&resp)
    } else {
        pretty_print(&resp)
    };

    let exit = determine_exitcode(&resp);
    process::exit(exit);
}

fn json_print(resp: &Response) {
    match serde_json::to_string_pretty(resp) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("Error serializing response to JSON: {e}"),
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
            str_resp.push_str(&format!(
                "---\nNode Balance: {node}\nSafe Balance: {safe}\nTicket Price: {ticket_price}\nWinning Probability: {winning_probability:.4}\n"
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
            let mut str_resp = format!("Gnosis VPN client service {version}", version = info.version);
            if let Some(ref file) = info.log_file {
                str_resp.push_str(&format!("\nLog file: {file}", file = file.display()));
            }
            println!("{str_resp}");
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
    }
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
    str_resp.push_str(
        format!(
            "Session entry: {}\n",
            stats
                .session_bound_host
                .map(|h| h.to_string())
                .unwrap_or("--pending session creation--".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Session ID: {}\n",
            stats
                .session_id
                .clone()
                .unwrap_or("--pending session creation--".to_string())
        )
        .as_str(),
    );
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
    if let Some(bound_host) = stats.session_bound_host {
        str_resp.push_str(format!("Session entry: {bound_host}\n").as_str());
    }
    if let Some(ref id) = stats.session_id {
        str_resp.push_str(format!("Session ID: {id}\n").as_str());
    }

    if let Some(ref wg_pubkey) = stats.wg_server_pubkey {
        str_resp.push_str(format!("---\nExit WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    println!("{str_resp}");
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
