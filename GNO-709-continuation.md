# Continuation prompt — GNO-709 stepwise fixes

Paste this file as the first message of a new Claude Code session (working dir:
`gnosis_vpn-client` checkout). It contains the full context; do not re-investigate unless
the code has moved past the stated baseline.

---

Continue GNO-709 (https://linear.app/gnosis-circles/issue/GNO-709 = GitHub
gnosis/gnosis_vpn-client#753): "Killswitch stays armed while status reports disconnected,
leaving no way to restore connectivity". Assignee: Ronny. Repos involved:

- `gnosis_vpn-client` (daemon: `gnosis_vpn-lib`, `gnosis_vpn-root`, `gnosis_vpn-ctl`) — primary
- `gnosis_vpn-app` (Tauri v2 + SolidJS/TS UI) — sibling checkout, pins `gnosis_vpn-lib`
  by `branch = "main"` via Cargo.lock (`src-tauri/Cargo.toml:26`); renovate deliberately
  excludes this dep (`renovate.json:25-27`), bumps are manual `cargo update -p gnosis_vpn-lib`

Analysis baseline: `gnosis_vpn-client` main @ `0e49e377` (2026-09-09). All findings below
were verified against that rev with exact line anchors. If HEAD moved, re-check anchors
before editing, not the conclusions.

## Verified analysis (investigation is DONE)

The bug: after a tunnel drop that never recovers, the daemon parks in `Phase::HoprRunning`
with `target_destination` still set. Killswitch stays armed (host fully offline), but
`StatusResponse` looks identical to "idle, never connected", so the app shows Disconnected
and hides the Disconnect button — the only escape. Incident: 83 minutes offline, user had
to kill the app.

Confirmed mechanics (all in `gnosis_vpn-client`):

1. `disconnect_from_connection` (`gnosis_vpn-lib/src/core/mod.rs:1670-1688`) resets phase
   to `HoprRunning`, keeps `target_destination` (reconnect intent), does NOT set
   `reconnecting_since` — its tunnel-drop callers do (`WgPumpExited` :935, tunnel-ping
   max-failures :954, ForceReconnect :597). Destination-switch callers (:1646-1654) set
   nothing.
2. `act_on_target` `(Some(dest), Phase::HoprRunning)` arm (`core/mod.rs:1631-1644`): if
   route not ready, both the unhealthy and `is_unrecoverable()` branches are pure logging
   no-ops. No deadline, no escalation, anywhere in core. `Unrecoverable` is terminal
   (`route_health.rs:414` no-ops peers updates in that state).
3. Status construction (`core/mod.rs:434-457`): `active_conn_phase` is `Some` only for
   `Phase::Connecting`; both `connecting` AND `reconnecting` are gated on it, so
   `reconnecting_since` is ignored while parked → status = fully idle.
   `StatusResponse.target_destination` IS on the wire (`core/mod.rs:486`);
   `DestinationState.route_health: Option<RouteHealthView>` (incl. `Unrecoverable{reason}`)
   is too — the raw material for a truthful UI exists, only the aggregate fields lie.
4. Killswitch disarmed ONLY by `WorkerCommand::Disconnect` (`gnosis_vpn-root/src/main.rs:1514`),
   `StopClient` (:1076/:1089), or daemon shutdown (Actor::teardown). Routing teardown never
   touches the firewall — by design (`routing_actor.rs:6-14`, teardown :266-279). Worker
   has NO `RequestToRoot` variant to request disarm (`gnosis_vpn-lib/src/event/mod.rs:93-115`).
5. `gnosis_vpn-ctl disconnect` works as escape: hybrid cmd runs worker-independently
   (`main.rs:880-882`), clears target unconditionally (`core/mod.rs:535-551`) + disarms.
6. Down-runner: `?` on session open (`connection/down/runner.rs:53`) fail-fasts BEFORE
   unregister (:62) — wg-pubkey deregistration needs a fresh HOPR session to the dead exit,
   so it's skipped entirely; stale key stays registered server-side.

NEW bugs found during investigation (not in the issue):

- **Bug A — edge-triggered reconnect miss:** `HealthCheck` re-fires `act_on_target` only on
  the `!was_ready && is_ready` transition (`core/mod.rs:1019-1031`). But
  `RouteHealth::disconnecting` (`route_health.rs:595-613`) leaves a healthy route already
  in `ReadyToConnect` → no edge ever fires. A parked daemon then depends solely on a
  `DisconnectionResult`/`OpenBridge` event still being in flight, which
  `spawn_disconnection_runner`'s `if let Some(hopr)` guard (`core/mod.rs:1583`) or task
  cancellation can swallow → healthy route + set target + nothing to nudge the machine.
- **Bug C — killswitch allowlist freezes while parked (recovery blocker):** the peers poll
  keeps running while parked (10s cadence with target set, `core/mod.rs:815-821`) and sends
  `UpdatePeerIps`, but in `routing_actor.rs:281-340` the `alive` set reaches the firewall
  only via `active_bypass`, whose reconciliation is gated on `self.router` — `None` after
  teardown (which also cleared `active_bypass`). Firewall stays frozen at the static floor
  (blokli + connect-time peer snapshot). Peer churn while parked → hopr can't reach new
  peer IPs → all route-health probes time out → parked forever. Likely the real cause of
  the 83-min stall. Fixing it keeps the killswitch armed — invariant-compatible.
- (Bug B — `keep_alive_expired` (`main.rs:1317-1328`) stops the worker without disarming.
  DROPPED as a fix: design-consistent under the invariant below, and nearly unreachable.)

## HARD DESIGN INVARIANT (Ronny, overrides the issue's own proposals)

**The killswitch never auto-lifts.** Once the user hits Connect, it stays armed until an
explicit Disconnect or the app is closed — even if the machine stays offline forever. No
give-up timer, no countdown, no keepalive-based disarm. Accidental leaks destroy trust;
staying offline does not. Consequences:

- GNO-709's "Daemon — bound the stall" proposal is REJECTED (should be said in an issue comment).
- Recovery work must function _with the killswitch armed_ (hence bug C matters).
- Truthful status + the app's Disconnect affordance are the only escape → elevated priority.

## Decisions already made (do not re-litigate)

- `ReconnectingInfo.phase` becomes `Option<connection::up::Phase>` — honest `None` while
  parked (user chose this over a sentinel or a new Phase variant).
- App adoption is in scope; **app ships first** (nullable schema accepts old + new daemon
  output), daemon second. A daemon emitting `phase: null` against the OLD app kills the
  status stream: Zod rejects it → `criticalError` (`gnosis_vpn-app/src/stores/appStore.ts:466-471`).
- Workflow: stepwise and interactive, one conventional commit per fix. Before each fix,
  present the concrete plan + repos to touch; the user prepares branches; only then implement.

## Task list

**1a. App (`gnosis_vpn-app`) — ships first, backwards compatible**
Commit: `fix(status): handle parked reconnecting state from daemon`

- `src/services/vpnService.ts:48` → `phase: UpPhaseSchema.nullable()` (the hard blocker).
- `src/components/status/ConnectionStatus.tsx:17-27` — `formatConnectionPhase(null)` prints
  `undefined`; add null branch (copy like "Waiting for route to {destination}").
- `src-tauri/src/commands.rs:719-724` — fast-poll (222ms) triggers on `reconnecting.is_some()`;
  gate on `phase.is_some()` so an indefinite park polls at 2.3s.
- Cosmetic: `appStore.ts:391` log prints "- null".
- Tests/fixtures: `src/utils/status.test.ts:161`, `src/services/vpnService.test.ts`,
  `src/services/fixtures/*.json`.
- Rust side (`src-tauri/src/types.rs:14-22,302-314`) is passthrough/`destination_id`-only —
  compiles unchanged; `cargo update -p gnosis_vpn-lib` only after 1b merges.
- Free win: `deriveVPNStatus` (`src/utils/status.ts:57-77`) checks `reconnecting` first and
  `ConnectButton.tsx:11-16,51` flips to "Disconnect" for `vpnStatus === "Reconnecting"` —
  affordance appears with NO gating change. Optional resilience: also derive parked state
  from `target_destination` + `RunMode::Running` (field stored at `appStore.ts:314`, unused).
- Note: owl animation (`StatusHero.tsx:65-73`) sits at 8% fill for a phase-less reconnect —
  acceptable, or tweak.

**1b. Daemon (`gnosis_vpn-client`)**
Commit: `fix(core): report reconnecting state while waiting for route health`

- `gnosis_vpn-lib/src/command/mod.rs:106-113`: `phase: Option<connection::up::Phase>`; fix
  stale "WAN change" doc comment on `since`; adjust `Display` (:582-592) — "waiting for
  route health" when phase is `None`.
- `core/mod.rs` status handler (~:434): emit `reconnecting` when
  `phase == HoprRunning && target_destination.is_some()`, `since = reconnecting_since`.
  Existing `Phase::Connecting` behavior stays.
- `disconnect_from_connection` (~:1670): set `reconnecting_since` if a target remains and
  it's not already set (covers destination-switch parks).
- Extract status construction into a testable `build_status(&self) -> StatusResponse`
  method; unit tests: parked-with-target → `reconnecting: Some(phase: None)`; idle → all
  None; explicit Disconnect clears. (No tests exist for the core state machine today.)
- Check `gnosis_vpn-ctl/src/main.rs:232-239` "Waiting to connect" fallback doesn't
  double-print alongside the new reconnecting line.

**2. Daemon — level-triggered reconnect (bug A)**
Commit: `fix(core): re-check route readiness while a target is parked`

- Make the trigger level-aware: re-check readiness on every `HealthCheck` while a target is
  set and phase is `HoprRunning` (not only the not-ready→ready edge, `core/mod.rs:1019-1031`),
  and/or after `disconnecting()`/`DisconnectionResult`. Since we never give up, this retry
  loop must be airtight — it is the only path back online besides user Disconnect.

**3. Daemon — killswitch allowlist freshness while parked (bug C)**
Commit: `fix(root): keep killswitch peer allowlist fresh while disconnected`

- `routing_actor.rs:update_peer_ips` (:281-340): decouple firewall refresh from the router.
  Track the `alive` peer set independently of `active_bypass` (bypass _routes_ need a
  router; firewall rules don't — while parked, traffic uses the normal default route and
  only the firewall gates it). Reapply policy with floor ∪ alive even when `router == None`.
  Killswitch stays armed throughout.

**4. Down-runner unregister resilience — file as a SEPARATE issue first**

- `down/runner.rs:53` fail-fast skips unregister; consider deferring stale-key cleanup to
  the next connection's bridge task (pattern exists: `up/runner.rs:600-615`,
  `force_reconnect` `core/mod.rs:1690-1700`).

**Also:** comment on GNO-709: reject "bound the stall" per the invariant; record bugs A and C
(or file as new issues — ask Ronny).

## Current status / next action

No code written yet. Next action: Ronny prepares branches for step 1a/1b, then implement
step by step (present each fix's plan before touching code — workflow above).

## Verification

- Daemon: `nix fmt`, `cargo clippy --fix`, `cargo test` (devShell:
  `nix develop --command <cmd>`). Manual: simulate tunnel-ping failures (unreachable exit);
  `gnosis_vpn-ctl status` must show reconnecting/waiting (never idle) while parked;
  killswitch stays armed the whole time and allowlist tracks peer churn (`nft list ruleset`);
  `ctl disconnect` remains the escape.
- App: `deno lint --fix` per user prefs if applicable, plus its own test suite
  (status.test.ts / vpnService.test.ts); manual check that the parked daemon shows
  "Reconnecting" + Disconnect button, and no `criticalError`.

## User conventions (from Ronny's global prefs)

- One-line conventional commits; always commit when a task finishes; print the message.
- Comments: sparse, one-line, "why" not "what". Tagged serde representations for new
  serialized enums. Never add `unsafe` without asking. Lint order: `nix fmt` →
  `cargo clippy --fix` → `deno lint --fix`. No inline lint suppressions.
- Beware `push.default=tracking`: a branch cut from origin/main pushes to main unless you
  pass an explicit refspec.
