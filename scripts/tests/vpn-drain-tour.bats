#!/usr/bin/env bats
# Offline tests for scripts/vpn-drain-tour.sh: pure helpers via sourcing, whole-run behavior via the curl/ping/gnosis_vpn-ctl fakes injected through GVPN_CURL/GVPN_PING/GVPN_CTL - no network or real gnosis_vpn-ctl.

setup() {
    SCRIPT="${BATS_TEST_DIRNAME}/../vpn-drain-tour.sh"
    FAKES="${BATS_TEST_DIRNAME}/fakes"
    export GVPN_CURL="${FAKES}/curl"
    export GVPN_PING="${FAKES}/ping"
    export GVPN_CTL="${FAKES}/gnosis_vpn-ctl"
    export FAKE_CTL_STATE="${BATS_TEST_TMPDIR}/ctl-state"
    OUT_DIR="${BATS_TEST_TMPDIR}/run"
}

# --- unit: ranking -----------------------------------------------------

@test "rank_destinations sorts ReadyToConnect by used-capacity fraction then latency" {
    local status_json
    status_json='{"Status":{"destinations":[
        {"destination":{"id":"high-use","address":"0xA"},
         "route_health":{"state":{"state":"ReadyToConnect","exit":{"ping_rtt":10,"health":{"slots":{"available":10,"connected":9}}}}}},
        {"destination":{"id":"low-use","address":"0xB"},
         "route_health":{"state":{"state":"ReadyToConnect","exit":{"ping_rtt":50,"health":{"slots":{"available":10,"connected":1}}}}}},
        {"destination":{"id":"no-channel","address":"0xC"},
         "route_health":{"state":{"state":"NeedsChannel"}}}
    ]}}'
    run bash -c "source '$SCRIPT'; rank_destinations '$status_json' | jq -c '[.[].id]'"
    [ "$status" -eq 0 ]
    [ "$output" = '["low-use","high-use","no-channel"]' ]
}

@test "rank_destinations puts Routable (no exit data) after known-capacity destinations" {
    local status_json
    status_json='{"Status":{"destinations":[
        {"destination":{"id":"routable","address":"0xA"}, "route_health":{"state":{"state":"Routable"}}},
        {"destination":{"id":"known","address":"0xB"},
         "route_health":{"state":{"state":"ReadyToConnect","exit":{"ping_rtt":10,"health":{"slots":{"available":10,"connected":5}}}}}}
    ]}}'
    run bash -c "source '$SCRIPT'; rank_destinations '$status_json' | jq -c '[.[].id]'"
    [ "$status" -eq 0 ]
    [ "$output" = '["known","routable"]' ]
}

@test "skip_reason prefers last_error over the bare state name" {
    run bash -c "source '$SCRIPT'; skip_reason '{\"state\":\"Unrecoverable\",\"last_error\":\"not allowed\"}'"
    [ "$output" = "not allowed" ]
    run bash -c "source '$SCRIPT'; skip_reason '{\"state\":\"NeedsChannel\",\"last_error\":null}'"
    [ "$output" = "state: NeedsChannel" ]
}

# --- cli surface ------------------------------------------------------------

@test "--help exits 0 with usage" {
    run "$SCRIPT" --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"Usage: vpn-drain-tour.sh"* ]]
}

@test "unknown option exits 2" {
    run "$SCRIPT" --bogus
    [ "$status" -eq 2 ]
}

@test "value-taking options reject a missing value" {
    for option in --out-dir --max-rounds --connect-timeout --poll-interval -s --socket-path --targets --sizes; do
        run "$SCRIPT" "$option"
        [ "$status" -eq 2 ]
    done
}

@test "a missing dependency aborts with an apt-get hint" {
    export PATH="${FAKES}:${PATH}"
    export GVPN_CURL="${BATS_TEST_TMPDIR}/no-such-curl"
    run "$SCRIPT" --out-dir "${BATS_TEST_TMPDIR}/run" --max-rounds 1
    [ "$status" -eq 2 ]
    [[ "$output" == *"missing"* ]]
    [[ "$output" == *"apt-get install"* ]]
    [[ "$output" == *curl* ]]
}

@test "--install-deps apt-get installs the missing package and continues" {
    export PATH="${FAKES}:${PATH}"
    export FAKE_APT_GET_LOG="${BATS_TEST_TMPDIR}/apt-get.log"
    export GVPN_CURL="${BATS_TEST_TMPDIR}/no-such-curl"
    export FAKE_CTL_EMPTY_AFTER=1

    run "$SCRIPT" --out-dir "${BATS_TEST_TMPDIR}/run" --max-rounds 1 --install-deps
    [ "$status" -eq 0 ]
    [ -f "$FAKE_APT_GET_LOG" ]
    run grep -c "install -y curl" "$FAKE_APT_GET_LOG"
    [ "$output" -eq 1 ]
}

# --- whole run ---------------------------------------------------------

@test "tour connects best-first, records a rejected connect, skips a needs-channel destination, and stops when funds drain" {
    export FAKE_CTL_FAIL_CONNECT="us1"
    export FAKE_CTL_EMPTY_AFTER=3

    run "$SCRIPT" --out-dir "$OUT_DIR" --max-rounds 5 --quick --poll-interval 1 --connect-timeout 5
    [ "$status" -eq 0 ]
    [[ "$output" == *"funds drained: traffic funding status is Empty"* ]]

    [ -f "$OUT_DIR/runs.jsonl" ]
    run jq -s 'length' "$OUT_DIR/runs.jsonl"
    [ "$output" -eq 6 ]

    run jq -s '[.[] | select(.outcome=="connected")] | length' "$OUT_DIR/runs.jsonl"
    [ "$output" -eq 2 ]
    run jq -s '[.[] | select(.outcome=="failed" and .destination_id=="us1")] | length' "$OUT_DIR/runs.jsonl"
    [ "$output" -eq 2 ]
    run jq -s '[.[] | select(.outcome=="skipped" and .destination_id=="fr1")] | length' "$OUT_DIR/runs.jsonl"
    [ "$output" -eq 2 ]

    # de1 (used-capacity fraction 0.2) is attempted before us1 (0.8) every round.
    run jq -sc '[.[] | select(.round==1) | .destination_id]' "$OUT_DIR/runs.jsonl"
    [ "$output" = '["de1","us1","fr1"]' ]

    [ "$(wc -l <"$OUT_DIR/metrics.csv")" -eq 7 ] # header + 6 attempts
}

@test "tour records a timeout when connect succeeds but the tunnel never comes up" {
    export FAKE_CTL_STALL_CONNECT="de1 us1"

    run "$SCRIPT" --out-dir "$OUT_DIR" --max-rounds 1 --quick --poll-interval 1 --connect-timeout 2
    [ "$status" -eq 0 ]
    [[ "$output" == *"no destination connectable this round"* ]]

    run jq -s '[.[] | select(.outcome=="failed") | .reason] | unique' "$OUT_DIR/runs.jsonl"
    [[ "$output" == *"did not come up"* ]]
}

@test "tour stops at --max-rounds when funding never drains" {
    export FAKE_CTL_EMPTY_AFTER=0

    run "$SCRIPT" --out-dir "$OUT_DIR" --max-rounds 2 --quick --poll-interval 1 --connect-timeout 5
    [ "$status" -eq 0 ]
    [[ "$output" == *"hit --max-rounds (2)"* ]]

    run jq -s '[.[].round] | unique | length' "$OUT_DIR/runs.jsonl"
    [ "$output" -eq 2 ]
}
