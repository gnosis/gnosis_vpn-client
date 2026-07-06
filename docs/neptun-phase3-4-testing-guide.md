# NepTUN port: remaining work & manual testing guide

Companion to `neptun-spec.md`. The port is **code-complete** on
`tb/202607-neptun`: all spec phases are implemented, wg/wg-quick is gone, and
the workspace is green (build, tests, clippy, treefmt) on macOS with the Linux
half verified via the remote-builder clippy check. What remains cannot be
exercised on a laptop alone - it needs root privileges and a live gvpn staging
server. This guide is the checklist for that pass, plus the follow-ups.

## What is landed

| Piece                                                                                         | Where                                                 | Tests                                                  |
| --------------------------------------------------------------------------------------------- | ----------------------------------------------------- | ------------------------------------------------------ |
| In-process keygen (x25519-dalek)                                                              | `gnosis_vpn-lib/src/wireguard.rs`                     | RFC 7748 KAT, base64 round-trip                        |
| `WgTunnel` engine + pump                                                                      | `wg_tunnel/{tunnel,pump}.rs`                          | 18 (handshake, drain, expiry, allowed-IPs, boundaries) |
| SCM_RIGHTS fd passing (dedicated socket, `INTERNAL_WORKER_TUN_FD`)                            | `socket/fd_passing.rs`, `socket/worker.rs`            | 8 (transfer, CLOEXEC, EOF, orphan drain)               |
| Session splice adapters                                                                       | `wg_tunnel/session.rs`                                | 3                                                      |
| TUN adapters (macOS utun header)                                                              | `wg_tunnel/tun.rs`                                    | 5                                                      |
| UDP bridge adapter (interim default)                                                          | `wg_tunnel/udp.rs`                                    | 2                                                      |
| Data-plane selection                                                                          | `wg_tunnel::data_plane()`                             | 4                                                      |
| Direct `HoprSession` splice                                                                   | `hopr/api.rs::open_wg_session`, runner step 6/8/11    | compile + adapter tests; e2e pending (risk #1)         |
| Pump exit -> reconnect                                                                        | `Results::WgPumpExited`, `core/mod.rs`                | via pump exit tests                                    |
| Ordered teardown (pump stops before `TearDownWg`)                                             | `TaskTracker` in core + runner                        | - (see storm test below)                               |
| `SetupTunnel`/`TunnelReady` protocol                                                          | `event/mod.rs`                                        | serde round-trips                                      |
| Root TUN provisioning                                                                         | `routing/tun.rs`, `routing/{linux,macos}.rs`          | root-gated (below)                                     |
| DNS: resolvectl -> resolvconf fallback (Linux), scutil key (macOS), restore guard             | `routing/dns.rs`                                      | 7 argv/script builder tests                            |
| IPv6 blackholes                                                                               | `routing/ipv6_blackhole.rs`                           | 2 (wg-quick verbatim argv)                             |
| Crash-recovery sweep (state file + startup sweep)                                             | `routing/sweep.rs`, root `daemon()`                   | 7                                                      |
| `listen_port` deprecation (accept + warn + ignore)                                            | `config/v6.rs`, `documented-config.toml`              | 1                                                      |
| CI/packaging: no wireguard-tools/resolvconf, deny.toml allows neptun source, diagrams updated | `.github/workflows/pr.yml`, `deny.toml`, `docs/*.mmd` | cargo-deny green                                       |

## Runtime switch

`GNOSISVPN_WG_DATAPLANE` (worker env) selects the pump's network side:

- unset / `udp-bridge` (default): loopback UDP socket connected to the HOPR
  session bridge port (`bound_host`). Datagram boundaries guaranteed by UDP.
- `splice`: the raw `HoprSession` spliced directly into the pump - the spec's
  target architecture. Gated on validating spec risk #1 below.

## Manual validation checklist (root + staging)

Targets: `gvpn-staging.toml` / `gvpn-staging-0hop.toml` in the repo root. Run
everything on **both** platforms (Linux x86_64, macOS arm64).

### 1. Baseline e2e with `udp-bridge`

- [ ] Connect; `TunnelReady` logs the interface (`wg0_gnosisvpn` / `utunN`).
- [ ] `ping 10.128.0.1` through the tunnel succeeds (step-10 verification and by
      hand).
- [ ] Sustained transfer >= 60 s (e.g. `curl` a large file through the tunnel);
      no pump warnings, no stalls.
- [ ] DNS applied: Linux `resolvectl status <iface>` shows the configured
      servers (or `/etc/resolv.conf` via resolvconf on non-systemd distros);
      macOS `scutil --dns` lists the servers for non-scoped queries. Watch for
      the macOS caveat: verify the `State:/Network/Service/<utunN>/DNS` key is
      actually adopted by configd, not just written.
- [ ] IPv6 blackholes present while up (`ip -6 route` / `netstat -rn -f inet6`
      show `::/1` and `8000::/1`), absent after disconnect.
- [ ] Killswitch: drop the tunnel (kill the WG server session or force expiry);
      verify traffic is blocked and the nftables/pfctl rules reference the
      NepTUN-owned interface name.
- [ ] Disconnect: routes, DNS, and interface fully removed; teardown-state file
      (`/var/run/gnosisvpn/teardown-state.json`) gone.

### 2. Reconnect storm & teardown ordering

- [ ] At least 10 rapid connect/disconnect/reconnect cycles plus forced
      reconnects (WAN flap). Each converges to Connected.
- [ ] `lsof -p <worker-pid> | grep -c tun` (macOS: utun) stays at <= 1 - no
      accumulating TUN fds (exercises `recv_latest_fd` drain and the
      TaskTracker-ordered teardown; on Linux specifically watch that a reconnect
      never lands on a stale multi-queue device - symptom would be a connected
      state with silently blackholed flows).
- [ ] No duplicate bypass/split routes after the storm.

### 3. Unclean-kill recovery

- [ ] `kill -9` the **worker**; root detects exit, cleans routes/DNS/TUN;
      restart connects cleanly.
- [ ] `kill -9` **root** while connected; on next root start the startup sweep
      logs what it removed (blackholes, DNS via the recorded mechanism); no
      leaked `ip -6` blackholes, no stale scutil DNS key, no orphan interface.

### 4. Splice validation (spec risk #1 - the go/no-go)

Repeat sections 1-3 with `GNOSISVPN_WG_DATAPLANE=splice`, plus:

- [ ] Frame boundaries under load: sustained bidirectional transfer >= 60 s. A
      coalesced read surfaces as NepTUN decrypt errors / dropped-datagram
      warnings from the pump (`dropping datagram`, `wireguard protocol error`)
      and degraded throughput. Zero such warnings = boundaries hold. If they do
      not: switch `wg_tunnel/session.rs` to the documented length-prefix framing
      fallback (local change, pump untouched).
- [ ] Expiry soak: idle past WG Reject-After-Time (~3 min past last handshake +
      rekey attempts) and confirm `WgPumpExited` triggers an immediate reconnect
      (log: "wg pump exited - reconnecting"), not a ping-timeout wait.
- [ ] Throughput comparison vs `udp-bridge` (spec risk #5; expectation: parity,
      the mixnet is the bottleneck).
- [ ] Server side treats the registration-reported `bound_host` as informational
      (registration succeeds; server logs show no use of it).

### 5. Post-validation flip (code change, after 4 is green)

- Make `Splice` the default in `wg_tunnel::data_plane()`, then delete:
  `wg_tunnel/udp.rs`, the `DATA_PLANE_ENV` gate, the `udp-bridge` arm in the
  runner (binding/connecting the loopback socket), and the session-monitor gate
  in core (the listener monitor dies with the bridge).
- Update `neptun-spec.md` status + this guide.

## Follow-ups (out of scope for the swap)

- Surface `Tunn::stats()` (handshake age, RTT, loss) in `Status` for the UIs.
- CI: run cargo tests on darwin (builds exist; the old root-test compile blocker
  is fixed on this branch); add a CAP_NET_ADMIN job for root-side integration
  tests (TUN create, DNS set/restore, blackholes).
- fd-passing child-process integration test across the env-var handshake (unit
  tests cover the socketpair layer; the spawn wiring is exercised only e2e
  today).
- Declare a workspace `rust-version` (NepTUN needs >= 1.89; toolchain pins
  1.94).
- External installer repo: drop wireguard-tools / wireguard-go install
  requirements.
- Rekey/expiry timing against a real `Tunn` needs upstream clock injection;
  currently delegated to NepTUN's own tests + the expiry soak above.
