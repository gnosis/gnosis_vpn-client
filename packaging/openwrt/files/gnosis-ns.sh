#!/bin/sh
# Gnosis VPN as a LAN-only egress.
#
# The client insists on owning the default route and installs its own
# default-drop killswitch, so left alone it takes the router down with it. It
# therefore runs inside its own network namespace: its routes, its killswitch
# and its DNS rewrites stay in there, and only LAN traffic is steered in via
# table 100 and a veth pair. The router's own path (tailscale, AWG, DNS) is
# never touched.
#
# The daemon is owned by procd (/etc/init.d/gnosisvpn + /usr/bin/gnosis-ns-run.sh):
# a daemon started from a shell dies with the session that opened it.
#
# usage: gnosis-ns.sh {ensure|up [--no-lan]|down|recover|lan on|lan off|status|probe|watch|unwatch}

NS=gvns
VH=veth-gvh
VG=veth-gv
NHOST=10.200.0.1
NNS=10.200.0.2
LANDEV=br-lan
TABLE=100
PRIO=100
B=/tmp/gnosisvpn/bin
CONF=/tmp/gnosisvpn/config.toml
SOCK=/run/gnosisvpn/gnosisvpn.sock
EXITF=/etc/vpn/gnosis-exit
CTL=/usr/bin/gnosis-ctl
TUN=wg0_gnosisvpn
# State lives in a root-only directory: /tmp is world-writable, so a process
# running as the unprivileged worker could pre-create these paths (or symlink
# them) and redirect root's writes into a file it still owns.
NSDIR=/run/gnosis-ns
LOG=$NSDIR/ns.log
DIRECTF=$NSDIR/direct-ip
PIDF=$NSDIR/watch.pid
STATED=$NSDIR
mkdir -p "$NSDIR" 2>/dev/null && chmod 700 "$NSDIR"

log() { logger -t gnosis-ns "$*"; echo "$(date +%H:%M:%S) $*" >> "$LOG"; }

# The LAN prefix comes from the device, not from this file: a router whose
# subnet is not the one the script was written on would silently steer nothing.
lan_subnet() { ip route show dev "$LANDEV" scope link 2>/dev/null | awk '{print $1; exit}'; }

ensure_ns() {
	ip netns list 2>/dev/null | grep -qw "$NS" || ip netns add "$NS" || return 1
	if ! ip link show "$VH" >/dev/null 2>&1; then
		ip link add "$VH" type veth peer name "$VG" || return 1
		ip link set "$VG" netns "$NS" || return 1
	fi
	ip addr replace "$NHOST/24" dev "$VH" 2>/dev/null
	ip link set "$VH" up
	ip netns exec "$NS" ip addr replace "$NNS/24" dev "$VG" 2>/dev/null
	ip netns exec "$NS" ip link set "$VG" up 2>/dev/null
	ip netns exec "$NS" ip link set lo up 2>/dev/null
	ip netns exec "$NS" ip route replace default via "$NHOST" 2>/dev/null

	mkdir -p "/etc/netns/$NS"
	# Derive the resolver from the LAN device itself: a hardcoded address breaks
	# on any router whose subnet is not the one in the script.
	dnsip=$(ip -4 addr show dev "$LANDEV" 2>/dev/null | awk '/inet /{sub(/\/.*/,"",$2); print $2; exit}')
	[ -n "$dnsip" ] || dnsip=1.1.1.1
	grep -q "^nameserver $dnsip" "/etc/netns/$NS/resolv.conf" 2>/dev/null ||
		printf 'nameserver %s\nnameserver 1.1.1.1\n' "$dnsip" > "/etc/netns/$NS/resolv.conf"

	# Namespace uplink: NAT on the host, and let fw4 pass the veth leg. A
	# separate table's accept does not override fw4's policy drop.
	nft list table inet gvnat >/dev/null 2>&1 || nft add table inet gvnat
	nft list chain inet gvnat post >/dev/null 2>&1 || {
		nft add chain inet gvnat post '{ type nat hook postrouting priority srcnat; policy accept; }'
		# NAT the namespace leg on the WAN side. Naming the uplink device here
	# breaks on any router whose WAN is not called phy1-sta0, so exclude the
	# two legs we own instead of naming the one we do not.
	nft add rule inet gvnat post ip saddr 10.200.0.0/24 oifname != { "$VH", "$LANDEV" } masquerade
	}
	# fw4 owns inet fw4 and rebuilds it from scratch on every reload, so the
	# accepts are shipped as a drop-in (nftables.d/chain-pre/forward) and this
	# insert only covers the window before the next reload. A WAN reconnect,
	# PPPoE redial or DHCP renew reloads fw4, and without the drop-in the LAN
	# would be black-holed by the forward policy while the watchdog still
	# probes fine from inside the namespace.
	nft list ruleset | grep -q "iifname \"$VH\" accept" ||
		nft insert rule inet fw4 forward iifname "$VH" accept
	nft list ruleset | grep -q "oifname \"$VH\" accept" ||
		nft insert rule inet fw4 forward oifname "$VH" accept
	return 0
}

daemon_running() { ps w 2>/dev/null | grep -q '[g]nosis_vpn-root'; }
daemon_ready() { "$CTL" status 2>/dev/null | grep -q '^Ready'; }

start_daemon() {
	ensure_ns || { log "namespace setup failed"; return 1; }
	# procd owns the daemon: it survives sessions, reboots and crashes.
	daemon_running || /etc/init.d/gnosisvpn start >/dev/null 2>&1
	i=0
	while [ $i -lt 30 ]; do
		daemon_running && "$CTL" status >/dev/null 2>&1 && return 0
		sleep 1; i=$((i+1))
	done
	log "daemon did not answer on $SOCK"
	return 1
}

restart_daemon() {
	# A restart keeps the same worker state, and root does not respawn a worker
	# it saw exit, so a plain restart can come back Ready-less forever. Fall
	# through to a full stop/start, which forces a fresh worker process.
	# Tell stop_service this is an internal bounce, not an admin stopping the
	# service: otherwise stop tears down the steering and kills the watchdog
	# that is asking for the restart.
	: > "$STATED/internal-restart"
	/etc/init.d/gnosisvpn restart >/dev/null 2>&1
	i=0
	while [ $i -lt 25 ]; do
		daemon_ready && return 0
		sleep 2; i=$((i+1))
	done
	log "restart left the daemon not ready, bouncing the service"
	/etc/init.d/gnosisvpn stop >/dev/null 2>&1
	sleep 3
	/etc/init.d/gnosisvpn start >/dev/null 2>&1
	i=0
	while [ $i -lt 30 ]; do
		daemon_ready && return 0
		sleep 2; i=$((i+1))
	done
	return 1
}

arm_worker() {
	daemon_ready && return 0
	restart_daemon
}

wait_ready() {
	i=0
	while [ $i -lt 40 ]; do
		daemon_ready && return 0
		sleep 2; i=$((i+1))
	done
	return 1
}

tun_up() { ip netns exec "$NS" ip link show "$TUN" >/dev/null 2>&1; }

probe() {
	# Answer only when the reply really came from an exit node. With the TUN
	# down the namespace has no route of its own and falls through to the host,
	# so curl still succeeds and returns the router's own WAN address: counting
	# that as a proof would steer the LAN into a plaintext leak. Two ways that
	# check fails open, both closed here: an error page or proxy notice is not
	# an address, and an unknown router address must not be compared at all.
	c=$(ip netns exec "$NS" curl -4 -s -m 35 https://api.ipify.org 2>/dev/null)
	echo "$c" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$' || return 0
	d=$(direct_ip)
	[ -n "$d" ] || return 0
	[ "$c" = "$d" ] && return 0
	echo "$c"
}

direct_ip() {
	# Cached briefly: a PPPoE re-dial changes the address, and a stale value
	# would make a genuine exit look like a leak (or hide one).
	if [ -s "$DIRECTF" ] && [ -z "$(find "$DIRECTF" -mmin +10 2>/dev/null)" ]; then
		cat "$DIRECTF"; return 0
	fi
	d=$(curl -4 -s -m 10 https://api.ipify.org 2>/dev/null)
	echo "$d" | grep -Eq '^([0-9]{1,3}\.){3}[0-9]{1,3}$' || d=""
	[ -n "$d" ] && echo "$d" > "$DIRECTF"
	echo "$d"
}

lan_on() {
	sub=$(lan_subnet)
	[ -n "$sub" ] || { log "no LAN subnet on $LANDEV, refusing to steer"; return 1; }
	# Rebuild the namespace NAT table rather than skip-if-present: a table left
	# over from an older LAN prefix would masquerade the wrong source.
	ip netns exec "$NS" nft delete table inet gvlan 2>/dev/null
	ip netns exec "$NS" nft add table inet gvlan
	ip netns exec "$NS" nft add chain inet gvlan post '{ type nat hook postrouting priority srcnat; policy accept; }'
	ip netns exec "$NS" nft add rule inet gvlan post ip saddr "$sub" oifname "$TUN" masquerade
	# Whatever held table 100 before (awg, mullvad, tailscale) must go, or the
	# rule looks installed while the LAN still leaves on the old path.
	ip route flush table "$TABLE" 2>/dev/null
	ip route replace default via "$NNS" dev "$VH" table "$TABLE"
	ip route replace "$sub" dev "$LANDEV" table "$TABLE"
	ip rule show | grep -q "iif $LANDEV lookup $TABLE" ||
		ip rule add iif "$LANDEV" lookup "$TABLE" priority "$PRIO"
}

lan_off() {
	while ip rule show 2>/dev/null | grep -q "iif $LANDEV lookup $TABLE"; do
		ip rule del iif "$LANDEV" lookup "$TABLE" 2>/dev/null || break
	done
	ip route flush table "$TABLE" 2>/dev/null
}

# The rule alone proves nothing: it may be left over from another path, or the
# route may still point somewhere else. Steering means table 100 sends the LAN
# into the veth.
lan_steered() {
	ip route show table "$TABLE" 2>/dev/null | grep -q "via $NNS dev $VH"
}

gnosis_exit() {
	x=$(cat "$EXITF" 2>/dev/null)
	[ -n "$x" ] || x=gvpn-ger-1-j9os
	echo "$x"
}

up() {
	x=$(gnosis_exit)
	start_daemon || return 1

	# A dead worker leaves the daemon answering status but refusing to connect,
	# and root will not respawn a worker it saw die: bounce the service once.
	if ! daemon_ready; then
		log "daemon not Ready, bouncing"
		restart_daemon || { log "daemon never reached Ready"; return 1; }
	fi

	"$CTL" connect "$x" >/dev/null 2>&1
	i=0
	while [ $i -lt 25 ]; do
		tun_up && break
		sleep 2; i=$((i+1))
	done
	if ! tun_up; then
		# The first connect is often refused while the worker finishes warming up.
		"$CTL" connect "$x" >/dev/null 2>&1
		i=0
		while [ $i -lt 20 ]; do
			tun_up && break
			sleep 2; i=$((i+1))
		done
	fi
	tun_up || { log "no $TUN after 90s"; "$CTL" disconnect >/dev/null 2>&1; return 1; }

	# A mixnet path warms up slowly, and a single answer can come from a circuit
	# that dies a second later, so the LAN only moves after two clean proofs
	# spaced out: the steering is worth nothing if it flaps straight back.
	c=""; i=0
	while [ $i -lt 8 ]; do
		c=$(probe)
		[ -n "$c" ] && break
		sleep 10; i=$((i+1))
	done
	if [ -z "$c" ]; then
		log "no exit outside $(direct_ip) after 80s, LAN left alone"
		"$CTL" disconnect >/dev/null 2>&1
		return 1
	fi
	sleep 20
	c2=$(probe)
	if [ -z "$c2" ]; then
		log "exit answered once then stopped, LAN left alone"
		"$CTL" disconnect >/dev/null 2>&1
		return 1
	fi
	log "mixnet up via $x, exit $c2 (2/2 proofs)"

	# A plain `up` should leave the LAN working, not just a tunnel nobody uses.
	# lan_on is idempotent, so a mode switch calling it again is harmless.
	if [ "$1" = "--no-lan" ]; then
		log "tunnel up, LAN left on the direct path (--no-lan)"
		return 0
	fi
	lan_on || { log "steering failed, LAN left alone"; "$CTL" disconnect >/dev/null 2>&1; return 1; }
	log "LAN steered into the mixnet"
	start_watch
	return 0
}

# One watchdog, owned by the steering it guards.
start_watch() {
	[ -x /usr/bin/gnosis-ns-watch.sh ] || return 0
	pidof_check=$(cat "$PIDF" 2>/dev/null)
	[ -n "$pidof_check" ] && kill -0 "$pidof_check" 2>/dev/null && return 0
	setsid /usr/bin/gnosis-ns-watch.sh </dev/null >/dev/null 2>&1 &
	log "watchdog started"
}

stop_watch() {
	wp=$(cat "$PIDF" 2>/dev/null)
	[ -n "$wp" ] && kill "$wp" 2>/dev/null
	rm -f "$PIDF"
}

down() {
	stop_watch
	lan_off
	"$CTL" disconnect >/dev/null 2>&1
	log "disconnected, LAN back on the direct path"
}

# Used by the watchdog: bring the tunnel back without touching the LAN.
recover() {
	x=$(gnosis_exit)
	daemon_ready || restart_daemon || return 1
	if ! tun_up; then
		"$CTL" connect "$x" >/dev/null 2>&1
		i=0
		while [ $i -lt 25 ]; do
			tun_up && break
			sleep 2; i=$((i+1))
		done
	fi
	tun_up || return 1
	i=0
	while [ $i -lt 6 ]; do
		c=$(probe)
		[ -n "$c" ] && { log "recovered, exit $c"; return 0; }
		sleep 10; i=$((i+1))
	done
	return 1
}

case "$1" in
	ensure) ensure_ns ;;
	up) shift; up "$@" ;;
	watch) start_watch ;;
	unwatch) stop_watch ;;
	down) down ;;
	recover) recover ;;
	lan)
		case "$2" in
			on) lan_on ;;
			off) lan_off ;;
			*) echo "usage: gnosis-ns.sh lan {on|off}" >&2; exit 1 ;;
		esac ;;
	probe) probe ;;
	status)
		echo "daemon=$(daemon_running && echo running || echo stopped)"
		echo "ready=$(daemon_ready && echo yes || echo no)"
		echo "tun=$(tun_up && echo up || echo down)"
		echo "lan=$(lan_steered && echo on || echo off)"
		echo "exit_ip=$(probe)"
		echo "status=$("$CTL" status 2>/dev/null | head -1)"
		;;
	*) echo "usage: gnosis-ns.sh {ensure|up|down|recover|lan on|lan off|status|probe}" >&2; exit 1 ;;
esac
