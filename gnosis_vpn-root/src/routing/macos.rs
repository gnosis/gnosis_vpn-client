//! MacOS specific routing using the pf.
//!
//! Currently only supports setting up WireGuard interface and determining default interface.
//!
//! 1. Create a PF anchor in `/etc/pf.anchors/libp2p_bypass``
//! ```
//! # Route libp2p user traffic outside VPN via physical interface
//! action = "pass out quick"
//! $ext_if = "en0" # replace if needed
//! $ext_gw = "192.168.1.1" # replace with your real gateway
//! ${action} user libp2p route-to ($ext_if $ext_gw)
//! ```
//!
//! 2. PF Main Config Patch: Add to bottom of `/etc/pf.conf`
//! ```
//! anchor "libp2p_bypass"
//! load anchor "libp2p_bypass" from "/etc/pf.anchors/libp2p_bypass"
//! ```
//!
//! 3. launchd service for the libp2p enabled process
//!
//! 4. Setup Script setup_libp2p_split_routing.sh
//!
//! ```
//! #!/bin/bash
//! set -e
//!
//!
//! # 1. Create libp2p user
//! echo "Creating libp2p user..."
//! sudo dscl . -create /Users/libp2p || true
//! sudo dscl . -create /Users/libp2p UserShell /usr/bin/false
//! sudo dscl . -create /Users/libp2p UniqueID 510
//! sudo dscl . -create /Users/libp2p PrimaryGroupID 20
//! sudo dscl . -create /Users/libp2p NFSHomeDirectory /var/empty
//!
//! # 2. Install PF anchor
//! sudo cp libp2p_bypass /etc/pf.anchors/libp2p_bypass
//!
//! # 3. Patch main pf.conf
//! sudo grep -q "libp2p_bypass" /etc/pf.conf || \
//! echo -e "\nanchor \"libp2p_bypass\"\nload anchor \"libp2p_bypass\" from \"/etc/pf.anchors/libp2p_bypass\"" | sudo tee -a /etc/pf.conf
//!
//! # 4. Apply PF
//! sudo pfctl -f /etc/pf.conf
//! sudo pfctl -e || true
//!
//! # 5. Install launchd service
//! sudo cp com.libp2p.node.plist /Library/LaunchDaemons/com.libp2p.node.plist
//! sudo launchctl load /Library/LaunchDaemons/com.libp2p.node.plist
//!
//! echo "Setup complete: libp2p routed outside VPN."
//! ```
//!
//! 5. Teardown Script remove_libp2p_split_routing.sh
//!
//! ```
//! #!/bin/bash
//! set -e
//!
//!
//! echo "Removing launchd service..."
//! sudo launchctl unload /Library/LaunchDaemons/com.libp2p.node.plist || true
//! sudo rm -f /Library/LaunchDaemons/com.libp2p.node.plist//!
//!
//! Remove PF anchor
//! sudo rm -f /etc/pf.anchors/libp2p_bypass
//!
//! Remove pf.conf patch
//! sudo sed -i '' '/libp2p_bypass/d' /etc/pf.conf
//!
//! Reload PF
//! sudo pfctl -f /etc/pf.conf
//!
//! echo "(Optional) Remove user libp2p manually if desired:"
//! echo "sudo dscl . -delete /Users/libp2p"
//!
//! echo "Cleanup complete."
//! ```

use tokio::process::Command;

use gnosis_vpn_lib::shell_command_ext::ShellCommandExt;
// use gnosis_vpn_lib::dirs;
use gnosis_vpn_lib::{event, worker};

use crate::wg_tooling;

use super::Error;

// const PF_RULE_FILE: &str = "pf_gnosisvpn.conf";

/**
 * Refactor logic to use:
 * - [pfctl](https://docs.rs/pfctl/0.7.0/pfctl/index.html)
 */
pub async fn setup(_worker: &worker::Worker, wg_data: &event::WgData) -> Result<(), Error> {
    // 1. generate wg quick content
    let wg_quick_content = wg_data.wg.to_file_string(
        &wg_data.interface_info,
        &wg_data.peer_info,
        // true to route all traffic
        false, // START WITH TRUE TO KILL YOURSELF
    );
    // 2. run wg-quick up
    wg_tooling::up(wg_quick_content).await?;
    // 3. determine interface
    let (_device, _gateway) = interface().await?;
    Ok(())
}

pub async fn teardown(_worker: &worker::Worker, _wg_data: &event::WgData) -> Result<(), Error> {
    // 1. run wg-quick down
    wg_tooling::down().await?;
    Ok(())
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
