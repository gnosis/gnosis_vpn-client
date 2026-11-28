#!/bin/sh
# procd entry point: run the Gnosis root daemon inside its own network
# namespace. procd owns the process, so it survives terminal/session teardown
# and restarts on crash; the namespace keeps its routes and default-drop
# killswitch away from the router's own path (tailscale, AWG).
NS=gvns
B=/tmp/gnosisvpn/bin
CONF=/tmp/gnosisvpn/config.toml
SOCK=/run/gnosisvpn/gnosisvpn.sock
# The client writes multi-megabyte scratch files and does not remove them when
# it is killed, so they pile up in /tmp (tmpfs) until its own writes fail with
# ENOSPC. Point TMPDIR at a directory the init script wipes on every start.
TMPD=/tmp/gnosisvpn/tmp
mkdir -p "$TMPD" && chmod 1777 "$TMPD"

[ -x "$B/gnosis_vpn-root" ] || { echo "gnosis_vpn-root missing (payload not expanded?)" >&2; exit 1; }

exec /sbin/ip netns exec "$NS" env \
	GNOSISVPN_CONFIG_PATH="$CONF" \
	GNOSISVPN_WORKER_BINARY="$B/gnosis_vpn-worker" \
	GNOSISVPN_SOCKET_PATH="$SOCK" \
	GNOSISVPN_FORCE_STATIC_ROUTING=true \
	TMPDIR="$TMPD" \
	TMP="$TMPD" \
	TEMP="$TMPD" \
	RUST_LOG="${RUST_LOG:-info}" \
	"$B/gnosis_vpn-root" -c "$CONF" \
		--worker-binary "$B/gnosis_vpn-worker" \
		--hopr-blokli-url "${GNOSIS_BLOKLI_URL:-https://blokli.piz-palu.gnosisvpn.io}" \
		--socket-path "$SOCK"