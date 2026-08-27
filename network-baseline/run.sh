#!/usr/bin/env bash
# Baseline network test orchestrator: for each exit, connect the Gnosis VPN,
# wait until the tunnel is up, drive the browser harness, then disconnect.
# Usage: ./run.sh [--destination EXIT ...]   (default: USA UK_1 India)
set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
CTL="$REPO/target/release/gnosis_vpn-ctl"
# Provided by `nix develop .#headless`; $HERE/bin is the pre-nix fallback.
OBSCURA="${OBSCURA:-$(command -v obscura || echo "$HERE/bin/obscura")}"
CDP="ws://127.0.0.1:9222/devtools/browser"

usage() { echo "Usage: $0 [--destination EXIT ...]   (default: USA UK_1 India)"; }

EXITS=()
while [ $# -gt 0 ]; do
    case "$1" in
    -d | --destination)
        shift
        while [ $# -gt 0 ] && [ "${1#-}" = "$1" ]; do
            EXITS+=("$1")
            shift
        done
        ;;
    -h | --help)
        usage
        exit 0
        ;;
    *)
        echo "unknown argument: $1"
        usage
        exit 2
        ;;
    esac
done
[ ${#EXITS[@]} -eq 0 ] && EXITS=(USA UK_1 India)

expected_cc() { case "$1" in USA) echo US ;; UK_*) echo GB ;; India) echo IN ;; *) echo "" ;; esac }

# Drawn once per run so all exits are compared on the same video.
YT_VIDEO_INDEX="${YT_VIDEO_INDEX:-$RANDOM}"

TS="$(date -u +%Y%m%dT%H%M%SZ)"
OUTDIR="$HERE/results/$TS"
mkdir -p "$OUTDIR"
echo "== baseline run $TS -> $OUTDIR =="

# ensure obscura CDP server is up (start if port not answering)
STARTED_OBSCURA=""
if ! curl -s --max-time 3 http://127.0.0.1:9222/json/version >/dev/null 2>&1; then
    echo "== starting obscura serve =="
    "$OBSCURA" serve --port 9222 --stealth >"$OUTDIR/obscura-serve.log" 2>&1 &
    STARTED_OBSCURA=$!
    for i in $(seq 1 20); do
        curl -s --max-time 2 http://127.0.0.1:9222/json/version >/dev/null 2>&1 && break
        sleep 0.5
    done
fi

status_json() { "$CTL" -o json status 2>/dev/null; }
is_connected() { status_json | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{try{process.exit(JSON.parse(s).Status.connected?0:1)}catch{process.exit(1)}})"; }
is_disconnected() { status_json | node -e "let s='';process.stdin.on('data',d=>s+=d).on('end',()=>{try{const j=JSON.parse(s).Status;const disc=Array.isArray(j.disconnecting)?j.disconnecting.length>0:!!j.disconnecting;process.exit((!j.connected&&!j.connecting&&!disc)?0:1)}catch{process.exit(1)}})"; }

wait_for() { # wait_for <fn> <timeout_s> <label>
    local fn="$1" to="$2" label="$3" i=0
    while [ "$i" -lt "$to" ]; do
        "$fn" && return 0
        sleep 3
        i=$((i + 3))
    done
    echo "!! timeout waiting for $label (${to}s)"
    return 1
}

# disconnected-baseline egress snapshot (labels the whole run)
BASELINE_TRACE="$(curl -s --max-time 8 https://cloudflare.com/cdn-cgi/trace 2>/dev/null | tr '\n' ';')"

cat >"$OUTDIR/meta.json" <<EOF
{
  "run_ts": "$TS",
  "exits": "$(
    IFS=,
    echo "${EXITS[*]}"
)",
  "git_commit": "$(cd "$REPO" && git rev-parse HEAD 2>/dev/null)",
  "git_status_dirty": $(cd "$REPO" && [ -n "$(git status --porcelain 2>/dev/null)" ] && echo true || echo false),
  "machine": "$(uname -sm) / $(sysctl -n machdep.cpu.brand_string 2>/dev/null)",
  "node": "$(node -v)",
  "obscura_version": "$($OBSCURA --version 2>/dev/null)",
  "disconnected_baseline_trace": "$BASELINE_TRACE",
  "notes": "download capped ~2Mbps by rotsee.toml SURB balancing; headless YouTube/CNN measure page+player load not DRM video decode; speedtest is in-page fetch against speed.cloudflare.com __down/__up"
}
EOF
echo "== wrote meta.json (baseline egress: $BASELINE_TRACE) =="

for EXIT in "${EXITS[@]}"; do
    CC="$(expected_cc "$EXIT")"
    echo ""
    echo "======================================================================"
    echo "== EXIT $EXIT (expect $CC)  $(date -u +%H:%M:%SZ)"
    echo "======================================================================"

    # ensure clean slate
    if ! is_disconnected; then
        "$CTL" disconnect >/dev/null 2>&1
        wait_for is_disconnected 60 "disconnect(pre)"
    fi

    echo "-- connect $EXIT"
    "$CTL" connect "$EXIT"
    if ! wait_for is_connected 150 "connect $EXIT"; then
        echo "{\"exit\":\"$EXIT\",\"expected_cc\":\"$CC\",\"fatal_error\":\"connect timeout\"}" >"$OUTDIR/$EXIT.json"
        "$CTL" disconnect >/dev/null 2>&1
        wait_for is_disconnected 60 "disconnect"
        continue
    fi
    echo "-- connected; waiting for data path to egress via $CC (guard against local leak)"
    DP_OK=""
    LASTLOC=""
    for i in $( # up to ~100s; tunnel needs time to actually route (esp. far exits)
        seq 1 20
    ); do
        LASTLOC="$(curl -s --max-time 20 https://cloudflare.com/cdn-cgi/trace 2>/dev/null | sed -n 's/^loc=//p')"
        if [ "$LASTLOC" = "$CC" ]; then
            echo "-- data path up, egress=$CC (after ${i}x)"
            DP_OK=1
            break
        fi
        sleep 3
    done
    if [ -z "$DP_OK" ]; then
        echo "!! egress never reached $CC (last loc=${LASTLOC:-none}) — tunnel not routing / leaking to local. SKIP $EXIT."
        echo "{\"exit\":\"$EXIT\",\"expected_cc\":\"$CC\",\"fatal_error\":\"egress not via exit (last loc=${LASTLOC:-none}); connected but not routing\"}" >"$OUTDIR/$EXIT.json"
        "$CTL" disconnect >/dev/null 2>&1
        wait_for is_disconnected 60 "disconnect"
        sleep 5
        continue
    fi
    "$CTL" -o json nerd-stats >"$OUTDIR/$EXIT.nerdstats.json" 2>/dev/null || true
    sleep 2

    EXIT="$EXIT" OUTDIR="$OUTDIR" EXPECTED_CC="$CC" CDP="$CDP" \
        YT_VIDEO_INDEX="$YT_VIDEO_INDEX" node "$HERE/harness.mjs"

    echo "-- disconnect $EXIT"
    "$CTL" disconnect >/dev/null 2>&1
    wait_for is_disconnected 60 "disconnect $EXIT"
    sleep 5 # cooldown
done

echo ""
echo "== aggregating =="
OUTDIR="$OUTDIR" EXIT_ORDER="$(
    IFS=,
    echo "${EXITS[*]}"
)" node "$HERE/aggregate.mjs"

if [ -n "$STARTED_OBSCURA" ]; then
    echo "== stopping obscura serve ($STARTED_OBSCURA) =="
    kill "$STARTED_OBSCURA" 2>/dev/null
fi
echo "== done -> $OUTDIR =="
