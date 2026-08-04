#!/usr/bin/env bats
# Unit, integration and end-to-end tests for scripts/wg-traffic.sh.

setup() {
    load helpers
    # shellcheck source=../wg-traffic.sh
    source "${SCRIPT}"
    SYSFS="${BATS_TEST_TMPDIR}/sys"
    CSV="${BATS_TEST_TMPDIR}/usage.csv"
}

# ---------------------------------------------------------------------------
# unit: parse_interval
# ---------------------------------------------------------------------------

@test "parse_interval accepts bare seconds" {
    [[ $(parse_interval 90) == "90" ]]
}

@test "parse_interval accepts second suffixes" {
    [[ $(parse_interval 30s) == "30" ]]
    [[ $(parse_interval 30sec) == "30" ]]
}

@test "parse_interval accepts minute suffixes" {
    [[ $(parse_interval 5m) == "300" ]]
    [[ $(parse_interval 5min) == "300" ]]
}

@test "parse_interval accepts hour and day suffixes" {
    [[ $(parse_interval 1h) == "3600" ]]
    [[ $(parse_interval 2d) == "172800" ]]
}

@test "parse_interval rejects garbage, negatives, zero and unknown units" {
    ! parse_interval abc
    ! parse_interval -5
    ! parse_interval 0
    ! parse_interval 5x
    ! parse_interval ""
}

# ---------------------------------------------------------------------------
# unit: format_bytes / format_rate
# ---------------------------------------------------------------------------

@test "format_bytes stays in whole bytes below 1 kB" {
    [[ $(format_bytes 0) == "0 B" ]]
    [[ $(format_bytes 999) == "999 B" ]]
}

@test "format_bytes uses decimal units" {
    [[ $(format_bytes 1000) == "1.00 kB" ]]
    [[ $(format_bytes 1000000) == "1.00 MB" ]]
    [[ $(format_bytes 1000000000) == "1.00 GB" ]]
    [[ $(format_bytes 1073741824) == "1.07 GB" ]]
}

@test "format_bytes handles values above 2^31 without overflow" {
    [[ $(format_bytes 5000000000) == "5.00 GB" ]]
    [[ $(format_bytes 12345678901234) == "12.35 TB" ]]
}

@test "format_rate divides by the interval" {
    [[ $(format_rate 2000000 2) == "1.00 MB/s" ]]
    [[ $(format_rate 0 60) == "0 B/s" ]]
}

@test "format_rate guards against a zero-length interval" {
    [[ $(format_rate 1000 0) == "0 B/s" ]]
}

@test "format_rate shows a fraction instead of rounding measurable traffic down to zero" {
    [[ $(format_rate 1 10) == "0.10 B/s" ]]
}

# ---------------------------------------------------------------------------
# unit: apply_delta
# ---------------------------------------------------------------------------

@test "apply_delta returns the increase for a normal sample" {
    apply_delta 5000 3000
    [[ ${DELTA} == "2000" ]]
    [[ ${COUNTER_RESET} == "0" ]]
}

@test "apply_delta returns zero for an unchanged counter" {
    apply_delta 3000 3000
    [[ ${DELTA} == "0" ]]
    [[ ${COUNTER_RESET} == "0" ]]
}

@test "apply_delta counts the new counter value after an interface restart" {
    apply_delta 120 999999
    [[ ${DELTA} == "120" ]]
    [[ ${COUNTER_RESET} == "1" ]]
}

@test "apply_delta flags a restart even when the new counter is zero" {
    apply_delta 0 999999
    [[ ${DELTA} == "0" ]]
    [[ ${COUNTER_RESET} == "1" ]]
}

# ---------------------------------------------------------------------------
# unit: csv_notes
# ---------------------------------------------------------------------------

@test "csv_notes is empty for a clean window" {
    [[ -z $(csv_notes 0 0 0) ]]
}

@test "csv_notes reports counter resets" {
    [[ $(csv_notes 2 0 0) == "counter-reset x2" ]]
}

@test "csv_notes reports downtime" {
    [[ $(csv_notes 0 42 0) == "iface-down 42s" ]]
}

@test "csv_notes combines markers without using commas" {
    run csv_notes 2 42 1
    [[ ${output} == "counter-reset x2; iface-down 42s; partial" ]]
    [[ ${output} != *,* ]]
}

# ---------------------------------------------------------------------------
# unit: read_counters via the sysfs source
# ---------------------------------------------------------------------------

@test "read_counters reads a sysfs interface" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 111 222
    SOURCE=sysfs
    SYSFS_ROOT="${SYSFS}"
    IFACE=wg0_gnosisvpn
    [[ $(read_counters) == "111 222" ]]
}

@test "read_counters fails for a missing sysfs interface" {
    mkdir -p "${SYSFS}"
    SOURCE=sysfs
    SYSFS_ROOT="${SYSFS}"
    IFACE=absent
    run read_counters
    [[ ${status} -ne 0 ]]
}

@test "read_counters fails on a non-numeric counter" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn "not-a-number" 222
    SOURCE=sysfs
    SYSFS_ROOT="${SYSFS}"
    IFACE=wg0_gnosisvpn
    run read_counters
    [[ ${status} -ne 0 ]]
}

# ---------------------------------------------------------------------------
# integration: argument validation
# ---------------------------------------------------------------------------

@test "rejects a report interval longer than the reset interval" {
    run "${SCRIPT}" --sysfs-root "${SYSFS}" --report 2h --reset 1h
    [[ ${status} -ne 0 ]]
    [[ ${output} == *"report interval"* ]]
}

@test "rejects an unknown flag" {
    run "${SCRIPT}" --bogus
    [[ ${status} -ne 0 ]]
    [[ ${output} == *"unknown option"* ]]
}

@test "rejects an invalid interval spec" {
    run "${SCRIPT}" --report nonsense
    [[ ${status} -ne 0 ]]
    [[ ${output} == *"invalid"* ]]
}

@test "rejects an output path in a non-existent directory" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    run "${SCRIPT}" --sysfs-root "${SYSFS}" --output "${BATS_TEST_TMPDIR}/nope/usage.csv"
    [[ ${status} -ne 0 ]]
    [[ ${output} == *"not writable"* ]]
}

@test "rejects an output path that is already a directory" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    mkdir -p "${CSV}"
    run "${SCRIPT}" --sysfs-root "${SYSFS}" --output "${CSV}"
    [[ ${status} -ne 0 ]]
    [[ ${output} == *"not a writable regular file"* ]]
}

@test "rejects an output path that is not writable" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    : >"${CSV}"
    chmod 400 "${CSV}"
    run "${SCRIPT}" --sysfs-root "${SYSFS}" --output "${CSV}"
    [[ ${status} -ne 0 ]]
    [[ ${output} == *"not a writable regular file"* ]]
}

@test "exits non-zero when the interface is unreadable at startup" {
    mkdir -p "${SYSFS}"
    run "${SCRIPT}" --sysfs-root "${SYSFS}" --iface absent --report 1 --reset 1
    [[ ${status} -ne 0 ]]
    [[ ${output} == *"absent"* ]]
}

@test "--help exits zero and documents the flags" {
    run "${SCRIPT}" --help
    [[ ${status} -eq 0 ]]
    [[ ${output} == *"--reset"* ]]
    [[ ${output} == *"--report"* ]]
    [[ ${output} == *"--output"* ]]
}

# ---------------------------------------------------------------------------
# integration: CSV handling
# ---------------------------------------------------------------------------

@test "writes a header when creating the file" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 1 --output "${CSV}" >/dev/null 2>&1 &
    local pid=$!
    wait_for_rows "${CSV}" 1 10
    kill -TERM "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
    [[ $(head -n 1 "${CSV}") == "${CSV_HEADER}" ]]
    [[ $(grep -c '^start_iso' "${CSV}") -eq 1 ]]
}

@test "does not repeat the header when appending to an existing file" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    echo "${CSV_HEADER}" >"${CSV}"
    echo "old,row,1,wg0_gnosisvpn,1,1,1 B,1 B," >>"${CSV}"
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 1 --output "${CSV}" >/dev/null 2>&1 &
    local pid=$!
    wait_for_rows "${CSV}" 2 10
    kill -TERM "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
    [[ $(grep -c '^start_iso' "${CSV}") -eq 1 ]]
    [[ $(csv_rows "${CSV}" | head -n 1) == "old,row,1,wg0_gnosisvpn,1,1,1 B,1 B," ]]
}

@test "writes a header when the existing file is empty" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    : >"${CSV}"
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 1 --output "${CSV}" >/dev/null 2>&1 &
    local pid=$!
    wait_for_rows "${CSV}" 1 10
    kill -TERM "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
    [[ $(head -n 1 "${CSV}") == "${CSV_HEADER}" ]]
}

@test "each row has nine fields and a clean window leaves notes empty" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 100 100
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 1 --output "${CSV}" >/dev/null 2>&1 &
    local pid=$!
    wait_for_rows "${CSV}" 1 10
    kill -TERM "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
    local row
    row=$(csv_rows "${CSV}" | head -n 1)
    [[ $(echo "${row}" | awk -F, '{ print NF }') -eq 9 ]]
    [[ $(echo "${row}" | cut -d, -f4) == "wg0_gnosisvpn" ]]
    [[ $(echo "${row}" | cut -d, -f9) == "" ]]
}

@test "SIGTERM flushes the partial window instead of discarding it" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 3600 --output "${CSV}" >/dev/null 2>&1 &
    local pid=$!
    sleep 2
    set_fake_counters "${SYSFS}" wg0_gnosisvpn 7000 3000
    sleep 2
    kill -TERM "${pid}"
    wait "${pid}" 2>/dev/null || true
    [[ $(csv_rows "${CSV}" | wc -l) -eq 1 ]]
    local row
    row=$(csv_rows "${CSV}" | head -n 1)
    [[ $(echo "${row}" | cut -d, -f5) == "7000" ]]
    [[ $(echo "${row}" | cut -d, -f6) == "3000" ]]
    [[ $(echo "${row}" | cut -d, -f9) == *partial* ]]
}

@test "SIGTERM samples once more so bytes since the last tick are not lost" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 0 0
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 30 --reset 3600 --output "${CSV}" >/dev/null 2>&1 &
    local pid=$!
    sleep 1
    # The report interval is long enough that no regular tick can observe this change,
    # so only the final sample taken by the signal handler can account for it.
    set_fake_counters "${SYSFS}" wg0_gnosisvpn 4242 2121
    kill -TERM "${pid}"
    wait "${pid}" 2>/dev/null || true
    local row
    row=$(csv_rows "${CSV}" | head -n 1)
    [[ $(echo "${row}" | cut -d, -f5) == "4242" ]]
    [[ $(echo "${row}" | cut -d, -f6) == "2121" ]]
}

@test "an interface disappearing mid-run is recorded but does not stop the script" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 500 500
    local errlog="${BATS_TEST_TMPDIR}/stderr"
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 3600 --output "${CSV}" \
        >/dev/null 2>"${errlog}" &
    local pid=$!
    sleep 2
    rm -rf "${SYSFS}/wg0_gnosisvpn"
    sleep 3
    kill -0 "${pid}"
    kill -TERM "${pid}"
    wait "${pid}" 2>/dev/null || true
    [[ $(csv_rows "${CSV}" | cut -d, -f9) == *"iface-down"* ]]
    grep -q "not readable" "${errlog}"
}

# ---------------------------------------------------------------------------
# e2e
# ---------------------------------------------------------------------------

@test "e2e: accumulated bytes telescope to the total counter increase" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 1000 100
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 3 --output "${CSV}" \
        >"${BATS_TEST_TMPDIR}/stdout" 2>&1 &
    local pid=$!
    sleep 1
    set_fake_counters "${SYSFS}" wg0_gnosisvpn 2000 200
    sleep 1
    set_fake_counters "${SYSFS}" wg0_gnosisvpn 3000 300
    sleep 1
    set_fake_counters "${SYSFS}" wg0_gnosisvpn 5000 500
    sleep 5
    kill -TERM "${pid}"
    wait "${pid}" 2>/dev/null || true

    # Sample-and-accumulate telescopes: every byte counted once, across all rows.
    [[ $(csv_sum 5 "${CSV}") -eq 4000 ]]
    [[ $(csv_sum 6 "${CSV}") -eq 400 ]]
    [[ $(csv_rows "${CSV}" | wc -l) -ge 2 ]]
    [[ $(grep -c 'window rx' "${BATS_TEST_TMPDIR}/stdout") -ge 5 ]]
}

@test "e2e: a counter restart mid-window is flagged in notes" {
    make_fake_iface "${SYSFS}" wg0_gnosisvpn 900000 900000
    "${SCRIPT}" --sysfs-root "${SYSFS}" --report 1 --reset 3600 --output "${CSV}" \
        >/dev/null 2>&1 &
    local pid=$!
    sleep 2
    set_fake_counters "${SYSFS}" wg0_gnosisvpn 250 125
    sleep 2
    kill -TERM "${pid}"
    wait "${pid}" 2>/dev/null || true
    local row
    row=$(csv_rows "${CSV}" | head -n 1)
    [[ $(echo "${row}" | cut -d, -f9) == *"counter-reset x1"* ]]
    # Post-restart bytes are counted; pre-teardown bytes are unrecoverable.
    [[ $(echo "${row}" | cut -d, -f5) == "250" ]]
}
