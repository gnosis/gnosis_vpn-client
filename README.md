# Gnosis VPN for OpenWrt

Installable package for the [Gnosis VPN](https://vpn.gnosis.io) client on
OpenWrt. It routes a network's traffic through the HOPR mixnet while the router
itself keeps its own uplink, so Tailscale, SSH and the LuCI page stay reachable
while the LAN exits through a Gnosis exit node.

Upstream builds its Linux binaries with nix against static musl
(`pkgsCross.*.pkgsStatic`), and the `arm64` `.deb` on the release page contains
exactly those binaries, statically linked. OpenWrt has no glibc and no use for a
distro package manager, so this package takes the official signed `.deb`,
verifies it by hash, and repacks the three binaries for `apk`.

## Status

Verified on a Cudy TR3000 v1 (MediaTek MT7981, OpenWrt 25.12.6, aarch64) with
client **1.2.0**:

| check | result |
| --- | --- |
| `gnosis-ctl status` | `Ready (Node is running) - traffic: Good, gas: Good` |
| namespace egress | `80.241.197.94` (`gvpn-ger-1-j9os`) |
| router egress | `122.170.192.224` (ISP), unchanged |
| `tailscale status` | `100.81.224.10` up the whole time |
| overlay used | ~35 MB of 44 MB, payload expanded into tmpfs |

Throughput is mixnet throughput: expect roughly 1-3 Mbit/s, not line rate.

## Install

aarch64 routers running OpenWrt 25.x or later (apk-based). The overlay needs
about 20 MB free.

```sh
wget -O /etc/apk/keys/gnosis-openwrt.pem \
  https://ivcained.github.io/gnosis-openwrt/gnosis-openwrt.pem
echo "https://ivcained.github.io/gnosis-openwrt/25.12/aarch64_cortex-a53/packages.adb" \
  >> /etc/apk/repositories.d/customfeeds.list
apk update && apk add gnosisvpn
```

Or install the single file without trusting a feed:

```sh
wget -O /tmp/gnosisvpn.apk \
  https://ivcained.github.io/gnosis-openwrt/25.12/aarch64_cortex-a53/gnosisvpn-1.2.0-r5.apk
apk add --allow-untrusted /tmp/gnosisvpn.apk
```

Either way the package pulls in `kmod-tun`, `nftables`, `ip-full`, `xz`,
`ca-bundle`, `shadow-su` and `zram-swap`, creates the unprivileged `gnosisvpn`
account (uid 65533: the worker refuses to run as root) and enables the service.

**On a 512 MB board swap is not optional.** The worker peaks near 190 MB of
anon RSS while the 48 MB payload is already occupying tmpfs, and without swap
the kernel OOM-kills it seconds after start (`Out of memory: Killed process …
(gnosis_vpn-work)`, then a permanent `Worker offline`). The package enables
zram on boards under 640 MB when no swap exists; on anything larger that is
unnecessary. With zram in place this box sits at ~54 MB swapped and stays
`Ready`.

**Upgrading needs the old copy gone first.** The payload is 15 MB compressed, and
`apk` writes the new one before dropping the old, so a 44 MB overlay fails
mid-extract with `No space left on device` (this is how the 1.2.0-r2 upgrade
failed on a stock TR3000 at 6.9 MB free). Remove, then add:

```sh
gnosis-ns.sh down
apk del gnosisvpn
rm -rf /usr/share/gnosisvpn /tmp/gnosisvpn /var/lib/gnosisvpn/.local/share/gnosisvpn/curvy-zk
apk add gnosisvpn
```

Keep `/var/lib/gnosisvpn` unless you want a new identity; the identity files
(`hopr.id`, `hopr.pass`) are what hold your funded node.

## Use

```sh
gnosis-ctl status       # Ready / Worker offline / Initializing
gnosis-ns.sh up         # connect, prove the circuit, steer the LAN
gnosis-ns.sh status     # daemon, tun, steering and exit IP in one screen
gnosis-ns.sh down       # LAN back on the direct path
```

`up` only steers after two successful requests 20 s apart, so a flapping circuit
never takes the LAN with it. It starts `gnosis-ns-watch.sh`, which keeps
watching for as long as the LAN is steered: on a hard failure it removes the
steering first (the LAN is never left dark behind a kill switch), then recovers
the tunnel and re-steers, and gives up with the LAN on the direct path if
recovery fails. `up --no-lan` connects without touching the LAN.

Exit selection is a UCI option, written to `/etc/vpn/gnosis-exit` on service
start, because the scripts read a file so they work without ubus:

```sh
uci set gnosisvpn.service.exit='gvpn-ger-1-j9os'   # or 'auto' to let the client pick
uci commit gnosisvpn && /etc/init.d/gnosisvpn restart
```

`uci show gnosisvpn` also holds the Blokli URL, log level and worker keepalive.
Client and driver logs go to `/run/gnosis-ns/` (root-only, tmpfs).

### Funding

The client creates its identity on first run under
`/var/lib/gnosisvpn/.local/share/gnosisvpn/`. Fund the printed node address with
wxHOPR on Gnosis Chain plus a little xDAI for gas. `gnosis-ctl --output json
status` prints the exact deficit. To reuse an identity you already funded, copy
`hopr.id` and `hopr.pass` into that directory, own them by 65533, mode 600, and
stop any other client holding the same peer id first: two clients on one
identity is the failure mode.

## Why a namespace

With `FORCE_STATIC_ROUTING=true` the root daemon installs `0.0.0.0/1` and
`128.0.0.0/1` on `wg0_gnosisvpn` and a `gnosis_vpn_ks` nft table that drops
anything not going through its TUN. Left on the router's main table it takes the
router down with it: tailscaled's DERP and control-plane sockets are public
addresses inside the mixnet, so the remote session dies while the LAN looks
fine.

`gnosis-ns.sh` therefore gives the client its own netns (`gvns`) on a veth pair
(`veth-gvh` ↔ `veth-gv`, `10.200.0.0/24`) with NAT on the host, and steers only
LAN traffic (`iif br-lan` → table 100 → `10.200.0.2`). The client's routes,
killswitch and `/etc/resolv.conf` rewrites stay inside that namespace. The LAN
prefix and the resolver are read from `br-lan` at runtime, and the namespace NAT
excludes both veth legs instead of naming a WAN device, so the script is not
tied to one board.

The daemon is owned by procd, not by a shell: a daemon started from an SSH
session dies with that session, and the log then reads `routing teardown
complete` plus `worker process exited unexpectedly`.

## Layout

| path | role |
| --- | --- |
| `/usr/share/gnosisvpn/payload/` | xz-compressed root, worker, ctl and geoip |
| `/tmp/gnosisvpn/bin/` | expanded binaries (tmpfs, wiped on boot) |
| `/etc/config/gnosisvpn` | UCI: enabled, blokli_url, loglevel, keepalive |
| `/etc/gnosisvpn/config.toml` | client config (v7) |
| `/etc/vpn/gnosis-exit` | destination id to connect to (optional) |
| `/usr/bin/gnosis-ctl` | control CLI, runs as the unprivileged account |
| `/usr/bin/gnosis-ns.sh` | namespace + steering driver |
| `/usr/bin/gnosis-ns-watch.sh` | watchdog: restore LAN first, then recover |
| `/usr/share/nftables.d/chain-pre/forward/10-gnosisvpn.nft` | fw4 drop-in: the veth accepts survive a firewall reload |
| `/run/gnosis-ns/` | root-only state: logs, watchdog pid, cached WAN address |

## Building

```sh
# in an OpenWrt SDK matching your target (25.12.x, mediatek/filogic):
make defconfig
cp -r gnosisvpn package/
make package/gnosisvpn/compile V=s
```

The Makefile unpacks the upstream release `.deb` with `ar`, so the build host
needs binutils; no Rust toolchain and no nix are involved.

## Uninstall

```sh
gnosis-ns.sh down
apk del gnosisvpn
rm -rf /var/lib/gnosisvpn /usr/share/gnosisvpn
```

## Notes

- `fw4` rebuilds `inet fw4` from scratch on every reload (WAN reconnect, DHCP
  renew, `ifup`), so the veth accepts are shipped as an `nftables.d` drop-in.
  An imperative `nft insert` lasts only until the next reload; without the
  drop-in the forward policy drops the LAN mid-session.
- LAN traffic is steered over IPv4 only. On an uplink that hands out a global
  IPv6 prefix, LAN clients would still reach the internet over IPv6 outside the
  mixnet; disable IPv6 on the LAN or accept that gap.
- DNS is not steered: LAN clients resolve through dnsmasq on the router, which
  forwards over the router's own uplink, so queries are visible to the ISP even
  while the traffic goes through the mixnet.
- `/etc/init.d/gnosisvpn stop` tears the steering down and stops the watchdog.
  An internal bounce (watchdog recovery) skips that teardown.
- The service is enabled on first install only, so an upgrade will not undo a
  deliberate disable. After removing and reinstalling, enable it again.
- The 39 MB `curvy-zk` proving key is not shipped; the v7 validator only needs
  the directory to exist, and the key is fetched on demand at first PIX.
- `xz -9` runs out of memory on a 496 MB router; the payload is `-6`.
- The client leaves 1.5-3 MB scratch files in `TMPDIR` when it is killed. They
  accumulate across restarts (82 files, 130 MB in one day of testing) until
  tmpfs is full and the client fails its own writes with `I/O error: No space
  left on device` while warming up. The package pins `TMPDIR` to
  `/tmp/gnosisvpn/tmp` and wipes it on every service start.
- Identity is per node. Do not run the desktop client and the router at the same
  time on one identity.
- `openwrt/packages` builds everything from source, so a repack of an upstream
  binary cannot go there. This feed is the install path; the packaging itself is
  proposed upstream in `gnosis/gnosis_vpn-client`.

## Licence

LGPL-3.0-only, the same as upstream. The packaged binaries are the unmodified
release binaries published by `gnosis/gnosis_vpn`.