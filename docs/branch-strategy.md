# Branch strategy: stable (hoprd v4) vs experimental (hoprd v5)

## Why

`gnosis_vpn-client` embeds hoprd directly as a Cargo git dependency (`edgli` +
`hopr-utils-session`, pinned in the root `Cargo.toml`) rather than talking to it
over a REST API. hoprd v5 brings breaking changes plus new capabilities (pix,
exit-node discovery). `edgli`/`hopr-utils-session` can't be dual-pinned in one
Cargo resolution graph, so this isn't something we can feature-flag inside a
single binary -- it needs two branches.

`main` is the experimental/leading-edge line, tracking hoprd v5.
`release/v4` is the stable, hoprd-v4 maintenance line, branched off `main`.

This mirrors the channel model Rust (nightly on trunk, beta/stable cut from
it) and browsers (canary/stable) use for the same kind of situation.

## How fixes move between the branches

Bug fixes and improvements that aren't hoprd-v5-specific should land on
`main` first, then get backported to `release/v4` by adding a
`backport release/v4` label to the merged PR. A bot
(`.github/workflows/backport.yaml`, using `korthout/backport-action`)
cherry-picks the commit and opens a PR against `release/v4` automatically.
That PR runs the same required CI as any other `release/v4` PR before merge.

Cherry-picks that touch `Cargo.lock` will likely conflict once `main`'s
dependency graph diverges around the v5 hopr crates -- that's expected. When
it happens, pull the bot's branch locally, resolve, run `cargo update` as
needed, and push the fix to the same branch.

`release/v4` does not get its own Renovate coverage (same as any other
non-default branch today) -- hoprd-v4-side dependency bumps land there only
via backport or a manual PR.

## Releases

`release.yaml`'s `release_type` input now includes `rc`, which produces
`X.Y.Z-rc.N` versions -- these are automatically flagged as GitHub
prereleases by the underlying `hoprnet/hopr-workflows` actions. Releases cut
from `main` should use `release_type: rc`; releases cut from `release/v4`
keep using `patch`/`minor`/`major` as before.

This matters downstream: `gnosis_vpn` (the installer repo)'s
`scripts/resolve-build-versions.sh` resolves the stable client version via
`gh api repos/gnosis/gnosis_vpn-client/releases/latest`, which already
excludes prereleases. So `main`'s rc releases won't show up there without any
changes on the installer side, and pointing at a specific rc tag later is a
ready-made path to an opt-in experimental distribution channel if that's ever
wanted.

## Where hoprd-specific code lives

`gnosis_vpn-lib/src/hopr/` (`api.rs`, `types.rs`, `config.rs`, `errors.rs`,
`identity.rs`, `blokli_config.rs`, `strategy_config.rs`) is the seam where all
`edgli`/`hopr_lib` calls live. Everything outside it (CLI, config schema,
routing/health, wg_tunnel) is hoprd-version-agnostic. Keeping new
pix/exit-node-discovery work inside this module keeps backports low-conflict.

## Open dependency: hoprd v5 testnet

`pr.yml`'s system-test job currently points both `main` and `release/v4` PRs
at the same rotsee (hoprd v4) testnet via `SYSTEM_TEST_BLOKLI_URL`, since no
hoprd v5 testnet exists yet. Once one does, make that branch-conditional
(`release/v4` -> rotsee, `main` -> the v5 net) -- see the `TODO(hoprd-v5)`
comment in `pr.yml`.

## Promotion

Not committing to specific numbers yet, but "main replaces release/v4"
should be gated on something like:

- Sustained green `pr.yml` system-tests against the hoprd v5 network for a
  meaningful stretch of time (weeks, not days).
- No open P0/P1 regressions on `main` relative to `release/v4`'s behavior.
- pix and exit-node discovery both past experimental status on the hoprd
  side, not just compiling against gnosis_vpn-client.

When that bar is met, retiring/renaming `release/v4` and re-pointing
default-branch-dependent tooling (README badges, contributor docs, branch
protection) is a separate decision to make at that time.
