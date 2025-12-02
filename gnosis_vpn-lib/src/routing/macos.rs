use tokio::process::Command;

use crate::user;
use crate::dirs;

use super::Error;

const PF_RULE_FILE: &str = "pf_gnosisvpn.conf";

/**
 * Refactor logic to use:
 * - [pfctl](https://docs.rs/pfctl/0.7.0/pfctl/index.html)
 */
impl Routing {
    pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
        let (device, gateway) = interface()?;
        let conf_file = dirs::cache_dir(WG_CONFIG_FILE)?;
        let content = format!("pass out route-to ({device} {gateway}) from any to any group {worker.group_name}").as_bytes();
        fs::write(&conf_file, content).await?;
        Command::new("pfctl")
            .arg("-a").arg(crate::IDENTIFIER)
            .arg("-f").arg(conf_file)
            .run()
            .await
            .map_err(Error::from)
    }
}

async fn interface() -> Result<(String, Option<String>), Error> {
    let output  = Command::new("route")
            .arg("-n")
            .arg("get")
            .arg("0.0.0.0")
            .run_stdout().await?;

            let parts: Vec<&str> = stdout.split_whitespace().collect();
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
            Ok(( device, gateway )
        }
