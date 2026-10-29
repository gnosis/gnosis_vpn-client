#!/usr/bin/env bash
# Streams host resource state into the live job log, the only output that survives a lost runner.
set -uo pipefail

interval="${1:-5}"

sudo dmesg --follow --time-format iso 2>&1 | sed -u 's/^/[monitor] dmesg: /' &

while true; do
    {
        echo "--- $(date -u +%H:%M:%S)"
        free -m
        df -h / /tmp
        ps -eo rss,pid,comm --sort=-rss | head -6
        ss -s | head -2
        echo "nft rules: $(sudo nft list ruleset 2>/dev/null | wc -l)"
    } 2>&1 | sed 's/^/[monitor] /'
    sleep "${interval}"
done
