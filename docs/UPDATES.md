# Update flow

User-triggered self-update for the gnosis_vpn daemon + ctl + bundled binaries.

## Components

- **`gnosis_vpn-lib::check_update`** — manifest fetch, `min_app_version` /
  downgrade gating, PGP path (currently disabled, see "Known temporary gap"
  below). `min_os_version` is carried but not consulted (see the note at the
  end).
- **`gnosis_vpn-lib::update`** — install engine (`install_engine`) running
  inside the daemon, and the streaming client wrapper (`install_stream`)
  consumed by both `gnosis_vpn-ctl` and the Tauri GUI (separate repo).
- **`gnosis_vpn-lib::update_apt`** — the Linux install engine. Instead of the
  download → SHA-256 → installer pipeline, it delegates the upgrade to `apt-get`
  against the gnosisvpn apt source the install script already configured. Emits
  the same `UpdateStatus` schema, collapsing check/download/verify into a single
  `Installing` event.
- **`gnosis_vpn-root`** — owns the privileged work and rejects concurrent
  `StartUpdate` requests. On **macOS** it downloads the artifact into a
  root-owned temp dir, SHA-256-verifies it, invokes the platform installer, and
  writes `last_update_attempt.json` + the audit log; on **Linux** it drives
  `apt-get` (the attempt-state file and audit log are macOS-only — apt keeps its
  own logs).
- **`gnosis_vpn-ctl`** — exposes `check-update` and `install-update`
  subcommands; the latter renders the daemon's `UpdateStatus` stream as an
  `indicatif` progress bar on stderr.

## Sequence

```
ctl/GUI                      daemon (gnosis_vpn-root)            external
   |                                  |                              |
   |  Command::CheckUpdate ---------->| fetch manifest (HTTPS) -----> download.gnosisvpn.io
   |                                  | semver + min_app gate
   |<--- Response::CheckUpdate(..) ---|
   |                                  |
   |  Command::StartUpdate ---------->| reject if an update is already
   |                                  | in progress (StartUpdateRejected)
   |                                  |
   |                                  | macOS: download artifact -----> download.gnosisvpn.io
   |                                  |   (https scheme enforced),
   |<-- Response::UpdateStatus(..) ×N |   sha256 verify, run `installer`
   |   (streaming, newline-delimited  | Linux: apt-get update + apt-get
   |    JSON over the Unix socket)    |   install --only-upgrade gnosisvpn
   |                                  |   (apt verifies the repo signature)
   |                                  | macOS: write last_update_attempt.json
   |<----- Completed / Failed --------| service restarts on the new binary
                                        (launchd/systemd or apt postinst)
```

Note: `check-update` fetches the manifest on every platform, but
`install-update` only consults the manifest on macOS — on Linux the install is
driven entirely by apt, so the manifest and the apt repo must be published
together.

## IPC contract

Defined in
[`gnosis_vpn-lib/src/command/mod.rs`](../gnosis_vpn-lib/src/command/mod.rs).

| Command                                           | Response shape                                                                        | Streaming? |
| ------------------------------------------------- | ------------------------------------------------------------------------------------- | ---------- |
| `CheckUpdate { channel, force }`                  | `Response::CheckUpdate(CheckUpdateResponse)`                                          | no         |
| `StartUpdate { channel, allow_downgrade, force }` | `Response::UpdateStatus(UpdateStatus)` ×N, or `Response::StartUpdateRejected(String)` | **yes**    |
| `GetCurrentVersion`                               | `Response::Version(String)`                                                           | no         |

`CheckUpdateResponse` distinguishes its failure modes over IPC rather than
flattening to a string: `VpnNotConnected` and `IntegrityError(String)` are
carried as their own variants (alongside `UpToDate`, `Available`,
`NoReleaseForChannel`, and a generic `Error(String)`), so `ctl` can map them to
distinct exit codes (`NOPERM` for VPN-not-connected, `SOFTWARE` for integrity).

Only one install may run at a time. While an install engine is active the daemon
tracks an in-progress flag and answers any further `StartUpdate` with
`StartUpdateRejected("an update is already in progress")`; the flag is cleared
when the engine reaches a terminal status, its channel closes, or the requesting
client disconnects. This prevents two callers from racing concurrent
`apt-get`/installer runs against the same package database and download paths.

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
3. **HTTPS enforced** — `reqwest` does **not** restrict schemes on its own, so
   the manifest URL is a hardcoded `https://` constant and the macOS artifact
   download explicitly rejects any non-`https` `download_url` before issuing the
   request (`update::download_artifact`).
4. **Artifact integrity is platform-specific:**
   - **macOS**: SHA-256 of the downloaded artifact against the manifest value is
     mandatory; mismatch → `Failed`, no retry. The hash is streamed through the
     hasher rather than buffering the whole file, so verification stays
     memory-bounded in the root daemon.
   - **Linux**: there is no client-side hash — `apt` verifies the repo's GPG
     signature (`Signed-By` keyring in the apt source) on its own.
5. **macOS installer authenticity** relies on the installer flow (the `.pkg`
   itself is signed and notarized by the packaging pipeline — see the
   `gnosis_vpn` repo's `generate-package-mac.sh`). The daemon does **not**
   perform an additional `pkgutil --check-signature` team-ID gate; the SHA-256
   from the manifest is the only client-side authenticity check on the artifact.
6. **Downgrade** requires explicit `--allow-downgrade` on the CLI; the GUI does
   not expose this.
7. **No shell interpolation** — `installer` (macOS) and `apt-get` / `dpkg-query`
   (Linux) are all invoked via `tokio::process::Command::new().arg()` with
   separate arguments.
8. **Root-owned temp dir** (macOS only), `0700`
   (`/Library/Application Support/GnosisVPN/updates/`). Symlinks at the download
   path are rejected before write. On Linux apt manages its own download/cache.
9. **Free disk space check** before download (macOS only):
   `size_bytes + 500 MB`.
10. **Audit log** (macOS only) at `/var/log/gnosisvpn/updates.log` — one line
    per terminal `UpdateStatus`. On Linux the apt engine relies on apt's own
    logging.

## Known temporary gap

PGP verification of the manifest is **currently disabled** in
[`check_update.rs::verify_and_parse`](../gnosis_vpn-lib/src/check_update.rs) —
the detached `.asc` signature is fetched but not checked. It will be re-enabled
in a follow-up workstream once the public key is hosted externally. Until then
the manifest's only integrity guarantee is HTTPS transport security.

This gap affects the **macOS** path, where the SHA-256 in the (unsigned)
manifest is the artifact's only end-to-end guarantee. On **Linux** the install
goes through apt, which verifies the repo's GPG signature independently of the
manifest, so the disabled manifest PGP does not weaken the Linux artifact chain.

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

**macOS only.** If the daemon is killed mid-install, the new daemon spawned by
launchd reads `last_update_attempt.json` at startup, surfaces the last status to
the operator log, and clears the file. The state file lives at
`/Library/Application Support/GnosisVPN/last_update_attempt.json`.

The Linux apt engine does not write an attempt-state file — a mid-install kill
is recoverable through apt's own state (`dpkg --configure -a`) and apt's logs.
