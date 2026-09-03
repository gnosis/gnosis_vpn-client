# Scripts

`vpn-smoke-test.sh`, `vpn-drain-tour.sh`, `vpn-drain-report.sh`, and `wg-traffic.sh` moved to
[`gnosis_vpn-testenv/scripts`](https://github.com/gnosis/gnosis_vpn-testenv/tree/main/scripts).

## `update-nix-git-hashes.sh`

Syncs `nix/gnosisvpn.nix`'s `outputHashes` with `Cargo.lock` after a dependency bump.
