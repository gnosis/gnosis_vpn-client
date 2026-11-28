#!/bin/sh
TMPDIR=/tmp/gnosisvpn/tmp; TMP=$TMPDIR; TEMP=$TMPDIR; export TMPDIR TMP TEMP
mkdir -p "$TMPDIR" && chmod 1777 "$TMPDIR"
# Keeps the worker's keepalive session open under procd. The root daemon spawns
# the actual worker; this only needs to hold the session. If a session is
# already open it waits quietly instead of exiting, so procd does not respawn
# in a tight loop.
CTL=/usr/bin/gnosis-ctl

i=0
while [ $i -lt 30 ]; do
	"$CTL" status >/dev/null 2>&1 && break
	sleep 2; i=$((i+1))
done

out=$("$CTL" start-client "${1:-5256000m}" 2>&1)
case "$out" in
	*"already running"*)
		sleep 3600
		exit 0 ;;
esac
echo "$out"
# start-client blocks for the keepalive window when it owns the session.
case "$out" in
	*Started*) sleep 3600 ;;
esac
exit 0
