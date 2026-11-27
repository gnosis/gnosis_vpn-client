# OpenWrt packaging

The `gnosis_vpn` binaries are already static musl (`nix/gnosisvpn.nix` builds
`pkgsCross.*.pkgsStatic`), so OpenWrt needs no Rust toolchain and no rebuild: the
package repacks the binaries from the released `arm64` `.deb` and ships them with
procd and namespace glue.

## Build

In an OpenWrt SDK matching the router target (25.12.x, `mediatek/filogic` for
the Cudy TR3000 v1):

```sh
make defconfig
cp -r packaging/openwrt package/gnosisvpn
make package/gnosisvpn/compile V=s
```

Result: `bin/packages/<arch>/base/gnosisvpn-<version>-r<N>.apk`.

The build host needs `ar` (binutils) to unpack the `.deb`, and `xz` comes from
the SDK's host tools. `PKG_VERSION` and `PKG_HASH` must be bumped together on
each release; the hash is the one published next to the `.deb` as `.sha256`.

## Layout

| path | role |
| --- | --- |
| `Makefile` | package recipe: unpack the release `.deb`, xz the payload |
| `files/gnosisvpn.init` | procd service, expands the payload into tmpfs |
| `files/gnosis-ns.sh` | network namespace, LAN steering, connect/probe |
| `files/gnosis-ns-watch.sh` | watchdog: restore LAN first, then recover |
| `files/gnosis-ns-run.sh` | starts `gnosis_vpn-root` inside the namespace |
| `files/gnosis-ns-client.sh` | holds the keepalive session as `gnosisvpn` |
| `files/gnosis-ctl` | wrapper: run `gnosis_vpn-ctl` as the worker account |
| `files/config.toml` | client config (v7) |
| `files/gnosisvpn.config` | UCI defaults: blokli URL, log level, keepalive |

## Why a namespace

`FORCE_STATIC_ROUTING=true` makes root install `0.0.0.0/1` + `128.0.0.0/1` on
`wg0_gnosisvpn` and an nft killswitch that drops anything not leaving through the
TUN. That is correct for a desktop, and fatal on a router: the box's own uplink
goes away, so tailscaled loses its DERP and control-plane sockets and the remote
session dies while the LAN still looks fine.

So root runs inside a netns (`gvns`) on a veth pair, and only `iif br-lan` is
steered into it through table 100. The router's own routing table is never
touched, which is what makes "Tailscale stays up while the LAN exits through the
mixnet" possible.

Two consequences worth knowing when reviewing this against the client:

- steering is opt-in and proven before it is applied (two probes 20 s apart), so
  a flapping circuit never takes the LAN with it;
- the worker still dies under HOPR swarm churn, and root will not respawn a
  worker it saw exit, so the watchdog restarts the service rather than waiting
  for a recovery the daemon will not perform.

## Verified

Cudy TR3000 v1, OpenWrt 25.12.6, client 1.2.0: `Ready (Node is running) -
traffic: Good, gas: Good`, LAN egress `80.241.197.94`, router egress unchanged
at the ISP address, `tailscale status` up throughout, 1-3 Mbit/s through the
mixnet.