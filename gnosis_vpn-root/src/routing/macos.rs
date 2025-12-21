use std::sync::Arc;

use gnosis_vpn_lib::hopr::hopr_lib::async_trait;
use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
// use gnosis_vpn_lib::dirs;
use gnosis_vpn_lib::{event, worker};

use crate::wg_tooling;

use super::{Error, Routing};

pub fn build_firewall_router(worker: worker::Worker, wg_data: event::WgData) -> Result<impl Routing, Error> {
    let pf = pfctl::PfCtl::new()?;
    Ok(Firewall {
        fw: Arc::new(std::sync::Mutex::new(pf)),
        worker,
        wg_data,
    })
}

// const PF_RULE_FILE: &str = "pf_gnosisvpn.conf";

pub struct Firewall {
    fw: Arc<std::sync::Mutex<pfctl::PfCtl>>,
    #[allow(dead_code)]
    worker: worker::Worker,
    wg_data: event::WgData,
}

#[async_trait]
impl Routing for Firewall {
    /**
     * Refactor logic to use:
     * - [pfctl](https://docs.rs/pfctl/0.7.0/pfctl/index.html)
     */
    #[tracing::instrument(name = "Firewall::setup",level = "info", skip(self), fields(interface = ?self.wg_data.interface_info, peer = ?self.wg_data.peer_info), ret, err)]
    async fn setup(&self) -> Result<(), Error> {
        // 1. generate wg quick content
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            // true to route all traffic
            false, // START WITH TRUE TO KILL YOURSELF
        );

        // 2. run wg-quick up
        wg_tooling::up(wg_quick_content).await?;

        // 3. determine interface
        let (device, gateway) = interface().await?;

        tracing::info!(%device, ?gateway, "Determined default interface");

        // Create a PfCtl instance to control PF with:
        let mut pf = self
            .fw
            .lock()
            .ok()
            .ok_or(Error::General("Failed to acquire lock on pfctl".into()))?;

        // Enable the firewall, equivalent to the command "pfctl -e":
        pf.try_enable()?;

        // // Add an anchor rule for packet filtering rules into PF. This will fail if it already exists,
        // // use `try_add_anchor` to avoid that:
        // let anchor_name = "testing-out-pfctl";
        // pf.add_anchor(anchor_name, pfctl::AnchorKind::Filter).unwrap();

        // // Create a packet filtering rule matching all packets on the "lo0" interface and allowing
        // // them to pass:
        // let rule = pfctl::FilterRuleBuilder::default()
        //     .action(pfctl::FilterRuleAction::Pass)
        //     .interface("lo0")
        //     .build()
        //     .unwrap();

        // // Add the filterig rule to the anchor we just created.
        // pf.add_rule(anchor_name, &rule).unwrap();

        Ok(())
    }

    #[tracing::instrument(name = "Firewall::teardown",level = "info", skip(self), fields(interface = ?self.wg_data.interface_info, peer = ?self.wg_data.peer_info), ret, err)]
    async fn teardown(&self) -> Result<(), Error> {
        // 1. run wg-quick down
        wg_tooling::down().await?;
        Ok(())
    }
}

/*
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
    let (device, gateway) = interface().await?;

    let route_to = match gateway {
        Some(gw) => format!("{} {}", device, gw),
        None => device,
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

*/

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

fn pre_up_routing(relayer_ip: &Ipv4Addr, interface: &InterfaceInfo) -> String {
    if cfg!(target_os = "macos") {
        if let Some(ref gateway) = interface.gateway {
            format!(
                "route -n add --host {relayer_ip} {gateway}",
                relayer_ip = relayer_ip,
                gateway = gateway
            )
        } else {
            format!(
                "route -n add -host {relayer_ip} -interface {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    } else {
        // assuming linux
        if let Some(ref gateway) = interface.gateway {
            format!(
                "ip route add {relayer_ip} via {gateway} dev {device}",
                relayer_ip = relayer_ip,
                gateway = gateway,
                device = interface.device
            )
        } else {
            format!(
                "ip route add {relayer_ip} dev {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    }
}

fn post_down_routing(relayer_ip: &Ipv4Addr, interface: &InterfaceInfo) -> String {
    if cfg!(target_os = "macos") {
        format!("route -n delete -host {relayer_ip}", relayer_ip = relayer_ip)
    } else {
        // assuming linux
        if let Some(ref gateway) = interface.gateway {
            format!(
                "ip route del {relayer_ip} via {gateway} dev {device}",
                relayer_ip = relayer_ip,
                gateway = gateway,
                device = interface.device
            )
        } else {
            format!(
                "ip route del {relayer_ip} dev {device}",
                relayer_ip = relayer_ip,
                device = interface.device
            )
        }
    }
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
