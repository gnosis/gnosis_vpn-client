# Scripts

Operational/diagnostic tooling for a running Gnosis VPN client. All are standalone bash - no build step.

`vpn-smoke-test.sh`, `vpn-drain-tour.sh`, and `vpn-drain-report.sh` moved to
[`gnosis_vpn-testenv/scripts`](https://github.com/gnosis/gnosis_vpn-testenv/tree/main/scripts).

## `wg-traffic.sh`

Samples a WireGuard interface's byte counters on an interval and logs per-window totals to CSV. `./wg-traffic.sh --help` for options.

## `update-nix-git-hashes.sh`

Syncs `nix/gnosisvpn.nix`'s `outputHashes` with `Cargo.lock` after a dependency bump.

## Tests

`scripts/tests/wg-traffic.bats` covers `wg-traffic.sh` offline, using fake sysfs interfaces (`scripts/tests/helpers.bash`). Run with `bats scripts/tests/`.
