# Shared helpers for the scripts/tests bats suites.

# Consumed by the .bats suites that load this file, which shellcheck cannot see.
# shellcheck disable=SC2034
SCRIPT="${BATS_TEST_DIRNAME}/../wg-traffic.sh"

# Create a fake sysfs interface tree.
#   $1 = sysfs root, $2 = interface name, $3 = rx_bytes, $4 = tx_bytes
make_fake_iface() {
    mkdir -p "${1}/${2}/statistics"
    set_fake_counters "$@"
}

# Overwrite the counters of an existing fake interface.
#   $1 = sysfs root, $2 = interface name, $3 = rx_bytes, $4 = tx_bytes
set_fake_counters() {
    echo "${3}" >"${1}/${2}/statistics/rx_bytes"
    echo "${4}" >"${1}/${2}/statistics/tx_bytes"
}

# Emit the data rows of a CSV file, skipping the header.
csv_rows() {
    grep -v '^start_iso' "${1}" || true
}

# Sum one numeric CSV column across all data rows.
#   $1 = column number (1-based), $2 = csv path
csv_sum() {
    csv_rows "${2}" | awk -F, -v col="${1}" '{ total += $col } END { print total + 0 }'
}

# Wait until a file has at least N data rows, or fail after a timeout.
#   $1 = csv path, $2 = expected row count, $3 = timeout in seconds
wait_for_rows() {
    local path="${1}" want="${2}" timeout="${3}" waited=0
    while [[ ${waited} -lt ${timeout} ]]; do
        if [[ -f ${path} ]] && [[ $(csv_rows "${path}" | wc -l) -ge ${want} ]]; then
            return 0
        fi
        sleep 1
        waited=$((waited + 1))
    done
    return 1
}
