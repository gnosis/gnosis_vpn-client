# Gnosis VPN network baseline harness

Repeatable end-to-end measurement of the Gnosis VPN exits from a real headless
browser. For each exit it verifies egress, browses real sites, crawls a light site
link-by-link, runs a Cloudflare speedtest twice, and samples ICMP latency — dumping
everything to JSON + CSV so a run **before** a network upgrade can be diff'd against
a run **after**.

Traffic is driven through the browser and the VPN is full-tunnel, so no proxy
config is needed — once an exit is connected, all traffic egresses through it.

## What it measures (per exit)

- **Egress verification** — Cloudflare `trace` (`ip`, `loc`, `colo`); asserts the
  country matches the expected exit. A run that egresses locally (leak) is
  **skipped**, never recorded as if it were the exit.
- **Site visits** — YouTube, CNN, Wikipedia, BBC, GitHub, Reddit, Amazon: per-page
  DOMContentLoaded / load time, HTTP status, failed sub-requests, outcome
  (`ok` / `http_error` / `timeout` / `error`).
- **YouTube playback** — one video drawn per run from a pool with known native
  ceilings (2160p ×2, 240p). The pick is stored in the JSON so runs stay
  comparable; rotating it stops one edge-cached asset from defining the numbers.
  Playback state and resolution are recorded, with the caveat below. `run.sh` draws
  the index once per run (`YT_VIDEO_INDEX`) so every exit is compared on the same
  video.
- **Link crawl** — from a random Wikipedia article, follow `CRAWL_HOPS` (default 5)
  internal links, timing each hop and never revisiting an article. One-shot page
  loads measure asset weight; this measures how fast you can actually move around
  behind the tunnel.
- **Speedtest ×2** — download / upload Mbps, latency (min/avg/max/median), jitter,
  against `speed.cloudflare.com` `__down`/`__up`.
- **ICMP latency** — continuous `ping` to `1.1.1.1` and the VPN gateway
  `10.128.0.1` for the whole exit window: min/avg/max/loss.

## Prerequisites

- **Nix with flakes** — the `headless` dev shell supplies obscura, Node, curl and
  jq. Supported: `aarch64-darwin`, `x86_64-linux`, `aarch64-linux` (the platforms
  obscura ships prebuilts for).
- The Gnosis VPN service running locally, reachable via
  `../target/release/gnosis_vpn-ctl` (control socket `/var/run/gnosisvpn.sock`).
  Build it from the default shell: `nix develop -c cargo build --release`.
- The exits you want must exist as `[destinations.<ID>]` in the running config and
  be `ReadyToConnect` (`gnosis_vpn-ctl -o json status`).

## Run

```bash
nix develop .#headless -c "run.sh"
```

The shell puts `run.sh` on `PATH` and installs the Node deps on first entry. To
choose exits:

```bash
nix develop .#headless -c "run.sh"                                   # default exits: USA UK_1 India
nix develop .#headless -c "run.sh --destination India"               # a single exit
nix develop .#headless -c "run.sh --destination USA UK_1 India"      # explicit list
```

Each exit takes ~10 min (dominated by the two speedtests). The orchestrator, per
exit: connects → waits until egress actually routes via the exit country →
snapshots `nerd-stats` → runs the browser harness → disconnects. It starts its own
obscura CDP server on `127.0.0.1:9222` if one isn't already up.

Country mapping for the egress gate lives in `run.sh` (`expected_cc`):
`USA→US`, `UK_*→GB`, `India→IN`. Add cases there for new exits.

Harness knobs, all env vars: `NAV_TIMEOUT_MS`, `DOWNLOAD_SECS`, `UPLOAD_SECS`,
`DWELL_MS`, `CRAWL_START`, `CRAWL_HOPS`, `YT_VIDEO_INDEX`.

## Output

Written to `results/<UTC-timestamp>/` (gitignored — commit a run yourself if you
want it kept for comparison):

- `<EXIT>.json` — full per-exit record (egress, navigations, crawl, speedtests, ping).
- `<EXIT>.nerdstats.json` — `nerd-stats` snapshot (hop path, route rtt, sessions).
- `summary.csv` — one row per exit, the headline numbers. Also printed to stdout at
  the end of a run, so it can be piped straight into another tool.
- `meta.json` — run label: timestamp, git commit, machine, obscura version, and the
  disconnected-baseline egress. **This is what makes two runs comparable.**

To compare before/after an upgrade: run once now, run again post-upgrade, diff the
`summary.csv`s (and per-site JSON for detail).

## Files

| File            | Role                                                                        |
| --------------- | --------------------------------------------------------------------------- |
| `run.sh`        | orchestrator: connect / gate on egress / drive / disconnect / aggregate     |
| `harness.mjs`   | per-exit driver over obscura CDP (egress, browsing, crawl, speedtest, ping) |
| `aggregate.mjs` | builds `summary.csv` + prints the table                                     |

obscura itself is packaged in `../nix/obscura.nix`, pinned to an upstream release
with a per-platform hash.

## Notes & caveats (learned the hard way)

- **Throughput cap.** The rotsee config paces download via SURB balancing
  (`[connection.surb_balancing.main] max_surb_upstream`). Speedtest reads whatever
  the tunnel allows — that IS the baseline. A 2026-08-27 rotsee run measured
  1.5–4.3 Mbps down / 1.0–2.2 Mbps up across USA, UK_1 and India. Raising the cap needs a config edit +
  service restart.
- **obscura ~30 s cap.** obscura aborts any single CDP `page.evaluate`
  (`Runtime.callFunctionOn`) **and** `page.goto` at ~30 s. The speedtest is therefore
  chunked into short Node-orchestrated calls with adaptive sizing, and every in-page
  fetch has an `AbortController` < 30 s so a stall fails gracefully instead of
  killing the run. Don't reintroduce a single long-running `evaluate`.
- **Heavy pages over a capped tunnel.** A 4–5 MB page (YouTube/CNN) may not finish
  DOMContentLoaded within obscura's 30 s over a ~2 Mbps tunnel → recorded as
  `timeout`/`error`. That's a real signal, not a harness bug. Reddit returns 403 to
  the headless UA. Light pages (Wikipedia/BBC/GitHub/Amazon) render fine.
- **YouTube playback does not actually start under obscura.** The harness asks for
  muted playback and then records `playing`, `width`/`height`, `quality` and
  `played_s`, but in practice obscura leaves the player paused at 0×0 with quality
  `unknown` — so `yt_res` is usually blank. What the watch page therefore measures
  is page + player load, not video decode. The recorded fields are there so the
  number appears by itself if a future obscura build can play; the pool entry's
  `max_res` is the upload's ceiling, never a forced target.
- **"connected" ≠ routing.** The control plane can report connected while traffic
  still egresses locally, or while the exit health is flapping. The harness only
  measures once egress verifies through the exit country; otherwise it skips with a
  `fatal_error` record. If an exit is stuck `Routable`/`Unrecoverable`, wait for it
  to reach `ReadyToConnect`.
