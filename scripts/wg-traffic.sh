#!/usr/bin/env bash
# Track download/upload volume on a WireGuard interface, reporting periodically and
# recording a per-window total to CSV.
set -euo pipefail

readonly DEFAULT_IFACE="wg0_gnosisvpn"
readonly DEFAULT_RESET="1h"
readonly DEFAULT_REPORT="1min"
readonly DEFAULT_SYSFS_ROOT="/sys/class/net"
readonly WG_RUN_DIR="/var/run/wireguard"
readonly CSV_HEADER="start_iso,end_iso,duration_s,iface,rx_bytes,tx_bytes,rx_human,tx_human,notes"

IFACE=""
RESET_SECS=0
REPORT_SECS=0
OUTPUT=""
SYSFS_ROOT="${DEFAULT_SYSFS_ROOT}"
SOURCE=""
SOURCE_OVERRIDE=""

# Sampling state. PREV_* hold the counter values of the previous sample; WIN_* accumulate
# the bytes seen in the window that opened at WIN_START.
PREV_RX=0
PREV_TX=0
WIN_RX=0
WIN_TX=0
WIN_START=0
WIN_START_ISO=""
DOWN_SECS=0
RESET_COUNT=0
DOWN_WARNED=0

# Outputs of apply_delta.
DELTA=0
COUNTER_RESET=0

# Outputs of accumulate_sample.
SAMPLE_D_RX=0
SAMPLE_D_TX=0

# PID of the in-flight sleep, so the signal handler can reclaim it.
SLEEP_PID=0

die() {
    echo "ERROR: $*" >&2
    exit 1
}

warn() {
    echo "WARN: $*" >&2
}

usage() {
    cat <<EOF
Usage: ${0##*/} [OPTIONS]

Sample the byte counters of a WireGuard interface, print a report every report
interval, and append the total for each reset interval to a CSV file.

Options:
  -i, --iface NAME     interface to watch (default: ${DEFAULT_IFACE})
  -r, --reset SPEC     reset interval, one CSV row per interval (default: ${DEFAULT_RESET})
  -p, --report SPEC    report interval, one stdout line per interval (default: ${DEFAULT_REPORT})
  -o, --output PATH    CSV file to append per-interval totals to
  -h, --help           show this help

Interval SPEC is a bare number of seconds or a suffixed value: 30s, 5m, 5min, 1h, 2d.

Test/diagnostic overrides:
      --sysfs-root DIR         sysfs network directory (default: ${DEFAULT_SYSFS_ROOT})
      --source SRC             force the counter source: sysfs, netstat or wg

WireGuard byte counters cannot be zeroed, so the reset interval resets this script's
accumulator rather than the device. If the interface is torn down and recreated the
counters restart from zero; that is detected and flagged in the CSV notes column.
EOF
}

# Convert an interval spec to seconds on stdout. Returns non-zero if the spec is invalid.
parse_interval() {
    local spec="${1:-}" num unit
    if [[ ! ${spec} =~ ^([0-9]+)(s|sec|m|min|h|d)?$ ]]; then
        return 1
    fi
    num="${BASH_REMATCH[1]}"
    unit="${BASH_REMATCH[2]:-}"
    case "${unit}" in
    m | min) num=$((num * 60)) ;;
    h) num=$((num * 3600)) ;;
    d) num=$((num * 86400)) ;;
    *) ;;
    esac
    if [[ ${num} -le 0 ]]; then
        return 1
    fi
    echo "${num}"
}

# Render a byte count with decimal units, where 1 kB is 1000 bytes. Fractional maths runs
# through awk because stock macOS has no bc and bash cannot divide with a remainder.
format_bytes() {
    awk -v bytes="${1}" 'BEGIN {
        split("B kB MB GB TB PB", units, " ")
        i = 1
        value = bytes + 0
        while (value >= 1000 && i < 6) {
            value /= 1000
            i++
        }
        if (i == 1) {
            printf "%d %s", value, units[i]
        } else {
            printf "%.2f %s", value, units[i]
        }
    }'
}

# Render a throughput for ${1} bytes observed over ${2} seconds. Division runs through awk
# rather than bash so traffic smaller than the interval (e.g. 1 byte/10s) shows as a fraction
# instead of rounding down to a misleading "0 B/s".
format_rate() {
    local bytes="${1}" secs="${2}"
    if [[ ${secs} -le 0 ]]; then
        echo "0 B/s"
        return 0
    fi
    awk -v bytes="${bytes}" -v secs="${secs}" 'BEGIN {
        split("B kB MB GB TB PB", units, " ")
        i = 1
        value = bytes / secs
        while (value >= 1000 && i < 6) {
            value /= 1000
            i++
        }
        if (i == 1 && value == int(value)) {
            printf "%d %s/s", value, units[i]
        } else {
            printf "%.2f %s/s", value, units[i]
        }
    }'
}

# Reconcile one sample against the previous one, setting DELTA to the bytes to add to the
# current window and COUNTER_RESET to 1 when the interface was detected as recreated.
#   $1 = current counter value
#   $2 = previous counter value
apply_delta() {
    local cur="${1}" prev="${2}"
    COUNTER_RESET=0
    if [[ ${cur} -lt ${prev} ]]; then
        # The interface was torn down and recreated, so the counter restarted near zero.
        # ${cur} bytes have genuinely crossed the new interface, so they are counted.
        # Whatever the old interface moved between the last sample and its teardown is
        # unrecoverable either way, which is why the window is marked in the notes column.
        COUNTER_RESET=1
        DELTA="${cur}"
        return 0
    fi
    DELTA=$((cur - prev))
}

# Build the CSV notes column. Deliberately comma-free so the column needs no quoting.
#   $1 = counter resets in the window, $2 = seconds the interface was down, $3 = partial
csv_notes() {
    local resets="${1}" down="${2}" partial="${3:-0}" notes=""
    if [[ ${resets} -gt 0 ]]; then
        notes="counter-reset x${resets}"
    fi
    if [[ ${down} -gt 0 ]]; then
        if [[ -n ${notes} ]]; then
            notes="${notes}; "
        fi
        notes="${notes}iface-down ${down}s"
    fi
    if [[ ${partial} -eq 1 ]]; then
        if [[ -n ${notes} ]]; then
            notes="${notes}; "
        fi
        notes="${notes}partial"
    fi
    echo "${notes}"
}

iso_now() {
    date -u +%Y-%m-%dT%H:%M:%SZ
}

epoch_now() {
    date -u +%s
}

# On macOS wg-quick creates a dynamically named utun device and records the real name in
# ${WG_RUN_DIR}, mirroring resolve_interface_name() in gnosis_vpn-root/src/wg_tooling.rs.
resolve_iface() {
    local name_file="${WG_RUN_DIR}/${DEFAULT_IFACE}.name"
    if [[ -n ${IFACE} ]]; then
        return 0
    fi
    if [[ ! -d ${SYSFS_ROOT} ]] && [[ -e ${name_file} ]]; then
        if [[ ! -r ${name_file} ]]; then
            die "cannot read ${name_file}; pass --iface with the utun name or re-run with sudo"
        fi
        IFACE=$(tr -d '[:space:]' <"${name_file}")
    fi
    if [[ -z ${IFACE} ]]; then
        IFACE="${DEFAULT_IFACE}"
    fi
}

detect_source() {
    if [[ -n ${SOURCE_OVERRIDE} ]]; then
        SOURCE="${SOURCE_OVERRIDE}"
        return 0
    fi
    if [[ -d ${SYSFS_ROOT} ]]; then
        SOURCE="sysfs"
    elif command -v netstat >/dev/null 2>&1; then
        SOURCE="netstat"
    elif command -v wg >/dev/null 2>&1; then
        SOURCE="wg"
    else
        die "no usable counter source: need ${SYSFS_ROOT}, netstat or wg"
    fi
}

# Echo "<rx_bytes> <tx_bytes>" for the interface, or return non-zero if it cannot be read.
read_counters() {
    case "${SOURCE}" in
    sysfs) read_counters_sysfs ;;
    netstat) read_counters_netstat ;;
    wg) read_counters_wg ;;
    *) die "unknown counter source '${SOURCE}'" ;;
    esac
}

read_counters_sysfs() {
    local dir="${SYSFS_ROOT}/${IFACE}/statistics" rx tx
    rx=$(cat "${dir}/rx_bytes" 2>/dev/null) || return 1
    tx=$(cat "${dir}/tx_bytes" 2>/dev/null) || return 1
    if [[ ! ${rx} =~ ^[0-9]+$ ]] || [[ ! ${tx} =~ ^[0-9]+$ ]]; then
        return 1
    fi
    echo "${rx} ${tx}"
}

# The Ibytes/Obytes columns are located from the header row rather than hardcoded, because
# netstat's column layout varies between platforms and flag combinations.
read_counters_netstat() {
    local out
    out=$(netstat -bnI "${IFACE}" 2>/dev/null) || return 1
    echo "${out}" | awk '
        NR == 1 {
            for (i = 1; i <= NF; i++) {
                if ($i == "Ibytes") { ib = i }
                if ($i == "Obytes") { ob = i }
            }
            next
        }
        !found && ib && ob && $ib ~ /^[0-9]+$/ && $ob ~ /^[0-9]+$/ {
            rx = $ib
            tx = $ob
            found = 1
        }
        END {
            if (!found) { exit 1 }
            print rx, tx
        }
    '
}

# wg reports transfer per peer, so the columns are summed across all peers.
read_counters_wg() {
    local out
    out=$(wg show "${IFACE}" transfer 2>/dev/null) || return 1
    echo "${out}" | awk '
        NF >= 3 && $2 ~ /^[0-9]+$/ && $3 ~ /^[0-9]+$/ {
            rx += $2
            tx += $3
            found = 1
        }
        END {
            if (!found) { exit 1 }
            print rx, tx
        }
    '
}

# Fail with a clear message when a value-taking option was given without a value, rather
# than letting `shift 2` abort the script on its own.
need_value() {
    if [[ ${1} -lt 2 ]]; then
        die "option '${2}' requires a value (see --help)"
    fi
}

parse_args() {
    local report_spec="${DEFAULT_REPORT}" reset_spec="${DEFAULT_RESET}"
    while [[ $# -gt 0 ]]; do
        case "${1}" in
        -i | --iface)
            need_value $# "${1}"
            IFACE="${2}"
            shift 2
            ;;
        -r | --reset)
            need_value $# "${1}"
            reset_spec="${2}"
            shift 2
            ;;
        -p | --report)
            need_value $# "${1}"
            report_spec="${2}"
            shift 2
            ;;
        -o | --output)
            need_value $# "${1}"
            OUTPUT="${2}"
            shift 2
            ;;
        --sysfs-root)
            need_value $# "${1}"
            SYSFS_ROOT="${2}"
            shift 2
            ;;
        --source)
            need_value $# "${1}"
            SOURCE_OVERRIDE="${2}"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            die "unknown option '${1}' (see --help)"
            ;;
        esac
    done

    RESET_SECS=$(parse_interval "${reset_spec}") || die "invalid reset interval '${reset_spec}'"
    REPORT_SECS=$(parse_interval "${report_spec}") || die "invalid report interval '${report_spec}'"
    if [[ ${REPORT_SECS} -gt ${RESET_SECS} ]]; then
        die "report interval (${REPORT_SECS}s) must not exceed the reset interval (${RESET_SECS}s)"
    fi
    case "${SOURCE_OVERRIDE}" in
    "" | sysfs | netstat | wg) ;;
    *) die "invalid --source '${SOURCE_OVERRIDE}' (sysfs, netstat or wg)" ;;
    esac
    # Checked up front rather than at the first flush, so a bad path fails immediately
    # instead of after a full reset interval of data has been collected.
    if [[ -n ${OUTPUT} ]]; then
        local dir="${OUTPUT%/*}"
        if [[ ${dir} == "${OUTPUT}" ]]; then
            dir="."
        fi
        if [[ ! -d ${dir} ]] || [[ ! -w ${dir} ]]; then
            die "output directory '${dir}' is not writable"
        fi
        if [[ -e ${OUTPUT} ]] && { [[ ! -f ${OUTPUT} ]] || [[ ! -w ${OUTPUT} ]]; }; then
            die "output path '${OUTPUT}' exists and is not a writable regular file"
        fi
    fi
}

csv_init() {
    # -s is false for both a missing and an empty file, which is exactly when a header is
    # wanted. Appending rather than truncating keeps any existing rows intact.
    if [[ ! -s ${OUTPUT} ]]; then
        echo "${CSV_HEADER}" >>"${OUTPUT}"
    fi
}

# Emit the current window as a CSV row and a stdout summary. ${1} marks it partial.
flush_window() {
    local partial="${1:-0}" end_iso notes duration
    end_iso=$(iso_now)
    duration=$(($(epoch_now) - WIN_START))
    notes=$(csv_notes "${RESET_COUNT}" "${DOWN_SECS}" "${partial}")
    if [[ -n ${OUTPUT} ]]; then
        printf '%s,%s,%s,%s,%s,%s,%s,%s,%s\n' \
            "${WIN_START_ISO}" "${end_iso}" "${duration}" "${IFACE}" \
            "${WIN_RX}" "${WIN_TX}" \
            "$(format_bytes "${WIN_RX}")" "$(format_bytes "${WIN_TX}")" \
            "${notes}" >>"${OUTPUT}"
    fi
    printf 'WINDOW %s -> %s  rx %s  tx %s%s\n' \
        "${WIN_START_ISO}" "${end_iso}" \
        "$(format_bytes "${WIN_RX}")" "$(format_bytes "${WIN_TX}")" \
        "${notes:+  [${notes}]}"
}

open_window() {
    WIN_START=$(epoch_now)
    WIN_START_ISO=$(iso_now)
    WIN_RX=0
    WIN_TX=0
    DOWN_SECS=0
    RESET_COUNT=0
}

# Take a sample and fold it into the current window, setting SAMPLE_D_RX/SAMPLE_D_TX to the
# bytes this sample contributed. Returns non-zero if the interface could not be read, in
# which case the window's downtime is extended and PREV_* are left alone.
accumulate_sample() {
    local sample cur_rx cur_tx rx_reset
    SAMPLE_D_RX=0
    SAMPLE_D_TX=0
    if ! sample=$(read_counters); then
        DOWN_SECS=$((DOWN_SECS + REPORT_SECS))
        if [[ ${DOWN_WARNED} -eq 0 ]]; then
            warn "interface '${IFACE}' is not readable; still polling"
            DOWN_WARNED=1
        fi
        return 1
    fi
    if [[ ${DOWN_WARNED} -eq 1 ]]; then
        warn "interface '${IFACE}' is readable again"
        DOWN_WARNED=0
    fi

    read -r cur_rx cur_tx <<<"${sample}"
    apply_delta "${cur_rx}" "${PREV_RX}"
    SAMPLE_D_RX="${DELTA}"
    rx_reset="${COUNTER_RESET}"
    apply_delta "${cur_tx}" "${PREV_TX}"
    SAMPLE_D_TX="${DELTA}"
    if [[ ${rx_reset} -eq 1 ]] || [[ ${COUNTER_RESET} -eq 1 ]]; then
        RESET_COUNT=$((RESET_COUNT + 1))
    fi

    PREV_RX="${cur_rx}"
    PREV_TX="${cur_tx}"
    WIN_RX=$((WIN_RX + SAMPLE_D_RX))
    WIN_TX=$((WIN_TX + SAMPLE_D_TX))
}

sample_and_report() {
    local elapsed
    if ! accumulate_sample; then
        return 0
    fi
    elapsed=$(($(epoch_now) - WIN_START))
    printf '%s  %s  rx %s  tx %s  | window rx %s  tx %s  (%ss/%ss)\n' \
        "$(iso_now)" "${IFACE}" \
        "$(format_rate "${SAMPLE_D_RX}" "${REPORT_SECS}")" \
        "$(format_rate "${SAMPLE_D_TX}" "${REPORT_SECS}")" \
        "$(format_bytes "${WIN_RX}")" "$(format_bytes "${WIN_TX}")" \
        "${elapsed}" "${RESET_SECS}"
}

# Sleep for ${1} seconds in a way that a signal can cut short. A foreground `sleep` would
# make bash defer the trap until it finished, stalling shutdown by up to a report interval;
# `wait` on a background child is interruptible, so the handler runs immediately.
interruptible_sleep() {
    sleep "${1}" &
    SLEEP_PID=$!
    wait "${SLEEP_PID}" 2>/dev/null || true
    SLEEP_PID=0
}

on_signal() {
    trap - INT TERM
    if [[ ${SLEEP_PID} -ne 0 ]]; then
        kill "${SLEEP_PID}" 2>/dev/null || true
    fi
    # Sample once more before flushing, otherwise everything that moved since the last
    # tick is dropped from the partial row.
    accumulate_sample || true
    flush_window 1
    exit 0
}

main() {
    parse_args "$@"
    resolve_iface
    detect_source

    local sample
    if ! sample=$(read_counters); then
        die "cannot read counters for interface '${IFACE}' via ${SOURCE}"
    fi
    read -r PREV_RX PREV_TX <<<"${sample}"

    if [[ -n ${OUTPUT} ]]; then
        csv_init
    fi

    echo "INFO: watching ${IFACE} via ${SOURCE}, reporting every ${REPORT_SECS}s, resetting every ${RESET_SECS}s${OUTPUT:+, appending to ${OUTPUT}}" >&2

    trap on_signal INT TERM
    open_window

    # Deadlines are absolute multiples of the report interval from a fixed start, so the
    # loop does not drift by the cost of each sample.
    local start tick=0 next now
    start=$(epoch_now)
    while true; do
        tick=$((tick + 1))
        next=$((start + tick * REPORT_SECS))
        now=$(epoch_now)
        if [[ ${next} -gt ${now} ]]; then
            interruptible_sleep $((next - now))
        fi
        sample_and_report
        if [[ $(($(epoch_now) - WIN_START)) -ge ${RESET_SECS} ]]; then
            flush_window 0
            open_window
        fi
    done
}

if [[ ${BASH_SOURCE[0]} == "${0}" ]]; then
    main "$@"
fi
