//use tokio::process::Command;

// use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
// use gnosis_vpn_lib::wireguard;
use gnosis_vpn_lib::{event, hopr::hopr_lib::async_trait, worker};

use crate::wg_tooling;

use super::{Error, Routing};

// const MARK: &str = "0xDEAD";

pub fn build_userspace_router(worker: worker::Worker, wg_data: event::WgData) -> Result<impl Routing, Error> {
    Ok(Router { worker, wg_data })
}

pub struct Router {
    worker: worker::Worker,
    wg_data: event::WgData,
}

impl Router {
    pub fn new(worker: worker::Worker, wg_data: event::WgData) -> Self {
        Self { worker, wg_data }
    }
}

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 */
#[async_trait]
impl Routing for Router {
    async fn setup(&self) -> Result<(), Error> {
        // 1. generate wg quick content
        let wg_quick_content = self.wg_data.wg.to_file_string(
            &self.wg_data.interface_info,
            &self.wg_data.peer_info,
            // true to route all traffic
            false,
        );
        // 2. run wg-quick up
        wg_tooling::up(wg_quick_content).await?;
        Ok(())
    }

    async fn teardown(&self) -> Result<(), Error> {
        // 1. run wg-quick down
        //  wg_tooling::down().await?;
        Ok(())
    }
}

/*
async fn add_ip_rules(worker: &worker::Worker) -> Result<(), Error> {
    // except wireguard subnet from bypassing traffic
    // wg show wg0_gnosisvpn fwmark
    let interface_parts: Vec<&str> = wireguard::WG_CONFIG_FILE.split('.').collect();
    let interface = interface_parts[0];
    let fwmark = Command::new("wg")
        .arg("show")
        .arg(interface)
        .arg("fwmark")
        .run_stdout()
        .await?;
    // TODO use dynamic ip from interface
    // TODO setup for macos
    // ip rule add to 10.128.0.0/24 lookup 0xca6c priority 50
    Command::new("ip")
        .arg("rule")
        .arg("add")
        .arg("to")
        .arg("10.128.0.0/24")
        .arg("lookup")
        .arg(fwmark)
        .arg("priority")
        .arg("50")
        .run()
        .await?;

    // forward rules need to be applied after wg-quick up so that their priority is higher
    // wg-quick up is quite clever and adjusts it's own rule setting if our bypass rules are
    // applied too early
    // we specifically want to bypass all packages marked and from our user, so that outgoing and
    // incoming traffic should work correctly in tandem with the iptables rules above
    // add rule affecting marked packages
    // ip rule add fwmark 0xDEAD lookup main priority 90;
    Command::new("ip")
        .arg("rule")
        .arg("add")
        .arg("fwmark")
        .arg(MARK)
        .arg("lookup")
        .arg("main")
        .arg("priority")
        .arg("90")
        .spawn_no_capture()
        .await?;

    // add rull affecting outgoing user packages
    // ip rule add uidrange 992-992 lookup main priority 100
    Command::new("ip")
        .arg("rule")
        .arg("add")
        .arg("uidrange")
        .arg(format!("{}-{}", worker.uid, worker.uid))
        .arg("lookup")
        .arg("main")
        .arg("priority")
        .arg("100")
        .spawn_no_capture()
        .await?;

    // setup is run outside of connection context and only applies global firewall routing
    // mark outgoing packages of worker process user
    // iptables -t mangle -A OUTPUT -m owner --uid-owner 992 -j MARK --set-mark 0xDEAD;
    Command::new("iptables")
        .arg("-t")
        .arg("mangle")
        .arg("-A")
        .arg("OUTPUT")
        .arg("-m")
        .arg("owner")
        .arg("--uid-owner")
        .arg(format!("{}", worker.uid))
        .arg("-j")
        .arg("MARK")
        .arg("--set-mark")
        .arg(MARK)
        .run()
        .await?;

    // save mark of those outgoing packages
    // iptables -t mangle -A OUTPUT -m mark --mark 0xDEAD -j CONNMARK --save-mark;
    Command::new("iptables")
        .arg("-t")
        .arg("mangle")
        .arg("-A")
        .arg("OUTPUT")
        .arg("-m")
        .arg("mark")
        .arg("--mark")
        .arg(MARK)
        .arg("-j")
        .arg("CONNMARK")
        .arg("--save-mark")
        .run()
        .await?;
    // restore mark on incoming packages belonging to those connections, so they can bypass routing
    // iptables -t mangle -I PREROUTING 1 -j CONNMARK --restore-mark
    Command::new("iptables")
        .arg("-t")
        .arg("mangle")
        .arg("-I")
        .arg("PREROUTING")
        .arg("1")
        .arg("-j")
        .arg("CONNMARK")
        .arg("--restore-mark")
        .run()
        .await?;

    Ok(())
}

async fn del_ip_rules(worker: &worker::Worker) -> Result<(), Error> {
    // run all del commands before evaluated results
    let res1 = Command::new("ip")
        .arg("rule")
        .arg("del")
        .arg("fwmark")
        .arg(MARK)
        .arg("lookup")
        .arg("main")
        .arg("priority")
        .arg("90")
        .spawn_no_capture()
        .await;
    let res2 = Command::new("ip")
        .arg("rule")
        .arg("del")
        .arg("uidrange")
        .arg(format!("{}-{}", worker.uid, worker.uid))
        .arg("lookup")
        .arg("main")
        .arg("priority")
        .arg("100")
        .spawn_no_capture()
        .await;
    res1.and(res2)?;

    // run all teardown commands before evaluated results
    let res1 = Command::new("iptables")
        .arg("-t")
        .arg("mangle")
        .arg("-D")
        .arg("OUTPUT")
        .arg("-m")
        .arg("owner")
        .arg("--uid-owner")
        .arg(format!("{}", worker.uid))
        .arg("-j")
        .arg("MARK")
        .arg("--set-mark")
        .arg(MARK)
        .spawn_no_capture()
        .await;
    let res2 = Command::new("iptables")
        .arg("-t")
        .arg("mangle")
        .arg("-D")
        .arg("OUTPUT")
        .arg("-m")
        .arg("mark")
        .arg("--mark")
        .arg(MARK)
        .arg("-j")
        .arg("CONNMARK")
        .arg("--save-mark")
        .spawn_no_capture()
        .await;
    let res3 = Command::new("iptables")
        .arg("-t")
        .arg("mangle")
        .arg("-D")
        .arg("PREROUTING")
        .arg("-j")
        .arg("CONNMARK")
        .arg("--restore-mark")
        .spawn_no_capture()
        .await;
    res1.and(res2).and(res3)?;
    Ok(())
}
*/
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
