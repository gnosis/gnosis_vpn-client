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
            eprintln!("Error processing {cmd}: {e}");
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
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("Error serializing response to JSON: {e}"),
    }
}

fn pretty_print(resp: &Response) {
    match resp {
        Response::Connect(command::ConnectResponse::Connecting(dest)) => {
            println!("Connecting to {dest}");
        }
        Response::Connect(command::ConnectResponse::AddressNotFound) => {
            eprintln!("Node address not found in available destinations");
        }
        Response::Disconnect(command::DisconnectResponse::Disconnecting(dest)) => {
            println!("Disconnecting from {dest}");
        }
        Response::Disconnect(command::DisconnectResponse::NotConnected) => {
            eprintln!("Currently not connected to any destination");
        }
        Response::Status(command::StatusResponse {
            status,
            available_destinations,
            funding,
            network,
        }) => {
            let mut str_resp = format!("Status: {status}\n");
            if let command::FundingState::TopIssue(issue) = funding {
                str_resp.push_str(&format!("WARNING: {issue}\n"));
            }
            if let Some(network) = network {
                str_resp.push_str(&format!("Network: {network}\n"));
            }
            if available_destinations.is_empty() {
                str_resp.push_str("No destinations available.\n")
            } else {
                str_resp.push_str("Available destinations:\n");
                for dest in available_destinations {
                    str_resp.push_str(&format!("  - {dest}\n"));
                }
            }
            println!("{str_resp}");
        }
        Response::Balance(Some(command::BalanceResponse {
            node,
            safe,
            channels_out,
            issues,
            addresses,
        })) => {
            let mut str_resp = format!("Node Balance: {node}\nSafe Balance: {safe}\nChannels Out: {channels_out}\n");
            str_resp.push_str(&format!(
                "---\nNode Address: {}\nSafe Address: {}\n",
                addresses.node, addresses.safe
            ));
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
    }
}

fn determine_exitcode(resp: &Response) -> ExitCode {
    match resp {
        Response::Connect(command::ConnectResponse::Connecting(..)) => exitcode::OK,
        Response::Connect(command::ConnectResponse::AddressNotFound) => exitcode::UNAVAILABLE,
        Response::Disconnect(command::DisconnectResponse::Disconnecting(..)) => exitcode::OK,
        Response::Disconnect(command::DisconnectResponse::NotConnected) => exitcode::PROTOCOL,
        Response::Status(..) => exitcode::OK,
        Response::Balance(..) => exitcode::OK,
        Response::Pong => exitcode::OK,
    }
}
