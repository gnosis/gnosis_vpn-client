use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::socket;

mod cli;

fn main() {
    let args = cli::parse();

    let cmd: Command = args.command.into();

    match socket::process_cmd(&args.socket_path, &cmd) {
        Ok(str_resp) => match serde_json::to_string_pretty(&str_resp) {
            Ok(json) => println!("{}", json),
            Err(e) => eprintln!("Error pretty printing: {}", e),
        },
        Err(e) => {
            eprintln!("Error processing {}: {}", cmd, e);
        }
    }
}
