# Branch strategy: stable (hoprd v4) vs experimental (hoprd v5)

## Why

`gnosis_vpn-client` embeds hoprd via edge-client. hoprd v4 and v5 can't be dual-pinned in one resolution graph.

## Lines and networks

| Line              | Versions                   | hoprd | Network             | Blokli endpoint                             |
| ----------------- | -------------------------- | ----- | ------------------- | ------------------------------------------- |
| `release/hoprdv4` | `0.9x.y`, kept `< 0.100.0` | v4    | jura-prod (shipped) | `https://blokli-jura.prod.hoprnet.link/`    |
|                   |                            |       | jura-dev (CI)       | `https://blokli-jura.dev.hoprnet.link/`     |
| `main`            | `0.100.0` and up           | v5    | piz-palu-dev (CI)   | `https://blokli-piz-palu.dev.hoprnet.link/` |

`pr.yml` derives its endpoint from the PR's target branch.

## Backports

Label a merged `main` PR `backport release/hoprdv4`; `.github/workflows/backport.yaml`
cherry-picks it and opens a PR that runs the same CI as any other.
