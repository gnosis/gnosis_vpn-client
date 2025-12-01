use crate::user;

use super::Error;

#[derive(Debug)]
pub struct Routing {
    worker: user::Worker,
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
