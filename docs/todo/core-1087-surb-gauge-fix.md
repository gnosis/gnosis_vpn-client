# TODO: CORE-1087 — session SURB buffer estimate gauge grows without bound

Parked plan for a **hoprnet** fix. It lives here because the symptom surfaces in this client's
`nerd-stats` output and the investigation started from a client-side suspicion that turned out wrong.

- Linear: https://linear.app/hoprnet/issue/CORE-1087 (hoprnet workspace)
- GitHub mirror: https://github.com/hoprnet/hoprnet/issues/8438 (open, unassigned as of 2026-09-21)
- Reporter: tolbrino, 2026-09-21, against `release/4.0`

## Verdict (2026-09-21)

**hoprnet bug. The client does no aggregation.**

- Client (`gnosis_vpn-lib/src/hopr/metrics.rs`, `command/mod.rs` `SurbStats::from_telemetry`) reads the single
  series `hopr_session_surb_buffer_estimate{session_id=<live session>}` from edgli's Prometheus text and prints
  it verbatim. First exact-label match wins. No sum, no delta accumulation, no closed-session series. The value
  feeds nothing but the ctl display.
- hoprnet `transport/session/src/telemetry/mod.rs` `refresh_surb_gauges` publishes the raw lifetime
  `produced − consumed`. No decay, no capacity clamp, never reads the Exit-reported level.
- The balancer keeps the corrected level separately in `BalancerStateValues::buffer_level`
  (`balancer/controller.rs`): seeded by the Exit's `KeepAlive(BalancerState)` report, decayed, clamped via
  `clamp_to_counterparty_capacity`, reset on return-path regime changes. Exported as
  `hopr_surb_balancer_current_buffer_estimate`, which the client does not read.
- Drift is structural: Entry `surb_decay` (5% of target / 60 s) makes the balancer refill ~350 SURBs/min that
  never appear in `consumed`, so the raw difference climbs forever even on a healthy session. Our
  `sustain_on_return_path_loss: true` (`connection/options.rs`) accelerates it.
- `hopr_session_surb_rate_per_sec` is the delta of the same unbounded difference (24390/s vs 5063/s setpoint).
- Same code on hoprnet `master`, `origin/release/4.0` and the rev the client pins. Unchanged since #7953 (2026-03).
- Side issue: `remove_session_metrics_state` never zeroes the `hopr_session_surb_*` gauges, so closed sessions
  leave frozen series behind. Harmless to the id-filtered client read, a cardinality leak for scrapers.

## Tracking

- Branch (Linear pattern `<user>/<id>-<url-slug>`, verified on GNO-793), off hoprnet `master`:
  `ronnyesterluss/core-1087-session-surb-buffer-estimate-gauge-grows-without-bound-and-ignores-the`
- hoprnet convention: fix lands on `master`, then a second PR cherry-picks to `release/4.0` (cf. #8422 → #8423).
- Assign the GitHub mirror when work starts: `gh issue edit 8438 -R hoprnet/hoprnet --add-assignee esterlus`.
- Linear assignment + Todo: needs the hoprnet workspace connected, see below.

## Plan — all in `hoprnet/transport/session/src/telemetry/mod.rs`

1. `SessionSurbRuntimeState`: rename `last_snapshot_total` → `last_snapshot_produced`. In
   `set_session_balancer_data` initialise it from `estimator.produced` so SURBs counted before telemetry
   attached don't land in the first rate window.
2. `refresh_surb_gauges`:
   - target gauge unchanged.
   - `METRIC_SESSION_SURB_BUFFER_ESTIMATE` ← `surb.state.buffer_level()`. One why-comment: only the balancer
     level is bounded by the counterparty store and corrected by Exit reports. `buffer_level` is written by
     `SurbBalancer::update` on both ends (Entry and Exit each run a balancer), so it is right on both sides.
   - rate: production rate from `produced` deltas only (same meaning on both ends, non-negative, immune to
     keep-alive/clamp discontinuities; the net rate already exists as `hopr_surb_balancer_surbs_rate`).
     Publish only once `elapsed >= SURB_RATE_WINDOW_US = 1_000_000` since the last snapshot, then advance the
     snapshot; early-return otherwise. Kills the "1 SURB over 1 ms = 1000/s" noise without a timer.
   - keep metric names (this client and dashboards depend on them); change descriptions, one line each, no `"`
     or `|` (METRICS.md is generated from them).
3. `remove_session_metrics_state`: zero `BUFFER_ESTIMATE`, `RATE_PER_SEC`, `REFILL_IN_FLIGHT` unconditionally
   (series already minted per session by `initialize_session_metrics`), same rationale as the existing PIX
   fill-rate comment there. Leave the target gauge alone.
4. Tests in the existing `mod tests` (`use super::*`; gauges readable via `MultiGauge::get`; module is
   `#[cfg(feature = "telemetry")]`, so run with `--features telemetry`). Refresh under a blocking
   `SESSION_RUNTIME.lock()` in tests (production uses `try_lock`, which can skip under parallel tests).
   Natural-phrase names, `anyhow::Result` where `?`/`expect` is used:
   - bounded by counterparty store: capacity 10 000, target 7 000, `produced += 1_960_000`, run the real
     `SurbBalancer` loop for 2 ticks (pattern from `controller.rs` tokio tests with `MockSurbFlowController`),
     assert gauge `<= max(capacity, target)` and `== state.buffer_level()`.
   - follows the Exit report: store 4 200 then 900 into `state.buffer_level`, refresh, assert gauge equals.
   - rate semantics/window: `produced += 500, consumed += 400`; refresh at +0.5 s → still 0; at +1 s → 500;
     `produced += 200`, at +3 s → 100.
   - close zeroes gauges: after `remove_session_metrics_state(&id, false)` all three read 0.
5. METRICS.md: never hand-edit. `./.github/scripts/generate-metrics-docs.sh --fix`.

### Verification (hoprnet CLAUDE.md order)

```
cargo check -p hopr-transport-session --features telemetry
cargo shear --fix -p hopr-transport-session && cargo check -p hopr-transport-session --features telemetry
nix fmt
cargo clippy -p hopr-transport-session --features telemetry --tests
cargo nextest run --lib -p hopr-transport-session --features telemetry
cargo nextest run --lib -p hopr-transport-session
./.github/scripts/generate-metrics-docs.sh
```

Optional live check via gnosis_vpn-testenv with a client pinned to the fix: nerd-stats estimate stays
`<= max(10_000, target)` and the rate stays near the applied setpoint during upload.

### Commit / PR / backport

- `fix(session): read hopr_session_surb_buffer_estimate from the balancer level`
  Body: raw counters vs clamped/reported level; rate is production-only over a 1 s window; SURB gauges zeroed
  on close; `Closes #8438`, `CORE-1087`.
- After the master PR merges: `git switch -c <same-branch>-4.0 origin/release/4.0 && git cherry-pick -x <sha>`.
  Expect conflicts in `remove_session_metrics_state` (release/4.0 has no PIX block; keep only the three SURB
  zero lines) and METRICS.md (re-run the generator). Re-verify, PR base `release/4.0`, title suffixed
  `(release/4.0) (#<master PR>)` like #8423.

### Risks / notes

- Exit sessions without rate control (`NoRateControl` branch in `manager.rs`) will show 0 instead of the raw
  diff; consistent with Entry no-balancer sessions, explained by `target_buffer == 0`. Say so in the PR.
- While the return path is degraded the controller writes 0 into `buffer_level` as an instruction; the gauge
  shows 0 for that window, same as `hopr_surb_balancer_current_buffer_estimate` today.
- Exit-side wire change (send the clamped level in keep-alives) stays out of scope, per the issue.
- `origin/kauki/fix/session/surb-estimate-blind-to-return-loss` (2026-08-13) touches the balancer, not
  `telemetry/mod.rs`; no overlap.
- Client follow-up, optional and independent: `SurbStats::buffer_estimate` could read
  `hopr_surb_balancer_current_buffer_estimate` to be correct on today's pinned hoprnet. That gauge is
  `#[cfg(all(feature = "telemetry", not(test)))]`; confirm it appears in edgli's exposition first.

## Connecting Claude to the hoprnet Linear workspace

The Linear connector used on 2026-09-21 was bound to the **Gnosis / Circles** workspace
(`linear.app/gnosis-circles`, teams GnosisVPN and GnosisVPN Support). A Linear connection is bound to one
workspace, so CORE-1087 returned "Could not find referenced Issue". To reach `linear.app/hoprnet`:

1. Web (claude.ai): Settings → Connectors → Linear → Disconnect, then Connect again. Linear's OAuth screen
   shows a workspace picker; choose **hoprnet**. This swaps the workspace; the gnosis-circles connection is gone
   until you reconnect it.
2. To keep both: add Linear a second time as a custom connector pointing at `https://mcp.linear.app/mcp`
   (Settings → Connectors → Add custom connector), authorize it against **hoprnet**. Tools then appear under a
   second server name; tell Claude which one to use.
3. Claude Code CLI only: `claude mcp add --transport http linear-hoprnet https://mcp.linear.app/mcp`, then
   `/mcp` → authenticate → pick **hoprnet** in the picker.
4. Verify with `get_workspace` (should print the hoprnet URL) before asking for CORE-1087.

Then: assign CORE-1087 to me, set status Todo, and confirm the generated `gitBranchName` matches the branch
above (the username part is assumed to be `ronnyesterluss` in that workspace too).

## Continuation prompt

Paste into a fresh Claude Code session started in `/home/este/task_hopr`:

> Read `gnosis_vpn-client/docs/todo/core-1087-surb-gauge-fix.md` (branch `todo/core-1087-surb-gauge-fix`).
> Implement the plan in the hoprnet repo: create the tracking branch named there off `origin/master`, make the
> changes in `transport/session/src/telemetry/mod.rs`, regenerate METRICS.md with the script, run the
> verification list, commit with the given conventional message, push and open a PR against `master` that
> closes hoprnet#8438. Assign hoprnet#8438 to `esterlus`. If the hoprnet Linear workspace is connected
> (`get_workspace` returns linear.app/hoprnet), assign CORE-1087 to me and move it to In Progress, else tell me
> the connection steps from the doc. Do not touch the Exit-side keep-alive wire format. Re-check
> `origin/master` for changes to `telemetry/mod.rs` and `balancer/controller.rs` since 2026-09-21 before editing.
