# Branch strategy

## Lines and networks

`main` is the only release line. It embeds hoprd v5 via edge-client and runs on the piz-palu networks.

| Branch            | Versions                       | hoprd | Network           | Blokli endpoint                             |
| ----------------- | ------------------------------ | ----- | ----------------- | ------------------------------------------- |
| `main`            | `0.100.0` and up, `1.0.0-rc.N` | v5    | piz-palu-dev (CI) | `https://blokli.piz-palu.dev.gnosisvpn.io/` |
| `release/hoprdv4` | frozen at `0.98.x`             | v4    | jura              | —                                           |

`release/hoprdv4` carried the hoprd v4 line, kept below `0.100.0` because hoprd v4 and v5 can't be dual-pinned in one
resolution graph. Its last release is 0.98.1 (installer 0.97.1, the final v4 stable). It takes no more changes, and the
backport/forwardport automation between the two branches is gone.

`pr.yml` picks the CI network via `SYSTEM_TEST_NETWORK`, which selects the non-secret fixture in
`gnosis_vpn-system_tests/networks/<network>/` — blokli endpoint and the client config carrying that network's
destinations. The safe/module pair is resolved on chain from the identity at startup. Only the hoprd keystore and its
password remain repository secrets, suffixed per network (`SYSTEM_TEST_HOPRD_ID_PIZ_PALU_DEV` and its `_PASSWORD_`
pair).

## Publishing

Merging a PR publishes `<version>+pr.<N>` from `main`'s `Cargo.toml` to the `gnosis_vpn-client` artifact registry
package. The installer (`gnosis_vpn`) packages the newest such build at or above `0.100.0` into its nightly snapshot, so
a stray v4 build never ships.

Label a merged PR `installer-build` to dispatch a snapshot build right away instead of waiting for the nightly.

Release candidates are set with Bump version's `custom_version` (`1.0.0-rc.1`, then `1.0.0-rc.2`, …, then `1.0.0`);
`release_type` refuses to bump a prerelease. "Close release" tags the version currently in `Cargo.toml`.
