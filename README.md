# Gnosis VPN Client

Gnosis VPN is a VPN solution build on top the HOPR mixnet. This repo contains
the system service and a control application. It is part of a larger project
consisting of:

- [Gnosis VPN App](https://github.com/gnosis/gnosis_vpn-app) handling user
  interface
- [Gnosis VPN Server](https://github.com/gnosis/gnosis_vpn-server) handling VPN
  server side

## Reporting Issues

We use **[Gnosis VPN](https://github.com/gnosis/gnosis_vpn)** repository as the
central hub for all user feedback.

### How to report an issue

1. Visit
   [Gnosis VPN Discussions](https://github.com/gnosis/gnosis_vpn/discussions)
   board.
1. Search existing Discussions and
   [Issues](https://github.com/gnosis/gnosis_vpn/issues) to see if your topic is
   already covered.
1. If not, start a new Discussion in the
   [Issues & Bug Reports](https://github.com/gnosis/gnosis_vpn/discussions/new?category=issues-bug-reports)
   category.
1. Provide as much detail as possible using the provided template.

The team will review all discussions and promote confirmed bugs or planned
features to actionable issues. This repository is reserved for tracking
actionable work on this component.

## General concept

The client offers three binaries, `gnosis_vpn-root`, `gnosis_vpn-worker` and
`gnosis_vpn-ctl`. The client system service (`gnosis_vpn-root`) runs with root
privileges and takes care of routing setup. It spawn the worker process
(`gnosis_vpn-worker`) which is responsible for the application logic. The
control application (`gnosis_vpn-ctl`) is used to manage the client and its
connections.

## Multi-user model

`gnosis_vpn-root` is the only privileged component; there is no service
account. Whoever runs `gnosis_vpn-ctl start-client` gets a worker running as
themselves, with state in their own per-user directories. The kernel reports the
caller's uid on the control socket, so nothing is configured for this.

- **One worker per machine.** While user A's worker runs (or is still shutting
  down), every other user's `start-client`, `status`, `destinations`, `connect`,
  … is answered with `InUseByOtherUser` (`gnosis_vpn-ctl` exits `NOPERM`).
  `ping` and `info` stay open to everyone; `info` reports the worker log
  directory only to the owner and root. The tunnel is machine-wide, so all local
  traffic uses A's connection while it is up.
- **Root may use anyone's worker, never own one.** `sudo gnosis_vpn-ctl status` and
  `stop-client` work on anyone's worker; `sudo gnosis_vpn-ctl start-client` is
  refused with `RootCannotOwnWorker`.
- **A failed spawn is a response, not an outage.** `start-client` answers
  `SpawnFailed { reason }` (exit `UNAVAILABLE`) and the service stays up.
- **Pinned mode** (`--worker-user <name>` / `GNOSISVPN_WORKER_USER`): every
  caller acts as that user and there is no ownership gate. Docker, the system
  tests and boot autostart use it. `--state-home`, `--hopr-identity-file`,
  `--hopr-identity-pass` and `--client-autostart` are only accepted together
  with `--worker-user`, because each would give all users one shared identity or
  directory.

### Per-user directories

The network name comes from the Blokli `chainInfo.network` the service talks
to; it is never configured. Per user and network:

| Content                                          | Linux                                          | macOS                                                         |
| ------------------------------------------------ | ---------------------------------------------- | ------------------------------------------------------------- |
| identity `hopr.id`/`hopr.pass`, PIX state `pix/` | `~/.local/share/gnosisvpn/networks/<network>/` | `~/Library/Application Support/GnosisVPN/networks/<network>/` |
| safe cache `hopr.safe`                           | `~/.cache/gnosisvpn/networks/<network>/`       | `~/Library/Caches/GnosisVPN/networks/<network>/`              |
| worker logs `worker.log.YYYY-MM-DD`              | `~/.local/state/gnosisvpn/`                    | `~/Library/Logs/GnosisVPN/`                                   |

With `--state-home <home>` (pinned mode only) the same layout sits under
`<home>/`, `<home>/cache/` and `<home>/logs/`. An empty `networks/<network>/`
means a fresh identity is generated; `hopr.pass` is `0600`, the directories are
`0700`. The worker keeps seven daily log files; the root log
(`--log-file`) is root-owned and rotated by logrotate/newsyslog via SIGHUP as
before.

## Development setup

1. _**Only for: Ubuntu 26.04 LTS**_ (TODO: remove once userspace wireguard is
   merged)

```
sudo tee /etc/apparmor.d/local/wg-quick >/dev/null <<'EOF'
# Local build: allow wg-quick to read the dev user's managed config.
/home/<VPN_USER>/wg0_gnosisvpn.conf r, # VPN_USER: e.g. gnosisvpn, gnosisvpnrotsee, gnosisvpnjura
EOF

sudo apparmor_parser -r /etc/apparmor.d/wg-quick`
```

2. Build binaries via: `cargo build` or `nix build .#gnosis_vpn-dev`.

3. Run the root binary with sudo and point it at the worker binary; the worker
   binary must be executable by whoever runs `start-client`:

```bash
> sudo RUST_LOG="debug" ./target/debug/gnosis_vpn-root -c <config.toml> \
    --hopr-blokli-url <hopr blokli url> --worker-binary ./target/debug/gnosis_vpn-worker
# or from nix build:
> sudo RUST_LOG="debug" ./result/bin/gnosis_vpn-root -c <config.toml> \
    --hopr-blokli-url <hopr blokli url> --worker-binary ./result/bin/gnosis_vpn-worker
```

4. From your own shell: `gnosis_vpn-ctl start-client`. The worker runs as you,
   with state under your home (see [Per-user directories](#per-user-directories)).

To run as a dedicated user instead (what docker and the system tests do), create
that user with a home directory and add `--worker-user <name>`; `--state-home`
then relocates its state, e.g. `GNOSISVPN_HOME=/var/lib/gnosisvpn`.

### Worker configuration

- `GNOSISVPN_WORKER_BINARY` / `--worker-binary`: the worker binary to spawn.
  Defaults to `./gnosis_vpn-worker`.

- `GNOSISVPN_WORKER_USER` / `--worker-user`: pin the worker to one user
  (pinned mode, see above). Unset by default: the worker runs as the caller.

- `GNOSISVPN_HOME` / `--state-home`: state directory override, pinned mode only.
  Without it the per-user directories above apply.

## Testing

### Unit tests

```bash
cargo test
```

## Installation

Use the latest
[installer](https://github.com/gnosis/gnosis_vpn/releases/latest).

Verify integrity before installing - see [SECURITY.md](./SECURITY.md).

## Idle shutdown

When `--client-autostart <duration>` (or `GNOSISVPN_CLIENT_AUTOSTART`) is set,
the service starts the worker automatically on launch and shuts it down after
the worker has been idle for the given duration.

The idle countdown behaves as follows:

- **Idle**: countdown runs; worker shuts down when it reaches zero.
- **Connected**: countdown is suspended for the lifetime of the VPN tunnel.
- **Probing**: `probe <id>` suspends the countdown like a connection; `unprobe` resumes it unless a connection target is still set. A probe started during a quick check of the same exit takes over that session instead of opening a second one. The probe `connect` opens on its way in is implicit - it never suspends the countdown and closes again once the target is cleared.
- **Quick probing**: `quickprobe <id>` only resets the countdown like any other command - it is bounded and holds no session. It returns as soon as the check starts; its result is kept under the destination in `status`.
- **Disconnected**: countdown resumes from the full duration.
- Any other command to the worker (e.g. status) resets the countdown.

Example — shut down the worker after 10 minutes of idle time:

```bash
gnosis_vpn-root --client-autostart 10m ...
```

## General usage

Check available params and env vars via:

`gnosis_vpn-root --help` `gnosis_vpn-ctl --help`

Diagnostic/ops scripts (connectivity smoke test, capacity/latency drain tour, WireGuard traffic sampling) live in [`scripts/`](scripts/README.md).

## Branches

`main` is the experimental line (hoprd v5); `release/hoprdv4` is the stable
maintenance line (hoprd v4). See [docs/branch-strategy.md](docs/branch-strategy.md).

## Dependency Updates

Renovate runs on Renovate's `schedule:earlyMondays` preset with a 14-day minimum
release age, so most PRs appear early Monday morning and only for packages that
have been released for at least two weeks.

Updates are grouped by ecosystem:

| Group               | What it covers                                        | Notes                                                                                                 |
| ------------------- | ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------- |
| `nix flake updates` | `flake.lock` inputs (nixpkgs, crane, rust-overlay, …) | digest/pinDigest updates; `pinDigests` disabled for the `nix` manager since nix pins via `flake.lock` |
| `github-actions`    | `.github/workflows` action refs                       | digest-pinned                                                                                         |
| _(individual PRs)_  | Cargo crates                                          | one PR per crate                                                                                      |

`prCreation: immediate` is intentional — CI only triggers on pull request
events, so waiting for branch checks would deadlock.

## Deployment

Show potential deployment targets:

`nix flake show`

Build for a target, e.g. `x86_64-linux` or `aarch64-linux`:

```bash
# For x86_64 (AMD64)
nix build -L .#binary-gnosis_vpn-x86_64-linux

# For ARM64
nix build -L .#binary-gnosis_vpn-aarch64-linux
```

These settle PIX through the Curvy pool (`pix-curvy`, the default). For testing against Exits built
with hoprd's `strategy-pix-test`, which settle to plain secp256k1 transfers, build the `pix-test`
variant instead: `nix build -L .#binary-gnosis_vpn-pix-test-x86_64-linux` or
`just docker-build-pix-test` (image `gnosis_vpn-client:pix-test`). The pool is fixed at build time
and never negotiated, so an Exit refuses every PIX session from a client built for the other one.

The resulting binaries are in `result/bin/`:

```
$ ls -l result*/bin/
result/bin/:
total 4752
-r-xr-xr-x 1 root root  4863367 Jan  1  1970 gnosis_vpn-root
-r-xr-xr-x 1 root root 14863367 Jan  1  1970 gnosis_vpn-worker
-r-xr-xr-x 1 root root  1740059 Jan  1  1970 gnosis_vpn-ctl
```
