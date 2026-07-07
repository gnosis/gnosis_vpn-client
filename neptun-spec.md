# GnosisVPN: replacing wg/wg-quick with embedded NepTUN

Spec for removing the external `wg` and `wg-quick` dependencies and replacing
them with NepTUN's sans-IO WireGuard implementation, spliced directly into the
HOPR session in-process - no loopback UDP hop.

Companion documents: `ios-client-spec.md` (this spec implements, on desktop,
what that spec's Phases 1-2 require for iOS), `split-tunneling-gvpn.md`,
`docs/neptun-phase3-4-testing-guide.md` (remaining work + manual test plan).

## Implementation status (branch `tb/202607-neptun`)

All phases are implemented; wg/wg-quick and the WG config file are gone.
Decisions that deviate from or refine this spec are marked **[decision]** inline
below. The one deliberate soft spot: the pump's network side defaults to a
loopback UDP socket dialing the session bridge (`bound_host`) because the direct
`HoprSession` splice's frame-boundary assumption (risk #1) has not been
validated against a live gvpn server - the Phase 0 spike was skipped. The splice
is fully implemented and selected via `GNOSISVPN_WG_DATAPLANE=splice`; once
validated on staging it becomes the default and the UDP bridge is deleted.
Manual validation steps live in the testing guide.

## Executive summary

- Today the client shells out to `wg` (key generation, worker process) and
  `wg-quick` (interface lifecycle, root process). WireGuard ciphertext leaves
  the kernel/wireguard-go tunnel as UDP datagrams to `127.0.0.1:<bound_host>`,
  where a byte pump inside the worker copies them into a HOPR session.
- This spec replaces all of that with `neptun::noise::Tunn` (sans-IO) running
  inside the worker: TUN device <-> Tunn <-> `HoprSession`, one process, zero
  sockets on the WG path.
- The direct splice is possible with NO upstream changes. `HoprSession` is a
  public type implementing tokio `AsyncRead`/`AsyncWrite`; the loopback UDP
  socket is client-side glue (`bind_session_to_stream`) that we simply do not
  attach. `SessionFactory::create_session` returns the raw session.
- Wins:
  - No runtime dependency on wireguard-tools or (macOS) wireguard-go.
  - The WG private key never leaves the worker and is never written to disk
    (today root writes it into `wg0_gnosisvpn.conf`, mode 0600).
  - Removes two layers of copies and the UDP bridge's silent-drop queue
    (8192-slot ingress queue, foreign-datagram discard); replaced by direct
    AsyncWrite backpressure into the session socket.
  - Tunnel introspection for free: `Tunn::stats()` exposes handshake age, RTT
    estimate, and loss estimate - today we have no WG state readback at all
    (liveness is inferred purely from ICMP ping).
  - This is the desktop dress rehearsal for the iOS port: the pump code is the
    same code the NEPacketTunnelProvider needs.
- Cost: we must absorb the things `wg-quick` did implicitly - TUN device
  creation, address/MTU assignment, DNS configuration, and the IPv6 blackhole
  routes. Routing and killswitch already live outside wg-quick (`Table = off`)
  and are almost untouched.

## Current state (what exactly gets replaced)

Verified against the codebase; file references are current as of this writing.

### Shell-outs

| Call                            | Where                                     | Process | Replacement                                   |
| ------------------------------- | ----------------------------------------- | ------- | --------------------------------------------- |
| `which wg`, `wg --version`      | `gnosis_vpn-lib/src/wireguard.rs:89-105`  | worker  | delete (probe obsolete)                       |
| `wg genkey`                     | `wireguard.rs:107-113`                    | worker  | `x25519_dalek::StaticSecret::random_from_rng` |
| `wg pubkey`                     | `wireguard.rs:115-129`                    | worker  | `PublicKey::from(&secret)`                    |
| `which wg-quick`, `wg-quick -h` | `gnosis_vpn-root/src/wg_tooling.rs:10-26` | root    | delete                                        |
| `wg-quick up <conf>`            | `wg_tooling.rs:30-55`                     | root    | TUN provisioning (below)                      |
| `wg-quick down <conf>`          | `wg_tooling.rs:83-87`                     | root    | TUN teardown                                  |

There is no `wg show`/`wg set`/stats readback anywhere; nothing else touches the
wg binaries.

### What wg-quick actually does for us

The generated config uses `Table = off` (`routing/linux.rs:157`,
`routing/macos.rs:166`), so wg-quick is already reduced to:

1. Interface creation: kernel WireGuard `wg0_gnosisvpn` on Linux; a dynamic
   `utunN` (wireguard-go) on macOS, name published via
   `/var/run/wireguard/wg0_gnosisvpn.name` (`wg_tooling.rs:57-81`).
2. Address assignment (from the server-assigned `registration.address()`).
3. MTU (hardcoded `WG_MTU = 1420`, `wireguard.rs:14`).
4. DNS (resolvconf on Linux, scutil on macOS, inside wg-quick).
5. IPv6 blackhole routes via PreUp/PostDown lines embedded in the config
   (`wireguard.rs:171-189`).
6. Peer configuration: server public key, preshared key,
   `Endpoint = 127.0.0.1:<bound_host>`, AllowedIPs.

Everything else (bypass routes, split-tunnel routes, killswitch, ping) is
already done by the root process itself and survives this change.

### The current data path

```
apps -> kernel WG / wireguard-go (utun)         [created by wg-quick, root]
     -> UDP 127.0.0.1:<bound_host>              [loopback hop]
     -> ConnectedUdpStream -> copy_duplex        [worker, hopr-utils-session]
     -> HoprSession -> edgli -> mixnet
```

The `bound_host` is bound by `create_udp_client_binding`
(`hopr-utils-session/src/lib.rs:714`), which creates ONE session via
`HopSessionFactory::create_session` and then spawns
`bind_session_to_stream(session, udp_socket, ...)` - a generic byte pump between
any two `AsyncRead + AsyncWrite` endpoints. The UDP socket is not structural; it
exists only because the WG endpoint had to be a real socket for the kernel to
send to.

## Target architecture

```
apps -> TUN device (fd)                          [created by root, fd passed to worker]
     -> worker pump: read TUN
        -> Tunn::encapsulate                     [neptun sans-IO, worker]
        -> HoprSession::write                    [tokio AsyncWrite, in-process]
     <- HoprSession::read
        -> Tunn::decapsulate (+ drain loop)
        -> write TUN
     -> edgli -> mixnet                          [unchanged]
```

- Root keeps its role: privileged network plumbing only. It creates and
  configures the TUN device, then passes the file descriptor to the worker over
  the existing Unix socket (SCM_RIGHTS). Root never sees key material.
- Worker owns the entire WG data plane: keys, `Tunn` state machine, the pump
  task, and the session. All unprivileged.
- The `Endpoint`, `ListenPort`, and the config file `wg0_gnosisvpn.conf` cease
  to exist. There is no WG UDP socket anywhere.

### Session splice (the core of this spec)

Confirmed against hopr-utils-session v1.2.1 (the pinned rev) and the edgli
sources: no fork, no upstream patch.

**[decision]** Implemented as `Hopr::open_wg_session` (`hopr/api.rs`), returning
a `SplicedWgSession { session, configurator, metadata }`. The spliced session
bypasses the listener registry entirely, which has three knock-on effects
handled in code: `list_sessions` cannot see it (core skips the listener-polling
session monitor and relies on the pump task reporting its own exit via
`Results::WgPumpExited`), `close_session` does not apply (dropping the session
on pump cancellation closes it), and the step-11 SURB balancer adjust goes
through the retained `configurator` handle instead of `adjust_session`.
Data-plane selection lives in `wg_tunnel::data_plane()`:
`GNOSISVPN_WG_DATAPLANE` = `udp-bridge` (default) | `splice`. The UDP bridge
keeps `create_udp_client_binding` and dials `bound_host` over loopback - kernel
WG's old path with NepTUN as the client - and exists only until risk #1 is
validated live; the env gate and `wg_tunnel/udp.rs` are deleted with it.

```rust
// worker, replacing the create_udp_client_binding call for the wg session
let factory = HopSessionFactory::new(self.edgli.as_hopr());
let (session, configurator) = factory
    .create_session(destination, target, cfg)   // pub trait method
    .await?;
// session: HoprSession, implements tokio AsyncRead + AsyncWrite
// (runtime-tokio feature, which we already enable)
// configurator: keep it - same handle we store today for SURB balancer
// updates; it is returned alongside the session, not tied to the socket.
```

`HoprSession` (`hopr-transport-session/src/types.rs:340`, re-exported as
`hopr_lib::exports::transport::HoprSession`) wraps one of three inner sockets
chosen by capability flags: `ReliableSocket` (retransmission, TCP-like),
`UnreliableSocket` (segmentation only, UDP-like - what the WG session uses today
via the default `Segmentation` capability), or raw duplex. We keep the exact
same capabilities and `SessionTarget::UdpStream` as today; only the local
consumption changes.

### The pump

**[decision]** Landed as `gnosis_vpn-lib/src/wg_tunnel/` with two deltas from
the sketch below: a single `tokio::select!` loop owns the `Tunn` outright (no
mutex, per-write backpressure) instead of a task per direction, and every
endpoint write is bounded by a 30 s `SEND_TIMEOUT` so a wedged endpoint can
never stall expiry detection. Per-packet protocol errors (replay, garbage,
post-rekey stragglers) drop the packet, never the tunnel. `ConnectionExpired`
surfaces as `PumpExit::Expired`; the spawned pump task reports any
self-termination through `Results::WgPumpExited`, which core maps to the
existing disconnect-reconnect cycle (same path as a session-monitor failure).
Teardown ordering is enforced with a `TaskTracker`: core waits (bounded 5 s) for
the pump task - and thus the worker's TUN fd - to die before sending
`TearDownWg`, because Linux NepTUN TUNs are multi-queue and re-provisioning over
a stale fd would attach a second queue to the old device.

A dedicated module (proposed: `gnosis_vpn-lib/src/wg_tunnel/`) owning:

- One `Tunn` instance (single peer, index 0, rate limiter `None`, persistent
  keepalive `None` - matching today's config, tunable later).
- A tokio task per direction plus a 250 ms timer tick:
  - outbound: TUN read -> `encapsulate` -> on `WriteToNetwork(buf)`, one
    `session.write_all(buf)` per WG datagram.
  - inbound: session read -> `decapsulate` -> `WriteToTunnel(buf, src)` ->
    validate src against allowed IPs -> TUN write. When `decapsulate` returns
    `WriteToNetwork`, drain by calling `decapsulate(None, &[], dst)` until
    `Done` (post-handshake queue flush; this is the documented contract).
  - timer: `update_timers` every 250 ms (NepTUN's own device loop cadence);
    `ConnectionExpired` triggers the existing reconnect path.
- Buffers: one scratch buffer per direction, sized frame_mtu + overhead
  (encapsulate needs src.len() + 32, min 148; a single 2048-byte buffer per
  direction is comfortable).
- Handshake initiation: call `format_handshake_initiation` once at pump start so
  the tunnel comes up without waiting for first app traffic (today the ICMP
  verification ping doubles as the trigger; do not rely on that).

Key types: `neptun::x25519` re-exports x25519-dalek 2.0.1. WG keys remain base64
on the wire and in config (`force_private_key`); decode to `[u8; 32]` ->
`StaticSecret::from`. The server public key and preshared key from
`Registration` convert the same way.

### Datagram boundaries over a byte stream (design point, spike item)

WireGuard packets are not self-delimiting; over real UDP, datagram boundaries
delimit them. `HoprSession` exposes a byte-stream interface, but the framing
underneath is message-based: each write becomes one frame (segmented into
1018-byte session packets and reassembled at the exit), and the exit node turns
frames back into UDP datagrams toward the WG server.

The current UDP bridge works because `copy_duplex` effectively forwards one
datagram per write. Our pump must uphold the same contract:

- One `WriteToNetwork` output = one session write (never coalesce two WG packets
  into one write).
- One session read should yield one WG packet (one reassembled frame). The spike
  must verify read-boundary preservation under load; if reads can coalesce
  frames, the inbound side needs a frame-aware read wrapper rather than raw
  `AsyncRead` (the underlying `Stream<ApplicationDataIn>` interface preserves
  boundaries by construction - worst case we read at that level, still public
  API territory but deeper; resolve in Phase 0).

### MTU and session framing

- Session segment payload: `SESSION_MTU = 1018` bytes. Default
  `frame_mtu = 1500` with the `Segmentation` capability.
- WG at MTU 1420 produces ciphertext datagrams up to 1452 bytes (1420 + 32
  overhead): fits one 1500-byte frame, spans 2 session segments. This is exactly
  what happens today through the UDP bridge - keeping `WG_MTU = 1420` is the
  status quo, not a regression.
- Optional future tuning (out of scope here): WG MTU 986 would make every
  ciphertext datagram fit a single session segment (1018 - 32), trading
  per-packet overhead for the loss-amplification of 2-segment reassembly on the
  unreliable socket. Measure before touching.
- Handshake packets (148 B) and keepalives (32 B) always fit one segment.

## Privilege split and protocol changes

### Root-side TUN provisioning (replaces wg_tooling)

New root capability, dispatched from the routing actor like wg-quick today.
**[decision]** Landed with three inversions of the recommendations below: root
uses NepTUN's `TunSocket` (`device` feature) for TUN _creation_ on both
platforms (`routing/tun.rs`) while the worker does raw `AsyncFd` I/O and
implements the macOS 4-byte utun header itself (`wg_tunnel/tun.rs`, tested); TUN
provisioning is phase 2 of the existing 4-phase router setup rather than a
separate capability; and Linux DNS targets systemd-resolved (`resolvectl`) first
with wg-quick's `resolvconf -a/-d` as the fallback, while macOS writes a
`State:/Network/Service/<utun>/DNS` dynamic-store key (the WireGuard-app style)
instead of `networksetup`. DNS and IPv6-blackhole operations are best-effort:
failures degrade leak protection, never connectivity, and are logged as
warnings.

- Linux: open `/dev/net/tun`, `TUNSETIFF` with name `wg0_gnosisvpn` (keeping the
  name preserves killswitch and routing references unchanged), assign address +
  MTU 1420 via rtnetlink (already a dependency), set link up.
- macOS: create a `utunN` via a `SYSPROTO_CONTROL` socket, assign address via
  `ifconfig`-equivalent ioctls (or the existing shell-out style used by
  `route_ops_macos.rs`), MTU 1420. The dynamic-name dance through
  `/var/run/wireguard/wg0_gnosisvpn.name` disappears - root created the fd, root
  knows the name.
- Recommendation: reuse NepTUN's `TunSocket` (behind its `device` feature) in
  the WORKER for fd I/O via `TunSocket::new_from_fd` - it handles the macOS
  4-byte protocol-family header on utun reads/writes, which raw fd I/O would
  otherwise have to reimplement. Root only needs plain libc/ioctl to create and
  configure; it never does packet I/O.
- IPv6 blackhole routes (`::/1`, `8000::/1`) move from config PreUp/PostDown
  lines into the routing setup/teardown phases in `routing/linux.rs` and
  `routing/macos.rs`, alongside the existing phase-1/phase-3 route logic.
- DNS: the one genuinely new reimplementation. wg-quick did this internally; we
  take it over in root: Linux via `resolvconf` (matching what wg-quick calls
  today; systemd-resolved via `resolvectl` as the modern path), macOS via
  `scutil`/`networksetup`. Same inputs as today (`config.dns`, default
  `1.1.1.1,8.8.8.8`, `overwrite` semantics from `config/v6.rs:323-341`).
  Teardown must restore prior DNS on `TearDownWg` and on crash recovery.

### Protocol (worker <-> root)

`RequestToRoot::StaticWgRouting { wg_data: WireGuardData, peer_ips }` today
carries the full WG material including the private key, and returns the resolved
interface name. It becomes:

```
RequestToRoot::SetupTunnel {
    request_id,
    interface_address,   // from registration.address()
    mtu,                 // 1420
    peer_ips,            // unchanged, for bypass routes
}
ResponseFromRoot::TunnelReady {
    request_id,
    interface_name,      // wg0_gnosisvpn / utunN
    // + TUN fd via SCM_RIGHTS ancillary data on the same Unix socket
}
```

- No key material crosses the boundary in either direction. `WireGuardData`'s
  peer info (server pubkey, PSK, endpoint) stays in the worker where the `Tunn`
  lives; the endpoint field is deleted outright.
- **[decision]** `SetupTunnel` additionally carries `dns: Option<String>`: the
  overwrite semantics are resolved worker-side from config, and `None` means
  root leaves DNS unmanaged. `TunnelReady` returns
  `res: Result<interface_name, String>` (one field for success/error), and the
  worker echoes the interface name back in `KillswitchLockdown` rather than root
  remembering it.
- **[decision]** fd passing rides a DEDICATED per-worker `AF_UNIX` socketpair
  (env `INTERNAL_WORKER_TUN_FD`, `socket/fd_passing.rs`), not ancillary data on
  the JSON channel as sketched below: the control channel is read through a
  `BufReader`, which would buffer bytes past a newline that a raw `recvmsg`
  needs. Ordering reduces to a happens-before (root sends the fd, then replies
  `TunnelReady`; the worker calls `recv_fd` only after seeing `TunnelReady`).
  `recv_latest_fd` drains fds orphaned by aborted connection attempts. On any
  post-setup hand-off failure root fully tears routing down before replying with
  an error.
- `TearDownWg`, `KillswitchLockdown`, `Ping`, `UpdatePeerIps` are unchanged in
  shape. Teardown ordering: worker stops the pump and drops its fd copy first,
  then root removes routes/DNS and drops its fd (a TUN interface lives while any
  fd is open, so root closing last guarantees routes are gone before the
  interface vanishes).

### Config surface changes

- `[wireguard] listen_port`: meaningless without a UDP socket - deprecate
  (accept + warn, ignore). **[decision]** Implemented: the v6 schema still
  accepts the key, the conversion warns once, and the field is removed from
  `wireguard::Config`. No new config version was needed - `allowed_ips` was
  re-purposed in place.
- `force_private_key`, `dns`: unchanged semantics.
- `allowed_ips` moves from a wg-quick config field to the pump's decapsulate
  source-IP check (`WriteToTunnel`'s `IpAddr` is provided for exactly this).
  **[decision]** It is now an ingress-only source filter; egress destination
  scoping stays with the OS routing table (single peer, `Table = off`). Parse
  policy: comma-separated CIDRs, unparseable entries warned and skipped, unset
  or nothing-parses falls back to `0.0.0.0/0`.

## What does NOT change

- Session establishment, registration flow, bridge sessions, route health
  checks, SURB balancer handling (`connection/up/runner.rs` order is preserved;
  step 8 sends `SetupTunnel` instead of `StaticWgRouting`).
- Routing actor, bypass/split routes, nftables/pfctl killswitch (fed the
  interface name from `TunnelReady` instead of the `.name` file).
- ICMP tunnel ping and its reconnect logic. Optional follow-up: surface
  `Tunn::stats()` (handshake age, RTT, loss) in `Status` for the UIs - new
  capability, out of scope for the swap itself.
- The gvpn server side: it still sees a standard WireGuard client through the
  exit node. `bound_host` reported at registration becomes vestigial - verify
  the server treats it as informational (it received the session's loopback
  address before, which was already meaningless to it).

## Dependency notes (NepTUN)

- Git dependency, not on crates.io: pin tag `v3.0.0`
  (`neptun = { git = "https://github.com/NordSecurity/neptun.git", tag = "v3.0.0" }`),
  BSD-3-Clause. Ignore the stale `version` field in its manifest.
- Sans-IO `Tunn` needs no features; add `features = ["device"]` only if we adopt
  `TunSocket` for worker fd I/O (pulls socket2 + thiserror).
- Actively maintained (backs NordVPN's libtelio; upstream boringtun is frozen).
  API deltas from boringtun to know: `Tunn::new` returns `Result`, and
  `TunnResult::WriteToTunnel(buf, IpAddr)` unifies the V4/V6 variants.
- aarch64-apple-darwin is not in NepTUN's official support table, but libtelio
  ships on Apple Silicon; CI must cover it from day one.
- rust-toolchain: NepTUN pins 1.89.0; workspace MSRV must be >= that.

## Risks

| #   | Risk                                                                                            | Mitigation                                                                                                                                                    |
| --- | ----------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Frame/read boundary semantics: session reads coalescing two WG packets would corrupt the stream | Phase 0 spike verifies under load; fallback is reading at the `Stream<ApplicationDataIn>` layer which preserves boundaries by construction                    |
| 2   | DNS reimplementation regressions (resolvconf/scutil edge cases, restore-on-crash)               | Port wg-quick's exact commands first, refactor later; add teardown-restore e2e test                                                                           |
| 3   | fd passing bugs (leaks, ordering with JSON messages)                                            | Isolated integration test on the socket layer before wiring into the runner                                                                                   |
| 4   | NepTUN as git-tag dep: supply-chain and update discipline                                       | Pin by tag + `Cargo.lock` hash; the sans-IO seam means swapping to boringtun/GotaTun later costs ~1 week                                                      |
| 5   | Userspace WG throughput below kernel WG on Linux                                                | Non-issue in practice: the mixnet path, not WG crypto, is the bottleneck; today's macOS path is already userspace (wireguard-go). Benchmark in Phase 0 anyway |
| 6   | Losing wg-quick's battle-tested cleanup on unclean exit                                         | Root already tracks and removes routes explicitly; add TUN-fd and DNS cleanup to the existing crash-recovery path                                             |

## Phases

| Phase | Scope                                                                                                                                                                                                                                   | Status                                                                                                                                                                            |
| ----- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 0     | Spike: standalone binary opens a session via `HopSessionFactory`, drives a `Tunn` against a live gvpn server, passes ICMP through a manually created TUN on macOS and Linux. Verifies risk #1 and benchmarks throughput vs current path | **Skipped [decision]** - replaced by shipping both data planes behind `GNOSISVPN_WG_DATAPLANE` and A/B-validating on staging (see testing guide); risk #1 remains open until then |
| 1     | In-process keygen: replace `wg genkey`/`wg pubkey`/probes in `wireguard.rs` with x25519-dalek; keep everything else                                                                                                                     | **Done**                                                                                                                                                                          |
| 2     | `wg_tunnel` pump module in gnosis_vpn-lib: Tunn + session splice, unit-tested against an in-memory duplex with a second `Tunn` as the fake server                                                                                       | **Done**                                                                                                                                                                          |
| 3     | Root TUN provisioning + fd passing + `SetupTunnel`/`TunnelReady` protocol, DNS + IPv6 blackhole relocation; delete `wg_tooling.rs` and the config-file writer                                                                           | **Done** (hard swap: the wg-quick path was deleted with the cutover rather than kept behind a flag **[decision]**)                                                                |
| 4     | Integration: runner wiring, teardown/reconnect storms, crash recovery, packaging/docs (drop wireguard-tools + wireguard-go install requirements)                                                                                        | **Done** in code (incl. `listen_port` deprecation, pump-exit reconnect, ordered teardown, crash-recovery sweep); storms/recovery need the manual e2e pass                         |

## Test plan

- Unit: pump state machine against a peer `Tunn` acting as the server (handshake,
  data, expiry, drain-loop correctness, boundary preservation, allowed-IPs
  rejection incl. IPv6 sources). **[decision]** The pump is covered two ways: a
  scripted engine over boundary-preserving mpsc doubles for control-flow paths
  (expiry, endpoint close, the `SEND_TIMEOUT` wedged-write and fatal-write
  branches under paused time), and a real `WgTunnel` peer for crypto. The
  production `SessionSender`/`SessionReceiver` splice adapters are composed with
  the pump over a `tokio::io::duplex` (`pump_carries_data_over_the_session_splice_adapters`),
  driven strictly lock-step so it does not depend on the unresolved frame-boundary
  question (risk #1). Keygen: base64 round-trip against RFC 7748 §6.1 known-answer
  vectors (equivalent to wg-generated fixtures, dependency-free). Rekey/expiry
  _timing_ is delegated to NepTUN's own timer tests plus the e2e soak - NepTUN
  offers no clock injection, so a simulated-time test is not possible without
  patching upstream; the pump's reaction to expiry is covered via the scripted
  engine.
- Integration: real TUN + pump on macOS and Linux (needs root or CAP_NET_ADMIN
  in CI), fd passing over the socket layer, DNS set/restore, IPv6 blackhole
  presence during up and absence after down.
- e2e: full client against the staging gvpn server on both platforms: connect,
  ICMP ping through tunnel, sustained transfer, reconnect storm, unclean-kill
  recovery (no leaked routes/DNS/interfaces), killswitch behavior with the
  NepTUN-owned interface name.

## Remaining work

Code-complete; what is left needs a live staging server and/or root privileges.
The full checklist with commands lives in
`docs/neptun-phase3-4-testing-guide.md`:

1. e2e pass with the default `udp-bridge` data plane on both platforms (connect,
   in-tunnel ping, sustained transfer, reconnect storm, unclean-kill recovery,
   killswitch).
2. Splice validation: same pass with `GNOSISVPN_WG_DATAPLANE=splice`, asserting
   frame-boundary preservation under load (risk #1) and comparing throughput
   (risk #5). Then: flip the default, delete `wg_tunnel/udp.rs`, the env gate,
   and the loopback bridge usage; verify the server treats the
   registration-reported `bound_host` as informational.
3. Follow-ups (out of scope for the swap): surface `Tunn::stats()` in `Status`;
   darwin cargo-test job in CI (builds exist, tests do not run); CAP_NET_ADMIN
   CI harness for root-side integration tests; update the external installer repo
   to drop wireguard-tools/wireguard-go. (The workspace `rust-version` is now
   declared as `1.94`.)
