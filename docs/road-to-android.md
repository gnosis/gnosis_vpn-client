# Road to Android

Findings from an initial feasibility pass on porting the client to Android.
Not a plan — a map of what breaks and what the replacement looks like.

## Current architecture (recap)

Three privilege-separated binaries: `gnosis_vpn-root` (root daemon — routing,
kill switch, spawns worker), `gnosis_vpn-worker` (unprivileged — runs the
mixnet client via the embedded `edgli` Rust crate), `gnosis_vpn-ctl`/UI (talk
to root over a named Unix socket). WireGuard is set up by shelling out to
`wg-quick`, which drives the kernel WireGuard module. Routing is direct
netlink table manipulation; kill switch is `nftables` (Linux) / `pfctl`
(macOS).

`edgli` is already a pure Rust library, not a subprocess — this is the most
portable piece of the stack.

## Why the current design doesn't map to Android

Android apps are single-sandboxed-process. There is no root daemon, no
separate unprivileged system user, and no `wg`/`wg-quick`, kernel WireGuard
access, raw routing tables, or `nftables` available to an app. The only
sanctioned path for a custom VPN is
[`VpnService`](https://developer.android.com/reference/android/net/VpnService)
([guide](https://developer.android.com/develop/connectivity/vpn),
[`Builder` reference](https://developer.android.com/reference/android/net/VpnService.Builder),
[AOSP sample](https://android.googlesource.com/platform/development/+/master/samples/ToyVpn)):
the OS grants one-time user consent, then hands the app a TUN file
descriptor. The app owns everything downstream of that fd.

## What replaces what

| Current (desktop) | Android replacement |
|---|---|
| `wg-quick up/down` (`gnosis_vpn-root/src/wg_tooling.rs`) | Embedded userspace WireGuard (e.g. `boringtun`, Rust — fits the existing stack) driven by the fd from `Builder.establish()` |
| Netlink route manipulation (`gnosis_vpn-root/src/routing/route_ops_linux.rs`'s `NetlinkRouteOps`, behind the `RouteOps` trait) | `VpnService.Builder.addRoute()/addAddress()/addDnsServer()` |
| `nftables` kill switch (`gnosis_vpn-lib/src/killswitch/linux.rs`) | Android's built-in "Always-on VPN + Block connections without VPN" |
| root/worker Unix-socket IPC | Collapses into one process — direct JNI calls / callbacks, no privilege boundary to bridge |
| Separate UI app talking over named socket | Native Kotlin/Compose UI bound to the same in-process `Service`, via JNI (likely `uniffi-rs` for the binding layer) |
| `app_nap.rs` (macOS App Nap opt-out) | Foreground service + `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` (Doze exemption) |
| `device_monitor/linux/rtnetlink.rs` (interface/route watching) | `ConnectivityManager.NetworkCallback` feeding the existing `route_health.rs` reconnect/backoff logic |

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
(`add_peer_bypass_route`/`remove_peer_bypass_route`, `active_bypass` diffing)
entirely — that machinery exists on Linux because root can't reach into the
unprivileged worker's socket calls, so it punches holes in the routing table
from outside instead. That constraint disappears once there's one process.

- `VpnService.protect(fd)` needs a raw fd, but `edgli`'s mixnet connections
  go through `libp2p` (both TCP and QUIC/`quinn` transports are in the
  dependency tree), which creates sockets internally — not worth fighting
  the library for.
- Use `ConnectivityManager.bindProcessToNetwork()` instead: bind once, to
  the underlying Wi-Fi/cellular network, before the engine opens any
  connections. Every socket the process creates afterward routes through
  that network by default, regardless of what library opened it. Coarse
  (process-wide, not per-socket) but correct here — nothing this engine
  opens is supposed to cross the tunnel it's building.
- Run the engine in its own process (`android:process=":vpn_engine"`) so the
  bind doesn't also redirect the UI's own networking — same isolation
  instinct as today's root/worker split, different reason.
- `protect()` still applies to any socket the code opens directly (e.g. a
  hand-rolled `boringtun` UDP transport), but the process-wide bind already
  covers it — moot either way.
- Side effect: `ping.rs`'s primary path (`Command::new("ping")`, a shell-out)
  has no Android equivalent (no exec, no `ping` binary) — but it's not a
  from-scratch problem: `ping.rs` already falls back to an in-process ICMP
  socket via the `ping` crate (`ping_using_ping_crate()`) when the system
  binary is unavailable, `cfg_if!`-branching the socket type per OS. That
  `cfg_if!` only has `target_os = "linux"`/`"macos"` arms today — Android is
  `target_os = "android"` in Rust, a distinct value from `"linux"` despite
  running a Linux kernel, so it falls through neither arm as written and
  needs its own arm added (almost certainly `ping::RAW`, same as Linux) —
  a small addition, not new plumbing. Also covered by the same process bind.

## Starting and keeping the edge client running

`edgli`'s lifecycle logic (`Core::init` / `Core::start` in
`gnosis_vpn-lib/src/core/`) is OS-agnostic and reusable as-is. What changes is
everything around it:

- **Entry point**: today `gnosis_vpn-worker/src/main.rs` calls
  `hopr_lib::prepare_tokio_runtime(...)` then `rt.block_on(...)`. On Android
  there's no `main()` — the Rust code is a `cdylib` loaded via
  `System.loadLibrary`, invoked via JNI from `VpnService.onStartCommand()`
  (not from the Activity — tying the client to the UI's lifecycle kills the
  session on backgrounding).
- **Runtime lifecycle**: build the tokio runtime once, lazily, held in a
  Rust-side static/singleton. The JNI `start()` call does
  `runtime.spawn(core.start())` and returns immediately — `block_on` on a
  JNI-called thread risks an ANR.
- **Foreground service is load-bearing**: `startForeground()` with a
  persistent notification is what stops Android from reclaiming the process
  when the screen turns off. Without it nothing else here matters.
- **Battery optimization exemption**: even foregrounded, Doze can starve
  keep-alive/ping traffic (`ping.rs`, `route_health.rs`), silently stalling
  the tunnel. Request `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`. Some OEMs
  (Xiaomi, Huawei, OnePlus) override stock Doze behavior and kill background
  VPN apps regardless — a known-issues item for users on those devices.
- **Process death means full re-init**: `START_STICKY` gets the service
  restarted, but the runtime, `Core`, and mixnet session are gone. Restart
  means re-running `Core::init()` from scratch — no cheap resume.
- **Network handoff**: feed `ConnectivityManager.NetworkCallback` transitions
  into the existing `route_health.rs` reconnect/backoff path instead of
  netlink-based interface watching.

## Build

Add `aarch64-linux-android` / `x86_64-linux-android` Rust targets
(`cargo-ndk`), and verify `edgli` and its dependency tree actually
cross-compile clean — TLS backend especially (want `rustls`, not native
OpenSSL). The `nftnl`/`mnl`/`pfctl` platform code is simply dropped for this
target.

## Bottom line

The mixnet client (`edgli`) is largely reusable once cross-compiled.
Everything in `gnosis_vpn-root` (routing, kill switch, `wg-quick`
orchestration) needs to be rewritten against `VpnService` + an embedded
WireGuard implementation, and the three-binary IPC architecture collapses
into one Kotlin app + Rust core linked via JNI. This is a substantial rewrite
of the privileged/networking half of the client, not a cross-compile.
