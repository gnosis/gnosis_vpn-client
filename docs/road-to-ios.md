# Road to iOS

## Current architecture (recap)

Three privilege-separated binaries:

- **`gnosis_vpn-root`** (privileged) — creates the TUN device
  (`neptun::device::TunSocket`, a cross-platform ioctl helper; no WireGuard
  crypto involved), sets up bypass/split routes, DNS, IPv6 blackhole, and
  the kill switch (`nftables` on Linux/`pfctl` on macOS), persists
  crash-recovery state, and spawns the worker. Root does no WireGuard
  protocol work at all.
- **`gnosis_vpn-worker`** (unprivileged) — the entry point into the mixnet
  (`edgli`/hoprd session client) _and_ the entire WireGuard data plane: it
  receives the TUN fd from root over a dedicated Unix socket via
  `SCM_RIGHTS`, builds a `WgTunnel` (`gnosis_vpn-lib/src/wg_tunnel/`,
  wrapping `neptun::noise::Tunn` — NordSecurity's WireGuard-rs fork), and
  runs the async pump moving packets between the TUN device and the mixnet
  session. WireGuard keys and all crypto live only here.
- **`gnosis_vpn-ctl`**/UI — talks to root over a named Unix socket, no
  visibility into the TUN fd or WireGuard state.

Both `edgli` and the entire `wg_tunnel` module are pure Rust with no
platform calls — strong reuse candidates.

## The sanctioned API: `NetworkExtension` / `NEPacketTunnelProvider`

No root, no CLI tools, no kernel WireGuard access — but a different
sanctioned API and process model than Android's.

- [`NEPacketTunnelProvider`](https://developer.apple.com/documentation/networkextension/nepackettunnelprovider) — the class you subclass to implement the tunnel
- [`NEPacketTunnelNetworkSettings`](https://developer.apple.com/documentation/networkextension/nepackettunnelnetworksettings) — replaces the routing/DNS setup root does today
- [`NETunnelProviderManager`](https://developer.apple.com/documentation/networkextension/netunnelprovidermanager) — app-side config/lifecycle management
- [TN3120: Expected use cases for packet tunnel providers](https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers) — Apple's guidance on what this API is (and isn't) meant for

## What replaces what

| Current (desktop)                                                                                                         | iOS replacement                                                                                                                                                                                                                                                                   |
| ------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Worker's `WgTunnel`/pump (`gnosis_vpn-lib/src/wg_tunnel/`, wraps `neptun::noise::Tunn`)                                   | Runs inside the extension, driven by `packetFlow.readPackets`/`writePackets` instead of a raw fd + `SCM_RIGHTS`                                                                                                                                                                   |
| Root's TUN creation + fd hand-off (`gnosis_vpn-root/src/routing/tun.rs`, `gnosis_vpn-lib/src/socket/fd_passing.rs`)       | Not needed — the extension gets its tunnel interface implicitly, no manual TUN device creation                                                                                                                                                                                    |
| Netlink/route setup (`route_ops_linux.rs`'s `NetlinkRouteOps`, behind the `RouteOps` trait) + DNS push (`routing/dns.rs`) | `NEPacketTunnelNetworkSettings` + `NEIPv4Settings`/`NEDNSSettings`, applied via `setTunnelNetworkSettings()`                                                                                                                                                                      |
| `nftables`/`pfctl` kill switch (`gnosis_vpn-lib/src/killswitch/`)                                                         | Not a separate component — once tunnel network settings are applied, the OS owns the default route through the tunnel interface; if the extension dies, the route is torn down (fail-closed by default)                                                                           |
| Crash-recovery sweep (`gnosis_vpn-root/src/routing/sweep.rs`), IPv6 blackhole (`routing/ipv6_blackhole.rs`)               | Not needed — no long-lived daemon to leave state behind, and network settings are all-or-nothing per session                                                                                                                                                                      |
| Root/worker IPC                                                                                                           | Closest analog to what already exists: the extension is a genuinely separate **OS process** from the main app, communicating via `NETunnelProviderSession.sendProviderMessage` or shared state through an **App Group** container — not in-process calls like Android's JNI model |
| Separate UI app talking over named socket                                                                                 | Main app target bound to the extension through system VPN APIs                                                                                                                                                                                                                    |
| `app_nap.rs` (macOS App Nap opt-out) / Android Doze exemption                                                             | No equivalent needed or available — the OS manages the extension process lifecycle automatically while the tunnel is active, with no notification requirement and no battery-optimization toggle to request                                                                       |
| `device_monitor/linux/rtnetlink.rs` (interface/route watching)                                                            | `NEProvider.reasserting` for handling underlying network changes (Wi-Fi ↔ cellular) without full teardown                                                                                                                                                                         |
| Android manifest (`BIND_VPN_SERVICE`, `specialUse` foreground type)                                                       | Info.plist entitlements — see below; gated by Apple approval rather than a Play Console self-declaration                                                                                                                                                                          |

## The dominant constraint: extension memory ceiling

Packet tunnel provider extensions run under a **hard memory limit** — around
50 MiB on iOS 15/16. Apple has moved this limit before and explicitly warns
against hardcoding assumptions about it, but it has always been small
(historically as low as ~15 MiB on 64-bit devices). This is a single OS
process, killed by jetsam the instant it exceeds budget — there is no
degraded mode, just termination.

Everything has to fit inside that budget simultaneously: `edgli`'s HOPR node
(crypto, multi-hop session framing, HTTP client for chain/Blokli calls), the
`wg_tunnel` pump plus `neptun`'s WireGuard state, and the tokio runtime's own
overhead. This has no equivalent on desktop or Android and needs real
profiling before anything else here matters — a multi-threaded tokio
runtime may need to become single-threaded (`current_thread`) just to cut
per-thread stack overhead.

## Starting and keeping the edge client running

- **Entry point**: `startTunnel(options:completionHandler:)` on the
  `NEPacketTunnelProvider` subclass — configure network settings, call
  `setTunnelNetworkSettings()`, then invoke the completion handler. There's
  no `main()`/stdin control-socket handshake like today's
  `gnosis_vpn-worker/src/main.rs` — the FFI entry point (`uniffi-rs` or
  manual `cbindgen`) is called directly by the extension's own Swift code.
- **Runtime lifecycle**: the tokio runtime is built once as a static/
  singleton behind the FFI boundary, with `Core::start()` and the
  `wg_tunnel` pump spawned onto it; the FFI call itself returns immediately.
- **No foreground-service equivalent needed**: the OS keeps the extension
  process alive automatically while the tunnel is active and shows the
  system VPN status indicator itself. The tradeoff: no control over this
  beyond staying under the memory ceiling and not misbehaving.
- **Socket exclusion is automatic**: unlike Android's `VpnService.protect()`,
  sockets opened from within the extension process (the mixnet session's
  transport) are excluded from the tunnel's own routes by the system — no
  manual protect step needed.
- **Process death**: no `START_STICKY`-style automatic restart. Recovery
  depends on `NEOnDemandRule` configuration and/or the user or main app
  reconnecting. `NEProvider.reasserting` covers brief connectivity blips
  without a full teardown — closer in spirit to `route_health.rs`'s
  reconnect path than a cold restart.

## Entitlement gate

`com.apple.developer.networking.networkextension` (packet-tunnel-provider)
is a restricted entitlement — Apple must approve the app for it before a
build can even be provisioned or distributed. This is a business/timeline
dependency with no Android equivalent (Google Play's `specialUse`
foreground-service declaration is a self-service Play Console field
reviewed after the fact, not a pre-approval gate).

## Build

Rust targets `aarch64-apple-ios` (device) and
`aarch64-apple-ios-sim`/`x86_64-apple-ios-sim` (simulator), packaged as an
XCFramework and linked into the extension target — the Apple-toolchain
equivalent of Android's `cargo-ndk`.

## Bottom line

Architecturally iOS is a smaller conceptual leap in one respect — the
app/extension process split mirrors the existing root/worker separation more
closely than Android's single-process model does, and the extension gets
its tunnel interface without a TUN-creation/fd-handoff step at all. But the
~50 MiB memory ceiling on the extension is a hard constraint the desktop
worker was never designed against, and the NetworkExtension entitlement
approval is a gate outside our control. A memory profile of `edgli` +
`wg_tunnel`/`neptun` running standalone is needed before treating this as
feasible at all — that result determines whether this is "a rewrite" or
"not possible without upstream changes to `edgli`."
