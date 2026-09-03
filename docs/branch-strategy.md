# Branch strategy: stable (hoprd v4) vs experimental (hoprd v5)

## Why

`gnosis_vpn-client` embeds hoprd directly as Cargo git dependencies (`edgli` +
`hopr-utils-session`, pinned in the root `Cargo.toml`) rather than talking to it over a
REST API. hoprd v5 brings breaking changes plus new capabilities (pix, exit-node
discovery), and the two can't be dual-pinned in one Cargo resolution graph -- so this
can't be feature-flagged inside a single binary. It needs two branches.

- `main` is the experimental line, moving toward hoprd v5.
- `release/v0.96` is the stable maintenance line for the current hoprd-v4 client.

The branch is named after **our** version line, not the dependency's. hoprd may bump
majors again; our maintenance lines shouldn't be renamed when it does, and the mapping
belongs in the table below rather than encoded in a branch name.

## Compatibility

| Line            | Versions        | hoprd / edgli | Test network | Blokli endpoint                             |
| --------------- | --------------- | ------------- | ------------ | ------------------------------------------- |
| `release/v0.96` | `0.96.x`        | v4            | jura-dev     | `https://blokli-jura.dev.hoprnet.link/`     |
| `main`          | `0.97.0` and up | v5            | piz-palu-dev | `https://blokli-piz-palu.dev.hoprnet.link/` |

The version prefix is what distinguishes the two lines everywhere downstream -- release
tags, GCP artifact registry versions, and installer version resolution. Keep `main` off
`0.96.x` and the release line off `0.97+`.

`pr.yml` picks the endpoint from the PR's target branch, so the workflow file stays
identical on both lines and backports never conflict on it. The system-test CLI default
in `gnosis_vpn-system_tests/src/cli.rs` does differ per branch -- that one is the local
developer default, where pointing a v5 client at a v4 network would be a real footgun.

## How fixes move between the branches

Fixes that aren't hoprd-v5-specific land on `main` first, then get backported by adding a
`backport release/v0.96` label to the merged PR. `.github/workflows/backport.yaml`
cherry-picks the commit and opens a PR against `release/v0.96`, which then runs the same
CI as any other PR.

Cherry-picks touching `Cargo.lock` will conflict once the dependency graphs diverge
around the v5 hopr crates -- that's expected. Pull the bot's branch, resolve, and push
back to the same branch.

`release/v0.96` gets no Renovate coverage (same as any non-default branch today).
hoprd-v4-side dependency bumps land there via backport or a manual PR.

## Dependency pinning on the release line

`hopr-utils-session` is pinned by **branch** (`release/4.0`), not by rev -- deliberately,
see the comment at `Cargo.toml:35`. `Cargo.lock` holds the exact commit, and
`bump-version.yaml` only runs `cargo update --workspace`, which touches workspace members
alone. A bare `cargo update` on `release/v0.96` would silently pull newer `release/4.0`
commits and undercut the point of the stable line -- don't.

## Where hoprd-specific code lives

`gnosis_vpn-lib/src/hopr/` (`api.rs`, `types.rs`, `config.rs`, `errors.rs`,
`identity.rs`, `blokli_config.rs`, `strategy_config.rs`) is the seam where all
`edgli`/`hopr_lib` calls live. Everything outside it (CLI, config schema, routing/health,
wg_tunnel) is hoprd-version-agnostic. Keeping new pix/exit-node-discovery work inside
that module keeps backports low-conflict.

## Installer integration

Not wired up yet. The `gnosis_vpn` installer resolves the client via
`repos/gnosis/gnosis_vpn-client/releases/latest`, which is **chronological** -- it returns
the most recently published non-prerelease, not the highest version. Once `main` cuts
`0.97.0`, that resolution flips to the experimental line.

Before releasing from `main`, the installer must pin each of its lanes to a client
version prefix instead of "latest". The same applies to
`resolve-registry-version.sh`, which picks the newest artifact by upload time with no
branch awareness. Until that lands, `release/v0.96` is deliberately absent from
`merge.yaml`'s branch filter so it publishes nothing to the shared registry package.

## Promotion

No specific numbers yet, but "main replaces release/v0.96" should be gated on roughly:

- Sustained green `pr.yml` system-tests against piz-palu for weeks, not days.
- No open P0/P1 regressions on `main` relative to the release line's behavior.
- pix and exit-node discovery past experimental status on the hoprd side, not just
  compiling against gnosis_vpn-client.

Retiring the release line and re-pointing default-branch-dependent tooling (README
badges, contributor docs, branch protection) is a separate decision for that time.
