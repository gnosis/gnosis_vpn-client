# Scripts

`vpn-smoke-test.sh`, `vpn-drain-tour.sh`, `vpn-drain-report.sh`, and `wg-traffic.sh` moved to
[`gnosis_vpn-testenv/scripts`](https://github.com/gnosis/gnosis_vpn-testenv/tree/main/scripts).

## `update-nix-git-hashes.sh`

Syncs `nix/gnosisvpn.nix`'s `outputHashes` with `Cargo.lock` after a dependency bump.

## `fetch-curvy-zk.sh`

Downloads the Curvy pix-aggregation proving files matching `Cargo.lock` into a directory; the system tests install them
at the default `curvy_zk_dir`. Update its version, names and digests when curvy-witnesscalc moves.
