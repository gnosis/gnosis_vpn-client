#!/bin/sh
set -eu

state_home="${GNOSISVPN_HOME:-/var/lib/gnosisvpn}"
mkdir -p "${state_home}"
chown -R gnosisvpn:gnosisvpn "${state_home}"

exec ./gnosis_vpn-root --worker-binary /app/gnosis_vpn-worker --allow-insecure --allow-experimental "$@"
