# NepTUN port: Phase 3-4 implementation & testing guide

Companion to `neptun-spec.md` and `neptun-phase3-4-blueprint.md`. This guide covers
the **privileged / integration** half that cannot be exercised on a laptop (it needs
root + a live gvpn staging server, and the Linux TUN path does not even *compile* on
an arm64 macOS host). Every file:line anchor here was re-verified against the current
tree during the Phase 3 work.

## What is landed and verified (worker-side, root-free)

These are implemented, unit-tested, and committed on `tb/202607-neptun`:

| Piece | Module | Tests |
|---|---|---|
| Phase 1 in-process keygen | `gnosis_vpn-lib/src/wireguard.rs` | RFC 7748 KAT |
| Phase 2 `WgTunnel` + pump | `gnosis_vpn-lib/src/wg_tunnel/{tunnel,pump}.rs` | 18 |
| **3a fd passing** (SCM_RIGHTS) | `gnosis_vpn-lib/src/socket/fd_passing.rs` | 5 (transfer, CLOEXEC, no-cmsg, EOF, closed-peer) |
| **Session adapter** (`NetworkSender/Receiver`) | `gnosis_vpn-lib/src/wg_tunnel/session.rs` | 3 (roundtrip, EOF, sequential) |
| **TUN adapter** (`TunSender/Receiver`, utun header) | `gnosis_vpn-lib/src/wg_tunnel/tun.rs` | 5 (header select, strip, prepend, headerless, shared-fd) |

The pump is generic over its four endpoint traits, so the runner cutover (4b) is just
"construct these adapters and call `pump::run`."

## Architecture: the data-path change

- **Today:** worker opens a UDP session -> `create_udp_client_binding` bridges it to a
  loopback UDP socket -> root's `wg-quick` brings up a *kernel* WG interface that dials
  that loopback port. Crypto is in the kernel; the private key travels to root.
- **NepTUN:** worker owns the WG crypto (`WgTunnel`), splices the raw `HoprSession`
  straight into the pump, and writes plaintext to a TUN fd that **root** created and
  handed over. No loopback UDP, no kernel WG, no key crosses the process boundary.

## Recommended strategy: additive, not a hard swap

Do **not** delete the working `wg-quick` path until the NepTUN path is proven on
staging. Add the new protocol variants and the new root capability **alongside**
`StaticWgRouting`, gate the worker's choice behind config/env (e.g.
`GNOSISVPN_DATAPLANE=neptun`), and only do the Phase 3f deletions once e2e is green on
both platforms. This keeps every intermediate commit shippable and CI-green.

---

## 3a-wiring. Dedicated TUN-fd socket at worker spawn

The fd travels on a **second** socketpair, never on the newline-JSON channel (a
`BufReader` there would eat bytes past a newline that a raw `recvmsg` needs). Mirror
the existing `INTERNAL_WORKER_FD` setup in `gnosis_vpn-root/src/main.rs::setup_worker`
(lines 1172-1218):

```rust
// after the existing (parent_socket, child_socket) pair:
let (parent_tun_sock, child_tun_sock) = std::os::unix::net::UnixStream::pair()?;
// clear CLOEXEC on the child end exactly as main.rs:1178-1190 does for child_socket
clear_cloexec(child_tun_sock.as_raw_fd());
worker_command.env(socket::worker::ENV_VAR_TUN_FD, format!("{}", child_tun_sock.into_raw_fd()));
// keep `parent_tun_sock` (std UnixStream) in WorkerChild for later send_fd
```

- Add `pub const ENV_VAR_TUN_FD: &str = "INTERNAL_WORKER_TUN_FD";` next to
  `ENV_VAR` in `gnosis_vpn-lib/src/socket/worker.rs`.
- Worker reads the fd number from that env at startup, rebuilds a
  `std::os::unix::net::UnixStream` via `FromRawFd`, and keeps it for `recv_fd`.
- Store `parent_tun_sock` on the root `WorkerChild` struct (main.rs:91-94) so the
  `SetupTunnel` handler can `fd_passing::send_fd(&parent_tun_sock, tun_raw_fd)`.

**Test (root-free, add now):** already covered structurally by
`socket::fd_passing` tests. Add one integration test that spawns a child process,
passes a pipe fd through a real `UnixStream::pair` across the env-var handshake, and
asserts bytes flow -- gated behind a helper binary (or `#[ignore]` + a CI job).

---

## 3b. Protocol change (`gnosis_vpn-lib/src/event/mod.rs`)

Add (do not yet remove `StaticWgRouting`):

```rust
// RunnerToRoot (in-process, carries the oneshot responder) -- around line 73
SetupTunnel {
    interface_address: String,   // registration.address()
    mtu: u32,                    // wireguard::WG_MTU (1420)
    dns: Option<String>,         // wg_data.wg.config.dns, already overwrite-resolved
    peer_ips: Vec<Ipv4Addr>,
    resp: oneshot::Sender<Result<String, String>>, // Ok = interface name
},

// RequestToRoot (IPC, request_id replaces the responder) -- around line 107
SetupTunnel {
    request_id: u64,
    interface_address: String,
    mtu: u32,
    dns: Option<String>,
    peer_ips: Vec<Ipv4Addr>,
},

// ResponseFromRoot -- around line 135
/// Ok = the resolved TUN interface name (e.g. "utun8" / "wg0_gnosisvpn").
TunnelReady { request_id: u64, res: Result<String, String> },
```

**No key material crosses.** `WireGuardData` (event/mod.rs:84) and its private key stay
in the worker where `WgTunnel` lives. The fd is delivered out-of-band (3a-wiring), not
in `TunnelReady`.

Wire the two relays (both are one-line additions mirroring `StaticWgRouting`):

- **core forwarder** `gnosis_vpn-lib/src/core/mod.rs:963-976`: match
  `RunnerToRoot::SetupTunnel { .. }`, allocate `request_id` via `next_request_id()`,
  store `Responder::Str(resp)`, emit `RequestToRoot::SetupTunnel`.
- **core response** `core/mod.rs:302-314`: match `ResponseFromRoot::TunnelReady`
  identically to the `StaticWgRouting` arm (look up `request_id`, forward into the
  runner's oneshot).
- The worker relay (`gnosis_vpn-worker/src/main.rs:427` / `:401`) is opaque -- it
  forwards any `RequestToRoot` / `ResponseFromRoot`, so **no change** needed there.

**Test (root-free, add now):** serde round-trip for each new variant
(`serde_json::to_string` -> `from_str` -> assert equal) in an `event` test module.

---

## 3c. Root TUN provisioning (`gnosis_vpn-root`)

Dispatch from `main.rs::incoming_worker_request` (add a `RequestToRoot::SetupTunnel`
arm next to `StaticWgRouting` at main.rs:1066). It should:

1. create the TUN, get `(OwnedFd, interface_name)`;
2. assign address + MTU, bring the link up;
3. install IPv4 split routes + peer/RFC1918 bypass (reuse existing `StaticRouter`
   logic; see 3e for the IPv6 blackhole);
4. set DNS (3d);
5. `fd_passing::send_fd(&worker_child.parent_tun_sock, fd.as_raw_fd())`;
6. reply `TunnelReady { Ok(interface_name) }` on the JSON channel **after** the fd is
   queued (happens-before; the worker waits for `TunnelReady` then `recv_fd`).

Keep root's copy of the fd (in the routing actor state) open until teardown (4c).

### Linux (`gnosis_vpn-root/src/routing/linux.rs`) -- DOES NOT COMPILE ON macOS HOST

Replace the `wg_quick_up` call (linux.rs:159). New helper, e.g. `tun_linux.rs`:

```rust
// open /dev/net/tun, request a named TUN with no packet-info header
let fd: OwnedFd = OwnedFd::from(std::fs::OpenOptions::new()
    .read(true).write(true).open("/dev/net/tun")?);
let mut ifr: libc::ifreq = std::mem::zeroed();
// name = wireguard::WG_INTERFACE ("wg0_gnosisvpn"); KEEP this name (killswitch/routing ref it)
copy_name(&mut ifr.ifr_name, wireguard::WG_INTERFACE);
ifr.ifr_ifru.ifru_flags = (libc::IFF_TUN | libc::IFF_NO_PI) as i16;
// TUNSETIFF = _IOW('T', 202, int)
if libc::ioctl(fd.as_raw_fd(), tunsetiff_code(), &ifr) < 0 { .. }
// address + MTU 1420 + up via rtnetlink (already a dep): AddressMessage + LinkMessage
```

- MTU = `wireguard::WG_MTU` (1420). Address = `interface_address` from `SetupTunnel`.
- Interface name stays the compile-time `wireguard::WG_INTERFACE`, so
  `setup_vpn_routes` (linux.rs:75-85) needs no change.
- `TUNSETIFF` ioctl number: use the `nix`/`ioctl_write_int!` macro or hardcode
  `0x400454ca`. Verify with a debug print against `/usr/include/linux/if_tun.h` on the
  target.

### macOS (`gnosis_vpn-root/src/routing/macos.rs`) -- compiles on host, needs root to run

Replace `wg_quick_up` (macos.rs:168). Create a `utun` via a control socket:

```rust
let fd = libc::socket(libc::PF_SYSTEM, libc::SOCK_DGRAM, libc::SYSPROTO_CONTROL);
let mut info: libc::ctl_info = zeroed();
copy_bytes(&mut info.ctl_name, b"com.apple.net.utun_control\0");
libc::ioctl(fd, CTLIOCGINFO, &mut info)?;           // CTLIOCGINFO = _IOWR('N', 3, ctl_info)
let mut addr: libc::sockaddr_ctl = zeroed();
addr.sc_len = size_of::<libc::sockaddr_ctl>() as u8;
addr.sc_family = libc::AF_SYSTEM as u8;
addr.ss_sysaddr = libc::AF_SYS_CONTROL as u16;
addr.sc_id = info.ctl_id;
addr.sc_unit = 0;                                    // 0 = kernel picks the utunN unit
libc::connect(fd, &addr as *const _ as *const libc::sockaddr, size_of_val(&addr) as u32)?;
// read the assigned name (utun8) back:
let mut name = [0u8; libc::IFNAMSIZ]; let mut len = name.len() as libc::socklen_t;
libc::getsockopt(fd, libc::SYSPROTO_CONTROL, UTUN_OPT_IFNAME, name.as_mut_ptr().cast(), &mut len)?;
```

- `UTUN_OPT_IFNAME = 2`. The kernel returns the real name (e.g. `utun8`); store it in
  `wg_interface_name` (macos.rs:65) and thread it through `setup_vpn_routes(&name)`
  exactly as the current code already does with the wg-quick-resolved name.
- Assign address + MTU with the existing macOS `route`/`ifconfig` style already used in
  `route_ops_macos.rs` (`ifconfig <utun> <addr> <addr> up`, `ifconfig <utun> mtu 1420`).
- The worker's `tun::tun_endpoints(fd, PLATFORM_TUN_HEADER_LEN)` already handles the
  4-byte utun AF header (verified by the `tun.rs` tests).

The dynamic-name dance through `/var/run/wireguard/<iface>.name`
(`wg_tooling.rs:62-81`) disappears -- root created the fd and knows the name.

**Test:** integration only, needs root/CAP_NET_ADMIN. See the test matrix.

---

## 3d. DNS (the one genuinely new reimplementation)

`wg-quick` did this internally; take it over in root. The DNS string is already
overwrite-resolved by config (`config/v6.rs:323-342`): absent -> `1.1.1.1,8.8.8.8`;
`overwrite=true` -> servers-or-default; `overwrite=false` -> `None` (push nothing).
Pass it through `SetupTunnel { dns }` and apply in root:

- **Linux:** `resolvconf -a <iface>` fed the servers (what wg-quick calls), with
  `resolvectl dns <iface> <servers>` (systemd-resolved) as the modern path.
- **macOS:** `networksetup -setdnsservers <service> <servers>` or `scutil`.
- **Teardown must restore prior DNS** on `TearDownWg` and crash recovery (4c) -- root
  has no DNS code today because it lived in `wg-quick down`. Capture the pre-change
  resolver state at setup and restore it at teardown.

**Test (root-free helper, worth adding):** put the server-string parse + argv
construction in a pure function (`fn set_dns_argv(iface, servers) -> Vec<String>`,
`fn restore_dns_argv(...)`) and unit-test the argv. Running + restore is
integration-tested under root.

---

## 3e. IPv6 blackhole relocation

`wg-quick` emitted these as PreUp/PostDown config lines; move them into the routing
setup/teardown phases. Verbatim strings from `wireguard.rs:167-185`:

**Linux** (add in `setup_vpn_routes` phase 3, remove in `remove_vpn_routes`):
```
ip -6 route del blackhole ::/1 || true
ip -6 route del blackhole 8000::/1 || true
ip -6 route add blackhole ::/1
ip -6 route add blackhole 8000::/1
```
**macOS**:
```
route -n add -blackhole -inet6 ::/1 ::1
route -n add -blackhole -inet6 8000::/1 ::1
route -n delete -blackhole -inet6 ::/1 ::1
route -n delete -blackhole -inet6 8000::/1 ::1
```

`route_ops` is IPv4-only today (`route_ops.rs:8`), so add a small IPv6 blackhole helper
(shell-based on macOS mirrors the existing `route`-CLI approach; Linux can shell `ip`
or extend netlink). Keep it idempotent (del-before-add) so a stale route never blocks
setup.

**Test (root-free helper):** pure argv construction unit-tested against the verbatim
strings above; running is integration-tested.

---

## 4a. Session splice (`gnosis_vpn-lib/src/hopr/api.rs`)

Today the WG UDP session is made by `Hopr::open_session` (api.rs:79), whose UDP branch
calls `create_udp_client_binding` (api.rs:145), which internally does
`factory.create_session(...)` then spawns `bind_session_to_stream` (the loopback byte
pump to delete). Add a sibling that calls `create_session` **directly**:

```rust
use hopr_utils_session::{HopSessionFactory, SessionFactory}; // SessionFactory brings create_session into scope
// HoprSession + HoprSessionConfigurator via edgli::hopr_lib::exports::transport

pub async fn open_wg_session(
    &self,
    destination: Address,
    target: SessionTarget,          // keep SessionTarget::UdpStream
    cfg: HoprSessionClientConfig,   // keep default Segmentation capability
) -> Result<(HoprSession, HoprSessionConfigurator), HoprError> {
    let factory = HopSessionFactory::new(self.edgli.as_hopr());
    factory.create_session(destination, target, cfg)
        .await
        .map_err(|e| HoprError::Construction(format!("failed to create wg session: {e}")))
}
```

Verified signatures (hopr-utils-session `d670a6c`):
`SessionFactory::create_session(&self, dest, target, cfg) -> Result<(HoprSession, HoprSessionConfigurator), anyhow::Error>`
(lib.rs:276); `HopSessionFactory::new(Arc<Hopr<..>>)` (lib.rs:294); `HoprSession`
implements tokio `AsyncRead + AsyncWrite + Unpin` under `runtime-tokio` (types.rs:359-373).

Then in the runner: `let (r, w) = tokio::io::split(session);` and build the pump's
network halves with the **landed** adapters:
`SessionReceiver::new(r)` and `SessionSender::new(w)`.

> **Spec risk #1 (must verify on staging):** `SessionReceiver::recv` assumes the
> session preserves datagram boundaries (one peer `send` = one local `recv`). This is
> the expected behavior for `SessionTarget::UdpStream`. If two WG datagrams ever
> coalesce into one read under load, switch `session.rs` to explicit `u16` length-prefix
> framing (the fallback is documented in that file) -- a local change that does not touch
> the pump. **Validate with the sustained-transfer + reconnect-storm e2e below** and by
> asserting `recv` lengths equal the peer's `send` lengths under load.

Keep the `HoprSessionConfigurator` for SURB balancer updates: `adjust_session`
(api.rs:258) still applies for the step-11 main-session adjust.

---

## 4b. Runner wiring (`gnosis_vpn-lib/src/connection/up/runner.rs`)

Preserve the 11-step order (runner.rs:69-167). Changes:

- **Step 6** (`open_ping_session`, runner.rs:119): when the NepTUN dataplane is on,
  call `open_wg_session` and hold the real `HoprSession` instead of only metadata.
- **Step 8** (`request_static_wg_tunnel`, runner.rs:335-378): send `SetupTunnel`
  instead of `StaticWgRouting`; drop the `endpoint`/`peer_info` construction
  (runner.rs:343-359) -- there is no WG UDP endpoint. On `TunnelReady`, `recv_fd` the
  TUN fd (3a-wiring), build `WgTunnel::new(priv, server_pub, psk, allowed_ips)` and
  `tun::tun_endpoints(fd, PLATFORM_TUN_HEADER_LEN)`, then spawn
  `pump::run(wg_tunnel, SessionSender, SessionReceiver, TunWriter, TunReader)`.
- **Step 9** (killswitch, runner.rs:143-145): feed it the interface name from
  `TunnelReady` (unchanged shape).
- Map `PumpExit::Expired` -> the existing reconnect path (`force_reconnect`,
  `core/mod.rs:1740`); `PumpExit::{NetworkClosed,TunClosed}` -> teardown + reconnect.

The pump task handle lives for the connection's lifetime; abort it on teardown before
root drops the fd (4c).

---

## 4c. Teardown ordering

Worker stops the pump and drops its TUN-fd copy **first**; then root removes
routes/DNS and drops its fd. A TUN interface persists while any fd is open, so root
closing last guarantees routes are gone before the interface vanishes.

- `TearDownWg` is still sent by core before the down runner (`core/mod.rs:1606`,
  `:1737`). Extend root's `teardown_any_routing` (main.rs:1272) -> routing actor
  teardown to also: close the held TUN fd, restore DNS (3d), delete the IPv6 blackhole
  routes (3e).
- Add the fd-close + DNS-restore to the crash-recovery path
  (`incoming_worker_exit`, main.rs:1107; `cleanup_worker_resources`, main.rs:1291).

---

## 3f. Deletions (only after e2e is green on both platforms)

- `gnosis_vpn-root/src/wg_tooling.rs` (probes + `wg-quick up/down` + name-file dance);
  its callers: `main.rs:35,461,463`, `routing/wg_ops.rs:16,34,39`.
- `gnosis_vpn-root/src/routing/wg_ops.rs` (`WgOps`/`RealWgOps`); its callers:
  `routing/mod.rs:13`, `linux.rs:{23,42,65,159,171,188}`, `macos.rs:{25,46,64,168,181,198}`.
- The WG-config-file writer in `wireguard.rs::to_file_string` + `WG_CONFIG_FILE`.
  Keep `WG_INTERFACE` / `WG_MTU`.
- Prune `wireguard::Error::{ShellCommandExt, Dirs, WgGenKey}` once their last producers
  are gone (verify with `cargo build` -- they exist only for `#[from]` today).
- Deprecate `[wireguard] listen_port` (config/v6.rs:109): accept + warn + ignore
  (meaningless without a UDP socket).
- Drop `wireguard-tools` / `wireguard-go` from install docs / Nix.

---

## Test matrix

| Layer | Where | Needs |
|---|---|---|
| Keygen KAT, `WgTunnel`, pump | `wg_tunnel/{tunnel,pump}.rs` | none -- **done** |
| fd passing | `socket/fd_passing.rs` | none -- **done** |
| Session/TUN adapters | `wg_tunnel/{session,tun}.rs` | none -- **done** |
| Protocol serde round-trip | `event/mod.rs` | none -- add with 3b |
| DNS / blackhole argv builders | root helpers | none -- add with 3d/3e |
| Real TUN + pump; DNS set/restore; blackhole present-then-absent | both platforms | **root / CAP_NET_ADMIN** |
| Full client e2e: connect, `ping 10.128.0.1`, sustained transfer, reconnect storm, unclean-kill recovery (no leaked routes/DNS/iface), killswitch with NepTUN iface name | `gvpn-staging*.toml` | **root + staging server** |
| Throughput vs current path | both | staging; expected non-issue (mixnet is the bottleneck; macOS was already userspace) |

## Blockers to clear first

- **macOS root unit tests do not compile on `main`**: `route_ops_macos.rs:250-276`
  `assert_eq!` on `Result<(String, Option<String>), routing::Error>`, but
  `routing::Error` is not `PartialEq`. Pre-existing, unrelated to this port (CI is
  green because it runs Linux). Fix those asserts (match on the `Ok`/`Err` explicitly)
  before running any root-crate tests on darwin -- it blocks all Phase 3/4 macOS testing.
- CI must add `aarch64-apple-darwin` for NepTUN (not in its official table, but libtelio
  ships there). Workspace MSRV >= 1.89 (NepTUN's pin); currently 1.94 -- fine.
- Optional follow-up: surface `Tunn::stats()` (handshake age, RTT, loss) in `Status`.
