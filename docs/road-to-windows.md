# Road to Windows

Findings from an initial feasibility pass on porting the client to Windows.
Not a plan — a map of what breaks and what the replacement looks like.
Companion to [road-to-android.md](./road-to-android.md) and
[road-to-ios.md](./road-to-ios.md); read either for the shared background
(`edgli` is a pure Rust library, current three-binary privilege-separated
architecture: `gnosis_vpn-root` (root daemon), `gnosis_vpn-worker`
(unprivileged), `gnosis_vpn-ctl`/UI (named Unix socket)).

## Windows is a port, not a rewrite

Mobile forced an architecture change: Android collapses to one sandboxed
process, iOS splits into an app + a memory-capped extension, and both are
gated behind a single OS-blessed VPN API (`VpnService`,
`NEPacketTunnelProvider`). Windows has no such gatekeeper — a desktop OS with
admin rights and a service model, closer in shape to today's Linux/macOS
split than either mobile target. The root/worker privilege separation maps
over conceptually: `gnosis_vpn-root` becomes a Windows Service (running as
`LocalSystem` or a dedicated service account), `gnosis_vpn-worker` and the UI
stay unprivileged processes in the user's session.

The TUN side has a de facto standard: [Wintun](https://www.wintun.net/), the
WireGuard project's NDIS driver, ships pre-built and signed — no custom
kernel driver development or signing needed, just calls into its C API (the
`wintun` crate). This is what WireGuard-Windows, Mullvad, and most consumer
Windows VPN clients build on.

## What's already portable: the trait boundaries

The Linux/macOS split isn't ad hoc `cfg_if!` sprinkled through business
logic — it's organized behind real trait boundaries that a Windows backend
slots into without touching the abstraction itself:

- **`Routing`** (`gnosis_vpn-root/src/routing/mod.rs`) — `setup`/`teardown`/
  `wan_changed`/bypass-route bookkeeping, dispatched via `cfg_if!` to
  `linux.rs` or `macos.rs`.
- **`RouteOps`** (`route_ops.rs`) — `get_wan_route_for`/`route_add`/
  `route_del`, implemented by `NetlinkRouteOps` (`route_ops_linux.rs`, typed
  netlink via `rtnetlink::Handle`) and `DarwinRouteOps` (`route_ops_macos.rs`,
  shells out to BSD `route`).
- **`WgOps`** (`wg_ops.rs`) — `wg_quick_up`/`wg_quick_down`, with
  `RealWgOps` delegating to `wg_tooling::up`/`down`. Its own doc comment
  calls this out as "the only routing abstraction that still uses an
  external CLI tool."
- **`killswitch::Firewall`** (`gnosis_vpn-lib/src/killswitch/`) — no trait,
  but `linux.rs` (`nftnl`-built `nftables` table) and `macos.rs` (`pfctl`
  crate, named PF anchor) expose the *identical* struct API: `new()`,
  `apply_policy(interface, allowed_ips, lan_lockdown)`, `reapply_policy(...)`,
  `reset_policy()`.
- **`route_health.rs`** (1291 lines) — confirmed zero `cfg(target_os)`
  matches. Pure async/Tokio reconnect-and-health-check logic over TCP/HTTP,
  no platform calls at all. Directly reusable, same conclusion as the
  Android doc.

None of this needs restructuring for Windows — it needs new arms.

## What replaces what

| Current (Linux/macOS) | Windows replacement |
|---|---|
| `NetlinkRouteOps` / `DarwinRouteOps` (`RouteOps` impls) | `WindowsRouteOps` via IP Helper API (`GetIpForwardTable2`/`CreateIpForwardEntry2`/`DeleteIpForwardEntry2`, `windows`/`windows-sys` crate) |
| `killswitch::linux::Firewall` (nftables) / `killswitch::macos::Firewall` (pfctl) | `killswitch::windows::Firewall` via the Windows Filtering Platform (WFP) — same approach Mullvad and WireGuard-Windows use for their kill switches; matches the existing `new()`/`apply_policy()`/`reset_policy()` shape |
| `device_monitor/linux/rtnetlink.rs` (netlink) / `device_monitor/macos.rs` (raw `PF_ROUTE` socket) | `NotifyIpInterfaceChange`/`NotifyRouteChange2` (IP Helper API callbacks) feeding the same shared `NetworkEvent` enum |
| `wg_tooling.rs` shelling out to `wg-quick` | No direct equivalent — see below |
| Unix socket (ctl↔root, `/var/run/gnosisvpn.sock`) | Named pipe (`\\.\pipe\gnosisvpn`) via `tokio::net::windows::named_pipe` — mechanical swap |
| Unix socket pair + raw fd env var + `uid()`/`gid()` (root↔worker) | No direct equivalent — see below |
| `app_nap.rs` (macOS-only real impl, Linux no-op) | Add a `#[cfg(target_os = "windows")]` no-op arm — services aren't App-Nap-throttled, nothing else needed |
| `ping.rs` `cfg_if!` arms (Linux/macOS CLI flags + `ping::RAW`/`ping::DGRAM`) | New arm needed — see below |
| No systemd/launchd unit in-repo | Windows Service wrapper (`windows-service` crate) + installer, built from scratch either way |

## `wg-quick` has no Windows equivalent either

This isn't a Windows-specific gap — WireGuard-Windows itself doesn't use
`wg-quick` (a Bash script wrapping `ip`/`route`/`resolvconf`, none of which
exist on Windows). It has its own service/driver model instead
(`wireguard.exe /installtunnelservice`). Two options, in increasing effort
and decreasing CLI dependency:

- Shell out to `wireguard.exe /installtunnelservice <conf>` /
  `/uninstalltunnelservice`, mirroring today's `wg_tooling::up`/`down` shape
  behind `WgOps` — requires the official WireGuard-Windows MSI/driver bundle
  present on the target machine.
- Embed a userspace WireGuard implementation (`boringtun`, already a
  candidate in the Android doc) driving a `wintun` adapter directly — no
  external installer dependency, consistent with how `RouteOps` and
  `Firewall` are heading (library calls, not CLI shelling), at the cost of
  reimplementing what `wireguard.exe` already does.

## The two IPC problems are not the same size

**ctl↔root** (`gnosis_vpn-lib/src/socket/root.rs`, `UnixStream::connect` +
JSON) is a clean swap to a named pipe — same request/response shape, just a
different transport.

**root↔worker** (`gnosis_vpn-root/src/main.rs::setup_worker()`) is the
single largest redesign item in this doc. Today it does:

```rust
let (parent_socket, child_socket) = UnixStream::pair()...;
let fd = child_socket.as_raw_fd();
/* clear FD_CLOEXEC via libc::fcntl */
worker_command
    .env(socket::worker::ENV_VAR, format!("{}", child_socket.into_raw_fd()))
    .uid(self.worker_user.uid)
    .gid(self.worker_user.gid);
```

Both parts are POSIX-specific with no Windows equivalent in the same shape:

- Passing a raw fd number through an env var relies on fork/exec fd
  inheritance. Windows would use a second named pipe instead, or duplicate a
  `HANDLE` into the child via `STARTUPINFOEX`/
  `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
- `.uid()`/`.gid()` drop privileges by exec'ing as the unprivileged
  `gnosisvpn` system user (resolved via the `uzers` crate in
  `Worker::from_system()`). Windows has no uid/gid model; the equivalent is
  `CreateProcessAsUser`/`CreateProcessWithTokenW` with a token for a
  dedicated low-privilege service account — a materially different API, not
  a parameter swap.

## `ping.rs` needs real platform work, not just a new arm

Already handles Linux vs. macOS CLI flags and `ping`-crate socket types via
`cfg_if!`, but every piece needs a Windows branch:

- The `which ping` availability probe doesn't exist on Windows (`where`, or
  drop the probe and always try `ping.exe`).
- `ping.exe`'s own flags differ (e.g. `-n` count, `-w` timeout in
  milliseconds) from both the Linux and macOS argument sets already
  branched on.
- `parse_duration()` greps for `"rtt"`/`"round-trip"` in output — Windows
  `ping.exe` prints `Average = Xms`, so the parser needs a third pattern,
  not just new flags.
- The `ping` crate fallback (`ping::RAW`/`ping::DGRAM`) does support Windows
  via WinSock per its own docs, but no `cfg_if!` arm selects it yet.

## Service installation

No `.service` or `.plist` file exists anywhere in this repo to adapt — on
Linux/macOS, daemon supervision is handled by packaging outside this
codebase. A Windows Service wrapper (via the `windows-service` crate,
registered through the Service Control Manager) and an installer (WiX/Inno
Setup, to place the binaries, register the service, and install the Wintun
driver) would need to be built from scratch — the same amount of new work
either platform would require, so not a Windows-specific deficit.

## Build

Add the `x86_64-pc-windows-msvc` Rust target (and `aarch64-pc-windows-msvc`
if ARM64 Windows matters), and verify `edgli`'s dependency tree cross-compiles
clean — `rustls`, not a native TLS backend, same consideration as the Android
doc. The `mnl`/`nftnl`/`pfctl`/`objc2`/`objc2-foundation` platform
dependencies are simply dropped for this target, same pattern as the
existing `cfg(target_os = "linux")` / `cfg(target_os = "macos")` dependency
sections in `gnosis_vpn-root/Cargo.toml` and `gnosis_vpn-lib/Cargo.toml`
(neither has a `cfg(windows)` section today — Windows was never scaffolded
as a target).

## Bottom line

Compared to mobile, this is much closer to an actual port: no single
gatekeeper API forcing an architecture change, no entitlement approval gate,
and the existing `RouteOps`/`WgOps`/`Firewall` seams are exactly where a
Windows backend needs to attach — new modules, not restructuring. The real
work is concentrated in three places: a WireGuard control path that doesn't
shell out to a script that doesn't exist on this OS, a from-scratch
Windows Service + installer (nothing in-repo to adapt), and — the one
genuinely hard problem — replacing fork/exec fd-inheritance and uid/gid
privilege dropping in the root↔worker channel with Windows' token- and
handle-based equivalents.
