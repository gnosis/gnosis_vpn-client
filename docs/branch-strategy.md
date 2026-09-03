# Branch strategy: stable (hoprd v4) vs experimental (hoprd v5)

## Why

`gnosis_vpn-client` embeds hoprd via edge-client. hoprd v4 and v5 can't be dual-pinned in one resolution graph.

The branch is named after the dependency (`hoprdv4`), not our version line: the stable
line takes many client version bumps (features, fixes) while hoprd itself stays v4, so a
version-line name would mean cutting a new branch per bump.

## Lines and networks

| Line              | Versions                   | hoprd | Network             | Blokli endpoint                             |
| ----------------- | -------------------------- | ----- | ------------------- | ------------------------------------------- |
| `release/hoprdv4` | `0.9x.y`, kept `< 0.100.0` | v4    | jura-prod (shipped) | `https://blokli-jura.prod.hoprnet.link/`    |
|                   |                            |       | jura-dev (CI)       | `https://blokli-jura.dev.hoprnet.link/`     |
| `main`            | `0.100.0` and up           | v5    | piz-palu-dev (CI)   | `https://blokli-piz-palu.dev.hoprnet.link/` |

`pr.yml` derives its endpoint from the PR's target branch.

`0.100.0` is a reserved gap, not a real milestone — it keeps `release/hoprdv4` and
`main`'s version numbers from ever colliding. Bump only patch/minor on
`release/hoprdv4`; never let it reach `0.100.0`. Manual discipline, not CI-enforced.

## Backports

Label a merged `main` PR `backport release/hoprdv4`; `.github/workflows/backport.yaml`
cherry-picks it and opens a PR that runs the same CI as any other.
