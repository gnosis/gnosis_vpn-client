# Rotsee MTU A/B test — current (U1040) vs 3248 — findings & rerun guide

**Goal:** compare the rotsee/jura-dev network at the current Sphinx packet size (U1040)
vs 3248 bytes (+ Session MTU cap 1452), via the `network-baseline` harness.

**Status (2026-08-24):** setup complete + deployed; one before-run and one after-run
done. **After-run is inconclusive** — it ran while the freshly-deployed 3248 nodes were
still settling (14–25% packet loss). Needs a clean rerun once the network stabilizes.

---

## 1. Code / commit map (all on hoprnet PR #8360)

PR #8360 `lukas/packet-size-increase`, base **release/4.0**, head **`1c75b174`**,
hopr-api 2.0.0. Title: "bump Sphinx packet size to 3248 B and cap the Session MTU
(MAX_SESSION_MTU=1452)". This is the complete release/4.0 implementation (packet size +
session plumbing). Earlier attempts (master PR #8313 `4bb7cea5`/`a38e213`, hand
const-backport `4f78353c`) were abandoned.

| repo | branch | commit | notes |
|------|--------|--------|-------|
| hoprnet | `lukas/packet-size-increase` (PR #8360) | `1c75b174` | source of the change |
| edge-client | `em/test_3248_payload_size` | `6e3f324` | hopr-lib → 1c75b174 |
| hoprd | `em/test_3248_payload_size` | `768d29f` | git deps → 1c75b174; crates.io HOPR pins bumped to 2.0.0 line; rest-api adapted to 2.0.0 graph API |
| gnosis_vpn-client | `em/rotsee_test` | `47253a86` | edgli → 6e3f324, hopr-utils-session → 1c75b174; has the harness |

Both current-MTU and 3248 sides are on the hopr-api **2.0.0** line, so the only
functional delta is the packet size — no version-line confound.

### Adaptations made (for reference on rebuild)
- **hoprd rest-api** (hopr-api 2.0.0): `score()`/`average_probe_rate()`/`is_connected()`
  now return `Option` → `.unwrap_or(0.0)` / `.unwrap_or(false)`; intermediate QoS
  `capacity()` → `balance()`.
- **vpn** `HoprSessionClientConfig`: removed `always_max_out_surbs` bool mapped to
  `max_surbs_per_data_packet` — `true → usize::MAX` (max out / saturates at packet size),
  `false → 0` (leave to SURB balancer). ← re-review if SURB density matters to the test.

### hoprd CI (PR #118)
Builds green (linux binary + Docker published as `5.0.0-rc.1-commit.768d29f`). The only
red check is the **"Release branch dependencies"** policy gate — it forbids `rev =` pins
and requires `branch = "release/4.0"`. Can't be satisfied until #8360 merges into
release/4.0; it does NOT block the artifact build/publish.

---

## 2. Deploy to jura-dev (staging cluster)

jura-dev nodes are deployed from **products-ci** (`core-team/jura-dev`), rendered by the
`cluster-hoprd` helm chart into a **ClusterHoprd** CR whose `spec.version` = image tag.
ArgoCD app `core-team-jura-dev` (defined in gitops `argocd/main/values-main.yaml`, but
sourced from products-ci `main`) has `automated + selfHeal`.

- **products-ci branch `em/deploy_packet_changes`** (pushed, not merged):
  `core-team/jura-dev/values-jura-dev.yaml` `version: latest` → `5.0.0-rc.1-commit.768d29f`.
- **Manual deploy used (no merge)** — staging cluster:
  ```
  kubectl config use-context gke_hopr-staging_europe-west3_gke-staging
  kubectl -n core-team get clusterhoprd
  kubectl -n core-team patch clusterhoprd jura-dev --type merge \
    -p '{"spec":{"version":"5.0.0-rc.1-commit.768d29f"}}'
  # restart node if operator doesn't roll it; verify:
  kubectl -n core-team get pod -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.spec.containers[*].image}{"\n"}{end}' | grep hoprd
  ```
  Caveat: ArgoCD selfHeal may revert `spec.version` to `latest`. If it does, disable
  auto-sync for the test window: `argocd app set core-team-jura-dev --sync-policy none`
  (restore with `--sync-policy automated` after).
- To revert to current MTU: set `version` back to `latest` (or re-enable auto-sync).

---

## 3. How to run the baseline

Prereqs: 3248 nodes deployed (above) AND the local vpn daemon running the matching
build (current-MTU build for the before-run; #8360 build for the after-run).

```
# 1. build + (re)start the vpn daemon with the right binaries
cd gnosis_vpn-client && cargo build --release
cd .. && ./run_rotsee.sh            # copies worker + launches gnosis_vpn-root (blokli.rotsee.hoprnet.link)

# 2. wait for exits to be healthy (they flap ReadyToConnect <-> Routable)
gnosis_vpn-client/target/release/gnosis_vpn-ctl -o json status
#    -> want USA, UK_0, India all "ReadyToConnect" and loss low before running

# 3. run the harness (same exit set both runs so they compare)
cd gnosis_vpn-client/network-baseline
./run_baseline.sh USA UK_0 India     # ~10 min/exit; results/<UTC-ts>/ (summary.csv + per-exit JSON)
```

Exit set = **USA UK_0 India** (the ReadyToConnect ones; UK_2/UK_3 are Unrecoverable,
UK_1 flaps). Egress country map in `run_baseline.sh` (`USA→US`, `UK_*→GB`, `India→IN`).

---

## 4. Results so far

### Before (current MTU, U1040) — `results/20260824T101451Z/` — HEALTHY
| exit | nav ok | down r1 | up | ping avg | loss | notes |
|------|--------|---------|-----|----------|------|-------|
| USA  | 6/9 | 4.5 Mbps | 1.04 | 1016 ms | **1.6%** | clean |
| UK_0 | 5/9 | NaN | 3.28 / 2.56 | 656 ms | **0%** | download speedtest NaN |
| India| — | — | — | — | — | dropped (~23min stall in speedtest) |

### After (3248) — `results/20260824T124701Z/` — UNSTABLE (inconclusive)
| exit | nav ok | down r1 | up | ping avg | loss | notes |
|------|--------|---------|-----|----------|------|-------|
| USA  | 5/9 | 4.36 Mbps | 0.19 | 662 ms | **19.5%** | real reconnect mid-run |
| UK_0 | 6/9 | — | — | 811 ms | **25.4%** | speedtest 30s ProtocolError |
| India| 3/9 | 1.8 Mbps | 0.58 | 709 ms | 13.9% | completed this time |

**Interpretation:** after-run happened right after the 3248 redeploy; every exit showed
14–25% loss (vs 0–1.6% before) with live reconnections. Deltas are dominated by network
instability (nodes settling), NOT the packet size. Not a valid A/B. USA download flat
(4.5→4.36), upload collapse (1.04→0.19) tracks the loss.

Full write-up: `results/COMPARISON-current-vs-3248.md`. Per-run detail:
each run's `REPORT.md` / `summary.csv` / per-exit `*.json`.

---

## 5. To rerun cleanly (the actual next step)

1. Confirm 3248 build still deployed on jura-dev (section 2 verify command) and the vpn
   daemon is the `em/rotsee_test` `47253a86` build (rebuild + `run_rotsee.sh` if unsure).
2. **Wait for the network to stabilize** — poll until USA/UK_0/India are steadily
   `ReadyToConnect` and ping loss is single-digit. Don't run during flapping.
3. `./run_baseline.sh USA UK_0 India` → new `results/<ts>/`.
4. Compare its `summary.csv` against the before-run `20260824T101451Z` (or re-take a
   fresh current-MTU before-run the same way for a same-conditions pair).

### Known issues / fixed
- **Harness run2 crash FIXED**: `measureLatency` now filters non-finite samples
  (`harness.mjs`, was `s[0].toFixed is not a function`). Was dropping run2 latency per
  exit.
- **India is flaky/slow** — furthest exit; may need retries or a longer speedtest budget.
- **Disk**: a prior run died with ENOSPC — keep an eye on free space (large logs in the
  repo root; `crash_3.log` ~1.3GB was the culprit).
- **Download speedtest** frequently returns NaN / ProtocolError on the capped, lossy
  tunnel — upload is more reliable. Treat download as lower-bound.

---

## 6. USA exit-node log analysis (`mtu_change.log`, 08:01–13:38)

Analysed the USA exit hoprd log for the after-run reconnections. 330k lines; 297k WARN,
2.3k ERROR.

**Cause of the reconnections = transport-level connection drops (not the packet size):**
- Recurring `ERROR "Error decoding object from the underlying stream" / error="connection
  lost"` — 11× across the test window (12:50, 12:55, 12:59 land inside the USA window and
  line up with the client-observed USA reconnect). This is a **libp2p stream dropping**,
  i.e. the return/transport path breaking → session tears down → client reconnects. The
  error is "connection lost", NOT a malformed-packet/decode error, so there is **no
  evidence the 3248 packets themselves fail to decode**.
- The exit node itself **never stalled** (no log gaps >5s).

**Amplifier — the ChannelLifecycle strategy churns channels mid-test:** 12 "channel
closure initiated" + several "channel closed" during the run. The jura-dev config closes
channels on `close_below_quality_score: 0.3` every 15s; when links degrade from the
connection drops, it closes those channels → path re-selection → more session churn. A
feedback loop that makes a short measurement noisier.

**Background noise (constant, not a test spike):**
- SURB/reply-opener eviction storm: 175k "evicting reply opener" + 115k "evicting surb"
  (~880/min steady). Heavy return-path SURB churn (§2.4/§5.1). Constant across the whole
  log, so not the trigger — but abnormally high volume; **suspect the vpn config
  `max_surbs_per_data_packet: usize::MAX`** (the inferred mapping of the old
  `always_max_out_surbs=true`) over-producing SURBs and pressuring the transport. Worth
  setting a bounded value and re-testing.
- 681 `"ticket factory error: channel must be open to create a multihop ticket"` — all at
  **08:00–09:00, pre-test**, unrelated to the run.
- `frame discarded` + one `received data from an unestablished session` (13:17) —
  downstream artifacts of the session churn.

**Conclusion:** freshly-redeployed 3248 nodes on an unstable/settling network (dropping
transport streams), with the channel-lifecycle strategy amplifying the churn. Not a
packet-size decode problem. Actions before rerun: (a) let the network settle; (b) try a
bounded `max_surbs_per_data_packet` instead of `usize::MAX`; (c) consider relaxing/pinning
ChannelLifecycle during measurement so it doesn't close path channels mid-run.
