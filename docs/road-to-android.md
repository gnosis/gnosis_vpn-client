# Road to Android

Findings from a feasibility pass on porting the client to Android. Not a
plan — a map of what breaks and what the replacement looks like. Companion
to [road-to-ios.md](./road-to-ios.md) and
[road-to-windows.md](./road-to-windows.md).

## Current architecture (recap)

Three privilege-separated binaries:

- **`gnosis_vpn-root`** (privileged) — creates the TUN device
  (`neptun::device::TunSocket`, a cross-platform ioctl helper; no WireGuard
  crypto involved), sets up bypass/split routes, DNS, IPv6 blackhole, and
  the kill switch (`nftables` on Linux/`pfctl` on macOS), persists
  crash-recovery state, and spawns the worker. Root does no WireGuard
  protocol work at all.
- **`gnosis_vpn-worker`** (unprivileged) — the entry point into the mixnet
  (`edgli`/hoprd session client) *and* the entire WireGuard data plane: it
  receives the TUN fd from root over a dedicated Unix socket via
  `SCM_RIGHTS`, builds a `WgTunnel` (`gnosis_vpn-lib/src/wg_tunnel/`,
  wrapping `neptun::noise::Tunn` — NordSecurity's WireGuard-rs fork), and
  runs the async pump moving packets between the TUN device and the mixnet
  session. WireGuard keys and all crypto live only here.
- **`gnosis_vpn-ctl`**/UI — talks to root over a named Unix socket, no
  visibility into the TUN fd or WireGuard state.

Both `edgli` and the entire `wg_tunnel` module are pure Rust with no
platform calls — the module's own doc comment calls it "pure, unprivileged
worker-side code." Both are strong reuse candidates for any platform.

## Why the current design doesn't map to Android

Android apps are single-sandboxed-process. There is no root daemon, no
separate unprivileged system user, and no raw routing tables or `nftables`
available to an app. The only sanctioned path for a custom VPN is
[`VpnService`](https://developer.android.com/reference/android/net/VpnService)
([guide](https://developer.android.com/develop/connectivity/vpn),
[`Builder` reference](https://developer.android.com/reference/android/net/VpnService.Builder),
[AOSP sample](https://android.googlesource.com/platform/development/+/master/samples/ToyVpn)):
the OS grants one-time user consent, then hands the app a TUN file
descriptor. The app owns everything downstream of that fd — the same shape
as today's root→worker hand-off, just a different boundary.

## What replaces what

| Current (desktop) | Android replacement |
|---|---|
| Worker's `WgTunnel`/pump (`gnosis_vpn-lib/src/wg_tunnel/`, wraps `neptun::noise::Tunn`) | Reusable close to as-is — just needs the fd from `Builder.establish()` instead of root's `SCM_RIGHTS` hand-off |
| Root's TUN creation + fd hand-off (`gnosis_vpn-root/src/routing/tun.rs`, `gnosis_vpn-lib/src/socket/fd_passing.rs`) | `VpnService.Builder.establish()` |
| Netlink route setup (`route_ops_linux.rs`'s `NetlinkRouteOps`, behind the `RouteOps` trait, orchestrated from `routing/linux.rs`) | `VpnService.Builder.addRoute()/addAddress()` |
| DNS push (`gnosis_vpn-root/src/routing/dns.rs`) | `Builder.addDnsServer()` |
| `nftables` kill switch (`gnosis_vpn-lib/src/killswitch/linux.rs`) | Android's built-in "Always-on VPN + Block connections without VPN" |
| Root/worker IPC (JSON control channel + `SCM_RIGHTS` fd channel) | Collapses into one process — direct JNI calls/callbacks, no privilege boundary to bridge |
| Separate UI app talking over named socket | Native Kotlin/Compose UI bound to the same in-process `Service`, via JNI (likely `uniffi-rs` for the binding layer) |
| `app_nap.rs` (macOS App Nap opt-out) | Foreground service + `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` (Doze exemption) |
| `device_monitor/linux/rtnetlink.rs` (interface/route watching) | `ConnectivityManager.NetworkCallback` feeding the existing `route_health.rs` reconnect/backoff logic |
| Crash-recovery sweep (`gnosis_vpn-root/src/routing/sweep.rs`), IPv6 blackhole (`routing/ipv6_blackhole.rs`) | Not needed — no long-lived daemon to leave state behind, and `Builder` only routes what's explicitly added |

## Manifest requirements

`VpnService.prepare()`/`Builder`/`protect()` handle the runtime consent and
tunnel-establishment plumbing, but the manifest still needs explicit entries
— this is what actually plugs the app into the system's VPN mechanism, not
just boilerplate:

```xml
<uses-permission android:name="android.permission.INTERNET"/>
<uses-permission android:name="android.permission.FOREGROUND_SERVICE"/>
<uses-permission android:name="android.permission.POST_NOTIFICATIONS"/> <!-- Android 13+, for the persistent notification -->

<service
    android:name=".GnosisVpnService"
    android:permission="android.permission.BIND_VPN_SERVICE"
    android:exported="false"
    android:foregroundServiceType="specialUse">
    <intent-filter>
        <action android:name="android.net.VpnService"/>
    </intent-filter>
    <property
        android:name="android.app.PROPERTY_SPECIAL_USE_FGS_SUBTYPE"
        android:value="vpn"/>
</service>
```

- `android:permission="android.permission.BIND_VPN_SERVICE"` on the
  `<service>` tag, plus the `android.net.VpnService` intent-filter action, is
  what tells the OS to trust this service as a VPN provider — without it
  nothing can bind to it as one.
- `foregroundServiceType="specialUse"` + the
  `PROPERTY_SPECIAL_USE_FGS_SUBTYPE` property is an Android 14+ (API 34)
  requirement — there's no dedicated `vpn` foreground-service type, so VPN
  apps use `specialUse` and must justify it here. Google Play also requires
  declaring this subtype in the Play Console's app content page at
  submission, and reviews it — a light gate, though nowhere near iOS's
  NetworkExtension entitlement approval (see `road-to-ios.md`).
- `FOREGROUND_SERVICE` + `POST_NOTIFICATIONS` are what make `startForeground()`
  legal on a modern targetSdk — the persistent notification is the mechanism
  itself, not optional decoration.

## Excluding the engine's own traffic from the tunnel

Replaces `routing_actor.rs`'s dynamic peer-IP bypass-route bookkeeping
entirely — that machinery exists on Linux/macOS because root can't reach
into the unprivileged worker's socket calls, so it punches holes in the
routing table from outside instead. That constraint disappears once there's
one process.

- `VpnService.protect(fd)` needs a raw fd, but `edgli`'s mixnet connections
  go through `libp2p` (both TCP and QUIC/`quinn` transports are in the
  dependency tree), which creates sockets internally — not worth fighting
  the library for.
- Use `ConnectivityManager.bindProcessToNetwork()` instead: bind once, to
  the underlying Wi-Fi/cellular network, before the engine opens any
  connections. Every socket the process creates afterward routes through
  that network by default, regardless of what library opened it. Coarse
  (process-wide, not per-socket) but correct here.
- Run the engine in its own process (`android:process=":vpn_engine"`) so the
  bind doesn't also redirect the UI's own networking.
- `ping.rs`'s primary path (`Command::new("ping")`, a shell-out) has no
  Android equivalent, but it already falls back to an in-process ICMP
  socket via the `ping` crate when the system binary is unavailable,
  `cfg_if!`-branching the socket type per OS. That `cfg_if!` only has
  `target_os = "linux"`/`"macos"` arms today — Android is
  `target_os = "android"` in Rust, distinct from `"linux"` despite the
  shared kernel — so it needs its own arm (almost certainly `ping::RAW`,
  same as Linux). Also covered by the same process bind.

## Starting and keeping the edge client running

`edgli`'s lifecycle logic (`Core::init`/`Core::start`) and the `wg_tunnel`
pump are both OS-agnostic and reusable as-is. What changes is the entry
point and process lifecycle around them:

- **Entry point**: today `gnosis_vpn-worker/src/main.rs` claims a control
  socket on stdin, then a second `SCM_RIGHTS` socket carrying the TUN fd,
  before starting `Core`. On Android there's no `main()` — the Rust code is
  a `cdylib` loaded via `System.loadLibrary`, invoked via JNI from
  `VpnService.onStartCommand()` (not the Activity — tying the client to the
  UI's lifecycle kills the session on backgrounding), which supplies the
  `Builder.establish()` fd directly instead of the two stdin sockets.
- **Runtime lifecycle**: build the tokio runtime once, lazily, as a
  Rust-side static/singleton; the JNI `start()` call does
  `runtime.spawn(core.start())` and returns immediately — `block_on` on a
  JNI-called thread risks an ANR.
- **Foreground service is load-bearing**: `startForeground()` with a
  persistent notification is what stops Android from reclaiming the process
  when the screen turns off.
- **Battery optimization exemption**: even foregrounded, Doze can starve
  keep-alive/ping traffic, silently stalling the tunnel. Request
  `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`. Some OEMs (Xiaomi, Huawei,
  OnePlus) override stock Doze behavior and kill background VPN apps
  regardless — a known-issues item.
- **Process death means full re-init**: `START_STICKY` restarts the
  service, but the runtime, `Core`, `WgTunnel`, and mixnet session are gone
  — no cheap resume, re-run `Core::init()` from scratch.
- **Network handoff**: feed `ConnectivityManager.NetworkCallback`
  transitions into the existing `route_health.rs` reconnect/backoff path.

## Build

Add `aarch64-linux-android`/`x86_64-linux-android` Rust targets
(`cargo-ndk`), and verify `edgli`, `neptun`, and their dependency trees
cross-compile clean — TLS backend especially (want `rustls`, not native
OpenSSL). The `nftnl`/`mnl`/`pfctl` platform code and root's entire
`routing`/`killswitch` module tree are simply dropped for this target.

## Bottom line

The worker's `wg_tunnel` pump only needs a raw TUN fd and is otherwise
platform-agnostic, so it carries over close to as-is once wired to
`Builder.establish()`. What's left is what mobile always requires
regardless of the desktop client's internals: the three-binary IPC
architecture collapses into one Kotlin app + Rust core linked via JNI,
root's entire routing/DNS/killswitch responsibility moves to
`VpnService.Builder`, and the manifest/foreground-service/battery-exemption
plumbing has to be built. A real rewrite of the privileged half of the
client — not a cross-compile.
