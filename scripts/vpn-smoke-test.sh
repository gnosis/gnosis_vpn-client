#!/usr/bin/env bash
#
# vpn-smoke-test.sh - validate Gnosis VPN connectivity after connecting.
#
# Runs a series of checks against a live tunnel: tunnel-gateway ping and
# latency, packet loss, path-MTU, DNS resolution, HTTPS reachability, sized
# HTTPS downloads (1 KB .. 3 MB), a sustained/streaming transfer, the egress
# IP + geolocation, and an IPv6 leak check. Each check prints PASS / WARN /
# FAIL / SKIP; the process exits non-zero if any check FAILs.
#
# Required commands: awk, bash, cat, curl, grep, ping, and uname. The optional
# ip and ifconfig commands enable tunnel-interface discovery; getent and
# dscacheutil add resolved addresses to verbose DNS output.
#
# Run `./vpn-smoke-test.sh --help` for options.

set -euo pipefail

# Force a predictable locale so curl's numeric write-out uses '.' as the
# decimal separator and tool output is not translated.
export LC_ALL=C

# ---------------------------------------------------------------------------
# Defaults (all overridable via flags / environment)
# ---------------------------------------------------------------------------

GATEWAY="${GVPN_GATEWAY:-10.128.0.1}" # server-side tunnel IP the client health-checks
IFACE_OVERRIDE=""                     # skip iface auto-detection when set
BASELINE_IP="${GVPN_BASELINE_IP:-}"   # pre-VPN public IP; matching egress == leak
SIZES="1024 102400 1048576 3145728"   # download ladder: 1 KB, 100 KB, 1 MB, 3 MB
TARGETS="example.com openbsd.org freebsd.org"
DOWN_URL="https://speed.cloudflare.com/__down"
TRACE_URL="https://cloudflare.com/cdn-cgi/trace"
HTTP_TIMEOUT=60 # matches the client's default HTTP timeout
DL_TIMEOUT=120  # generous: even a few MB over the mixnet is slow
PING_TIMEOUT=15 # matches the client's default tunnel-ping timeout
PING_TTL=6      # matches the client's default tunnel-ping TTL
STREAM_SECS=20  # sustained-transfer duration cap
IPV6_TIMEOUT=10 # short: an IPv6 attempt is expected to fail fast
QUICK=0         # --quick: skip the 3 MB download and shorten streaming
VERBOSE=0

# Internal, non-flag constants.
PING_COUNT_QUICK=3
PING_COUNT_LOSS=10
LOSS_WARN_PCT=20
MTU_PAYLOAD=1392      # 1420 MTU - 20 (IPv4) - 8 (ICMP) header bytes
STREAM_BYTES=52428800 # 50 MB target (under Cloudflare's 1e8 __down cap); capped by --stream-secs, so it streams for the duration

CURL_BIN="${GVPN_CURL:-curl}"
PING_BIN="${GVPN_PING:-ping}"

# curl write-out field order shared by every metrics call.
CURL_W='%{http_code} %{size_download} %{speed_download} %{time_total} %{time_namelookup}'

# ---------------------------------------------------------------------------
# Result tallying
# ---------------------------------------------------------------------------

PASS_COUNT=0
WARN_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0
GATEWAY_UP=0

setup_colors() {
    if [ -n "${NO_COLOR:-}" ] || [ "${GVPN_COLOR:-auto}" = "never" ] || [ ! -t 1 ]; then
        C_GREEN="" C_YELLOW="" C_RED="" C_DIM="" C_BOLD="" C_RESET=""
    else
        C_GREEN=$'\033[32m'
        C_YELLOW=$'\033[33m'
        C_RED=$'\033[31m'
        C_DIM=$'\033[2m'
        C_BOLD=$'\033[1m'
        C_RESET=$'\033[0m'
    fi
}

# record <PASS|WARN|FAIL|SKIP> <name> [detail]
record() {
    local status="$1" name="$2" detail="${3:-}" color=""
    case "$status" in
    PASS)
        PASS_COUNT=$((PASS_COUNT + 1))
        color="$C_GREEN"
        ;;
    WARN)
        WARN_COUNT=$((WARN_COUNT + 1))
        color="$C_YELLOW"
        ;;
    FAIL)
        FAIL_COUNT=$((FAIL_COUNT + 1))
        color="$C_RED"
        ;;
    SKIP)
        SKIP_COUNT=$((SKIP_COUNT + 1))
        color="$C_DIM"
        ;;
    esac
    printf '  %s%-4s%s  %-22s %s\n' "$color" "$status" "$C_RESET" "$name" "$detail"
}

info() { printf '  %s%s%s\n' "$C_DIM" "$1" "$C_RESET"; }
section() { printf '\n%s%s%s\n' "$C_BOLD" "$1" "$C_RESET"; }

# ---------------------------------------------------------------------------
# Pure helpers (unit-tested)
# ---------------------------------------------------------------------------

# human_bytes <integer-bytes> -> "1.2 MB" / "3.4 KB" / "512 B"
human_bytes() {
    local b="${1:-0}"
    b="${b%%.*}"
    [ -n "$b" ] || b=0
    if [ "$b" -ge 1048576 ]; then
        printf '%d.%01d MB' "$((b / 1048576))" "$(((b % 1048576) * 10 / 1048576))"
    elif [ "$b" -ge 1024 ]; then
        printf '%d.%01d KB' "$((b / 1024))" "$(((b % 1024) * 10 / 1024))"
    else
        printf '%d B' "$b"
    fi
}

# human_rate <integer-bytes-per-second> -> "1.2 MB/s"
human_rate() {
    printf '%s/s' "$(human_bytes "${1:-0}")"
}

# ping_argv <linux|macos|other> <count> <timeout-s> <ttl> <address>
# Emits the ping arguments for the platform. The -t/-W/-m flags differ:
# Linux uses -W <timeout> -t <ttl>; macOS uses -t <timeout> -m <ttl>.
ping_argv() {
    local os="$1" count="$2" timeout="$3" ttl="$4" addr="$5"
    case "$os" in
    linux) printf -- '-c %s -W %s -t %s %s' "$count" "$timeout" "$ttl" "$addr" ;;
    macos) printf -- '-c %s -t %s -m %s %s' "$count" "$timeout" "$ttl" "$addr" ;;
    *) printf -- '-c %s %s' "$count" "$addr" ;;
    esac
}

# mtu_argv <linux|macos|other> <timeout-s> <payload> <address>
# Emits a single don't-fragment ping used to probe path MTU.
mtu_argv() {
    local os="$1" timeout="$2" size="$3" addr="$4"
    case "$os" in
    linux) printf -- '-c 1 -W %s -M do -s %s %s' "$timeout" "$size" "$addr" ;;
    macos) printf -- '-c 1 -t %s -D -s %s %s' "$timeout" "$size" "$addr" ;;
    *) printf -- '' ;;
    esac
}

# parse_rtt_avg <ping-output> -> average RTT in ms (e.g. "13.5"), or empty.
# Handles both Linux ("rtt min/avg/max/mdev = ...") and macOS
# ("round-trip min/avg/max/stddev = ...") summary lines.
parse_rtt_avg() {
    printf '%s\n' "$1" | awk '
        /rtt|round-trip/ {
            n = split($0, a, "=")
            if (n < 2) next
            split(a[2], b, "/")
            v = b[2]
            gsub(/[^0-9.]/, "", v)
            if (v != "") { print v; exit }
        }'
}

# parse_loss <ping-output> -> packet-loss percent (e.g. "0" or "0.0"), or empty.
parse_loss() {
    printf '%s\n' "$1" | awk '
        /packet loss/ {
            n = split($0, a, ",")
            for (i = 1; i <= n; i++) {
                if (a[i] ~ /packet loss/) {
                    v = a[i]
                    gsub(/[^0-9.]/, "", v)
                    print v
                    exit
                }
            }
        }'
}

# detect_os -> linux | macos | other
detect_os() {
    case "$(uname -s 2>/dev/null || printf 'unknown')" in
    Linux) printf 'linux' ;;
    Darwin) printf 'macos' ;;
    *) printf 'other' ;;
    esac
}

# ---------------------------------------------------------------------------
# curl wrappers
# ---------------------------------------------------------------------------

# http_metrics <url> <max-time> [extra curl args...]
# Discards the body, prints the CURL_W fields, returns curl's exit code.
http_metrics() {
    local url="$1" maxtime="$2"
    shift 2
    "$CURL_BIN" -sS -o /dev/null -L --max-time "$maxtime" -w "$CURL_W" "$@" "$url"
}

# http_body <url> <max-time> [extra curl args...]
# Prints the response body, returns curl's exit code.
http_body() {
    local url="$1" maxtime="$2"
    shift 2
    "$CURL_BIN" -sS -L --max-time "$maxtime" "$@" "$url"
}

# ---------------------------------------------------------------------------
# Checks
# ---------------------------------------------------------------------------

detect_iface() {
    if [ -n "$IFACE_OVERRIDE" ]; then
        printf '%s' "$IFACE_OVERRIDE"
        return 0
    fi
    if command -v ip >/dev/null 2>&1; then
        ip -o link 2>/dev/null | awk -F': ' \
            '$2 ~ /^wg0_gnosisvpn$/ || $2 ~ /^utun[0-9]+$/ { print $2; exit }'
    elif command -v ifconfig >/dev/null 2>&1; then
        local i
        for i in $(ifconfig -l 2>/dev/null); do
            case "$i" in
            wg0_gnosisvpn | utun[0-9]*)
                printf '%s' "$i"
                return 0
                ;;
            esac
        done
    fi
}

check_preflight() {
    section "Preflight"
    info "os=$OS  bash=${BASH_VERSION:-?}  curl=$CURL_BIN  ping=$PING_BIN"

    if ! command -v "$CURL_BIN" >/dev/null 2>&1; then
        record FAIL "curl present" "curl not found - cannot run HTTP checks"
        printf '\n%sAborting: curl is required.%s\n' "$C_RED" "$C_RESET" >&2
        exit 2
    fi
    record PASS "curl present" "$(command -v "$CURL_BIN")"

    local iface=""
    iface="$(detect_iface || true)"
    if [ -n "$iface" ]; then
        if [ "$OS" = "macos" ]; then
            record PASS "tunnel iface" "$iface (heuristic; utun is dynamic on macOS)"
        else
            record PASS "tunnel iface" "$iface"
        fi
    else
        record WARN "tunnel iface" "no wg0_gnosisvpn/utun found - are you connected?"
    fi
}

check_ping() {
    section "Tunnel data path"
    local argv arr=() out="" rc=0
    argv="$(ping_argv "$OS" "$PING_COUNT_QUICK" "$PING_TIMEOUT" "$PING_TTL" "$GATEWAY")"
    read -r -a arr <<<"$argv"
    out="$("$PING_BIN" "${arr[@]}" 2>&1)" || rc=$?
    if [ "$rc" -ne 0 ]; then
        record FAIL "tunnel ping" "$GATEWAY unreachable (ping rc=$rc) - tunnel down?"
        return
    fi
    GATEWAY_UP=1
    local avg loss
    avg="$(parse_rtt_avg "$out")"
    loss="$(parse_loss "$out")"
    record PASS "tunnel ping" "$GATEWAY  avg ${avg:-?} ms  loss ${loss:-?}%"
}

check_loss() {
    if [ "$GATEWAY_UP" -ne 1 ]; then
        record SKIP "packet loss" "gateway unreachable"
        return
    fi
    local argv arr=() out="" rc=0
    argv="$(ping_argv "$OS" "$PING_COUNT_LOSS" "$PING_TIMEOUT" "$PING_TTL" "$GATEWAY")"
    read -r -a arr <<<"$argv"
    out="$("$PING_BIN" "${arr[@]}" 2>&1)" || rc=$?
    local loss lossint
    loss="$(parse_loss "$out")"
    lossint="${loss%%.*}"
    [ -n "$lossint" ] || lossint=100
    if [ "$rc" -ne 0 ] || [ "$lossint" -ge 100 ]; then
        record FAIL "packet loss" "100% over $PING_COUNT_LOSS packets"
    elif [ "$lossint" -gt "$LOSS_WARN_PCT" ]; then
        record WARN "packet loss" "${loss}% over $PING_COUNT_LOSS packets (>${LOSS_WARN_PCT}%)"
    else
        record PASS "packet loss" "${loss}% over $PING_COUNT_LOSS packets"
    fi
}

check_mtu() {
    if [ "$GATEWAY_UP" -ne 1 ]; then
        record SKIP "mtu probe" "gateway unreachable"
        return
    fi
    local argv arr=() out="" rc=0
    argv="$(mtu_argv "$OS" "$PING_TIMEOUT" "$MTU_PAYLOAD" "$GATEWAY")"
    if [ -z "$argv" ]; then
        record SKIP "mtu probe" "unsupported OS"
        return
    fi
    read -r -a arr <<<"$argv"
    out="$("$PING_BIN" "${arr[@]}" 2>&1)" || rc=$?
    if [ "$rc" -eq 0 ]; then
        record PASS "mtu probe" "${MTU_PAYLOAD}B DF payload ok (path MTU >= 1420)"
    elif printf '%s' "$out" | grep -qiE 'too long|fragment|frag needed'; then
        record WARN "mtu probe" "fragmentation at ${MTU_PAYLOAD}B - path MTU below 1420?"
    else
        record SKIP "mtu probe" "inconclusive (ping rc=$rc)"
    fi
}

check_dns() {
    section "DNS resolution"
    local host resolved=0 failed=0
    for host in $TARGETS cloudflare.com; do
        local rc=0
        http_metrics "https://$host" "$HTTP_TIMEOUT" --head >/dev/null 2>&1 || rc=$?
        # curl exit code 6 == "couldn't resolve host"; anything else means DNS
        # succeeded (the request may still fail later for other reasons).
        if [ "$rc" -eq 6 ]; then
            record FAIL "resolve $host" "DNS resolution failed"
            failed=$((failed + 1))
        else
            resolved=$((resolved + 1))
            if [ "$VERBOSE" -eq 1 ]; then
                record PASS "resolve $host" "resolved$(verbose_resolved "$host")"
            fi
        fi
    done
    if [ "$failed" -eq 0 ]; then
        record PASS "dns" "$resolved/$resolved hosts resolved"
    elif [ "$resolved" -eq 0 ]; then
        record FAIL "dns" "0 hosts resolved"
    else
        record WARN "dns" "$resolved resolved, $failed failed"
    fi
}

# Best-effort resolved-IP lookup for verbose output only (not asserted).
verbose_resolved() {
    local host="$1" ip=""
    if command -v getent >/dev/null 2>&1; then
        ip="$(getent hosts "$host" 2>/dev/null | awk 'NR==1{print $1}')"
    elif command -v dscacheutil >/dev/null 2>&1; then
        ip="$(dscacheutil -q host -a name "$host" 2>/dev/null | awk -F': ' '/^ip_address/{print $2; exit}')"
    fi
    if [ -n "$ip" ]; then
        printf ' (%s)' "$ip"
    fi
}

check_https() {
    section "HTTPS reachability"
    local host ok=0 bad=0
    for host in $TARGETS; do
        local metrics rc=0 code time
        metrics="$(http_metrics "https://$host" "$HTTP_TIMEOUT")" || rc=$?
        read -r code _ _ time _ <<<"$metrics"
        if [ "$rc" -eq 0 ] && printf '%s' "$code" | grep -qE '^[23]'; then
            record PASS "GET $host" "HTTP $code in $(fmt_secs "$time")"
            ok=$((ok + 1))
        else
            record FAIL "GET $host" "HTTP ${code:-000} (curl rc=$rc)"
            bad=$((bad + 1))
        fi
    done
    [ "$ok" -gt 0 ] || record FAIL "https" "no target reachable"
}

check_downloads() {
    section "Sized HTTPS downloads"
    local n label
    for n in $SIZES; do
        label="$(human_bytes "$n")"
        if [ "$QUICK" -eq 1 ] && [ "$n" -ge 3145728 ]; then
            record SKIP "download $label" "--quick"
            continue
        fi
        local metrics rc=0 code size speed
        metrics="$(http_metrics "${DOWN_URL}?bytes=${n}" "$DL_TIMEOUT")" || rc=$?
        read -r code size speed _ _ <<<"$metrics"
        size="${size%%.*}"
        [ -n "$size" ] || size=0
        if [ "$rc" -eq 0 ] && [ "$size" -eq "$n" ]; then
            record PASS "download $label" "$(human_bytes "$size") @ $(human_rate "$speed")"
        elif [ "$rc" -eq 0 ] && [ "$size" -gt 0 ]; then
            record WARN "download $label" "short: got $(human_bytes "$size") of $label"
        else
            record FAIL "download $label" "HTTP ${code:-000} (curl rc=$rc)"
        fi
    done
}

check_streaming() {
    section "Streaming / sustained transfer"
    local secs="$STREAM_SECS"
    [ "$QUICK" -eq 1 ] && secs=8
    local metrics rc=0 code size speed
    metrics="$(http_metrics "${DOWN_URL}?bytes=${STREAM_BYTES}" "$secs")" || rc=$?
    read -r code size speed _ _ <<<"$metrics"
    size="${size%%.*}"
    [ -n "$size" ] || size=0
    # Two success shapes: a clean finish (curl rc 0 with a 2xx code) or a
    # time-capped transfer (curl rc 28 - expected when the target is too large
    # to complete within the window). Requiring a 2xx on the clean path rejects
    # an error/short body (e.g. an over-cap 403) that would otherwise false-pass.
    local finished=0 capped=0
    if [ "$rc" -eq 0 ] && printf '%s' "$code" | grep -qE '^2'; then finished=1; fi
    [ "$rc" -eq 28 ] && capped=1
    if [ "$size" -gt 0 ] && { [ "$finished" -eq 1 ] || [ "$capped" -eq 1 ]; }; then
        local note=""
        [ "$capped" -eq 1 ] && note=" (time-capped at ${secs}s)"
        record PASS "streaming" "$(human_bytes "$size") @ $(human_rate "$speed")$note"
    else
        record FAIL "streaming" "no sustained data (HTTP ${code:-000}, curl rc=$rc, got $(human_bytes "$size"))"
    fi
}

check_egress() {
    section "Egress routing"
    local body rc=0 ip loc colo
    body="$(http_body "$TRACE_URL" "$HTTP_TIMEOUT")" || rc=$?
    ip="$(printf '%s\n' "$body" | awk -F= '/^ip=/{print $2; exit}')"
    loc="$(printf '%s\n' "$body" | awk -F= '/^loc=/{print $2; exit}')"
    colo="$(printf '%s\n' "$body" | awk -F= '/^colo=/{print $2; exit}')"
    if [ "$rc" -ne 0 ] || [ -z "$ip" ]; then
        record FAIL "egress ip" "could not determine egress IP (curl rc=$rc)"
        return
    fi
    if [ -n "$BASELINE_IP" ] && [ "$ip" = "$BASELINE_IP" ]; then
        record FAIL "egress ip" "$ip matches baseline - traffic NOT routed via VPN"
    else
        record PASS "egress ip" "$ip  (${loc:-?} via ${colo:-?})"
        if [ -n "$BASELINE_IP" ]; then
            info "baseline $BASELINE_IP differs from egress - routed via VPN"
        fi
    fi
}

check_ipv6_leak() {
    section "IPv6 leak"
    local rc=0
    http_metrics "$TRACE_URL" "$IPV6_TIMEOUT" -6 >/dev/null 2>&1 || rc=$?
    if [ "$rc" -eq 0 ]; then
        record WARN "ipv6 blocked" "IPv6 egress reachable - blackhole not active (possible leak)"
    else
        record PASS "ipv6 blocked" "no IPv6 egress (blackholed or unavailable)"
    fi
}

# fmt_secs <float-seconds> -> "1.2s"
fmt_secs() {
    local s="${1:-0}"
    printf '%ss' "$(printf '%.1f' "$s" 2>/dev/null || printf '%s' "$s")"
}

# ---------------------------------------------------------------------------
# Summary + argument parsing
# ---------------------------------------------------------------------------

print_summary() {
    section "Summary"
    printf '  %s%d passed%s  %s%d warn%s  %s%d failed%s  %s%d skipped%s\n' \
        "$C_GREEN" "$PASS_COUNT" "$C_RESET" \
        "$C_YELLOW" "$WARN_COUNT" "$C_RESET" \
        "$C_RED" "$FAIL_COUNT" "$C_RESET" \
        "$C_DIM" "$SKIP_COUNT" "$C_RESET"
    if [ "$FAIL_COUNT" -gt 0 ]; then
        printf '\n%sVPN connectivity smoke test FAILED.%s\n' "$C_RED" "$C_RESET"
        return 1
    fi
    printf '\n%sVPN connectivity smoke test passed.%s\n' "$C_GREEN" "$C_RESET"
    return 0
}

usage() {
    cat <<'EOF'
Usage: vpn-smoke-test.sh [options]

Validate Gnosis VPN connectivity after connecting. Checks tunnel ping,
packet loss, path MTU, DNS, HTTPS reachability, sized downloads, streaming,
egress IP/geo, and IPv6 leak protection. Exits non-zero if any check fails.

Options:
  -g, --gateway IP      Tunnel gateway to ping        (default 10.128.0.1)
      --iface NAME      Force tunnel interface name   (default auto-detect)
      --baseline-ip IP  Your pre-VPN public IP; flags a leak if egress matches
      --targets "A B"   HTTPS reachability hosts       (default example.com openbsd.org freebsd.org)
      --sizes "N N"     Download ladder in bytes       (default 1024 102400 1048576 3145728)
      --stream-secs N   Sustained-transfer duration    (default 20)
      --http-timeout N  Per-request HTTP timeout (s)   (default 60)
      --dl-timeout N    Sized-download timeout (s)     (default 120)
      --ping-timeout N  Ping timeout (s)               (default 15)
      --down-url URL    Sized-download endpoint        (default speed.cloudflare.com/__down)
      --trace-url URL   Egress-IP trace endpoint       (default cloudflare.com/cdn-cgi/trace)
      --quick           Skip the 3 MB download; shorten streaming
      --no-color        Disable colored output
  -v, --verbose         Extra per-host detail
  -h, --help            Show this help

Environment overrides: GVPN_GATEWAY, GVPN_BASELINE_IP, NO_COLOR.
Testing hooks: GVPN_CURL, GVPN_PING (substitute the curl/ping binaries).
EOF
}

require_option_value() {
    if [ "$#" -lt 2 ] || [ "${2#-}" != "$2" ]; then
        printf 'Option %s requires a value.\n\n' "$1" >&2
        usage >&2
        exit 2
    fi
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
        -g | --gateway | --iface | --baseline-ip | --targets | --sizes | --stream-secs | --http-timeout | --dl-timeout | --ping-timeout | --down-url | --trace-url)
            require_option_value "$@"
            ;;
        esac
        case "$1" in
        -g | --gateway)
            GATEWAY="$2"
            shift 2
            ;;
        --iface)
            IFACE_OVERRIDE="$2"
            shift 2
            ;;
        --baseline-ip)
            BASELINE_IP="$2"
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
        --down-url)
            DOWN_URL="$2"
            shift 2
            ;;
        --trace-url)
            TRACE_URL="$2"
            shift 2
            ;;
        --quick)
            QUICK=1
            shift
            ;;
        --no-color)
            GVPN_COLOR="never"
            shift
            ;;
        -v | --verbose)
            VERBOSE=1
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

    printf '%sGnosis VPN connectivity smoke test%s\n' "$C_BOLD" "$C_RESET"

    check_preflight
    check_ping
    check_loss
    check_mtu
    check_dns
    check_https
    check_downloads
    check_streaming
    check_egress
    check_ipv6_leak

    print_summary
}

# Run main only when executed directly, so tests can source the pure helpers.
if [ "${BASH_SOURCE[0]}" = "${0}" ]; then
    main "$@"
fi
