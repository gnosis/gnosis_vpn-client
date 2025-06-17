use exitcode::{self, ExitCode};
use std::process;

use gnosis_vpn_lib::command::{self, Command, Response};
use gnosis_vpn_lib::socket;

mod cli;

fn main() {
    let args = cli::parse();

    let cmd: Command = args.command.into();
    let resp = match socket::process_cmd(&args.socket_path, &cmd) {
        Ok(resp) => resp,
        Err(e) => {
            eprintln!("Error processing {}: {}", cmd, e);
            return;
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
        Ok(s) => println!("{}", s),
        Err(e) => eprintln!("Error serializing response to JSON: {}", e),
    }
}

fn pretty_print(resp: &Response) {
    match resp {
        Response::Connect(command::ConnectResponse::Connecting(dest)) => {
            println!("Connecting to {}", dest);
        }
        Response::Connect(command::ConnectResponse::PeerIdNotFound) => {
            eprintln!("Peer ID not found in available destinations");
        }
        Response::Disconnect(command::DisconnectResponse::Disconnecting(dest)) => {
            println!("Disconnecting from {}", dest);
        }
        Response::Disconnect(command::DisconnectResponse::NotConnected) => {
            eprintln!("Currently not connected to any destination");
        }
        Response::Status(command::StatusResponse {
            wireguard,
            status,
            available_destinations,
        }) => {
            let mut str_resp = format!("WireGuard status: {}\n", wireguard);
            str_resp.push_str(&format!("Status: {}\n", status));
            if available_destinations.is_empty() {
                str_resp.push_str("No destinations available.\n")
            } else {
                str_resp.push_str("Available destinations:\n");
                for dest in available_destinations {
                    str_resp.push_str(&format!("  - {}\n", dest));
                }
            }
            println!("{}", str_resp);
        }
        Response::Pong => {
            println!("Pong");
        }
    }
}

fn determine_exitcode(resp: &Response) -> ExitCode {
    match resp {
        Response::Connect(command::ConnectResponse::Connecting(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::PeerIdNotFound) => exitcode::UNAVAILABLE,
        Response::Disconnect(command::DisconnectResponse::Disconnecting(..)) => exitcode::OK,
        Response::Disconnect(command::DisconnectResponse::NotConnected) => exitcode::PROTOCOL,
        Response::Status(..) => exitcode::OK,
        Response::Pong => exitcode::OK,
    }
}
