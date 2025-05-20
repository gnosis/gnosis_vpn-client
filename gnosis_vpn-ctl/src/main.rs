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

    let exit = -pretty_print(&resp);
    process::exit(exit);
}

fn pretty_print(resp: &Response) -> ExitCode {
    // pretty print for users
    match resp {
        Response::Connect(command::ConnectResponse::Connecting(dest)) => {
            println!("Connecting to {}", dest);
            return exitcode::OK;
        }
        Response::Connect(command::ConnectResponse::PeerIdNotFound) => {
            eprintln!("Peer ID not found in available destinations");
            return exitcode::UNAVAILABLE;
        }
        Response::Disconnect(command::DisconnectResponse::Disconnecting(dest)) => {
            println!("Disconnecting from {}", dest);
            return exitcode::OK;
        }
        Response::Disconnect(command::DisconnectResponse::NotConnected) => {
            eprintln!("Currently not connected to any destination");
            return exitcode::PROTOCOL;
        }
        Response::Status(command::StatusResponse {
            wireguard,
            status,
            available_destinations,
        }) => {
            let mut str_resp = format!("WireGuard status: {}\n", wireguard);
            str_resp.push_str(&format!("Status: {}\n", status));
            str_resp.push_str("Available destinations:\n");
            for dest in available_destinations {
                str_resp.push_str(&format!("  - {}\n", dest));
            }
            println!("{}", str_resp);
            return exitcode::OK;
        }
    }
}
