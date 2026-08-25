#!/usr/bin/env bash
# vpn-drain-tour.sh - each round, connect to every configured destination once (best exit-capacity/latency first, recording why any destination can't connect), smoke-test it, and repeat rounds until `gnosis_vpn-ctl balance` reports funding as Empty or nothing connects; writes raw runs.jsonl/metrics.csv only (run vpn-drain-report.sh against the output dir for the comparison report).
# Required commands: bash, gnosis_vpn-ctl, jq, curl, ping, awk, uname. Run `./vpn-drain-tour.sh --help` for options.
# Fetch this alongside vpn-smoke-test.sh/vpn-drain-report.sh: see scripts/README.md.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Sourced (main() only runs when executed directly) for its record/section/info/detect_os/ping_argv/parse_rtt_avg/parse_loss/http_metrics helpers and *_TIMEOUT/SIZES/TARGETS defaults; its PASS/WARN/FAIL/SKIP tallies are reused per-connection in collect_connection_metrics rather than for a whole-run summary.
# shellcheck source=vpn-smoke-test.sh
# shellcheck disable=SC1091
source "$SCRIPT_DIR/vpn-smoke-test.sh"

# ---------------------------------------------------------------------------
# Defaults (all overridable via flags / environment)
# ---------------------------------------------------------------------------

CTL_BIN="${GVPN_CTL:-gnosis_vpn-ctl}"
SOCKET_PATH=""
OUT_DIR=""
MAX_ROUNDS=50
CONNECT_TIMEOUT=60
POLL_INTERVAL=2
INSTALL_DEPS=0

readonly DISCONNECT_SETTLE=2 # brief pause after disconnect before the next connect

RUNS_JSONL=""
METRICS_CSV=""
STOP_REASON_FILE=""

# Set by collect_connection_metrics / run_round for the caller to pick up.
LAST_METRICS_JSON="{}"
ROUND_SUCCESSES=0

# ---------------------------------------------------------------------------
# gnosis_vpn-ctl wrappers
# ---------------------------------------------------------------------------

ctl() {
    if [ -n "$SOCKET_PATH" ]; then
        "$CTL_BIN" -s "$SOCKET_PATH" "$@"
    else
        "$CTL_BIN" "$@"
    fi
}

ctl_json() { ctl -o json "$@"; }

get_status_json() { ctl_json status; }
get_balance_json() { ctl_json balance; }

iso_now() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# ---------------------------------------------------------------------------
# Destination ranking
# ---------------------------------------------------------------------------

# rank_destinations <status-json> -> best-to-worst JSON array: tier 0 = ReadyToConnect/Connecting with known exit health (sorted by used-capacity fraction then latency), tier 1 = Routable (connectable, health unknown), tier 2 = not connectable now (recorded as skipped, no connect attempt).
rank_destinations() {
    jq -c '
      .Status.destinations
      | map(
          (.route_health) as $rh
          | (.destination) as $d
          | ($rh.state.state // "NoHealth") as $state
          | (if ($state == "ReadyToConnect" or $state == "Connecting") then 0
             elif ($state == "Routable") then 1
             else 2 end) as $tier
          | {
              id: $d.id,
              address: $d.address,
              state: $state,
              last_error: ($rh.last_error // null),
              available: ($rh.state.exit.health.slots.available // null),
              connected_slots: ($rh.state.exit.health.slots.connected // null),
              ping_rtt_ms: ($rh.state.exit.ping_rtt // null),
              tier: $tier,
              used_fraction: (if (($rh.state.exit.health.slots.available // 0) > 0)
                              then ($rh.state.exit.health.slots.connected / $rh.state.exit.health.slots.available)
                              else 999 end)
            }
        )
      | sort_by([.tier, .used_fraction, (.ping_rtt_ms // 999999)])
    ' <<<"$1"
}

# skip_reason <ranked-entry-json> -> human-readable reason a tier-2 destination can't connect.
skip_reason() {
    jq -r '.last_error // ("state: " + .state)' <<<"$1"
}

# ---------------------------------------------------------------------------
# Connect / wait / metrics
# ---------------------------------------------------------------------------

wait_for_connected() {
    local id="$1" timeout="$2" waited=0 status_json cid
    while [ "$waited" -lt "$timeout" ]; do
        status_json="$(get_status_json)" || true
        cid="$(jq -r '.Status.connected.destination_id // empty' <<<"$status_json" 2>/dev/null || true)"
        [ "$cid" = "$id" ] && return 0
        sleep "$POLL_INTERVAL"
        waited=$((waited + POLL_INTERVAL))
    done
    return 1
}

# collect_connection_metrics -> sets LAST_METRICS_JSON from gateway ping/loss, HTTPS reachability, sized downloads, a short stream, and egress IP/geo (vpn-smoke-test.sh's probes reimplemented for raw numeric values instead of PASS/WARN/FAIL text); MTU/IPv6-leak checks are skipped since they don't feed capacity/latency comparison and would slow a tour repeating this per destination per round.
collect_connection_metrics() {
    # Read by record() in the sourced vpn-smoke-test.sh, so shellcheck can't see the usage.
    # shellcheck disable=SC2034
    PASS_COUNT=0 WARN_COUNT=0 FAIL_COUNT=0 SKIP_COUNT=0 GATEWAY_UP=0
    local argv out="" rc=0 ping_avg="" loss_pct=""

    argv="$(ping_argv "$OS" "$PING_COUNT_LOSS" "$PING_TIMEOUT" "$PING_TTL" "$GATEWAY")"
    read -r -a argv <<<"$argv"
    out="$("$PING_BIN" "${argv[@]}" 2>&1)" || rc=$?
    if [ "$rc" -eq 0 ]; then
        # shellcheck disable=SC2034
        GATEWAY_UP=1
        ping_avg="$(parse_rtt_avg "$out")"
        loss_pct="$(parse_loss "$out")"
    fi

    local host https_ok=0 https_total=0
    for host in $TARGETS; do
        https_total=$((https_total + 1))
        if http_metrics "https://$host" "$HTTP_TIMEOUT" --head >/dev/null 2>&1; then
            https_ok=$((https_ok + 1))
        fi
    done

    local n metrics size speed downloads_json="{}"
    for n in $SIZES; do
        if [ "$QUICK" -eq 1 ] && [ "$n" -ge 3145728 ]; then
            continue
        fi
        size="" speed=""
        metrics="$(http_metrics "${DOWN_URL}?bytes=${n}" "$DL_TIMEOUT")" || true
        read -r _ size speed _ _ <<<"$metrics"
        size="${size%%.*}"
        [ -n "$size" ] || size=0
        speed="${speed%%.*}"
        [ -n "$speed" ] || speed=0
        downloads_json="$(jq -c --arg n "$n" --arg bytes "$size" --arg speed "$speed" \
            '. + {($n): {bytes: ($bytes|tonumber), speed_bps: ($speed|tonumber)}}' <<<"$downloads_json")"
    done

    local stream_secs="$STREAM_SECS" stream_metrics stream_size="" stream_speed=""
    [ "$QUICK" -eq 1 ] && stream_secs=8
    stream_metrics="$(http_metrics "${DOWN_URL}?bytes=${STREAM_BYTES}" "$stream_secs")" || true
    read -r _ stream_size stream_speed _ _ <<<"$stream_metrics"
    stream_size="${stream_size%%.*}"
    [ -n "$stream_size" ] || stream_size=0
    stream_speed="${stream_speed%%.*}"
    [ -n "$stream_speed" ] || stream_speed=0

    local egress_body egress_ip="" egress_loc=""
    egress_body="$(http_body "$TRACE_URL" "$HTTP_TIMEOUT" 2>/dev/null)" || true
    egress_ip="$(printf '%s\n' "$egress_body" | awk -F= '/^ip=/{print $2; exit}')"
    egress_loc="$(printf '%s\n' "$egress_body" | awk -F= '/^loc=/{print $2; exit}')"

    LAST_METRICS_JSON="$(jq -n \
        --arg ping_avg "$ping_avg" \
        --arg loss_pct "$loss_pct" \
        --argjson https_ok "$https_ok" \
        --argjson https_total "$https_total" \
        --argjson downloads "$downloads_json" \
        --arg streaming_bps "$stream_speed" \
        --arg egress_ip "$egress_ip" \
        --arg egress_loc "$egress_loc" \
        '{
           gateway_ping_avg_ms: (if $ping_avg == "" then null else ($ping_avg|tonumber) end),
           packet_loss_pct: (if $loss_pct == "" then null else ($loss_pct|tonumber) end),
           https_ok: $https_ok,
           https_total: $https_total,
           downloads: $downloads,
           streaming_bps: ($streaming_bps|tonumber),
           egress_ip: (if $egress_ip == "" then null else $egress_ip end),
           egress_loc: (if $egress_loc == "" then null else $egress_loc end)
         }')"
}

# ---------------------------------------------------------------------------
# Data recording
# ---------------------------------------------------------------------------

# record_attempt <round> <id> <address> <cap-used> <cap-avail> <ping-rtt-ms> <outcome> <reason> <metrics-json> <funding-json> <started> <ended>
record_attempt() {
    local row
    row="$(jq -c -n \
        --argjson round "$1" \
        --arg id "$2" \
        --arg address "$3" \
        --arg cap_used "$4" --arg cap_avail "$5" --arg ping_sel "$6" \
        --arg outcome "$7" --arg reason "$8" \
        --argjson metrics "$9" --argjson funding "${10}" \
        --arg started "${11}" --arg ended "${12}" \
        '{
           round: $round, destination_id: $id, address: $address,
           selection: {
             capacity_used: (if $cap_used == "" then null else ($cap_used|tonumber) end),
             capacity_available: (if $cap_avail == "" then null else ($cap_avail|tonumber) end),
             ping_rtt_ms: (if $ping_sel == "" then null else ($ping_sel|tonumber) end)
           },
           outcome: $outcome, reason: (if $reason == "" then null else $reason end),
           metrics: $metrics, funding_status_snapshot: $funding,
           started_at: $started, ended_at: $ended
         }')"
    printf '%s\n' "$row" >>"$RUNS_JSONL"
    printf '%s\n' "$row" | jq -r '[
        .round, .destination_id, .outcome, (.reason // "" | gsub("\r\n|\r|\n"; " ")),
        (.selection.capacity_used // ""), (.selection.capacity_available // ""), (.selection.ping_rtt_ms // ""),
        (.metrics.gateway_ping_avg_ms // ""), (.metrics.packet_loss_pct // ""),
        (.metrics.streaming_bps // ""), (.metrics.egress_ip // ""),
        (.metrics.https_ok // ""), (.metrics.https_total // ""),
        (.funding_status_snapshot.traffic // ""), (.funding_status_snapshot.gas // ""),
        .started_at, .ended_at
    ] | @csv' >>"$METRICS_CSV"
}

# ---------------------------------------------------------------------------
# Round loop
# ---------------------------------------------------------------------------

attempt_destination() {
    local round="$1" entry="$2" funding_json="$3"
    local id address cap_avail cap_used ping_sel started ended
    id="$(jq -r '.id' <<<"$entry")"
    address="$(jq -r '.address' <<<"$entry")"
    cap_avail="$(jq -r '.available // ""' <<<"$entry")"
    cap_used="$(jq -r '.connected_slots // ""' <<<"$entry")"
    ping_sel="$(jq -r '.ping_rtt_ms // ""' <<<"$entry")"
    started="$(iso_now)"

    local connect_out connect_rc=0
    connect_out="$(ctl connect "$id" 2>&1)" || connect_rc=$?
    if [ "$connect_rc" -ne 0 ]; then
        ended="$(iso_now)"
        record_attempt "$round" "$id" "$address" "$cap_used" "$cap_avail" "$ping_sel" \
            "failed" "$connect_out" '{}' "$funding_json" "$started" "$ended"
        record FAIL "$id" "connect rejected: $connect_out"
        ctl disconnect >/dev/null 2>&1 || true
        return 1
    fi

    if ! wait_for_connected "$id" "$CONNECT_TIMEOUT"; then
        ended="$(iso_now)"
        record_attempt "$round" "$id" "$address" "$cap_used" "$cap_avail" "$ping_sel" \
            "failed" "tunnel did not come up within ${CONNECT_TIMEOUT}s" '{}' "$funding_json" "$started" "$ended"
        record FAIL "$id" "tunnel did not come up within ${CONNECT_TIMEOUT}s"
        ctl disconnect >/dev/null 2>&1 || true
        return 1
    fi

    collect_connection_metrics
    ctl disconnect >/dev/null 2>&1 || true
    sleep "$DISCONNECT_SETTLE"
    ended="$(iso_now)"
    record_attempt "$round" "$id" "$address" "$cap_used" "$cap_avail" "$ping_sel" \
        "connected" "" "$LAST_METRICS_JSON" "$funding_json" "$started" "$ended"
    record PASS "$id" "connected + tested"
    return 0
}

# run_round <round> <funding-json> -> sets ROUND_SUCCESSES (not echoed: attempt_destination prints status lines via record(), which a command substitution would swallow into the "return value" instead of the terminal).
run_round() {
    local round="$1" funding_json="$2" status_json ranked count i=0 successes=0
    status_json="$(get_status_json)"
    ranked="$(rank_destinations "$status_json")"
    count="$(jq 'length' <<<"$ranked")"

    while [ "$i" -lt "$count" ]; do
        local entry tier id address started ended reason
        entry="$(jq -c ".[$i]" <<<"$ranked")"
        i=$((i + 1))
        tier="$(jq -r '.tier' <<<"$entry")"

        if [ "$tier" -ge 2 ]; then
            id="$(jq -r '.id' <<<"$entry")"
            address="$(jq -r '.address' <<<"$entry")"
            reason="$(skip_reason "$entry")"
            started="$(iso_now)"
            ended="$started"
            record_attempt "$round" "$id" "$address" "" "" "" "skipped" "$reason" '{}' "$funding_json" "$started" "$ended"
            record SKIP "$id" "not connectable: $reason"
            continue
        fi

        if attempt_destination "$round" "$entry" "$funding_json"; then
            successes=$((successes + 1))
        fi
    done

    ROUND_SUCCESSES="$successes"
}

run_tour() {
    local round=1 stop_reason="" balance_json funding_json traffic

    while :; do
        if [ "$round" -gt "$MAX_ROUNDS" ]; then
            stop_reason="hit --max-rounds ($MAX_ROUNDS)"
            break
        fi

        balance_json="$(get_balance_json)" || balance_json='{"Balance":{"Err":"gnosis_vpn-ctl balance failed"}}'
        funding_json="$(jq -c '.Balance.Ok.funding_status // {traffic:"Unknown", gas:"Unknown"}' <<<"$balance_json")"
        traffic="$(jq -r '.traffic' <<<"$funding_json")"
        if [ "$traffic" = "Empty" ]; then
            stop_reason="funds drained: traffic funding status is Empty"
            break
        fi

        section "Round $round"
        run_round "$round" "$funding_json"
        if [ "$ROUND_SUCCESSES" -eq 0 ]; then
            stop_reason="no destination connectable this round"
            break
        fi
        round=$((round + 1))
    done

    printf '%s\n' "$stop_reason" >"$STOP_REASON_FILE"
    section "Drain tour finished"
    info "$stop_reason"
}

print_final_summary() {
    section "Summary"
    local total connected failed skipped
    total="$(wc -l <"$RUNS_JSONL" | tr -d ' ')"
    connected="$(jq -s '[.[] | select(.outcome=="connected")] | length' "$RUNS_JSONL")"
    failed="$(jq -s '[.[] | select(.outcome=="failed")] | length' "$RUNS_JSONL")"
    skipped="$(jq -s '[.[] | select(.outcome=="skipped")] | length' "$RUNS_JSONL")"
    info "$total attempts: $connected connected, $failed failed, $skipped skipped"
    info "raw data: $RUNS_JSONL, $METRICS_CSV"
    info "report:   vpn-drain-report.sh --run-dir '$OUT_DIR'"
}

# ---------------------------------------------------------------------------
# Preflight, CLI parsing, main
# ---------------------------------------------------------------------------

# check_deps <cmd> <apt-pkg> [<cmd> <apt-pkg> ...] -> with --install-deps, apt-get installs any missing command's package and returns; otherwise aborts with the apt-get command to run.
check_deps() {
    local missing_cmds=() missing_pkgs=()
    while [ "$#" -gt 0 ]; do
        command -v "$1" >/dev/null 2>&1 || {
            missing_cmds+=("$1")
            missing_pkgs+=("$2")
        }
        shift 2
    done
    [ "${#missing_cmds[@]}" -eq 0 ] && return 0

    if [ "$INSTALL_DEPS" -eq 1 ]; then
        command -v apt-get >/dev/null 2>&1 || {
            printf '\n%sAborting: --install-deps needs apt-get, which was not found.%s\n' "$C_RED" "$C_RESET" >&2
            exit 2
        }
        printf '%sInstalling missing dependencies: %s%s\n' "$C_BOLD" "${missing_pkgs[*]}" "$C_RESET"
        if [ "${EUID:-1}" -eq 0 ]; then
            apt-get update && apt-get install -y "${missing_pkgs[@]}"
        elif command -v sudo >/dev/null 2>&1; then
            sudo apt-get update && sudo apt-get install -y "${missing_pkgs[@]}"
        else
            printf '\n%sAborting: --install-deps needs sudo (or re-run as root).%s\n' "$C_RED" "$C_RESET" >&2
            exit 2
        fi
        return 0
    fi

    printf '\n%sAborting: missing %s.%s\n' "$C_RED" "${missing_cmds[*]}" "$C_RESET" >&2
    if command -v apt-get >/dev/null 2>&1; then
        printf 'On Debian/Ubuntu: sudo apt-get install -y %s\n' "${missing_pkgs[*]}" >&2
        printf '(or re-run this script with --install-deps)\n' >&2
    fi
    exit 2
}

require_ctl() {
    if ! command -v "$CTL_BIN" >/dev/null 2>&1; then
        printf '\n%sAborting: %s not found (set GVPN_CTL to override). gnosis_vpn-ctl is this project'"'"'s own binary, not an apt package - install/build the client first.%s\n' "$C_RED" "$CTL_BIN" "$C_RESET" >&2
        exit 2
    fi
}

usage() {
    cat <<'EOF'
Usage: vpn-drain-tour.sh [options]

Cycle through every configured Gnosis VPN destination, best exit-capacity/
latency first, smoke-testing each connection and recording the results.
Repeats full rounds - destination capacity and account funding both change
over time - until the account's traffic funding is reported Empty or a whole
round fails to connect to anything.

Options:
      --out-dir DIR         Where to write run data (default ./vpn-drain-runs/<UTC timestamp>)
      --max-rounds N        Safety cap on rounds                    (default 50)
      --connect-timeout N   Seconds to wait for a tunnel to come up  (default 60)
      --poll-interval N     Seconds between status polls while waiting (default 2)
  -s, --socket-path PATH    Passed through to gnosis_vpn-ctl
      --targets "A B"       HTTPS reachability hosts                 (default example.com openbsd.org freebsd.org)
      --sizes "N N"         Download ladder in bytes                 (default 1024 102400 1048576 3145728)
      --stream-secs N       Sustained-transfer duration              (default 20)
      --http-timeout N      Per-request HTTP timeout (s)             (default 60)
      --dl-timeout N        Sized-download timeout (s)               (default 120)
      --ping-timeout N      Ping timeout (s)                         (default 15)
      --quick               Shrink the per-connection probe set (skip 3 MB download, shorten streaming)
      --install-deps        Install missing jq/curl/ping/awk via apt-get (Debian/Ubuntu) and continue
      --no-color            Disable colored output
  -h, --help                Show this help

Environment overrides: GVPN_CTL (gnosis_vpn-ctl binary), GVPN_CURL, GVPN_PING, NO_COLOR.

Afterwards, render a comparison report with:
  vpn-drain-report.sh --run-dir <out-dir>
EOF
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
        --out-dir | --max-rounds | --connect-timeout | --poll-interval | -s | --socket-path | --targets | --sizes | --stream-secs | --http-timeout | --dl-timeout | --ping-timeout)
            require_option_value "$@"
            ;;
        esac
        case "$1" in
        --out-dir)
            OUT_DIR="$2"
            shift 2
            ;;
        --max-rounds)
            MAX_ROUNDS="$2"
            shift 2
            ;;
        --connect-timeout)
            CONNECT_TIMEOUT="$2"
            shift 2
            ;;
        --poll-interval)
            POLL_INTERVAL="$2"
            shift 2
            ;;
        -s | --socket-path)
            SOCKET_PATH="$2"
            shift 2
            ;;
        --targets)
            TARGETS="$2"
            shift 2
            ;;
        --sizes)
            SIZES="$2"
            shift 2
            ;;
        --stream-secs)
            STREAM_SECS="$2"
            shift 2
            ;;
        --http-timeout)
            HTTP_TIMEOUT="$2"
            shift 2
            ;;
        --dl-timeout)
            DL_TIMEOUT="$2"
            shift 2
            ;;
        --ping-timeout)
            PING_TIMEOUT="$2"
            shift 2
            ;;
        --quick)
            QUICK=1
            shift
            ;;
        --install-deps)
            INSTALL_DEPS=1
            shift
            ;;
        --no-color)
            # Read by setup_colors() in the sourced vpn-smoke-test.sh.
            # shellcheck disable=SC2034
            GVPN_COLOR="never"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'Unknown option: %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
        esac
    done
}

main() {
    parse_args "$@"
    setup_colors
    OS="$(detect_os)"
    check_deps jq jq "$CURL_BIN" curl "$PING_BIN" iputils-ping awk gawk
    require_ctl

    OUT_DIR="${OUT_DIR:-./vpn-drain-runs/$(date -u +%Y%m%dT%H%M%SZ)}"
    mkdir -p "$OUT_DIR"
    RUNS_JSONL="$OUT_DIR/runs.jsonl"
    METRICS_CSV="$OUT_DIR/metrics.csv"
    STOP_REASON_FILE="$OUT_DIR/stop_reason.txt"
    : >"$RUNS_JSONL"
    printf 'round,destination_id,outcome,reason,capacity_used,capacity_available,selection_ping_rtt_ms,gateway_ping_avg_ms,packet_loss_pct,streaming_bps,egress_ip,https_ok,https_total,funding_traffic,funding_gas,started_at,ended_at\n' >"$METRICS_CSV"

    printf '%sGnosis VPN drain tour%s\n' "$C_BOLD" "$C_RESET"
    info "out-dir: $OUT_DIR"

    run_tour
    print_final_summary
}

if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    main "$@"
fi
