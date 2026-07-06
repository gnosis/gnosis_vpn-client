# NepTUN port: Phase 3-4 implementation blueprint

> **Historical.** All phases described here have landed on
> `tb/202607-neptun`; decisions and deviations are recorded in
> `neptun-spec.md`, remaining manual work in
> `neptun-phase3-4-testing-guide.md`. Kept for the verified API anchors.

Companion to `neptun-spec.md`. Phases 0-2 are landed on branch
`tb/202607-neptun`; this document is the executable plan for the remaining,
privileged/integration half. It requires **root privileges + a live gvpn staging
server** to implement _and test_, so it was deferred rather than written
unverified. Every anchor below was verified against the pinned revs
(`hopr-utils-session` d670a6c, NepTUN v3.0.0) during the Phase 0-2 work.

## Done (Phases 1-2, committed)

- `9faf1e81` Phase 1: in-process x25519 keygen in
  `gnosis_vpn-lib/src/wireguard.rs` (`StaticSecret::random` /
  `PublicKey::from`); deleted `wg genkey/pubkey` and the
  `which wg`/`wg --version` probes; RFC 7748 KAT.
- `c1434568` Phase 2: `gnosis_vpn-lib/src/wg_tunnel/` = `WgTunnel` sync core
  (`TunnelEngine`) + `pump::run` single-task `select!` loop over four endpoint
  traits (`NetworkSender/Receiver`, `TunSender/Receiver`). 18 tests.
  Per-datagram decap/encap errors are non-fatal drops; sends are
  `SEND_TIMEOUT`-bounded; `allowed_ips` is ingress-only. The pump is generic
  over its endpoints, so Phases 3-4 only need to (a) create the TUN, (b) hand
  its fd + the `HoprSession` to the pump via trait adapters.

## Corrected assumption (important)

The spec (lines 254-257) says the worker↔root fd rides "the same `sendmsg`" on a
"JSON-per-connection (`socket/root.rs`)" channel. **Both halves are wrong:**

- `socket/root.rs` is the **CTL-client↔root** control socket
  (`/var/run/gnosisvpn.sock`), one-JSON-per-connection. `RequestToRoot` never
  travels there.
- The real worker↔root channel is a **single long-lived `AF_UNIX` socketpair**
  (`UnixStream::pair`, `gnosis_vpn-root/src/main.rs:1173`) passed to the worker
  via the `INTERNAL_WORKER_FD` env var, framed as **newline-delimited JSON**
  over tokio `BufReader::lines()` / `BufWriter` (root `main.rs:559-580` /
  `994-1003`; worker `main.rs:88-154` / `184-205`). The protocol enums live in
  `gnosis_vpn-lib/src/event/mod.rs`, not `socket/`.

`BufReader::lines()` cannot receive ancillary data, and it may buffer bytes past
a newline that a raw `recvmsg` on the same fd would then miss. So SCM_RIGHTS
cannot simply be spliced onto the existing channel.

## Phase 3 - privileged half

### 3a. fd passing: use a dedicated raw socketpair (recommended)

Do **not** multiplex SCM_RIGHTS onto the newline-JSON socketpair. Instead
establish a **second, dedicated `AF_UNIX` socketpair at worker spawn**, passed
via a new env var (mirror `INTERNAL_WORKER_FD`, e.g. `INTERNAL_WORKER_TUN_FD`,
CLOEXEC-cleared the same way, `main.rs:1178-1196`). This socket carries only fd
transfers, never line-buffered JSON, so there is no `BufReader` contamination.

- Primitive: `send_fd(&sock, RawFd)` = `sendmsg` with a 1-byte payload + an
  `SCM_RIGHTS` cmsg carrying the fd; `recv_fd(&sock)` = `recvmsg` into a 1-byte
  buf + a cmsg buffer, extract the fd (set `MSG_CMSG_CLOEXEC` on Linux). Put it
  in `gnosis_vpn-lib/src/socket/fd_passing.rs`. Prefer the `sendfd` crate over
  hand-rolled `libc` cmsg/alignment unless a dep is unacceptable. Note: `libc`
  is currently only a Linux target dep for `gnosis_vpn-lib` - add it for macOS
  too (or use `sendfd`, which is cross-platform).
- **Root-free test** (testable in CI without root): `UnixStream::pair`, create a
  `pipe`, `send_fd` the read end, `recv_fd` on the peer, write to the pipe on
  the sender, read through the received fd - assert the bytes match, and assert
  no fd leak on the error paths (send to a closed peer, recv with no cmsg).
- Ordering with the JSON channel: root sends `TunnelReady` (JSON, interface
  name) on the main channel; the fd travels on the dedicated socket. The worker
  waits for `TunnelReady`, then `recv_fd` on the dedicated socket. Because the
  sockets are independent, ordering is a simple happens-before, not a framing
  hazard.

### 3b. protocol change (event/mod.rs)

Replace
`RequestToRoot::StaticWgRouting { request_id, wg_data: WireGuardData,
peer_ips }`
(`event/mod.rs:107-111`) and
`ResponseFromRoot::StaticWgRouting {
request_id, res: Result<String,String> }`
(`:136-139`) with:

```rust
RequestToRoot::SetupTunnel { request_id, interface_address: String, mtu: u32, peer_ips: Vec<Ipv4Addr> }
ResponseFromRoot::TunnelReady { request_id, res: Result<String, String> } // Ok = interface name
```

- No key material crosses: `WireGuardData` (`event/mod.rs:85`) and its private
  key stay in the worker where `WgTunnel` lives. Delete the `endpoint` field
  usage (`connection/up/runner.rs:349-353`) entirely - there is no WG UDP
  endpoint.
- Also update the in-process `RunnerToRoot::StaticWgRouting` (`event/mod.rs:73`)
  and the core forwarder (`core/mod.rs:965-972`) that assigns `request_id`.
- The fd is delivered out-of-band (3a), not in `TunnelReady`.

### 3c. root TUN provisioning (replaces wg_tooling.rs)

New root capability, dispatched from `main.rs` into the routing actor exactly
where `StaticWgRouting` is today (`main.rs:1066-1082` → `setup_static_routing`
`:1301` → `routing_actor::Msg::SetupRouting` → `routing/{linux,macos}.rs`
`Routing::setup()`), replacing the `wg_quick_up` call (`routing/linux.rs:159`,
`macos.rs:168`).

- **Linux**: open `/dev/net/tun`, `ioctl(TUNSETIFF, IFF_TUN | IFF_NO_PI)` with
  name `wg0_gnosisvpn` (keep the name - killswitch/routing reference it), assign
  address + MTU 1420 via `rtnetlink` (already a dep), set link up.
- **macOS**: create a `utunN` via a `SYSPROTO_CONTROL` socket (connect to
  `com.apple.net.utun_control`), read the assigned name via `getsockopt`
  `UTUN_OPT_IFNAME`, assign address + MTU via `ifconfig`-equivalent ioctls (or
  the shell style already in `route_ops_macos.rs`). The dynamic-name dance
  through `/var/run/wireguard/wg0_gnosisvpn.name` (`wg_tooling.rs:62-81`)
  disappears - root created the fd, root knows the name.
- Return `(RawFd, interface_name)`; send the name in `TunnelReady`, the fd via
  3a.
- **Worker** wraps the received fd with NepTUN's `TunSocket::new_from_fd`
  (behind the `device` feature; pulls `socket2` + `thiserror`) so the macOS
  4-byte utun protocol-family header on reads/writes is handled for free. Then
  implement the pump's `TunSender`/`TunReceiver` over the `TunSocket` (via
  `AsyncFd` for readiness). Root only needs plain `libc`/ioctl; it never does
  packet I/O.

### 3d. DNS (the one genuinely new reimplementation)

wg-quick did this internally; take it over in root. Inputs are unchanged:
`config.dns`, default `1.1.1.1,8.8.8.8` (`config/v6.rs:123`), overwrite
semantics (`config/v6.rs:328-339`): absent → default; `overwrite=true` →
servers-or-default; `overwrite=false` → no DNS pushed.

- **Linux**: `resolvconf` (match what wg-quick calls), with `resolvectl`
  (systemd-resolved) as the modern path.
- **macOS**: `scutil` / `networksetup`.
- Teardown must **restore prior DNS** on `TearDownWg` and on crash recovery
  (root has no DNS code today - DNS restore currently lives inside
  `wg-quick
down`). Add a teardown-restore e2e test.

### 3e. IPv6 blackhole relocation

Move the exact strings from `wireguard.rs:171-189` (currently emitted as
PreUp/PostDown config lines) into the routing setup/teardown phases in
`routing/linux.rs` and `routing/macos.rs`, alongside the existing
phase-1/phase-3 route logic:

- Linux: `ip -6 route add blackhole ::/1` and `8000::/1` (idempotent del-first);
  delete on teardown.
- macOS: `route -n add -blackhole -inet6 ::/1 ::1` and `8000::/1 ::1`; delete on
  teardown.

### 3f. deletions

`gnosis_vpn-root/src/wg_tooling.rs` (probes + `wg-quick up/down` + name-file
dance), `routing/wg_ops.rs` `WgOps`/`RealWgOps`, the config-file writer's WG
sections in `wireguard.rs::to_file_string`, and `WG_CONFIG_FILE`. Keep
`WG_INTERFACE`/`WG_MTU`. `wireguard::Error`'s `ShellCommandExt`/`Dirs` variants
lose their last producers once `wg_tooling.rs` is gone - prune them then.

## Phase 4 - integration

### 4a. session splice (hopr/api.rs)

Today the WG UDP session is created by `Hopr::open_session`
(`gnosis_vpn-lib/src/hopr/api.rs:79`), whose UDP branch calls
`create_udp_client_binding` (`:145`), which internally does
`factory.create_session(dest, target, cfg) -> (HoprSession, configurator)` then
spawns `bind_session_to_stream` (the loopback byte pump to delete).

Add a sibling `open_wg_session` that calls `create_session` **directly**:

```rust
let factory = HopSessionFactory::new(self.edgli.as_hopr());
let (session, configurator) = factory.create_session(destination, target, cfg).await?;
// session: HoprSession (edgli::hopr_lib::exports::transport), AsyncRead + AsyncWrite
// keep `configurator` for SURB balancer updates (adjust_session, api.rs:259)
```

Keep `SessionTarget::UdpStream` and the default `Segmentation` capability
(identical to today). Return the `HoprSession` and configurator to the worker.
Split the session with `tokio::io::split` and implement the pump's
`NetworkSender`/`NetworkReceiver` over the halves - `send` =
`write_all(one
datagram)` (one write = one frame), `recv` = **read one frame**.
This `recv` is spec risk #1: verify `HoprSession`'s `AsyncRead` does not
coalesce two frames under load; if it can, read at the boundary-preserving
`Stream<ApplicationDataIn>` layer instead (documented on the `NetworkReceiver`
trait in `pump.rs`).

### 4b. runner wiring (connection/up/runner.rs)

`Runner::run`'s 11-step order is preserved (`up/runner.rs:69-167`). Step 8
(`request_static_wg_tunnel`, `:136-141` / `:335-378`) sends `SetupTunnel`
instead of `StaticWgRouting`, receives `TunnelReady` (interface name) + the TUN
fd (3a), opens the WG session (4a), and spawns
`pump::run(WgTunnel, net halves, tun
halves)`. Step 9 killswitch (`:143-145`) is
fed the interface name from `TunnelReady`. Wire `PumpExit::Expired` → the
existing reconnect path (`force_reconnect`, `core/mod.rs:1740`).

### 4c. teardown ordering

Worker stops the pump and drops its TUN-fd copy **first**, then root removes
routes/DNS and drops its fd. A TUN interface lives while any fd is open, so root
closing last guarantees routes are gone before the interface vanishes.
`TearDownWg` is still sent by core before the down runner (`core/mod.rs:1610`);
extend root's `teardown_any_routing` to close the TUN fd + restore DNS. Add the
fd + DNS restore to the crash-recovery path (`main.rs:543` →
`teardown_any_routing`).

### 4d. config + packaging

- Deprecate `[wireguard] listen_port` (`config/v6.rs:109`, still emitted at
  `wireguard.rs:163-165`): accept + warn + ignore - meaningless without a UDP
  socket.
- Drop `wireguard-tools` + (macOS) `wireguard-go` from install requirements /
  Nix / docs.
- CI must cover `aarch64-apple-darwin` for NepTUN from day one (not in its
  official support table, but libtelio ships there). Workspace MSRV ≥ 1.89
  (NepTUN's pin); currently 1.94 - fine.

## Test plan

- **Unit** (done in Phase 2): pump state machine vs a second `Tunn` - handshake,
  data, drain, allowed-IPs, replay/garbage/oversized drops, expiry reaction,
  clean close. Keygen KAT (Phase 1).
- **fd passing** (root-free, add in 3a): socketpair send/recv of a pipe fd +
  leak checks on error paths.
- **Integration** (needs root / CAP_NET_ADMIN in CI): real TUN + pump on macOS
  and Linux; DNS set/restore; IPv6 blackhole present during up, absent after
  down.
- **e2e** (needs the staging gvpn server; targets `gvpn-staging*.toml`): full
  client both platforms - connect, ICMP `ping 10.128.0.1` through the tunnel,
  sustained transfer, reconnect storm, unclean-kill recovery (no leaked
  routes/DNS/interfaces), killswitch with the NepTUN-owned interface name.
- **Benchmark** userspace WG throughput vs the current path (expected non-issue:
  the mixnet, not WG crypto, is the bottleneck; macOS is already userspace).

## Blockers to clear first

- **macOS root unit tests do not compile on `main`** (`routing::Error` is not
  `PartialEq`, but `route_ops_macos.rs` tests `assert_eq!` on
  `Result<_, routing::Error>`). Pre-existing, unrelated to this port, CI is
  green because it runs Linux. Fix (rewrite those asserts) before running any
  root-crate tests on darwin - it blocks all of Phase 3/4 macOS testing.
- Optional follow-up (out of scope for the swap): surface `Tunn::stats()`
  (handshake age, RTT, loss) in `Status` for the UIs - free introspection the
  old path never had.
