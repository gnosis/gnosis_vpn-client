# Scripts

Operational/diagnostic tooling for a running Gnosis VPN client. All are standalone bash - no build step.

## Fetching just these scripts onto a fresh machine

`vpn-drain-tour.sh` sources `vpn-smoke-test.sh` for shared helpers, so grab all three together:

```bash
mkdir -p gnosis-vpn-scripts && cd gnosis-vpn-scripts && \
for f in vpn-smoke-test.sh vpn-drain-tour.sh vpn-drain-report.sh; do
  curl -fsSL -o "$f" "https://raw.githubusercontent.com/gnosis/gnosis_vpn-client/main/scripts/$f"
done && \
chmod +x vpn-smoke-test.sh vpn-drain-tour.sh vpn-drain-report.sh
```

On a fresh Debian/Ubuntu box, `./vpn-drain-tour.sh --install-deps ...` installs the missing `jq`/`curl`/`ping`/`awk` packages via `apt-get` on first run; `gnosis_vpn-ctl` itself still needs to be installed/built separately (it's this project's own binary, not an apt package).

## `vpn-smoke-test.sh`

Validates connectivity on an already-connected tunnel: gateway ping/loss, path MTU, DNS, HTTPS reachability, sized downloads, a sustained transfer, egress IP/geo, and an IPv6 leak check. `./vpn-smoke-test.sh --help` for options.

## `vpn-drain-tour.sh`

Each round, connects to every configured destination once (best exit-capacity/latency first, recording why any destination can't connect), smoke-tests it, then repeats rounds until `gnosis_vpn-ctl balance` reports funding as `Empty` or nothing connects. Writes raw `runs.jsonl`/`metrics.csv` to an output directory. `./vpn-drain-tour.sh --help` for options.

## `vpn-drain-report.sh`

Turns a `vpn-drain-tour.sh` run directory into one self-contained `report.html`: a cross-destination comparison table, an outcome heatmap, latency/throughput trend lines, a funding drain timeline, and per-destination histogram detail sections - printable straight to PDF from a browser. `./vpn-drain-report.sh --run-dir <dir>`.

## `wg-traffic.sh`

Samples a WireGuard interface's byte counters on an interval and logs per-window totals to CSV. `./wg-traffic.sh --help` for options.

## `update-nix-git-hashes.sh`

Syncs `nix/gnosisvpn.nix`'s `outputHashes` with `Cargo.lock` after a dependency bump.

## Tests

`scripts/tests/*.bats` cover the scripts above offline, using fakes in `scripts/tests/fakes/` for `curl`/`ping`/`gnosis_vpn-ctl`/`apt-get`/`sudo`. Run with `bats scripts/tests/`.
