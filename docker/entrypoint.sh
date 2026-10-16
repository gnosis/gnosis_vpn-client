#!/bin/sh
set -eu

worker_home=/var/lib/gnosisvpn
legacy_id="${worker_home}/.config/gnosisvpn-hopr.id"
legacy_pass="${worker_home}/.config/gnosisvpn-hopr.pass"
legacy_pix="${worker_home}/curvy-pix.redb"
has_legacy_identity=false
if [ -f "${legacy_id}" ] && [ -f "${legacy_pass}" ]; then
    has_legacy_identity=true
fi

# Keep an injected or pre-GNO-958 (possibly funded) identity; root only learns the network from blokli, so it is named here
if [ -n "${GNOSISVPN_HOPR_IDENTITY_FILE:-}" ] || [ "${has_legacy_identity}" = true ]; then
    : "${GNOSISVPN_NETWORK:?GNOSISVPN_NETWORK must name the blokli network to place the identity}"
    network_dir="${worker_home}/.local/share/gnosisvpn/networks/${GNOSISVPN_NETWORK}"
    mkdir -p "${network_dir}/pix"
    if [ ! -f "${network_dir}/hopr.id" ]; then
        if [ -n "${GNOSISVPN_HOPR_IDENTITY_FILE:-}" ]; then
            echo "using identity ${GNOSISVPN_HOPR_IDENTITY_FILE}"
            cp "${GNOSISVPN_HOPR_IDENTITY_FILE}" "${network_dir}/hopr.id"
            printf %s "${GNOSISVPN_HOPR_IDENTITY_PASS:?GNOSISVPN_HOPR_IDENTITY_PASS must be set with the identity file}" >"${network_dir}/hopr.pass"
        else
            echo "using legacy identity ${legacy_id}"
            mv "${legacy_id}" "${network_dir}/hopr.id"
            mv "${legacy_pass}" "${network_dir}/hopr.pass"
        fi
    fi
    # The PIX pool tracks pending deposits of that identity
    if [ -f "${legacy_pix}" ] && [ ! -f "${network_dir}/pix/curvy-pix.redb" ]; then
        mv "${legacy_pix}" "${network_dir}/pix/curvy-pix.redb"
    fi
    chmod 0600 "${network_dir}/hopr.pass"
    chmod -R go-rwx "${worker_home}/.local"
fi
chown -R gnosisvpn:gnosisvpn "${worker_home}"

# The worker runs as whoever starts it: `docker exec -u gnosisvpn gnosis_vpn-client gnosis_vpn-ctl start-client 10m`
exec ./gnosis_vpn-root --worker-binary /app/gnosis_vpn-worker "$@"
