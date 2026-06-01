# Update flow

User-triggered self-update for the gnosis_vpn daemon + ctl + bundled binaries.

## Components

- **`gnosis_vpn-lib::check_update`** — manifest fetch, version + OS gating, PGP
  path (currently disabled, see "Known temporary gap" below).
- **`gnosis_vpn-lib::update`** — install engine (`install_engine`) running
  inside the daemon, and the streaming client wrapper (`install_stream`)
  consumed by both `gnosis_vpn-ctl` and the Tauri GUI (separate repo).
- **`gnosis_vpn-root`** — owns the privileged work: downloads the artifact into
  a root-owned temp dir, verifies it, invokes the platform installer, writes
  `last_update_attempt.json`, appends to the audit log.
- **`gnosis_vpn-ctl`** — exposes `check-update` and `install-update`
  subcommands; the latter renders the daemon's `UpdateStatus` stream as an
  `indicatif` progress bar on stderr.

## Sequence

```
ctl/GUI                      daemon (gnosis_vpn-root)            external
   |                                  |                              |
   |  Command::CheckUpdate / -------->| fetch manifest (HTTPS) -----> download.gnosisvpn.io
   |  Command::StartUpdate            | semver + min_app + min_os
   |                                  | gate
   |<------------- Response::         | ...
   |              CheckUpdate(..)     |
   |              or UpdateStatus(..) | download artifact ----------> download.gnosisvpn.io
   |   (streaming, newline-           | sha256 verify
   |    delimited JSON over the       | spawn installer/dpkg/rpm (detached)
   |    Unix socket for StartUpdate)  |
   |                                  | write last_update_attempt.json
   |<----- Completed / Failed --------| daemon exits 0 (launchd/systemd
                                        respawns the new binary)
```

## IPC contract

Defined in
[`gnosis_vpn-lib/src/command/mod.rs`](../gnosis_vpn-lib/src/command/mod.rs).

| Command                                           | Response shape                               | Streaming? |
| ------------------------------------------------- | -------------------------------------------- | ---------- |
| `CheckUpdate { channel, force }`                  | `Response::CheckUpdate(CheckUpdateResponse)` | no         |
| `StartUpdate { channel, allow_downgrade, force }` | `Response::UpdateStatus(UpdateStatus)` ×N    | **yes**    |
| `GetCurrentVersion`                               | `Response::Version(String)`                  | no         |

`force = false` (the default) refuses to fetch the manifest or download an
artifact unless the daemon reports an active VPN connection. The check is
performed inside the engine task (`check_update::ensure_vpn_connected`), which
self-connects through the same Unix socket — both `CheckUpdate` and
`StartUpdate` are dispatched onto separate tokio tasks so this self-call doesn't
deadlock the socket listener.

Authorization invariants:

- Callers pass `Channel` and `allow_downgrade` only. The manifest URL, download
  paths, signing key, and audit log path are **all hardcoded** in the daemon
  binary.
- The daemon serves the existing Unix socket at `/var/run/gnosisvpn.sock` with
  permissions `0666`. Any local IPC caller can trigger an update; the daemon
  controls _what_ is installed.

The streaming protocol is newline-delimited JSON `Response` records over the
existing socket transport. See
[`socket::root::stream_cmd`](../gnosis_vpn-lib/src/socket/root.rs) for the
client-side helper.

## Security model

1. **Public key** is shipped in-tree at
   [`gnosisvpn-public-key.asc`](../gnosisvpn-public-key.asc) and is intended to
   be embedded via `include_str!` once PGP verification is re-enabled
   ([known temporary gap](#known-temporary-gap)). Never loaded from disk at
   runtime.
2. **Manifest URL** (`https://download.gnosisvpn.io/manifests/`) is hardcoded in
   [`check_update.rs`](../gnosis_vpn-lib/src/check_update.rs). Not overridable
   from IPC, env, or config.
3. **TLS required** — `reqwest::Client::builder().build()` defaults to
   HTTPS-only.
4. **SHA-256** on the downloaded artifact is mandatory; mismatch → `Failed`, no
   retry.
5. **macOS**: artifact authenticity relies on the installer flow (the `.pkg`
   itself is signed and notarized by the packaging pipeline — see the
   `gnosis_vpn` repo's `generate-package-mac.sh`). The daemon does **not**
   perform an additional `pkgutil --check-signature` team-ID gate; the SHA-256
   from the manifest is the only client-side authenticity check on the artifact.
6. **Downgrade** requires explicit `--allow-downgrade` on the CLI; the GUI does
   not expose this.
7. **No shell interpolation** — `installer`, `dpkg`, `rpm` are all invoked via
   `tokio::process::Command::new().arg()` with separate arguments.
8. **Root-owned temp dir**, `0700` (`/var/lib/gnosisvpn/updates/` on Linux,
   `/Library/Application Support/GnosisVPN/updates/` on macOS). Symlinks at the
   download path are rejected before write.
9. **Free disk space check** before download: `size_bytes + 500 MB`.
10. **Audit log** at `/var/log/gnosisvpn/updates.log` — one line per terminal
    `UpdateStatus`.

## Known temporary gap

PGP verification of the manifest and artifact is **currently disabled** in
[`check_update.rs::verify_and_parse`](../gnosis_vpn-lib/src/check_update.rs). It
will be re-enabled in a follow-up workstream once the public key is hosted
externally. Until then the manifest's only integrity guarantee is HTTPS
transport security, and the artifact's only end-to-end guarantee is the SHA-256
in the manifest.

## CLI

```
gnosis_vpn-ctl check-update    [--force] [--channel stable|snapshot]
gnosis_vpn-ctl install-update  [--channel stable|snapshot]
                               [--yes] [--allow-downgrade] [--force]
```

Both subcommands refuse to run unless the VPN is up. `--force` bypasses the
gate; the manifest and artifact then travel over your raw internet connection
rather than the VPN tunnel — use only for recovery from a broken VPN session.

`-o json` / `-o yaml` works on both. For `install-update`, `-o json` emits one
JSON document per `UpdateStatus` event (newline-delimited) — scripts can consume
the stream. `-o plain` renders a live `indicatif` progress bar on stderr when
stderr is a TTY.

## Release rollout

1. Build signed `.pkg` / `.deb` / `.rpm` artifacts in the packaging pipeline.
2. Compute SHA-256 of each artifact; place them at the published URLs.
3. Regenerate `linux-amd64.json`, `linux-arm64.json`, `macos-arm64.json`
   manifests via `scripts/generate-update-manifest.sh` (lives in the packaging
   repo). Each manifest must include `min_app_version` and `min_os_version` for
   every channel entry.
4. Publish manifests + matching detached `.asc` signatures to
   `download.gnosisvpn.io/manifests/`.
5. Users see the new version on the next `check-update` (or GUI tick), and
   trigger `install-update` (or click "Install" in the GUI) when ready.

## A note on `min_os_version`

The manifest still carries `min_os_version` per release, but the daemon **does
not consult it**. The field is currently authored with Ubuntu-style values
(`22.04`) and there's no good way to compare that against a Debian (`12`) or
Fedora (`40`) host; meanwhile, `.deb`/`.rpm` artifacts already declare their
real package dependencies (`libc6 >= …`, etc.), so `dpkg`/`rpm` surface a
meaningful error at install time if the host is too old. On macOS the `.pkg`
postinstall handles that itself.

If a daemon-side preflight is ever needed, the right fix is a per-distro field
in the manifest
(`min_os_version: { ubuntu: "20.04", debian: "11",
fedora: "39", macos: "14.0" }`),
not reviving the single-string compare.

## First-boot replay

If the daemon is killed mid-install, the new daemon spawned by launchd/systemd
reads `last_update_attempt.json` at startup, surfaces the last status to the
operator log, and clears the file.

The state file lives at `/var/lib/gnosisvpn/last_update_attempt.json` (Linux) or
`/Library/Application Support/GnosisVPN/last_update_attempt.json` (macOS).
