use gnosis_vpn_lib::command::Command;
use gnosis_vpn_lib::socket;
use std::path::PathBuf;

mod cli;

fn main() {
    let args = cli::parse();

    let cmd: Command = args.command.into();
    let json_output = args.json;

    match process_cmd(&args.socket_path, &cmd) {
        Ok(Some(s)) => {
            if json_output {
                match serde_json::to_string_pretty(&s) {
                    Ok(json) => println!("{}", json),
                    Err(e) => eprintln!("Error serializing to JSON: {:?}", e),
                }
            } else {
                println!("{:?}", s);
            }
        }
        Ok(None) => (),
        Err(e) => {
            eprintln!("Error processing {}: {:?}", cmd, e);
        }
    }
}

fn process_cmd(socket_path: &PathBuf, cmd: &Command) -> Result<Option<String>, socket::Error> {
    match socket::process_cmd(socket_path, cmd) {
        Ok(socket::ReturnValue::WithResponse(s)) => Ok(Some(s)),
        Ok(_) => Ok(None),
        Err(e) => Err(e),
    }
}
