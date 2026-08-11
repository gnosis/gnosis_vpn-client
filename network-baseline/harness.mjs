// Per-exit baseline driver. Drives obscura (CDP) to browse real sites and run
// an in-page Cloudflare-endpoint speedtest, samples ICMP latency in parallel,
// and writes <OUTDIR>/<EXIT>.json.
//
// Env: EXIT, OUTDIR, EXPECTED_CC, CDP (default ws://127.0.0.1:9222/devtools/browser)
//      NAV_TIMEOUT_MS, DOWNLOAD_SECS, UPLOAD_SECS, DWELL_MS
import puppeteer from 'puppeteer-core';
import { readFileSync, writeFileSync } from 'fs';
import { spawn } from 'child_process';

const EXIT = process.env.EXIT || 'unknown';
const OUTDIR = process.env.OUTDIR || '.';
const EXPECTED_CC = process.env.EXPECTED_CC || '';
const CDP = process.env.CDP || 'ws://127.0.0.1:9222/devtools/browser';
const NAV_TIMEOUT_MS = +(process.env.NAV_TIMEOUT_MS || 90000);
const DOWNLOAD_SECS = +(process.env.DOWNLOAD_SECS || 90);
const UPLOAD_SECS = +(process.env.UPLOAD_SECS || 45);
const DWELL_MS = +(process.env.DWELL_MS || 4000);

const SITES = [
  { label: 'youtube-home', url: 'https://www.youtube.com' },
  { label: 'youtube-watch', url: 'https://www.youtube.com/watch?v=aqz-KE-bpKQ' },
  { label: 'cnn-home', url: 'https://www.cnn.com' },
  { label: 'cnn-world', url: 'https://www.cnn.com/world' },
  { label: 'wikipedia', url: 'https://en.wikipedia.org/wiki/Special:Random' },
  { label: 'bbc', url: 'https://www.bbc.com/news' },
  { label: 'github', url: 'https://github.com/trending' },
  { label: 'reddit', url: 'https://www.reddit.com' },
  { label: 'amazon', url: 'https://www.amazon.com' },
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const nowISO = () => new Date().toISOString();

// ---- ICMP latency sampler (through the tunnel, full-tunnel routing) ----
function startPing(target) {
  const p = spawn('ping', ['-i', '1', target]);
  let out = '';
  p.stdout.on('data', (d) => (out += d));
  p.stderr.on('data', () => {});
  return {
    stop() {
      return new Promise((resolve) => {
        p.on('close', () => resolve(parsePing(out, target)));
        p.kill('SIGINT'); // macOS ping prints summary on SIGINT
        setTimeout(() => { try { p.kill('SIGKILL'); } catch {} resolve(parsePing(out, target)); }, 3000);
      });
    },
  };
}
function parsePing(out, target) {
  const rtts = [...out.matchAll(/time=([\d.]+)\s*ms/g)].map((m) => +m[1]);
  const lossM = out.match(/([\d.]+)%\s*packet loss/);
  const statsM = out.match(/round-trip min\/avg\/max\/stddev = ([\d.]+)\/([\d.]+)\/([\d.]+)\/([\d.]+)/);
  return {
    target,
    samples: rtts.length,
    loss_pct: lossM ? +lossM[1] : null,
    min_ms: statsM ? +statsM[1] : (rtts.length ? Math.min(...rtts) : null),
    avg_ms: statsM ? +statsM[2] : (rtts.length ? +(rtts.reduce((a, b) => a + b, 0) / rtts.length).toFixed(2) : null),
    max_ms: statsM ? +statsM[3] : (rtts.length ? Math.max(...rtts) : null),
    stddev_ms: statsM ? +statsM[4] : null,
  };
}

// ---- in-page speedtest against Cloudflare endpoints (browser traffic) ----
// obscura caps every CDP Runtime.callFunctionOn (page.evaluate) at ~30s, so the
// measurement is orchestrated from Node in short sub-30s calls. Chunk size adapts
// to the observed rate to target ~8s per call regardless of tunnel bandwidth.
const CLAMP = (v, lo, hi) => Math.max(lo, Math.min(hi, v));

async function measureLatency(page) {
  const s = [];
  for (let i = 0; i < 12; i++) {
    try {
      const ms = await page.evaluate(async (i) => {
        const ac = new AbortController(); const to = setTimeout(() => ac.abort(), 15000);
        const t = performance.now();
        try { await fetch('https://speed.cloudflare.com/__down?bytes=0&i=' + i, { cache: 'no-store', signal: ac.signal }); }
        finally { clearTimeout(to); }
        return performance.now() - t;
      }, i);
      s.push(ms);
    } catch { /* skip failed sample */ }
  }
  if (!s.length) return { error: 'all latency samples failed' };
  s.sort((a, b) => a - b);
  const avg = s.reduce((a, b) => a + b, 0) / s.length;
  const jitter = s.slice(1).reduce((a, v, i) => a + Math.abs(v - s[i]), 0) / (s.length - 1);
  return { samples: s.length, min_ms: +s[0].toFixed(2), avg_ms: +avg.toFixed(2), max_ms: +s[s.length - 1].toFixed(2), median_ms: +s[Math.floor(s.length / 2)].toFixed(2), jitter_ms: +jitter.toFixed(2) };
}

async function measureDownload(page, secs) {
  const STREAMS = 4;
  let totalBytes = 0, perStream = 256 * 1024;
  const t0 = Date.now();
  try {
    while ((Date.now() - t0) / 1000 < secs) {
      const r = await page.evaluate(async ({ bytes, streams }) => {
        const ac = new AbortController(); const to = setTimeout(() => ac.abort(), 25000);
        const t = performance.now(); let b = 0;
        try {
          await Promise.all(Array.from({ length: streams }, async () => {
            const resp = await fetch('https://speed.cloudflare.com/__down?bytes=' + bytes + '&t=' + performance.now(), { cache: 'no-store', signal: ac.signal });
            b += (await resp.arrayBuffer()).byteLength;
          }));
        } finally { clearTimeout(to); }
        return { bytes: b, ms: performance.now() - t };
      }, { bytes: perStream, streams: STREAMS });
      totalBytes += r.bytes;
      const bps = r.bytes * 8 / (r.ms / 1000);           // aggregate bits/s this call
      perStream = CLAMP(Math.round((bps / 8) / STREAMS * 8), 256 * 1024, 25_000_000); // target ~8s/call
    }
  } catch (e) { /* obscura 30s abort or reset: keep partial */ if (!totalBytes) return { error: String(e).split('\n')[0] }; }
  const elapsed = (Date.now() - t0) / 1000;
  return { bytes: totalBytes, secs: +elapsed.toFixed(2), mbps: +((totalBytes * 8) / elapsed / 1e6).toFixed(2), streams: STREAMS };
}

async function measureUpload(page, secs) {
  const STREAMS = 3;
  let totalBytes = 0, perStream = 256 * 1024;
  const t0 = Date.now();
  try {
    while ((Date.now() - t0) / 1000 < secs) {
      const r = await page.evaluate(async ({ bytes, streams }) => {
        const ac = new AbortController(); const to = setTimeout(() => ac.abort(), 25000);
        const payload = new Uint8Array(bytes);
        const t = performance.now(); let b = 0;
        try {
          await Promise.all(Array.from({ length: streams }, async () => {
            const resp = await fetch('https://speed.cloudflare.com/__up', { method: 'POST', body: payload, cache: 'no-store', signal: ac.signal });
            await resp.arrayBuffer(); b += bytes;
          }));
        } finally { clearTimeout(to); }
        return { bytes: b, ms: performance.now() - t };
      }, { bytes: perStream, streams: STREAMS });
      totalBytes += r.bytes;
      const bps = r.bytes * 8 / (r.ms / 1000);
      perStream = CLAMP(Math.round((bps / 8) / STREAMS * 8), 256 * 1024, 15_000_000);
    }
  } catch (e) { if (!totalBytes) return { error: String(e).split('\n')[0] }; }
  const elapsed = (Date.now() - t0) / 1000;
  return { bytes: totalBytes, secs: +elapsed.toFixed(2), mbps: +((totalBytes * 8) / elapsed / 1e6).toFixed(2), streams: STREAMS };
}

async function runSpeedtest(page, label) {
  const latency = await measureLatency(page);
  const download = await measureDownload(page, DOWNLOAD_SECS);
  const upload = await measureUpload(page, UPLOAD_SECS);
  return { label, at: nowISO(), latency, download, upload };
}

// ---- egress verification via Cloudflare trace ----
async function egressCheck(page) {
  try {
    return await page.evaluate(async () => {
      const ac = new AbortController();
      const to = setTimeout(() => ac.abort(), 20000); // keep in-page failure under obscura's 30s protocol cap
      try {
        const r = await fetch('https://cloudflare.com/cdn-cgi/trace', { cache: 'no-store', signal: ac.signal });
        const t = await r.text();
        const kv = Object.fromEntries(t.trim().split('\n').map((l) => l.split('=')));
        return { ip: kv.ip, loc: kv.loc, colo: kv.colo, warp: kv.warp };
      } catch (e) { return { error: String(e) }; } finally { clearTimeout(to); }
    });
  } catch (e) { return { error: 'evaluate-aborted: ' + String(e).split('\n')[0] }; }
}

// ---- single site navigation with node-side timing + failure counters ----
async function visit(page, site) {
  let failedResponses = 0, requestFailures = 0;
  const onResp = (resp) => { if (resp.status() >= 400) failedResponses++; };
  const onFail = () => { requestFailures++; };
  page.on('response', onResp);
  page.on('requestfailed', onFail);
  const rec = { label: site.label, url: site.url, at: nowISO() };
  const t0 = Date.now();
  try {
    // DOMContentLoaded = "page usable" over the capped tunnel; some sites (e.g. cnn)
    // resolve with no HTTP status object but still render — success = goto did not throw.
    const resp = await page.goto(site.url, { waitUntil: 'domcontentloaded', timeout: NAV_TIMEOUT_MS });
    rec.dcl_ms = Date.now() - t0;
    rec.http_status = resp ? resp.status() : null;
    // scroll to trigger lazy loads + a bounded settle window ("jump around")
    try { await page.evaluate(() => window.scrollBy(0, 3000)); } catch {}
    await sleep(DWELL_MS);
    rec.load_ms = Date.now() - t0;
    try {
      const nav = await page.evaluate(() => {
        const n = performance.getEntriesByType('navigation')[0];
        return n ? { ttfb_ms: Math.round(n.responseStart), domInteractive_ms: Math.round(n.domInteractive), load_ms: Math.round(n.loadEventEnd) } : null;
      });
      if (nav) rec.perf_timing = nav;
    } catch {}
    rec.outcome = (rec.http_status && rec.http_status >= 400) ? 'http_error' : 'ok';
  } catch (e) {
    rec.total_ms = Date.now() - t0;
    rec.outcome = /timeout|Timeout|Navigation timeout/.test(String(e)) ? 'timeout' : 'error';
    rec.error = String(e).split('\n')[0];
  }
  rec.failed_responses = failedResponses;
  rec.request_failures = requestFailures;
  page.off('response', onResp);
  page.off('requestfailed', onFail);
  return rec;
}

// ---- main ----
const result = {
  exit: EXIT,
  expected_cc: EXPECTED_CC,
  started_at: nowISO(),
  cdp: CDP,
  params: { NAV_TIMEOUT_MS, DOWNLOAD_SECS, UPLOAD_SECS, DWELL_MS },
};

const pings = [startPing('1.1.1.1'), startPing('10.128.0.1')];

const browser = await puppeteer.connect({ browserWSEndpoint: CDP });
const page = await browser.newPage();
try {
  result.egress = await egressCheck(page);
  result.egress_match = EXPECTED_CC ? (result.egress.loc === EXPECTED_CC) : null;
  console.log(`[${EXIT}] egress: ${JSON.stringify(result.egress)} match=${result.egress_match}`);

  result.navigations = [];
  for (const site of SITES) {
    const rec = await visit(page, site);
    console.log(`[${EXIT}] ${rec.label} -> ${rec.outcome} status=${rec.http_status ?? '-'} load=${rec.load_ms ?? rec.total_ms ?? '-'}ms fails=${rec.failed_responses}/${rec.request_failures}`);
    result.navigations.push(rec);
  }
  const okCount = result.navigations.filter((n) => n.outcome === 'ok').length;
  console.log(`[${EXIT}] navigations ok: ${okCount}/${result.navigations.length}`);

  result.speedtests = [];
  for (const label of ['run1', 'run2']) {
    console.log(`[${EXIT}] speedtest ${label} (down ${DOWNLOAD_SECS}s / up ${UPLOAD_SECS}s)...`);
    const st = await runSpeedtest(page, label);
    console.log(`[${EXIT}] ${label}: down=${st.download.mbps ?? st.download.error} up=${st.upload.mbps ?? st.upload.error} lat=${st.latency.avg_ms ?? st.latency.error}`);
    result.speedtests.push(st);
    await sleep(3000);
  }
} catch (e) {
  result.fatal_error = String(e);
  console.error(`[${EXIT}] FATAL`, e);
} finally {
  try { await page.close(); } catch {}
  try { await browser.disconnect(); } catch {}
}

result.ping = await Promise.all(pings.map((p) => p.stop()));
console.log(`[${EXIT}] ping 1.1.1.1: avg=${result.ping[0].avg_ms}ms loss=${result.ping[0].loss_pct}%`);
result.ended_at = nowISO();
result.wall_secs = Math.round((Date.parse(result.ended_at) - Date.parse(result.started_at)) / 1000);

writeFileSync(`${OUTDIR}/${EXIT}.json`, JSON.stringify(result, null, 2));
console.log(`[${EXIT}] wrote ${OUTDIR}/${EXIT}.json (wall ${result.wall_secs}s)`);
process.exit(0);
