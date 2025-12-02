use crate::user;

use super::Error;

#[derive(Debug)]
pub struct Routing {
    worker: user::Worker,
}


impl Routing {
    pub fn new(worker: worker::Worker) -> Result<Self, Error> {
        Ok(Routing { worker })
    }

    pub async fn setup(&mut self) -> Result<(), Error> {
        let (device, gateway) = determine_interface()?;


        let output = Command::new("ip")
            .arg("rule")
            .arg("add")
            .arg("uidrange")
            .arg(format!("{}-{}", self.worker.uid, self.worker.uid))
            .arg("lookup")
            .arg("main")
            .arg("priority")
            .arg("100")
            .output()
            .await?;

        let stderrempty = output.stderr.is_empty();
        match (stderrempty, output.status.success()) {
            (true, true) => Ok(()),
            (false, true) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                tracing::warn!(%stderr, "Non empty stderr on successful ip rule addition");
                OK(())
            },
            (_, false) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let status_code = output.status.code();
            tracing::error!(?status_code, %stdout, %stderr, "Error executing ip rule addition");
            Err(Error::IpRuleSetup)
            }


        }
    }
}

async fn determine_interface() -> Result<(String, Option<String>), Error> {
     let output = Command::new("route")
            .arg("-n")
            .arg("get")
            .arg("0.0.0.0")
            .output()
            .await?;
        if !output.stderr.is_empty() {
            tracing::error!(
                stderr = String::from_utf8_lossy(&output.stderr).to_string(),
                "error running route -n get"
            );
        }
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = stdout.split_whitespace().collect();

            let device_index = parts.iter().position(|&x| x == "interface");
            let via_index = parts.iter().position(|&x| x == "gateway");

            let device = match device_index.and_then(|idx| parts.get(idx + 1)) {
                Some(dev) => dev.to_string(),
                None => {
                    tracing::error!(%stdout, "Unable to determine default interface from route -n get output");
                    return Err(Error::NoInterface);
                }
            };

            let gateway = via_index.and_then(|idx| parts.get(idx + 1)).map(|gw| gw.to_string());
            Ok(InterfaceInfo { gateway, device })
        } else {
            Err(Error::RoutingDetection)
        }
/*
#!/bin/bash

# 1. Dynamic Detection
# Find the default interface (e.g., en0)
DEF_IF=$(route -n get 0.0.0.0 | grep 'interface:' | awk '{print $2}')

# Find the default gateway IP (e.g., 192.168.1.1)
DEF_GW=$(route -n get 0.0.0.0 | grep 'gateway:' | awk '{print $2}')

if [ -z "$DEF_IF" ] || [ -z "$DEF_GW" ]; then
    echo "Error: Could not determine Gateway or Interface."
    exit 1
fi

# 2. Define the Target Group
# Traffic from this group ID will bypass VPN
TARGET_GROUP="_mixnetbypass"

# 3. Generate the PF Rule
# "pass out": Allow traffic
# "route-to": FORCE it to the physical interface and gateway
# "group": Only for this process group
PF_RULE="pass out route-to ($DEF_IF $DEF_GW) proto udp from any to any group $TARGET_GROUP"

echo "Applying PF Rule: $PF_RULE"

# 4. Load into a temporary anchor
# We use 'echo' to pipe the rule directly into pfctl
echo "$PF_RULE" | sudo pfctl -a com.apple/bypass_vpn -f -

# 5. Enable PF (just in case it's off, though macOS usually has it on)
sudo pfctl -e 2>/dev/null || true
*/

fn apply_pf_bypass(interface: &str, gateway: &str, gid: u32) {
    // We remove 'proto udp' to allow TCP, UDP, and ICMP (Ping)
    let rule = format!(
        "pass out route-to ({interface} {gateway}) from any to any group {gid}"
    );

    let mut child = std::process::Command::new("pfctl")
        .arg("-a").arg("com.vpn.bypass")
        .arg("-f").arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("Failed to spawn pfctl");

    use std::io::Write;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(rule.as_bytes()).expect("Failed to write to pfctl");
    }
}
# 'quick' means: If this matches, stop processing other rules and do this immediately.
PF_RULE="pass out quick route-to ($DEF_IF $DEF_GW) from any to any group $TARGET_GROUP"
