#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 5 ]; then
  echo "Usage: $0 <device> <gateway> <utun_device> <uid> <wg_nat_cidr>" >&2
  exit 1
fi

device="$1"
gateway="$2"
utun_device="$3"
uid="$4"
wg_nat_cidr="$5"

filter_anchor="gnosisvpn_bypass"
nat_anchor="gnosisvpn_bypass_nat"

sudo sysctl -w net.inet.ip.forwarding=1
sudo pfctl -e || true

filter_rules=$(sudo pfctl -sr)
if ! grep -q "anchor \"${filter_anchor}\"" <<< "$filter_rules"; then
  sudo pfctl -Rf - <<< "$filter_rules
anchor \"${filter_anchor}\""
fi

#nat_rules=$(sudo pfctl -sn)
#if ! grep -q "nat-anchor \"${nat_anchor}\"" <<< "$nat_rules"; then
#  sudo pfctl -Nf - <<< "$nat_rules
#nat-anchor \"${nat_anchor}\""
#fi

sudo pfctl -a "${filter_anchor}" -F all
sudo pfctl -a "${nat_anchor}" -F all

sudo pfctl -a "${filter_anchor}" -f - <<EOF
scrub all fragment reassemble
pass quick on lo0 all flags any keep state
pass out quick on ${device} inet proto udp from any port = 68 to 255.255.255.255 port = 67 no state
pass in quick on ${device} inet proto udp from any port = 67 to any port = 68 no state
pass out quick route-to (${device} ${gateway}) inet all user ${uid} keep state
pass out quick on ${device} proto udp from any port 67:68 to any port 67:68 keep state
pass quick on ${utun_device} all user != ${uid}
block drop out on ${device} all
EOF

#sudo pfctl -a "${nat_anchor}" -Nf - <<EOF
#nat on ${utun_device} inet from ${wg_nat_cidr} -> (${device})
#EOF

sudo pfctl -a "${filter_anchor}" -sr -v
#sudo pfctl -a "${nat_anchor}" -sn -v
