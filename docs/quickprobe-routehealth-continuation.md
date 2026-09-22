# Continuation prompt: store quickprobe results per destination, deny quickprobe on a probed exit

Repo `/home/este/task_hopr/gnosis_vpn-client`, branch `este/routehealthing`, HEAD `5d4d12a2`
(`feat(probe): add quickprobe for one-shot exit checks`). Work in progress, nothing implemented yet.

## Context

`quickprobe <id>` (commit 5d4d12a2) opens a short-lived unbalanced session to an exit, runs
`/versions` and `/api/v1/status`, closes it, and answers the caller directly. Nothing of the result
is kept in Core. The user wants:

1. The quickprobe result stored in Core as per-destination route-health-like info, comparable to
   what the long-lived probe accumulates, so `status` can show it per exit.
2. `quickprobe` refused for an exit that the long-lived probe is already probing.

## Where things live today (verified)

- Quickprobe dispatch: `gnosis_vpn-lib/src/core/mod.rs:479` -> `Core::spawn_quick_probe` at
  `core/mod.rs:1634`. Takes `&self`, checks hopr/route health/`is_routable()`, then a bare
  `tokio::spawn` owning the `oneshot::Sender<Response>`; the task answers the caller itself.
  Nothing re-enters the Core loop, so Core never learns the outcome.
- Quickprobe work: `probe::quick_probe` / `run_quick_probe` / `quick_checks` in
  `gnosis_vpn-lib/src/probe.rs:337-383`, returning `QuickProbeOutcome { versions, api_version, health, rtt }`.
- Wire type: `command::QuickProbeResponse` at `gnosis_vpn-lib/src/command/mod.rs:343`
  (`Checked | Failed | UnableToProbe | NotReady | DestinationNotFound | DestinationAmbiguous`),
  Display at `command/mod.rs:887`, ctl rendering `gnosis_vpn-ctl/src/main.rs:267`, exit codes `main.rs:501`.
- Long-lived probe results: `Core.probe: Option<Probe>` (`core/mod.rs:109`), one probe total.
  `Probe` (`probe.rs:114`) accumulates state/versions/ping_rtt/load/checked_at/failures/last_error
  from `probe::Event`s handled at `core/mod.rs:964`; only the Version event touches route health
  (`set_incompatible` / `clear_incompatible`). Shown as top-level `StatusResponse.probe: ProbeView`.
- Route health: `Core.route_healths: HashMap<ExitKey, RouteHealth>` (`core/mod.rs:107`),
  `RouteHealth { key, state, last_error }` in `gnosis_vpn-lib/src/route_health.rs:30`. Per destination,
  lifecycle managed by `merge_discovered_destinations` (`core/mod.rs:1264`). Exposed as
  `RouteHealthView { state, last_error }` inside `DestinationState` (`command/mod.rs:154`), rendered
  under each destination by ctl at `gnosis_vpn-ctl/src/main.rs:316-321`.
- "Already probing" precedent: `probe_destination` at `core/mod.rs:1621` uses
  `self.probe.as_ref().is_some_and(|p| p.key() == dest.key())`.
- Late-answer-through-Core precedent: `Results::NerdStatsTicketStats { res, resp }`
  (`gnosis_vpn-lib/src/core/runner.rs:127`) carries the oneshot in a `Results` variant.
- Tests: pure free functions in `core/mod.rs` tests (e.g. `connect_step`), `Probe::apply` tests in
  `probe.rs`, `RouteHealth` transitions in `route_health.rs`, serde tag shape tests in `command/mod.rs`,
  compile-time type list in `gnosis_vpn-lib/tests/socket_types.rs`.

## Open design questions (asked, not yet answered)

1. **Storage.** Record inside `RouteHealth` (recommended: lifecycle for free, `RouteHealthView`
   grows a `quick_probe` field, status prints it under each destination) vs a sibling
   `DestinationState.quick_probe` map kept separately in Core.
2. **API latch.** Should a quickprobe's version result latch/unlatch
   `Unrecoverable(IncompatibleApiVersion)` like the long-lived probe's Version event does?
   Recommended: yes, same rule.
3. **In-flight duplicates.** Deny a second `quickprobe` while one for that exit is running
   (a per-destination Checking state, `AlreadyChecking` response) vs allow and overwrite.
   Recommended: deny.

## Sketch of the recommended implementation (pending answers)

- `route_health.rs`: add a quick-probe record (state Checking/Checked/Failed, checked_at, versions,
  api_version, load, rtt, error) to `RouteHealth` with `start_quick_probe()` / `apply_quick_probe()`.
- `runner.rs`: new `Results::QuickProbe { key: ExitKey, destination, outcome, resp }`.
- `core/mod.rs`: `spawn_quick_probe` becomes `&mut self`; deny when `self.probe` has the same key
  (`QuickProbeResponse::AlreadyProbing { destination }`), deny when a quick check is in flight,
  mark Checking, spawn the task sending `Results::QuickProbe`; handler stores the outcome, applies the
  API latch, answers `resp`. A vanished tracker still answers the caller.
- `command/mod.rs`: `RouteHealthView.quick_probe: Option<QuickProbeView>`, new response variants,
  Display, serde tag test; `socket_types.rs` type list; ctl prints a `Quick check:` line per destination
  and maps `AlreadyProbing` to stderr + `UNAVAILABLE`.
- Fix the doc comment on `QuickProbeResponse` ("nothing of it outlives the request") and the README
  idle-shutdown bullet wording ("holds nothing" -> "holds no session").

## Verification

`nix develop --command cargo test -p gnosis_vpn-lib`, then `cargo clippy`, `nix fmt`. Live: run the
daemon, `quickprobe <id>` twice quickly (second denied), `probe <id>` then `quickprobe <id>` (denied),
`status` shows the stored quick check under the destination.
