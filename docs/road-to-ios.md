# Road to iOS

Findings from an initial feasibility pass on porting the client to iOS.
Not a plan — a map of what breaks and what the replacement looks like.
Companion to [road-to-android.md](./road-to-android.md); read that first for
the shared background (`edgli` is a pure Rust library, current three-binary
privilege-separated architecture, `wg-quick`/netlink/`nftables`-based
networking).

## The sanctioned API: `NetworkExtension` / `NEPacketTunnelProvider`

Same underlying problem as Android — no root, no CLI tools, no kernel
WireGuard access — but a different sanctioned API and a different process
model.

- [`NEPacketTunnelProvider`](https://developer.apple.com/documentation/networkextension/nepackettunnelprovider) — the class you subclass to implement the tunnel
- [`NEPacketTunnelNetworkSettings`](https://developer.apple.com/documentation/networkextension/nepackettunnelnetworksettings) — replaces `wg-quick`/netlink routing
- [`NETunnelProviderManager`](https://developer.apple.com/documentation/networkextension/netunnelprovidermanager) — app-side config/lifecycle management
- [TN3120: Expected use cases for packet tunnel providers](https://developer.apple.com/documentation/technotes/tn3120-expected-use-cases-for-network-extension-packet-tunnel-providers) — Apple's guidance on what this API is (and isn't) meant for

## What replaces what

| Current (desktop) | iOS replacement |
|---|---|
| `wg-quick up/down` (`gnosis_vpn-root/src/wg_tooling.rs`) | Embedded userspace WireGuard (`boringtun`) driven by `packetFlow.readPackets`/`writePackets` |
| Netlink route manipulation (`gnosis_vpn-root/src/routing/route_ops_linux.rs`'s `NetlinkRouteOps`, behind the `RouteOps` trait) | `NEPacketTunnelNetworkSettings` + `NEIPv4Settings`/`NEDNSSettings`, applied via `setTunnelNetworkSettings()` |
| `nftables`/`pfctl` kill switch (`gnosis_vpn-lib/src/killswitch/`) | Not a separate component — once tunnel network settings are applied, the OS owns the default route through the tunnel interface; if the extension dies, the route is torn down (fail-closed by default) |
| root/worker Unix-socket IPC | Closest analog to what already exists: the extension is a genuinely separate **OS process** from the main app, communicating via `NETunnelProviderSession.sendProviderMessage` or shared state through an **App Group** container — not in-process calls like Android's JNI model |
| Separate UI app talking over named socket | Main app target bound to the extension through system VPN APIs |
| `app_nap.rs` (macOS App Nap opt-out) / Android Doze exemption | No equivalent needed or available — the OS manages the extension process lifecycle automatically while the tunnel is active, with no notification requirement and no battery-optimization toggle to request |
| `device_monitor/linux/rtnetlink.rs` (interface/route watching) | `NEProvider.reasserting` for handling underlying network changes (Wi-Fi ↔ cellular) without full teardown |
| Android manifest (`BIND_VPN_SERVICE`, `specialUse` foreground type) | Info.plist entitlements — see below; gated by Apple approval rather than a Play Console self-declaration |

## The dominant constraint: extension memory ceiling

Packet tunnel provider extensions run under a **hard memory limit** — around
50 MiB on iOS 15/16. Apple has moved this limit before and explicitly warns
against hardcoding assumptions about it, but it has always been small
(historically as low as ~15 MiB on 64-bit devices). This is a single OS
process, killed by jetsam the instant it exceeds budget — there is no
degraded mode, just termination.

Everything has to fit inside that budget simultaneously: `edgli`'s HOPR node
(crypto, multi-hop session framing, HTTP client for chain/Blokli calls),
`boringtun`, and the tokio runtime's own overhead. This has no equivalent on
desktop or Android and needs real profiling before anything else here
matters — a multi-threaded tokio runtime (what `hopr_lib::prepare_tokio_runtime`
sets up today) may need to become single-threaded (`current_thread`) just to
cut per-thread stack overhead.

## Starting and keeping the edge client running

- **Entry point**: `startTunnel(options:completionHandler:)` on the
  `NEPacketTunnelProvider` subclass — the iOS analog of `VpnService.onStartCommand()`
  / today's `main()` in `gnosis_vpn-worker`. Configure network settings, call
  `setTunnelNetworkSettings()`, then invoke the completion handler.
- **Runtime lifecycle**: same shape as Android — no `main()`, so the tokio
  runtime is built once as a static/singleton behind a Rust↔Swift FFI
  boundary (`uniffi-rs` or manual `cbindgen`), with `Core::start()` spawned
  onto it; the FFI call itself returns immediately.
- **No foreground-service equivalent needed**: the OS keeps the extension
  process alive automatically while the tunnel is active and shows the
  system VPN status indicator itself. The tradeoff: no control over this
  beyond staying under the memory ceiling and not misbehaving.
- **Socket exclusion is automatic**: unlike Android's `VpnService.protect()`,
  sockets opened from within the extension process (the WireGuard UDP
  socket, the mixnet session's transport) are excluded from the tunnel's own
  routes by the system — no manual protect step needed.
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
closely than Android's single-process model does. But the ~50 MiB memory
ceiling on the extension is a hard constraint the desktop worker was never
designed against, and the NetworkExtension entitlement approval is a gate
outside our control. A memory profile of `edgli` + `boringtun` running
standalone is needed before treating this as feasible at all — that result
determines whether this is "a rewrite" or "not possible without upstream
changes to `edgli`."
