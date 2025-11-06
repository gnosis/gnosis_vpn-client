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
    let resp = match socket::process_cmd(&args.socket_path, &cmd).await {
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
            run_mode,
            destinations,
            network,
        }) => {
            let mut str_resp = format!("Status: {run_mode}\n");
            str_resp.push_str(&format!("Network: {network}\n"));
            str_resp.push_str("Destinations:\n");
            for dest in destinations {
                str_resp.push_str(&format!("  {dest}\n"));
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
            let mut str_resp = format!("Node Address: {}\nSafe Address: {}\n", addresses.node, addresses.safe);
            str_resp.push_str(&format!(
                "---\nNode Balance: {node}\nSafe Balance: {safe}\nChannels Out: {channels_out}\n"
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
        Response::Empty => {
            println!();
        }
        Response::Metrics(metrics) => {
            println!("{metrics}");
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
        Response::Empty => exitcode::OK,
        Response::Metrics(..) => exitcode::OK,
    }
}
