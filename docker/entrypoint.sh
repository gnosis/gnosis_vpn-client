#!/bin/sh
set -eu

# A mounted identity arrives with the host's ownership; the worker runs as gnosisvpn
chown -R gnosisvpn:gnosisvpn /var/lib/gnosisvpn

# The worker runs as whoever starts it: `docker exec -u gnosisvpn gnosis_vpn-client gnosis_vpn-ctl start-client 10m`
exec ./gnosis_vpn-root --worker-binary /app/gnosis_vpn-worker "$@"
