use tokio::fs;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::{dirs, worker};

use super::Error;

const PF_RULE_FILE: &str = "pf_gnosisvpn.conf";

/**
 * Refactor logic to use:
 * - [pfctl](https://docs.rs/pfctl/0.7.0/pfctl/index.html)
 */
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    let (device, gateway) = interface().await?;

    let route_to = match gateway {
        Some(gw) => format!("{} {}", device, gw),
        None => format!("{}", device),
    };

    let conf_file = dirs::cache_dir(PF_RULE_FILE)?;

    let content = format!(
        r#"
        set skip on lo0
        pass out quick user {uid} route-to ({route_to}) keep state
    "#,
        route_to = route_to,
        uid = worker.uid,
    );

    fs::write(&conf_file, content.as_bytes()).await?;

    Command::new("pfctl")
        .arg("-a")
        .arg(gnosis_vpn_lib::IDENTIFIER)
        .arg("-f")
        .arg(conf_file)
        .run()
        .await
        .map_err(Error::from)
}

pub async fn teardown(_worker: &worker::Worker) -> Result<(), Error> {
    let cmd = Command::new("pfctl")
        .arg("-a")
        .arg(gnosis_vpn_lib::IDENTIFIER)
        .arg("-F")
        .arg("all")
        .spawn_no_capture()
        .await
        .map_err(Error::from);

    let conf_file = dirs::cache_dir(PF_RULE_FILE)?;
    if conf_file.exists() {
        let _ = fs::remove_file(conf_file).await;
    }

    cmd?;

    Ok(())
}

async fn interface() -> Result<(String, Option<String>), Error> {
    let output = Command::new("route")
        .arg("-n")
        .arg("get")
        .arg("0.0.0.0")
        .run_stdout()
        .await?;

    let res = parse_interface(&output)?;
    Ok(res)
}

fn parse_interface(output: &str) -> Result<(String, Option<String>), Error> {
    let parts: Vec<&str> = output.split_whitespace().collect();
    let device_index = parts.iter().position(|&x| x == "interface:");
    let via_index = parts.iter().position(|&x| x == "gateway:");
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

#[cfg(test)]
mod tests {
    #[test]
    fn parses_interface_gateway() -> anyhow::Result<()> {
        let output = r#"
           route to: default
        destination: default
               mask: default
            gateway: 192.168.178.1
          interface: en1
              flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
         recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire
               0         0         0         0         0         0      1500         0
        "#;

        let (device, gateway) = super::parse_interface(output)?;

        assert_eq!(device, "en1");
        assert_eq!(gateway, Some("192.168.178.1".to_string()));
        Ok(())
    }
}
