#!/usr/bin/env bats
#
# Offline tests for scripts/vpn-smoke-test.sh. Pure helpers are exercised by
# sourcing the script; whole-run behavior is exercised by executing it with the
# curl/ping fakes injected via GVPN_CURL / GVPN_PING. No network is used.

setup() {
    SCRIPT="${BATS_TEST_DIRNAME}/../vpn-smoke-test.sh"
    FAKES="${BATS_TEST_DIRNAME}/fakes"
    export GVPN_CURL="${FAKES}/curl"
    export GVPN_PING="${FAKES}/ping"
}

# --- unit: pure helpers -----------------------------------------------------

@test "human_bytes formats B / KB / MB" {
    run bash -c "source '$SCRIPT'; human_bytes 512"
    [ "$output" = "512 B" ]
    run bash -c "source '$SCRIPT'; human_bytes 1536"
    [ "$output" = "1.5 KB" ]
    run bash -c "source '$SCRIPT'; human_bytes 1572864"
    [ "$output" = "1.5 MB" ]
}

@test "human_rate appends /s" {
    run bash -c "source '$SCRIPT'; human_rate 1048576"
    [ "$output" = "1.0 MB/s" ]
}

@test "ping_argv linux uses -W timeout and -t ttl" {
    run bash -c "source '$SCRIPT'; ping_argv linux 3 15 6 10.128.0.1"
    [ "$output" = "-c 3 -W 15 -t 6 10.128.0.1" ]
}

@test "ping_argv macos uses -t timeout and -m ttl" {
    run bash -c "source '$SCRIPT'; ping_argv macos 3 15 6 10.128.0.1"
    [ "$output" = "-c 3 -t 15 -m 6 10.128.0.1" ]
}

@test "mtu_argv sets don't-fragment per platform" {
    run bash -c "source '$SCRIPT'; mtu_argv linux 15 1392 10.128.0.1"
    [ "$output" = "-c 1 -W 15 -M do -s 1392 10.128.0.1" ]
    run bash -c "source '$SCRIPT'; mtu_argv macos 15 1392 10.128.0.1"
    [ "$output" = "-c 1 -t 15 -D -s 1392 10.128.0.1" ]
}

@test "parse_rtt_avg reads Linux rtt line" {
    run bash -c "source '$SCRIPT'; parse_rtt_avg 'rtt min/avg/max/mdev = 9.458/10.937/13.208/1.629 ms'"
    [ "$output" = "10.937" ]
}

@test "parse_rtt_avg reads macOS round-trip line" {
    run bash -c "source '$SCRIPT'; parse_rtt_avg 'round-trip min/avg/max/stddev = 17.999/33.538/57.212/17.011 ms'"
    [ "$output" = "33.538" ]
}

@test "parse_loss extracts loss percent" {
    run bash -c "source '$SCRIPT'; parse_loss '3 packets transmitted, 3 received, 0% packet loss, time 2003ms'"
    [ "$output" = "0" ]
    run bash -c "source '$SCRIPT'; parse_loss '3 packets transmitted, 1 received, 66.6% packet loss'"
    [ "$output" = "66.6" ]
}

# --- cli surface ------------------------------------------------------------

@test "--help exits 0 with usage" {
    run "$SCRIPT" --help
    [ "$status" -eq 0 ]
    [[ "$output" == *"Usage: vpn-smoke-test.sh"* ]]
}

@test "unknown option exits 2" {
    run "$SCRIPT" --bogus
    [ "$status" -eq 2 ]
}

@test "value-taking options reject a missing value" {
    for option in \
        -g --gateway --iface --baseline-ip --targets --sizes \
        --stream-secs --http-timeout --dl-timeout --ping-timeout \
        --down-url --trace-url; do
        run "$SCRIPT" "$option"
        [ "$status" -eq 2 ]
        [[ "$output" == *"Option $option requires a value."* ]]
        [[ "$output" == *"Usage: vpn-smoke-test.sh"* ]]
    done
}

@test "missing curl aborts with code 2" {
    export GVPN_CURL="/nonexistent/curl"
    run "$SCRIPT" --no-color
    [ "$status" -eq 2 ]
    [[ "$output" == *"curl not found"* ]]
}

# --- integration: whole run with fakes --------------------------------------

@test "healthy fakes: everything passes, exit 0" {
    run "$SCRIPT" --no-color
    [ "$status" -eq 0 ]
    [[ "$output" == *"tunnel ping"*"avg 10.937 ms"* ]]
    [[ "$output" == *"download 3.0 MB"* ]]
    [[ "$output" == *"streaming"*"time-capped"* ]]
    [[ "$output" == *"egress ip"*"203.0.113.7"* ]]
    [[ "$output" == *"ipv6 blocked"*"blackholed or unavailable"* ]]
    [[ "$output" == *"passed"* ]]
}

@test "--quick skips the 3 MB download" {
    run "$SCRIPT" --no-color --quick
    [ "$status" -eq 0 ]
    [[ "$output" == *"download 3.0 MB"*"--quick"* ]]
}

@test "sized downloads request the exact byte counts" {
    export FAKE_LOG="${BATS_TEST_TMPDIR}/curl.log"
    run "$SCRIPT" --no-color
    [ "$status" -eq 0 ]
    grep -q "__down?bytes=1024$" "$FAKE_LOG"
    grep -q "__down?bytes=3145728$" "$FAKE_LOG"
    grep -q "__down?bytes=52428800$" "$FAKE_LOG"
}

@test "DNS failure for one host fails the run" {
    export FAKE_CURL_DNS_FAIL="openbsd.org"
    run "$SCRIPT" --no-color
    [ "$status" -eq 1 ]
    [[ "$output" == *"resolve openbsd.org"*"DNS resolution failed"* ]]
}

@test "reachable DNS but dead site: dns passes, https fails" {
    export FAKE_CURL_HTTP_FAIL="freebsd.org"
    run "$SCRIPT" --no-color
    [ "$status" -eq 1 ]
    [[ "$output" == *"dns"*"resolved"* ]]
    [[ "$output" == *"GET freebsd.org"* ]]
}

@test "short download warns but does not fail" {
    export FAKE_CURL_SHORT=1
    run "$SCRIPT" --no-color
    [ "$status" -eq 0 ]
    [[ "$output" == *"download"*"short"* ]]
}

@test "stalled stream fails the run" {
    export FAKE_CURL_STREAM_STALL=1
    run "$SCRIPT" --no-color
    [ "$status" -eq 1 ]
    [[ "$output" == *"streaming"*"no sustained data"* ]]
}

@test "reachable IPv6 warns about a possible leak" {
    export FAKE_CURL_IPV6_OK=1
    run "$SCRIPT" --no-color
    [ "$status" -eq 0 ]
    [[ "$output" == *"ipv6 blocked"*"possible leak"* ]]
}

@test "egress matching the baseline fails the run" {
    export FAKE_CURL_EGRESS_IP="198.51.100.9"
    run "$SCRIPT" --no-color --baseline-ip 198.51.100.9
    [ "$status" -eq 1 ]
    [[ "$output" == *"NOT routed via VPN"* ]]
}

@test "unreachable gateway fails and skips loss + mtu" {
    export FAKE_PING_FAIL=1
    run "$SCRIPT" --no-color
    [ "$status" -eq 1 ]
    [[ "$output" == *"tunnel ping"*"unreachable"* ]]
    [[ "$output" == *"packet loss"*"gateway unreachable"* ]]
    [[ "$output" == *"mtu probe"*"gateway unreachable"* ]]
}

@test "packet loss above threshold warns" {
    export FAKE_PING_LOSS=50
    run "$SCRIPT" --no-color
    [ "$status" -eq 0 ]
    [[ "$output" == *"packet loss"*"50% over 10 packets"* ]]
}

@test "MTU fragmentation warns" {
    export FAKE_PING_MTU_FRAG=1
    run "$SCRIPT" --no-color
    [[ "$output" == *"mtu probe"*"fragmentation"* ]]
}
