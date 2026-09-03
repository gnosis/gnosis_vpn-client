# Branch strategy: stable (hoprd v4) vs experimental (hoprd v5)

## Why

`gnosis_vpn-client` embeds hoprd as Cargo git dependencies (`edgli` +
`hopr-utils-session`) rather than talking to it over REST. v4 and v5 can't be dual-pinned
in one resolution graph, so this can't be feature-flagged inside one binary -- it needs
two branches. The branch is named after **our** version line, not the dependency's:
hoprd will bump majors again, and our maintenance lines shouldn't be renamed when it does.

## Lines and networks

| Line            | Versions        | hoprd | Network             | Blokli endpoint                             |
| --------------- | --------------- | ----- | ------------------- | ------------------------------------------- |
| `release/v0.96` | `0.96.x`        | v4    | jura-prod (shipped) | `https://blokli-jura.prod.hoprnet.link/`    |
|                 |                 |       | jura-dev (CI)       | `https://blokli-jura.dev.hoprnet.link/`     |
| `main`          | `0.97.0` and up | v5    | piz-palu-dev (CI)   | `https://blokli-piz-palu.dev.hoprnet.link/` |

The prod endpoint is reference only -- CI never points at it. `piz-palu-prod` is planned
but does not exist yet.

The version prefix is what distinguishes the lines everywhere downstream: release tags,
GCP registry versions, installer resolution. Keep `main` off `0.96.x` and the release
line off `0.97+`.

`pr.yml` derives its endpoint from the PR's target branch, so it stays identical on both
lines and never conflicts on backport. The CLI default in
`gnosis_vpn-system_tests/src/cli.rs` does differ per branch -- it's the local developer
default, where a v5 client aimed at a v4 network would be a real footgun.

## Backports

Label a merged `main` PR `backport release/v0.96`; `.github/workflows/backport.yaml`
cherry-picks it and opens a PR that runs the same CI as any other. `Cargo.lock` conflicts
are expected once the dependency graphs diverge -- pull the bot's branch, resolve, push
back to it.

`release/v0.96` gets no Renovate coverage (like any non-default branch); hoprd-v4 bumps
land there by backport or a manual PR.

## Dependency pinning

`hopr-utils-session` is pinned by **branch** (`release/4.0`), not by rev -- deliberately,
see `Cargo.toml:35`. `Cargo.lock` holds the exact commit and `bump-version.yaml` only runs
`cargo update --workspace` (workspace members only). A bare `cargo update` on
`release/v0.96` would silently pull newer `release/4.0` commits -- don't.

## Where hoprd-specific code lives

`gnosis_vpn-lib/src/hopr/` is the seam holding every `edgli`/`hopr_lib` call; everything
outside it is version-agnostic. Keeping pix and exit-node-discovery work inside that
module keeps backports low-conflict.

## Installer integration

Not wired up yet. The `gnosis_vpn` installer resolves the client via `releases/latest`,
which is **chronological** -- most recently published non-prerelease, not highest version.
Once `main` cuts `0.97.0`, that flips to the experimental line.

So before releasing from `main`, the installer must pin each lane to a version prefix
instead of "latest". Same for `resolve-registry-version.sh`, which picks the newest
artifact by upload time with no branch awareness. Until that lands, `release/v0.96` stays
out of `merge.yaml`'s branch filter and publishes nothing to the shared registry package.

## Promotion

No firm numbers, but "main replaces release/v0.96" should be gated on roughly:

- Sustained green system-tests against piz-palu for weeks, not days.
- No open P0/P1 regressions on `main` relative to the release line.
- pix and exit-node discovery past experimental on the hoprd side, not just compiling.

Retiring the release line and re-pointing branch-dependent tooling (README badges,
contributor docs, branch protection) is a separate decision for that time.
