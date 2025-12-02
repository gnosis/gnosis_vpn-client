use tokio::fs;
use tokio::process::Command;

use crate::dirs;
use crate::util::CommandExt;
use crate::worker;

use super::Error;

const PF_RULE_FILE: &str = "pf_gnosisvpn.conf";

/**
 * Refactor logic to use:
 * - [pfctl](https://docs.rs/pfctl/0.7.0/pfctl/index.html)
 */
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    let (device, gateway) = interface().await?;
    let interface_str = match gateway {
        Some(gw) => format!("({} {})", device, gw),
        None => format!("({})", device),
    };
    let conf_file = dirs::cache_dir(PF_RULE_FILE)?;
    let content = format!(
        "pass out route-to ({interface_str}) from any to any group {group_name}",
        group_name = worker.group_name
    );
    fs::write(&conf_file, content.as_bytes()).await?;
    Command::new("pfctl")
        .arg("-a")
        .arg(crate::IDENTIFIER)
        .arg("-f")
        .arg(conf_file)
        .run()
        .await
        .map_err(Error::from)
}

async fn interface() -> Result<(String, Option<String>), Error> {
    let output = Command::new("route")
        .arg("-n")
        .arg("get")
        .arg("0.0.0.0")
        .run_stdout()
        .await?;

    let parts: Vec<&str> = output.split_whitespace().collect();
    let device_index = parts.iter().position(|&x| x == "interface");
    let via_index = parts.iter().position(|&x| x == "gateway");
    let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
        Some(dev) => dev.to_string(),
        None => {
            tracing::error!(%output, "Unable to determine default interface");
            return Err(Error::NoInterface);
        }
    };

    let gateway = via_index.and_then(|idx| parts.get(idx + 1)).map(|gw| gw.to_string());
    Ok((device, gateway))
}
