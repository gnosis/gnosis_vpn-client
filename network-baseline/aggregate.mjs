// Build summary.csv from per-exit JSONs in OUTDIR and print a table.
import { readFileSync, writeFileSync, readdirSync } from "fs";

const OUTDIR = process.env.OUTDIR || ".";
// Only the per-exit records: the same directory also holds meta.json and the
// <EXIT>.nerdstats.json snapshots, which would otherwise land as blank rows.
const files = readdirSync(OUTDIR).filter(
  (f) =>
    f.endsWith(".json") && f !== "meta.json" && !f.endsWith(".nerdstats.json"),
);
const rows = [];

const cols = [
  "exit",
  "egress_loc",
  "egress_match",
  "colo",
  "nav_ok",
  "nav_total",
  "nav_fail_resp",
  "avg_load_ms",
  "crawl_ok",
  "crawl_avg_ms",
  "yt_video",
  "yt_res",
  "ping1111_avg_ms",
  "ping1111_loss",
  "gw_avg_ms",
  "dl1_mbps",
  "dl2_mbps",
  "ul1_mbps",
  "ul2_mbps",
  "st_lat_avg_ms",
  "st_jitter_ms",
  "wall_secs",
];

for (const f of files) {
  const j = JSON.parse(readFileSync(`${OUTDIR}/${f}`, "utf8"));
  const navs = j.navigations || [];
  const ok = navs.filter((n) => n.outcome === "ok");
  const avgLoad = ok.length
    ? Math.round(ok.reduce((a, n) => a + (n.load_ms || 0), 0) / ok.length)
    : "";
  const failResp = navs.reduce(
    (a, n) => a + (n.failed_responses || 0) + (n.request_failures || 0),
    0,
  );
  const st = j.speedtests || [];
  const p1111 = (j.ping || []).find((p) => p.target === "1.1.1.1") || {};
  const pgw = (j.ping || []).find((p) => p.target === "10.128.0.1") || {};
  const hops = j.crawl || [];
  const hopsOk = hops.filter((h) => h.outcome === "ok");
  const watch = navs.find((n) => n.video);
  rows.push({
    exit: j.exit,
    egress_loc: j.egress?.loc ?? "",
    egress_match: j.egress_match,
    colo: j.egress?.colo ?? "",
    nav_ok: ok.length,
    nav_total: navs.length,
    nav_fail_resp: failResp,
    avg_load_ms: avgLoad,
    crawl_ok: hops.length ? `${hopsOk.length}/${hops.length}` : "",
    crawl_avg_ms: hopsOk.length
      ? Math.round(hopsOk.reduce((a, h) => a + h.ms, 0) / hopsOk.length)
      : "",
    yt_video: j.youtube_video?.name ?? "",
    yt_res: watch?.video?.observed?.height
      ? `${watch.video.observed.height}p`
      : "",
    ping1111_avg_ms: p1111.avg_ms ?? "",
    ping1111_loss: p1111.loss_pct ?? "",
    gw_avg_ms: pgw.avg_ms ?? "",
    dl1_mbps: st[0]?.download?.mbps ?? "",
    dl2_mbps: st[1]?.download?.mbps ?? "",
    ul1_mbps: st[0]?.upload?.mbps ?? "",
    ul2_mbps: st[1]?.upload?.mbps ?? "",
    st_lat_avg_ms: st[0]?.latency?.avg_ms ?? "",
    st_jitter_ms: st[0]?.latency?.jitter_ms ?? "",
    wall_secs: j.wall_secs ?? "",
  });
}

// preserve intended exit order if present
const order = (process.env.EXIT_ORDER || "").split(",").filter(Boolean);
if (order.length)
  rows.sort((a, b) => order.indexOf(a.exit) - order.indexOf(b.exit));

const csv = [cols.join(",")]
  .concat(rows.map((r) => cols.map((c) => r[c]).join(",")))
  .join("\n");
// The file is what before/after runs get diffed against, so it stays; stdout is for
// eyeballing the run as it finishes and for piping straight into another tool.
writeFileSync(`${OUTDIR}/summary.csv`, csv + "\n");
console.log(`wrote ${OUTDIR}/summary.csv (${rows.length} exits)\n`);

// pretty table
const widths = cols.map((c) =>
  Math.max(c.length, ...rows.map((r) => String(r[c]).length)),
);
const line = (vals) =>
  vals.map((v, i) => String(v).padEnd(widths[i])).join("  ");
console.log(line(cols));
console.log(widths.map((w) => "-".repeat(w)).join("  "));
for (const r of rows) console.log(line(cols.map((c) => r[c])));

console.log("\n--- summary.csv ---");
process.stdout.write(csv + "\n");
