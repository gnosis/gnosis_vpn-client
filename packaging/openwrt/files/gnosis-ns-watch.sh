#!/bin/sh
# Keeps the Gnosis LAN path honest.
#
# The HOPR mixnet drops its whole peer swarm from time to time (mobile/CGNAT
# uplinks churn 190+ QUIC sessions at once), and when that happens the worker
# process is killed and the root daemon refuses to restart it: "worker process
# exited unexpectedly - not restarting". The tunnel is then gone without an
# error, and LAN clients sit in a black hole behind the kill switch. So this
# watch loop does two things the daemon will not:
#   1. hard failure (no tun, or not Ready) -> bounce the service and reconnect
#   2. soft failure (tun up, no exit traffic) -> revert after 4 strikes
# Both revert the LAN to the direct path instead of leaving it dark.
#
# It only runs while the LAN is actually steered into the namespace, and it
# stops on its own when the steering is removed, so nothing here has to know
# about any other VPN mode on the box.
CTL=/usr/bin/gnosis-ctl
NS=/usr/bin/gnosis-ns.sh
TUN=wg0_gnosisvpn
TABLE=100
NNS=10.200.0.2
VH=veth-gvh
LOG=/tmp/gnosis-watch.log

log() { logger -t gnosis-watch "$*"; echo "$(date +%H:%M:%S) $*" >> "$LOG"; }

# Steering means table 100 sends the LAN into the veth; a leftover rule alone
# does not count.
steered() { ip route show table "$TABLE" 2>/dev/null | grep -q "via $NNS dev $VH"; }
tun_up() { ip netns exec gvns ip link show "$TUN" >/dev/null 2>&1; }
ready() { "$CTL" status 2>/dev/null | grep -q '^Ready'; }

recover() {
	x=$(cat /etc/vpn/gnosis-exit 2>/dev/null); [ -n "$x" ] || x=gvpn-ger-1-j9os
	log "recovering: restarting the daemon and reconnecting"
	/etc/init.d/gnosisvpn restart >/dev/null 2>&1
	i=0
	while [ $i -lt 30 ]; do
		ready && break
		sleep 2; i=$((i+1))
	done
	ready || return 1
	"$CTL" connect "$x" >/dev/null 2>&1
	i=0
	while [ $i -lt 20 ]; do
		tun_up && break
		sleep 3; i=$((i+1))
	done
	tun_up || return 1
	# Nothing to steer until the mixnet actually carries a byte.
	i=0
	while [ $i -lt 4 ]; do
		c=$("$NS" probe)
		[ -n "$c" ] && { log "recovered, exit $c"; return 0; }
		sleep 10; i=$((i+1))
	done
	return 1
}

# One watcher only: two would fight over the tunnel and the kill switch.
PIDF=/tmp/gnosis-watch.pid
if [ -f "$PIDF" ] && kill -0 "$(cat "$PIDF" 2>/dev/null)" 2>/dev/null; then
	exit 0
fi
echo $$ > "$PIDF"
trap 'rm -f "$PIDF"' EXIT

hard=0
soft=0
log "watching"
[ -x /usr/bin/gnosis-ns.sh ] && "$NS" recover >/dev/null 2>&1

while :; do
	sleep 30
	steered || { log "LAN no longer steered, watchdog exits"; exit 0; }

	if ! tun_up || ! ready; then
		hard=$((hard+1))
		soft=0
		log "hard failure (tun=$(tun_up && echo up || echo down) ready=$(ready && echo yes || echo no))"
		if [ $hard -ge 1 ]; then
			# People are working right now. Give the LAN a working path back
			# first, then try to reclaim the mixnet.
			"$NS" lan off
			log "LAN restored on the direct path while recovering"
			if recover; then
				"$NS" lan on
				hard=0
				log "LAN back on the mixnet"
				continue
			fi
			log "recovery failed, LAN left on the direct path"
			"$CTL" disconnect >/dev/null 2>&1
			exit 0
		fi
		continue
	fi

	hard=0
	c=$("$NS" probe)
	if [ -n "$c" ]; then
		[ $soft -gt 0 ] && log "traffic back ($c)"
		soft=0
	else
		soft=$((soft+1))
		log "no exit traffic $soft/4 (tun up, daemon Ready)"
		if [ $soft -ge 4 ]; then
			log "mixnet stopped carrying traffic, LAN back on the direct path"
			"$NS" lan off
			"$CTL" disconnect >/dev/null 2>&1
			exit 0
		fi
	fi
done