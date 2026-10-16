#!/bin/sh
set -eu

state_home="${GNOSISVPN_HOME:-/var/lib/gnosisvpn}"
mkdir -p "${state_home}"
chown -R gnosisvpn:gnosisvpn "${state_home}"

# Keep a pre-GNO-958 volume's (possibly funded) identity; the per-network dir is unknown until root asks blokli
legacy_id="${state_home}/.config/gnosisvpn-hopr.id"
legacy_pass="${state_home}/.config/gnosisvpn-hopr.pass"
if [ -z "${GNOSISVPN_HOPR_IDENTITY_FILE:-}" ] && [ -f "${legacy_id}" ] && [ -f "${legacy_pass}" ]; then
    echo "using legacy identity ${legacy_id}"
    GNOSISVPN_HOPR_IDENTITY_PASS="$(cat "${legacy_pass}")"
    export GNOSISVPN_HOPR_IDENTITY_FILE="${legacy_id}" GNOSISVPN_HOPR_IDENTITY_PASS
fi

# Pinned worker user: there is one user in the container, so no ownership gate
exec ./gnosis_vpn-root --worker-user gnosisvpn --worker-binary /app/gnosis_vpn-worker "$@"
