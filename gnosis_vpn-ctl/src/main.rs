use exitcode::{self, ExitCode};

use std::process;

use gnosis_vpn_lib::command::{self, Command, Response};
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
        Response::Connect(command::ConnectResponse::Connecting(dest)) => {
            println!("Connecting to {dest}");
        }
        Response::Connect(command::ConnectResponse::WaitingToConnect(dest, health)) => {
            println!("Waiting to connect to {dest} once possible: {health}")
        }
        Response::Connect(command::ConnectResponse::UnableToConnect(dest, health)) => {
            eprintln!("Unable to connect to {dest}: {health}");
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
        Response::Status(command::StatusResponse { run_mode, destinations }) => {
            let mut str_resp = format!("{run_mode}\n");
            for dest_state in destinations {
                str_resp.push_str("---\n");
                let dest = dest_state.destination.clone();
                str_resp.push_str(&format!("{dest}\n"));
                str_resp.push_str(&format!(
                    "{id} Connection: {conn}\n",
                    id = dest.id,
                    conn = dest_state.connection_state
                ));
                str_resp.push_str(&format!(
                    "{id} Connectivity state: {connectivity}\n",
                    id = dest.id,
                    connectivity = dest_state.connectivity
                ));
                let health = dest_state.exit_health.clone();
                str_resp.push_str(&format!("{id} Exit health: {health}\n", id = dest.id));
            }
            println!("{str_resp}");
        }
        Response::Balance(Some(command::BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            info,
        })) => {
            let mut str_resp = String::new();
            str_resp.push_str(&format!(
                "Node Address: {}\nNode Peer ID: {}\nSafe Address: {}\n",
                info.node_address, info.node_peer_id, info.safe_address
            ));
            str_resp.push_str(&format!("---\nNode Balance: {node}\nSafe Balance: {safe}\n"));
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
        Response::Empty => {
            println!();
        }
        Response::Metrics(metrics) => {
            println!("{metrics}");
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
    }
}

fn determine_exitcode(resp: &Response) -> ExitCode {
    match resp {
        Response::Connect(command::ConnectResponse::Connecting(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::DestinationNotFound) => exitcode::UNAVAILABLE,
        Response::Connect(command::ConnectResponse::WaitingToConnect(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::UnableToConnect(..)) => exitcode::UNAVAILABLE,
        Response::Disconnect(command::DisconnectResponse::Disconnecting(..)) => exitcode::OK,
        Response::Disconnect(command::DisconnectResponse::NotConnected) => exitcode::PROTOCOL,
        Response::Status(..) => exitcode::OK,
        Response::Balance(..) => exitcode::OK,
        Response::Pong => exitcode::OK,
        Response::Empty => exitcode::OK,
        Response::Metrics(..) => exitcode::OK,
        Response::Telemetry(Some(_)) => exitcode::OK,
        Response::Telemetry(None) => exitcode::UNAVAILABLE,
        Response::NerdStats(command::NerdStatsResponse::NoInfo) => exitcode::UNAVAILABLE,
        Response::NerdStats(command::NerdStatsResponse::Connecting(_)) => exitcode::OK,
        Response::NerdStats(command::NerdStatsResponse::Connected(_)) => exitcode::OK,
    }
}

fn print_connecting_stats(stats: &command::ConnStats) {
    let mut str_resp = String::new();
    match stats.destination.routing {
        IntermediatePath(nodes) => match nodes {
            [one] => {
                str_resp.push_str(&format!(
                    "{node_addr}(me) --CONNECTING--VIA--> {one}(relayer) --TO--> {addr}(exit)\n",
                    node_addr = stats.node_address,
                    one = one,
                    addr = stats.destination.address
                ));
            }
            [one, two] => {
                str_resp.push_str(&format!("{node_addr}(me) --CONNECTING--VIA--> {one}(relayer_one) --VIA--> {two}(relayer_two) --TO--> {addr}(exit)\n", node_addr = stats.node_address, one = one, two = two, addr = stats.destination.address));
            }
            [one, two, three] => {
                str_resp.push_str(&format!("{node_addr}(me) --CONNECTING--VIA--> {one}(relayer_one) --VIA--> {two}(relayer_two) --VIA--> {three}(relayer_three) --TO--> {addr}(exit)\n", node_addr = stats.node_address, one = one, two = two, three = three, addr = stats.destination.address));
            }
        },
        Hops(nr) => {
            if nr == 0 {
                str_resp.push_str(&format!(
                    "{node_addr}(me) --CONNECTING--DIRECTLY--> {addr}(exit)\n",
                    node_addr = stats.node_address,
                    addr = stats.destination.address
                ));
            } else if nr == 1 {
                str_resp.push_str(&format!(
                    "{node_addr}(me) --CONNECTING--VIA--1HOP--> {addr}(exit)\n",
                    node_addr = stats.node_address,
                    addr = stats.destination.address
                ));
            } else {
                str_resp.push_str(&format!(
                    "{node_addr}(me) --CONNECTING--VIA--{nr}HOPS--> {addr}(exit)\n",
                    node_addr = stats.node_address,
                    addr = stats.destination.address,
                    nr = nr
                ));
            }
        }
    };
    str_resp.push_str("---\n");
    str_resp.push_str(
        format!(
            "WiregGuard Public Key: {}\n",
            stats.wg_pubkey.unwrap_or("<< pending generation >>".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Exit WireGuard Public Key: {}\n",
            stats
                .wg_server_pubkey
                .unwrap_or("<< pending registration >>".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Assigned WireGuard IP: {}\n",
            stats.wg_ip.unwrap_or("<< pending registration >>".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Session entry: {}\n",
            stats
                .session_bound_host
                .map(|h| h.to_string())
                .unwrap_or("<< pending session creation >>".to_string())
        )
        .as_str(),
    );
    str_resp.push_str(
        format!(
            "Session ID: {}\n",
            stats.session_id.unwrap_or("<< pending session creation >>".to_string())
        )
        .as_str(),
    );
    println!("{str_resp}");
}
fn print_connected_stats(stats: &command::ConnStats) {
    let mut str_resp = String::new();
    match stats.destination.routing {
        IntermediatePath(nodes) => {
            match nodes {
                [one] => {
                    str_resp.push_str(&format!(
                        "{node_addr}(me) --VIA--> {one}(relayer) --TO--> {addr}(exit)\n",
                        node_addr = stats.node_address,
                        one = one,
                        addr = stats.destination.address
                    ));
                }
                [one, two] => {
                    str_resp.push_str(&format!("{node_addr}(me) --VIA--> {one}(relayer_one) --VIA--> {two}(relayer_two) --TO--> {addr}(exit)\n", node_addr = stats.node_address, one = one, two = two, addr = stats.destination.address));
                }
                [one, two, three] => {
                    str_resp.push_str(&format!("{node_addr}(me) --VIA--> {one}(relayer_one) --VIA--> {two}(relayer_two) --VIA--> {three}(relayer_three) --TO--> {addr}(exit)\n", node_addr = stats.node_address, one = one, two = two, three = three, addr = stats.destination.address));
                }
            }
        }
        Hops(nr) => {
            if nr == 0 {
                str_resp.push_str(&format!(
                    "{node_addr}(me) --> {addr}(exit)\n",
                    node_addr = stats.node_address,
                    addr = stats.destination.address
                ));
            } else if nr == 1 {
                str_resp.push_str(&format!(
                    "{node_addr}(me) --1HOP--> {addr}(exit)\n",
                    node_addr = stats.node_address,
                    addr = stats.destination.address
                ));
            } else {
                str_resp.push_str(&format!(
                    "{node_addr}(me) --{nr}HOPS--> {addr}(exit)\n",
                    node_addr = stats.node_address,
                    addr = stats.destination.address,
                    nr = nr
                ));
            }
        }
    };
    str_resp.push_str("---\n");
    if let Some(wg_pubkey) = stats.wg_pubkey {
        str_resp.push_str(format!("WiregGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    if let Some(wg_pubkey) = stats.wg_server_pubkey {
        str_resp.push_str(format!("Exit WireGuard Public Key: {}\n", wg_pubkey).as_str());
    }
    if let Some(ip) = stats.wg_ip {
        str_resp.push_str(format!("Assigned WireGuard IP: {ip}\n").as_str());
    }
    if let Some(bound_host) = stats.session_bound_host {
        str_resp.push_str(format!("Session entry: {bound_host}\n").as_str());
    }
    if let Some(id) = stats.session_id {
        str_resp.push_str(format!("Session ID: {id}\n").as_str());
    }
    println!("{str_resp}");
}
