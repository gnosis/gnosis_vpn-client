#!/usr/bin/env bats
# Offline tests for scripts/vpn-drain-report.sh: runs it against small runs.jsonl fixtures and asserts on the generated report.html - no network, no gnosis_vpn-ctl.

setup() {
    SCRIPT="${BATS_TEST_DIRNAME}/../vpn-drain-report.sh"
    RUN_DIR="${BATS_TEST_TMPDIR}/run"
    mkdir -p "$RUN_DIR"
}

write_fixture() {
    cat >"$RUN_DIR/runs.jsonl"
}

@test "--help exits 0 with usage" {
    run "$SCRIPT" --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"Usage: vpn-drain-report.sh"* ]]
}

@test "missing --run-dir is an error" {
    run "$SCRIPT"
    [ "$status" -eq 1 ]
    [[ "$output" == *"--run-dir is required"* ]]
}

@test "missing runs.jsonl in the run dir is an error" {
    run "$SCRIPT" --run-dir "${BATS_TEST_TMPDIR}/does-not-exist"
    [ "$status" -eq 1 ]
    [[ "$output" == *"runs.jsonl not found"* ]]
}

@test "renders a report with a comparison row, heatmap and per-destination section for each destination" {
    write_fixture <<'JSONL'
{"round":1,"destination_id":"de1","address":"0xAAA","selection":{"capacity_used":1,"capacity_available":10,"ping_rtt_ms":40.0},"outcome":"connected","reason":null,"metrics":{"gateway_ping_avg_ms":12.3,"packet_loss_pct":0,"https_ok":3,"https_total":3,"downloads":{},"streaming_bps":500000,"egress_ip":"1.2.3.4","egress_loc":"DE"},"funding_status_snapshot":{"traffic":"Good","gas":"Good"},"started_at":"t1","ended_at":"t2"}
{"round":1,"destination_id":"fr1","address":"0xBBB","selection":{"capacity_used":null,"capacity_available":null,"ping_rtt_ms":null},"outcome":"skipped","reason":"state: NeedsChannel","metrics":{},"funding_status_snapshot":{"traffic":"Good","gas":"Good"},"started_at":"t1","ended_at":"t1"}
{"round":2,"destination_id":"de1","address":"0xAAA","selection":{"capacity_used":2,"capacity_available":10,"ping_rtt_ms":42.0},"outcome":"connected","reason":null,"metrics":{"gateway_ping_avg_ms":15.1,"packet_loss_pct":0,"https_ok":3,"https_total":3,"downloads":{},"streaming_bps":480000,"egress_ip":"1.2.3.4","egress_loc":"DE"},"funding_status_snapshot":{"traffic":"Low","gas":"Good"},"started_at":"t3","ended_at":"t4"}
JSONL

    run "$SCRIPT" --run-dir "$RUN_DIR"
    [ "$status" -eq 0 ]
    [ -f "$RUN_DIR/report.html" ]

    run grep -c 'dest-de1' "$RUN_DIR/report.html"
    [ "$output" -gt 0 ]
    run grep -c 'dest-fr1' "$RUN_DIR/report.html"
    [ "$output" -gt 0 ]
    run grep -c 'class="chart heatmap"' "$RUN_DIR/report.html"
    [ "$output" -eq 1 ]
    run grep -c '>null<' "$RUN_DIR/report.html"
    [ "$output" -eq 0 ]
}

@test "handles an empty runs.jsonl without error" {
    : >"$RUN_DIR/runs.jsonl"
    run "$SCRIPT" --run-dir "$RUN_DIR"
    [ "$status" -eq 0 ]
    [ -f "$RUN_DIR/report.html" ]
    run grep -c 'chart-empty' "$RUN_DIR/report.html"
    [ "$output" -gt 0 ]
}

@test "--out writes the report to a custom path" {
    write_fixture <<'JSONL'
{"round":1,"destination_id":"de1","address":"0xAAA","selection":{"capacity_used":1,"capacity_available":10,"ping_rtt_ms":40.0},"outcome":"connected","reason":null,"metrics":{"gateway_ping_avg_ms":12.3,"packet_loss_pct":0,"https_ok":3,"https_total":3,"downloads":{},"streaming_bps":500000,"egress_ip":"1.2.3.4","egress_loc":"DE"},"funding_status_snapshot":{"traffic":"Good","gas":"Good"},"started_at":"t1","ended_at":"t2"}
JSONL
    local custom="${BATS_TEST_TMPDIR}/custom.html"
    run "$SCRIPT" --run-dir "$RUN_DIR" --out "$custom"
    [ "$status" -eq 0 ]
    [ -f "$custom" ]
    [ ! -f "$RUN_DIR/report.html" ]
}
