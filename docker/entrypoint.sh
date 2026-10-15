#!/bin/sh
set -eu

state_home="${GNOSISVPN_HOME:-/var/lib/gnosisvpn}"
mkdir -p "${state_home}"
chown -R gnosisvpn:gnosisvpn "${state_home}"

# Pinned worker user: there is one user in the container, so no ownership gate
exec ./gnosis_vpn-root --worker-user gnosisvpn --worker-binary /app/gnosis_vpn-worker "$@"
