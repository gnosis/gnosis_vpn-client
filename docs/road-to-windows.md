# Road to Windows

## Current architecture (recap)

Three privilege-separated binaries:

- **`gnosis_vpn-root`** (privileged) — creates the TUN device
  (`neptun::device::TunSocket`, a cross-platform ioctl helper covering Linux
  and macOS; no WireGuard crypto involved), sets up bypass/split routes,
  DNS, IPv6 blackhole, and the kill switch (`nftables` on Linux/`pfctl` on
  macOS), persists crash-recovery state, and spawns the worker. Root does
  no WireGuard protocol work at all.
- **`gnosis_vpn-worker`** (unprivileged) — the entry point into the mixnet
  (`edgli`/hoprd session client) _and_ the entire WireGuard data plane: it
  receives the TUN fd from root over a dedicated Unix socket via
  `SCM_RIGHTS`, builds a `WgTunnel` (`gnosis_vpn-lib/src/wg_tunnel/`,
  wrapping `neptun::noise::Tunn` — NordSecurity's WireGuard-rs fork), and
  runs the async pump moving packets between the TUN device and the mixnet
  session. WireGuard keys and all crypto live only here.
- **`gnosis_vpn-ctl`**/UI — talks to root over a named Unix socket, no
  visibility into the TUN fd or WireGuard state.

The root/worker privilege separation maps over conceptually to Windows
— `gnosis_vpn-root` becomes a Windows Service, `gnosis_vpn-worker` and the
UI stay unprivileged processes in the user's session.

## What's already portable

- **`Routing`**/**`RouteOps`** (`gnosis_vpn-root/src/routing/`) —
  `NetlinkRouteOps` (`route_ops_linux.rs`, typed netlink) and
  `DarwinRouteOps` (`route_ops_macos.rs`, shells out to BSD `route`) both
  implement the same `RouteOps` trait.
- **`killswitch::Firewall`** (`gnosis_vpn-lib/src/killswitch/`) — `linux.rs`
  (`nftables` via `nftnl`) and `macos.rs` (`pfctl` crate) expose the
  identical struct API: `new()`, `apply_policy(...)`, `reapply_policy(...)`,
  `reset_policy(...)`, plus `capture_recovery_state()`/
  `reset_policy_with_state(...)` for crash recovery.
- **`route_health.rs`** — zero platform-specific code, pure async
  reconnect/health-check logic over TCP/HTTP. Directly reusable.
- **WireGuard itself needs no CLI tool on any platform already** — it's
  `neptun` (`gnosis_vpn-lib/src/wg_tunnel/`, wrapping `neptun::noise::Tunn`)
  running in-process in the worker. There's no `wg-quick`-shaped gap to fill
  for Windows the way there would be if the client still shelled out to an
  external WireGuard tool.

None of this needs restructuring for Windows — it needs new arms.

## What replaces what

| Current (Linux/macOS)                                                                             | Windows replacement                                                                                                                                                                                  |
| ------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `NetlinkRouteOps`/`DarwinRouteOps` (`RouteOps` impls)                                             | `WindowsRouteOps` via the IP Helper API (`GetIpForwardTable2`/`CreateIpForwardEntry2`/`DeleteIpForwardEntry2`, `windows`/`windows-sys` crate)                                                        |
| `killswitch::linux::Firewall` (nftables) / `killswitch::macos::Firewall` (pfctl)                  | `killswitch::windows::Firewall` via the Windows Filtering Platform (WFP) — the same approach Mullvad and WireGuard-Windows use; matches the existing `new()`/`apply_policy()`/`reset_policy()` shape |
| `device_monitor/linux/rtnetlink.rs` (netlink) / `device_monitor/macos.rs` (raw `PF_ROUTE` socket) | `NotifyIpInterfaceChange`/`NotifyRouteChange2` (IP Helper API callbacks) feeding the same shared `NetworkEvent` enum                                                                                 |
| Root's TUN creation (`routing/tun.rs`, `neptun::device::TunSocket`)                               | `neptun`'s device feature only ships Linux (`tun_linux.rs`) and macOS (`tun_darwin.rs`) backends — Windows needs the separate `wintun` crate (the WireGuard project's signed NDIS driver) instead    |
| Root→worker TUN fd hand-off (`gnosis_vpn-lib/src/socket/fd_passing.rs`, `SCM_RIGHTS`)             | No direct equivalent — see below                                                                                                                                                                     |
| `routing/dns.rs` (`resolvectl`/`resolvconf`/`scutil`)                                             | IP Helper API (`SetInterfaceDnsSettings`) or `netsh interface ip set dns`                                                                                                                            |
| `routing/ipv6_blackhole.rs`, `routing/sweep.rs` (crash-recovery)                                  | Same `WindowsRouteOps` backend for the blackhole routes; sweep needs a Windows-native `TeardownState` (same idea — persist what was applied, reverse it if the service restarts after a crash)       |
| ctl↔root Unix socket (`gnosis_vpn-lib/src/socket/root.rs`)                                        | Named pipe (`\\.\pipe\gnosisvpn`) via `tokio::net::windows::named_pipe` — mechanical swap                                                                                                            |
| Root→worker JSON control channel + privilege drop (`.uid()`/`.gid()`)                             | No direct equivalent — see below                                                                                                                                                                     |
| `app_nap.rs` (macOS-only real impl, Linux no-op)                                                  | Add a `#[cfg(target_os = "windows")]` no-op arm — services aren't App-Nap-throttled                                                                                                                  |
| `ping.rs` `cfg_if!` arms (Linux/macOS CLI flags + `ping::RAW`/`ping::DGRAM`)                      | New arm needed — see below                                                                                                                                                                           |

## Getting the TUN handle from root to worker

This is the least certain part of this doc. On Linux/macOS, root creates
the TUN device and hands the worker a bare fd over a dedicated Unix socket
via `SCM_RIGHTS` — a well-understood, already-implemented mechanism
(`gnosis_vpn-lib/src/socket/fd_passing.rs`).

Windows has no `SCM_RIGHTS` equivalent. The natural analog is
`DuplicateHandle(rootProcess, tunHandle, workerProcess, &dupHandle, ...)` —
callable by root since it already holds a handle to the worker process it
spawned — but this depends on Wintun's session handle actually being a
duplicable Win32 `HANDLE` in the first place, which needs verification
against Wintun's own API before committing to this design. If it isn't,
the alternative is restructuring so the worker opens the Wintun session
itself (Wintun adapter _creation_ needs admin, but _opening an existing_
adapter by name may not) — a materially different split than today's
root-creates/worker-receives model.

Separately, `gnosis_vpn-lib/src/wg_tunnel/tun.rs`'s async TUN reader/writer
wraps the fd via `tokio::io::unix::AsyncFd` — a Unix-only tokio API. Whatever
the handle-transfer mechanism turns out to be, this file needs its own
Windows arm built on Wintun's overlapped I/O (`WintunReceivePacket`/
`WintunSendPacket`), not a portable abstraction over the existing Unix code.

## The other IPC problem: root→worker privilege drop

Today, `gnosis_vpn-root/src/main.rs::setup_worker()` spawns the worker with
`.uid(self.worker_user.uid).gid(self.worker_user.gid)` — Unix-only inherent
methods on `tokio::process::Command` that drop privileges to an
unprivileged `gnosisvpn` system user (resolved via the `uzers` crate).
Windows has no uid/gid model; the equivalent is
`CreateProcessAsUser`/`CreateProcessWithTokenW` with a token for a
dedicated low-privilege service account — a materially different API, not
a parameter swap.

## `ping.rs` needs real platform work, not just a new arm

Already handles Linux vs. macOS CLI flags and `ping`-crate socket types via
`cfg_if!`, but every piece needs a Windows branch:

- The `which ping` availability probe doesn't exist on Windows (`where`, or
  drop the probe and always try `ping.exe`).
- `ping.exe`'s own flags differ (`-n` count, `-w` timeout in milliseconds)
  from both branched-on argument sets today.
- `parse_duration()` greps for `"rtt"`/`"round-trip"` — Windows `ping.exe`
  prints `Average = Xms`, so the parser needs a third pattern, not just new
  flags.
- The `ping` crate fallback does support Windows via WinSock per its own
  docs, but no `cfg_if!` arm selects it yet.

## Service installation

No `.service` or `.plist` file exists anywhere in this repo — on
Linux/macOS, daemon supervision is handled by packaging outside this
codebase. A Windows Service wrapper (via the `windows-service` crate,
registered through the Service Control Manager) and an installer (WiX/Inno
Setup, to place binaries, register the service, and install the Wintun
driver) need to be built from scratch — the same amount of new work either
platform would require, so not a Windows-specific deficit.

## Build

Add the `x86_64-pc-windows-msvc` Rust target (and `aarch64-pc-windows-msvc`
if ARM64 Windows matters), and verify `edgli` and `neptun`'s dependency
trees cross-compile clean — `rustls`, not a native TLS backend. The
`mnl`/`nftnl`/`pfctl`/`objc2`/`objc2-foundation` platform dependencies are
simply dropped for this target; no `cfg(windows)` dependency section exists
in any crate's `Cargo.toml` yet.

## Bottom line

No single gatekeeper API forces an architecture change, and the existing
`Routing`/`RouteOps`/`killswitch::Firewall` seams are exactly where a
Windows backend needs to attach — new modules, not restructuring. WireGuard
itself is already embedded and in-process, so there's no `wg-quick`-shaped
tool to replace. The open work is: a Windows Service + installer (nothing
in-repo to adapt), the root→worker privilege-drop redesign
(`CreateProcessAsUser`/token-based, not uid/gid), and — the one genuinely
uncertain problem — how a Wintun-backed TUN handle crosses from a
Windows-Service-owned root to an unprivileged worker process, which needs
verification against Wintun's handle model before it can be called
feasible rather than just plausible.
