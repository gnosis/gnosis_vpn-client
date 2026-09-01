# Continuation prompt: stable/experimental branch split

Paste this into a new Claude Code session (in this repo, on this branch) to resume.

## Task

Diverge `gnosis_vpn-client` into two long-lived branches:
- `main` becomes the experimental/leading-edge line, tracking hoprd v5 (pix,
  exit-node discovery). PR #766 (`este/pix`) is the first hoprd-v5-shaped
  change and is still open, not yet merged.
- `release/v4` becomes the stable, hoprd-v4 maintenance line, cut from `main`
  before PR #766 merges. Fixes flow from `main` to `release/v4` via a
  label-triggered backport bot, not the other direction.

Full rationale and design in `docs/branch-strategy.md` (committed on this
branch) — read that first for the "why" behind every decision below.

## What's already done (committed on this branch)

- `.github/CODEOWNERS`: fixed a missing path pattern (`* @gnosis/ext-hopr-admin`)
  so code-owner review can actually be enforced by branch protection.
- `.github/workflows/merge.yaml`: added `release/v4` to the post-merge build
  trigger's branch filter.
- `.github/workflows/pr.yml`: added `SYSTEM_TEST_BLOKLI_URL` env var to the
  system-test step. Currently both branches point at the same rotsee (v4)
  testnet — see the `TODO(hoprd-v5)` comment there.
- `.github/workflows/release.yaml`: added `rc` as a `release_type` choice.
  `hoprnet/hopr-workflows`' `set-build-version` action already auto-flags any
  `X.Y.Z-rc.N` version as a GitHub prerelease — no custom prerelease flag
  needed. Convention: releases from `main` use `release_type: rc`; releases
  from `release/v4` keep using patch/minor/major.
- `.github/workflows/backport.yaml` (new): label-triggered backport bot using
  `korthout/backport-action@v4.6.0`. Label a merged `main` PR
  `backport release/v4` to auto-open a cherry-pick PR against `release/v4`.
  Uses this repo's existing GitHub App bot token (not the default
  `GITHUB_TOKEN`) so the resulting PR actually triggers `pr.yml` CI.
- `docs/branch-strategy.md` (new): durable writeup of the whole strategy —
  why, backport flow, release convention, where hoprd-specific code lives
  (`gnosis_vpn-lib/src/hopr/`), the open v5-testnet dependency, and
  placeholder promotion criteria for eventually retiring `release/v4`.

All of the above only touches CI/docs — no hoprd pin changes, so it's safe to
merge into `main` regardless of exactly when `release/v4` gets cut.

## What's NOT done yet — pending your decisions

1. **Land this branch.** Open a PR from this branch into `main`, get it
   reviewed/merged. Recommended: do this *before* cutting `release/v4`, so
   the new branch inherits the updated CI/CODEOWNERS instead of missing it.
2. **Cut `release/v4`.** After step 1 merges into `main` and *before* PR #766
   (`este/pix`) merges: branch `release/v4` off `main`'s HEAD. This is a
   shared, hard-to-reverse GitHub action — confirm before pushing it.
3. **Branch-protect `release/v4`.** Require PR review via CODEOWNERS, block
   direct pushes, require the `pr.yml` status check. Needs repo admin access
   (`gh api repos/gnosis/gnosis_vpn-client/branches/release%2Fv4/protection`
   or the GitHub UI). Also a shared/hard-to-reverse action — confirm first.
4. **Let PR #766 merge into `main`** once `release/v4` exists — that's the
   point `main` actually starts diverging toward hoprd v5.
5. **Open dependency, not yet actionable:** no hoprd v5 testnet/blokli
   endpoint exists yet. Once one does, make `pr.yml`'s
   `SYSTEM_TEST_BLOKLI_URL` branch-conditional (see the `TODO(hoprd-v5)`
   comment) instead of both branches sharing the v4 testnet.

## Where the original plan lives

The full step-by-step plan (with the pros/cons discussion that led here) is
at `/home/este/.claude/plans/we-need-to-diverge-polished-shell.md` on the
machine this was drafted on — not synced elsewhere, so `docs/branch-strategy.md`
plus this file are the portable record.
