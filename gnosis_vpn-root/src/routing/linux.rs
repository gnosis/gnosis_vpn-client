use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
use gnosis_vpn_lib::{wireguard, worker};

use super::Error;

const MARK: &str = "0xDEAD";

/**
 * Refactor logic to use:
 * - [rtnetlink](https://docs.rs/rtnetlink/latest/rtnetlink/index.html)
 */
pub async fn setup(worker: &worker::Worker) -> Result<(), Error> {
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

pub async fn teardown(worker: &worker::Worker) -> Result<(), Error> {
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
        .run()
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
        .run()
        .await;
    let res3 = Command::new("iptables")
        .arg("-t")
        .arg("mangle")
        .arg("-D")
        .arg("PREROUTING")
        .arg("1")
        .arg("-j")
        .arg("CONNMARK")
        .arg("--restore-mark")
        .run()
        .await;
    res1.and(res2).and(res3)?;
    Ok(())
}

pub async fn add_ip_rules(worker: &worker::Worker) -> Result<(), Error> {
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
        .await
        .map_err(Error::from)?;
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
        .await
        .map_err(Error::from)?;
    Ok(())
}

pub async fn del_ip_rules(worker: &worker::Worker) -> Result<(), Error> {
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
        .await
        .map_err(Error::from);
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
        .await
        .map_err(Error::from);
    res1.and(res2)?;
    Ok(())
}
