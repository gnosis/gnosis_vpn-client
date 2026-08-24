#!/usr/bin/env bash
# vpn-drain-report.sh - reads <run-dir>/runs.jsonl (from vpn-drain-tour.sh) and writes one self-contained <run-dir>/report.html: a cross-destination comparison table, outcome heatmap, latency/throughput trend lines and a funding drain timeline, plus per-destination histogram/round-data sections reachable via anchor links (kept in the same document, not linked sub-pages, so a browser's Print -> Save as PDF captures everything, aided by a small `beforeprint`/`afterprint` script that expands the collapsed sections and a `@media print` stylesheet that page-breaks between them); charts are inline SVG built by awk, no charting library.
# Required commands: bash, jq, awk. Run `./vpn-drain-report.sh --help` for options.
# Fetch this alongside vpn-smoke-test.sh/vpn-drain-tour.sh: see scripts/README.md.

set -euo pipefail

RUN_DIR=""
OUT_FILE=""
HIST_BINS=8
INSTALL_DEPS=0

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Usage: vpn-drain-report.sh --run-dir DIR [options]

Render report.html from a vpn-drain-tour.sh run directory.

Options:
      --run-dir DIR   Run directory containing runs.jsonl (required)
      --out FILE      Output path                  (default <run-dir>/report.html)
      --hist-bins N   Histogram bin count           (default 8)
      --install-deps  Install missing jq/awk via apt-get (Debian/Ubuntu) and continue
  -h, --help          Show this help
EOF
}

need_value() {
    if [ "$#" -lt 2 ] || [ "${2#-}" != "$2" ]; then
        die "option '$1' requires a value (see --help)"
    fi
}

parse_args() {
    while [ "$#" -gt 0 ]; do
        case "$1" in
        --run-dir | --out | --hist-bins)
            need_value "$@"
            ;;
        esac
        case "$1" in
        --run-dir)
            RUN_DIR="$2"
            shift 2
            ;;
        --out)
            OUT_FILE="$2"
            shift 2
            ;;
        --hist-bins)
            HIST_BINS="$2"
            shift 2
            ;;
        --install-deps)
            INSTALL_DEPS=1
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            printf 'Unknown option: %s\n\n' "$1" >&2
            usage >&2
            exit 2
            ;;
        esac
    done
}

# ---------------------------------------------------------------------------
# Embedded jq/awk programs (written to a scratch dir, not escaped one-liners)
# ---------------------------------------------------------------------------

write_programs() {
    cat >"$WORK/aggregate.jq" <<'JQ'
. as $rows
| ($rows | map(.round) | unique | sort) as $rounds
| ($rows | map(.destination_id) | unique | sort) as $dests
| {
    rounds: $rounds,
    destinations: $dests,
    per_destination: ($dests | map(. as $id |
        ($rows | map(select(.destination_id == $id))) as $r |
        ($r | map(select(.outcome=="connected"))) as $ok |
        ($ok | map(.metrics.gateway_ping_avg_ms) | map(select(.!=null))) as $lat |
        ($ok | map(.metrics.streaming_bps) | map(select(.!=null))) as $thr |
        ($ok | map(.metrics.packet_loss_pct) | map(select(.!=null))) as $loss |
        {
          id: $id,
          attempts: ($r|length),
          connected: ($ok|length),
          failed: ($r | map(select(.outcome=="failed")) | length),
          skipped: ($r | map(select(.outcome=="skipped")) | length),
          success_rate: (if ($r|length) > 0 then (($ok|length) / ($r|length)) else 0 end),
          reasons: ($r | map(select(.outcome!="connected") | (.reason // "unknown"))
                       | group_by(.) | map({reason: .[0], count: length}) | sort_by(-.count)),
          last_round: ($r | map(.round) | max),
          latencies: $lat,
          losses: $loss,
          throughputs: $thr,
          avg_latency: (if ($lat|length)>0 then (($lat|add)/($lat|length)) else null end),
          min_latency: (if ($lat|length)>0 then ($lat|min) else null end),
          max_latency: (if ($lat|length)>0 then ($lat|max) else null end),
          avg_throughput: (if ($thr|length)>0 then (($thr|add)/($thr|length)) else null end),
          min_throughput: (if ($thr|length)>0 then ($thr|min) else null end),
          max_throughput: (if ($thr|length)>0 then ($thr|max) else null end),
          avg_loss: (if ($loss|length)>0 then (($loss|add)/($loss|length)) else null end),
          last_capacity_used: ($r | sort_by(.round) | last | .selection.capacity_used),
          last_capacity_available: ($r | sort_by(.round) | last | .selection.capacity_available),
          series: ($r | sort_by(.round) | map({
              round: .round, outcome: .outcome,
              latency: .metrics.gateway_ping_avg_ms,
              throughput: .metrics.streaming_bps,
              loss: .metrics.packet_loss_pct,
              reason: .reason
          }))
        }
    ) | sort_by(-.success_rate, (.avg_latency // 999999))),
    heatmap: ($dests | map(. as $id | {
        id: $id,
        cells: ($rounds | map(. as $rnd |
            (($rows | map(select(.destination_id==$id and .round==$rnd)) | first)) as $cell |
            if $cell == null then {round: $rnd, outcome: "absent", latency: null}
            else {round: $rnd, outcome: $cell.outcome, latency: $cell.metrics.gateway_ping_avg_ms} end
        ))
    })),
    drain_timeline: (
      [ $rounds[] as $rnd
          | ($rows | map(select(.round==$rnd))) as $rr
          | {
              round: $rnd,
              traffic: ($rr | map(.funding_status_snapshot.traffic) | first // "Unknown"),
              gas: ($rr | map(.funding_status_snapshot.gas) | first // "Unknown"),
              connected_count: ($rr | map(select(.outcome=="connected")) | length)
            }
      ] | reduce .[] as $item
          ([]; . + [ $item + {
              cumulative_connected: ((if length == 0 then 0 else .[-1].cumulative_connected end) + $item.connected_count)
          } ])
    )
  }
JQ

    cat >"$WORK/table.jq" <<'JQ'
def htmlescape: gsub("&"; "&amp;") | gsub("<"; "&lt;") | gsub(">"; "&gt;");
def slug: gsub("[^A-Za-z0-9_-]"; "-");
def human_ms: if . == null then "-" else (((.*10|round)/10 | tostring) + " ms") end;
def human_pct: if . == null then "-" else (((.*100*10|round)/10 | tostring) + "%") end;
def human_rate:
  if . == null then "-"
  else (
    . as $v
    | if $v >= 1000000 then ((($v/1000000*10|round)/10 | tostring) + " MB/s")
      elif $v >= 1000 then ((($v/1000*10|round)/10 | tostring) + " kB/s")
      else (($v|round|tostring) + " B/s") end
  ) end;

.per_destination[] |
"<tr>" +
"<td><a href=\"#dest-" + (.id|slug) + "\">" + (.id|htmlescape) + "</a></td>" +
"<td>" + (.connected|tostring) + "/" + (.attempts|tostring) + "</td>" +
"<td>" + (.success_rate|human_pct) + "</td>" +
"<td>" + (.avg_latency|human_ms) + "</td>" +
"<td>" + (.min_latency|human_ms) + " - " + (.max_latency|human_ms) + "</td>" +
"<td>" + (.avg_throughput|human_rate) + "</td>" +
"<td>" + (.avg_loss|human_pct) + "</td>" +
"<td>" + ((.last_capacity_used // "-")|tostring) + "/" + ((.last_capacity_available // "-")|tostring) + "</td>" +
"<td>" + (.last_round|tostring) + "</td>" +
"<td>" + ([.reasons[] | (.reason|htmlescape) + " (" + (.count|tostring) + ")"] | join("; ")) + "</td>" +
"</tr>"
JQ

    cat >"$WORK/dest_rows.jq" <<'JQ'
def htmlescape: gsub("&"; "&amp;") | gsub("<"; "&lt;") | gsub(">"; "&gt;");
.per_destination[] | select(.id == $id) | .series[] |
"<tr><td>" + (.round|tostring) + "</td><td>" + (.outcome|htmlescape) + "</td><td>" +
((.latency // "-")|tostring) + "</td><td>" + ((.throughput // "-")|tostring) + "</td><td>" +
((.loss // "-")|tostring) + "</td><td>" + ((.reason // "-")|htmlescape) + "</td></tr>"
JQ

    cat >"$WORK/heatmap.awk" <<'AWK'
function htmlescape(s) {
    gsub(/&/, "\\&amp;", s)
    gsub(/</, "\\&lt;", s)
    gsub(/>/, "\\&gt;", s)
    return s
}
function slugify(s) {
    gsub(/[^A-Za-z0-9_-]/, "-", s)
    return s
}
function hexval(c) {
    return index("0123456789abcdef", tolower(c)) - 1
}
function hexpair(s, i) {
    return hexval(substr(s, i, 1)) * 16 + hexval(substr(s, i + 1, 1))
}
function lerp_color(c1, c2, f,   r1, g1, b1, r2, g2, b2, r, g, b) {
    r1 = hexpair(c1, 2); g1 = hexpair(c1, 4); b1 = hexpair(c1, 6)
    r2 = hexpair(c2, 2); g2 = hexpair(c2, 4); b2 = hexpair(c2, 6)
    r = int(r1 + (r2 - r1) * f + 0.5)
    g = int(g1 + (g2 - g1) * f + 0.5)
    b = int(b1 + (b2 - b1) * f + 0.5)
    return sprintf("#%02x%02x%02x", r, g, b)
}
BEGIN {
    FS = "\t"
    n_ids = split(ids, idarr, " ")
    n_rounds = split(rounds, roundarr, " ")
    minlat = ""
    maxlat = ""
    LAT_LOW = "#a7f3d0"
    LAT_HIGH = "#065f46"
}
{
    key = $1 SUBSEP $2
    outcome[key] = $3
    if ($4 != "") {
        lat[key] = $4
        if (minlat == "" || $4 < minlat) minlat = $4
        if (maxlat == "" || $4 > maxlat) maxlat = $4
    }
}
END {
    if (n_ids == 0 || n_rounds == 0) { print "<p class=\"chart-empty\">no data</p>"; exit }
    label_w = 90
    cell_w = 36
    cell_h = 26
    header_h = 20
    width = label_w + n_rounds * cell_w
    height = header_h + n_ids * cell_h
    printf "<svg viewBox=\"0 0 %d %d\" class=\"chart heatmap\" role=\"img\" aria-label=\"connection outcome by round\">\n", width, height

    for (r = 1; r <= n_rounds; r++) {
        x = label_w + (r - 1) * cell_w + cell_w / 2
        printf "<text x=\"%.1f\" y=\"%d\" class=\"heatmap-round-label\">%s</text>\n", x, header_h - 6, htmlescape(roundarr[r])
    }

    latrange = maxlat - minlat
    if (latrange == 0) latrange = 1

    for (i = 1; i <= n_ids; i++) {
        y = header_h + (i - 1) * cell_h
        printf "<text x=\"0\" y=\"%.1f\" class=\"heatmap-id-label\">%s</text>\n", y + cell_h / 2 + 4, htmlescape(idarr[i])
        for (r = 1; r <= n_rounds; r++) {
            key = idarr[i] SUBSEP roundarr[r]
            x = label_w + (r - 1) * cell_w
            o = (key in outcome) ? outcome[key] : "absent"
            cls = "cell-" slugify(o)
            title = htmlescape(o)
            if (o == "connected" && (key in lat)) {
                frac = (lat[key] - minlat) / latrange
                title = htmlescape(o) " - " lat[key] " ms"
                printf "<rect x=\"%.1f\" y=\"%.1f\" width=\"%d\" height=\"%d\" class=\"%s\" fill=\"%s\"><title>%s round %s: %s</title></rect>\n", \
                    x + 1, y + 1, cell_w - 2, cell_h - 2, cls, lerp_color(LAT_LOW, LAT_HIGH, frac), htmlescape(idarr[i]), htmlescape(roundarr[r]), title
            } else {
                printf "<rect x=\"%.1f\" y=\"%.1f\" width=\"%d\" height=\"%d\" class=\"%s\"><title>%s round %s: %s</title></rect>\n", \
                    x + 1, y + 1, cell_w - 2, cell_h - 2, cls, htmlescape(idarr[i]), htmlescape(roundarr[r]), title
            }
        }
    }
    printf "</svg>\n"
}
AWK

    cat >"$WORK/line.awk" <<'AWK'
function htmlescape(s) {
    gsub(/&/, "\\&amp;", s)
    gsub(/</, "\\&lt;", s)
    gsub(/>/, "\\&gt;", s)
    return s
}
BEGIN {
    FS = "\t"
    n_ids = split(ids, idarr, " ")
    n_rounds = split(rounds, roundarr, " ")
    for (i = 1; i <= n_ids; i++) idpos[idarr[i]] = i
    minr = roundarr[1] + 0
    maxr = roundarr[n_rounds] + 0
    split("#2563eb #dc2626 #16a34a #d97706 #7c3aed #0891b2 #db2777 #65a30d", palette, " ")
    k = 0
}
{
    if ($3 == "") next
    id = $1; r = $2 + 0; v = $3 + 0
    pts[id] = pts[id] r "," v ";"
    if (!seen[id]++) order[++k] = id
    if (minv == "" || v < minv) minv = v
    if (maxv == "" || v > maxv) maxv = v
}
END {
    if (k == 0) { print "<p class=\"chart-empty\">no data</p>"; exit }
    if (minv == maxv) { minv -= 1; maxv += 1 }
    pad_l = 40; pad_r = 10; pad_t = 10; pad_b = 24
    plotw = width - pad_l - pad_r
    ploth = height - pad_t - pad_b
    printf "<svg viewBox=\"0 0 %d %d\" class=\"chart linechart\" role=\"img\" aria-label=\"%s trend across rounds\">\n", width, height + 24, ylabel

    printf "<text x=\"2\" y=\"%d\" class=\"axis-label\">%s</text>\n", pad_t + 8, ylabel
    printf "<text x=\"2\" y=\"%d\" class=\"axis-label\">%.1f</text>\n", pad_t + ploth, minv
    printf "<line x1=\"%d\" y1=\"%d\" x2=\"%d\" y2=\"%d\" class=\"axis-line\"></line>\n", pad_l, pad_t + ploth, pad_l + plotw, pad_t + ploth

    for (ki = 1; ki <= k; ki++) {
        id = order[ki]
        n = split(pts[id], parts, ";")
        color = palette[((idpos[id] - 1) % 8) + 1]
        line = ""
        for (p = 1; p < n; p++) {
            split(parts[p], rv, ",")
            r = rv[1]; v = rv[2]
            x = pad_l + (maxr > minr ? (r - minr) / (maxr - minr) : 0.5) * plotw
            y = pad_t + ploth - ((v - minv) / (maxv - minv)) * ploth
            line = line sprintf("%.1f,%.1f ", x, y)
        }
        printf "<polyline points=\"%s\" class=\"trend-line\" style=\"stroke:%s\"></polyline>\n", line, color
    }

    ly = height + 16
    for (ki = 1; ki <= k; ki++) {
        id = order[ki]
        color = palette[((idpos[id] - 1) % 8) + 1]
        lx = pad_l + (ki - 1) * 80
        printf "<rect x=\"%d\" y=\"%d\" width=\"10\" height=\"10\" fill=\"%s\"></rect>\n", lx, ly - 9, color
        printf "<text x=\"%d\" y=\"%d\" class=\"legend-label\">%s</text>\n", lx + 14, ly, htmlescape(id)
    }
    printf "</svg>\n"
}
AWK

    cat >"$WORK/histogram.awk" <<'AWK'
{ v[++n] = $1 + 0 }
END {
    pad_l = 30; pad_b = 16
    plotw = width - pad_l
    ploth = height - pad_b
    if (n == 0) { print "<p class=\"chart-empty\">no data</p>"; exit }
    min = v[1]; max = v[1]
    for (i = 1; i <= n; i++) { if (v[i] < min) min = v[i]; if (v[i] > max) max = v[i] }
    range = max - min
    if (range == 0) range = 1
    b = bins + 0
    if (b < 1) b = 1
    for (i = 0; i < b; i++) count[i] = 0
    for (i = 1; i <= n; i++) {
        bin = int((v[i] - min) / range * b)
        if (bin >= b) bin = b - 1
        count[bin]++
    }
    maxcount = 0
    for (i = 0; i < b; i++) if (count[i] > maxcount) maxcount = count[i]
    barw = plotw / b

    printf "<svg viewBox=\"0 0 %d %d\" class=\"chart histogram\" role=\"img\" aria-label=\"%s distribution\">\n", width, height + 20, unit
    printf "<line x1=\"%d\" y1=\"%d\" x2=\"%d\" y2=\"%d\" class=\"axis-line\"></line>\n", pad_l, ploth, width, ploth
    for (i = 0; i < b; i++) {
        h = (maxcount > 0) ? (count[i] / maxcount) * (ploth - 4) : 0
        x = pad_l + i * barw
        y = ploth - h
        lo = min + i * range / b
        hi = min + (i + 1) * range / b
        printf "<rect x=\"%.1f\" y=\"%.1f\" width=\"%.1f\" height=\"%.1f\" class=\"hist-bar\"><title>%.1f-%.1f %s: %d sample(s)</title></rect>\n", \
            x + 1, y, barw - 2, h, lo, hi, unit, count[i]
    }
    printf "<text x=\"%d\" y=\"%d\" class=\"axis-label\">%.1f</text>\n", pad_l, height + 12, min
    printf "<text x=\"%d\" y=\"%d\" class=\"axis-label\" text-anchor=\"end\">%.1f %s</text>\n", width, height + 12, max, unit
    printf "</svg>\n"
}
AWK

    cat >"$WORK/timeline.awk" <<'AWK'
function htmlescape(s) {
    gsub(/&/, "\\&amp;", s)
    gsub(/</, "\\&lt;", s)
    gsub(/>/, "\\&gt;", s)
    return s
}
function funding_color(level) {
    if (level == "Good") return "#16a34a"
    if (level == "Low") return "#d97706"
    if (level == "Empty") return "#dc2626"
    return "#9ca3af"
}
BEGIN { FS = "\t" }
{
    n++
    round[n] = $1
    traffic[n] = $2
    cum[n] = $3 + 0
    if (n == 1 || cum[n] > maxcum) maxcum = cum[n]
}
END {
    if (n == 0) { print "<p class=\"chart-empty\">no data</p>"; exit }
    if (maxcum == 0) maxcum = 1
    pad_l = 10; pad_r = 10; pad_t = 10; pad_b = 34
    plotw = width - pad_l - pad_r
    ploth = height - pad_t - pad_b
    barw = plotw / n

    printf "<svg viewBox=\"0 0 %d %d\" class=\"chart timeline\" role=\"img\" aria-label=\"funding and connections across rounds\">\n", width, height

    line = ""
    for (i = 1; i <= n; i++) {
        x = pad_l + (i - 1) * barw
        printf "<rect x=\"%.1f\" y=\"%d\" width=\"%.1f\" height=\"14\" fill=\"%s\"><title>round %s funding: %s</title></rect>\n", \
            x, pad_t + ploth, barw - 1, funding_color(traffic[i]), htmlescape(round[i]), htmlescape(traffic[i])
        if ((i - 1) % int((n / 12) + 1) == 0) {
            printf "<text x=\"%.1f\" y=\"%d\" class=\"axis-label\">%s</text>\n", x, pad_t + ploth + 26, htmlescape(round[i])
        }
        cx = x + barw / 2
        cy = pad_t + ploth - (cum[i] / maxcum) * (ploth - 4)
        line = line sprintf("%.1f,%.1f ", cx, cy)
    }
    printf "<polyline points=\"%s\" class=\"trend-line\" style=\"stroke:#2563eb\"></polyline>\n", line
    printf "<text x=\"%d\" y=\"%d\" class=\"axis-label\">cumulative connections (max %d)</text>\n", pad_l, pad_t, maxcum
    printf "</svg>\n"
}
AWK
}

# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------

htmlescape_str() {
    printf '%s' "$1" | jq -Rr '. | gsub("&"; "&amp;") | gsub("<"; "&lt;") | gsub(">"; "&gt;")'
}

slugify() {
    printf '%s' "$1" | jq -Rr 'gsub("[^A-Za-z0-9_-]"; "-")'
}

render_comparison_table() {
    printf '<table class="comparison">\n<thead><tr>'
    printf '<th>Destination</th><th>Connected</th><th>Success</th><th>Avg latency</th><th>Latency range</th><th>Avg throughput</th><th>Avg loss</th><th>Capacity (used/avail)</th><th>Last round seen</th><th>Failure/skip reasons</th>'
    printf '</tr></thead>\n<tbody>\n'
    jq -r -f "$WORK/table.jq" "$AGGREGATE"
    printf '</tbody>\n</table>\n'
}

render_heatmap() {
    local ids rounds
    ids="$(jq -r '.per_destination[].id' "$AGGREGATE" | tr '\n' ' ')"
    rounds="$(jq -r '.rounds[]' "$AGGREGATE" | tr '\n' ' ')"
    jq -r '.heatmap[] as $d | $d.cells[] | [$d.id, .round, .outcome, (.latency // "")] | @tsv' "$AGGREGATE" |
        awk -v ids="$ids" -v rounds="$rounds" -f "$WORK/heatmap.awk"
}

render_trend() {
    local field="$1" ylabel="$2" ids rounds n_ids
    ids="$(jq -r '.per_destination[].id' "$AGGREGATE" | tr '\n' ' ')"
    rounds="$(jq -r '.rounds[]' "$AGGREGATE" | tr '\n' ' ')"
    n_ids="$(jq '.destinations | length' "$AGGREGATE")"
    if [ "$n_ids" -gt 8 ]; then
        printf '<p class="chart-note">Too many destinations (%s) to overlay legibly; see the comparison table above.</p>\n' "$n_ids"
        return
    fi
    jq -r --arg field "$field" '.per_destination[] as $d | $d.series[] | [$d.id, .round, ((.[$field]) // "")] | @tsv' "$AGGREGATE" |
        awk -v ids="$ids" -v rounds="$rounds" -v width=700 -v height=220 -v ylabel="$ylabel" -f "$WORK/line.awk"
}

render_timeline() {
    jq -r '.drain_timeline[] | [.round, .traffic, .cumulative_connected] | @tsv' "$AGGREGATE" |
        awk -v width=700 -v height=140 -f "$WORK/timeline.awk"
}

render_destination_sections() {
    local ids id id_esc id_slug
    ids="$(jq -r '.per_destination[].id' "$AGGREGATE")"
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        id_esc="$(htmlescape_str "$id")"
        id_slug="$(slugify "$id")"
        cat <<HTML
<details id="dest-$id_slug" class="dest-section">
<summary>$id_esc</summary>
<div class="dest-charts">
<figure>
HTML
        jq -r --arg id "$id" '.per_destination[] | select(.id == $id) | .latencies[]' "$AGGREGATE" |
            awk -v width=280 -v height=120 -v bins="$HIST_BINS" -v unit="ms" -f "$WORK/histogram.awk"
        printf '<figcaption>Gateway latency</figcaption></figure>\n<figure>\n'
        jq -r --arg id "$id" '.per_destination[] | select(.id == $id) | .throughputs[]' "$AGGREGATE" |
            awk -v width=280 -v height=120 -v bins="$HIST_BINS" -v unit="B/s" -f "$WORK/histogram.awk"
        printf '<figcaption>Streaming throughput</figcaption></figure>\n<figure>\n'
        jq -r --arg id "$id" '.per_destination[] | select(.id == $id) | .losses[]' "$AGGREGATE" |
            awk -v width=280 -v height=120 -v bins="$HIST_BINS" -v unit="%" -f "$WORK/histogram.awk"
        printf '<figcaption>Packet loss</figcaption></figure>\n</div>\n'
        printf '<table class="dest-rounds"><thead><tr><th>Round</th><th>Outcome</th><th>Latency (ms)</th><th>Throughput (B/s)</th><th>Loss (%%)</th><th>Reason</th></tr></thead><tbody>\n'
        jq -r --arg id "$id" -f "$WORK/dest_rows.jq" "$AGGREGATE"
        printf '</tbody></table>\n</details>\n'
    done <<<"$ids"
}

STYLE='
:root {
  --bg: #ffffff; --fg: #1f2937; --muted: #6b7280; --border: #e5e7eb;
  --accent: #2563eb; --row-alt: #f9fafb; --card: #f3f4f6;
}
@media (prefers-color-scheme: dark) {
  :root { --bg: #111827; --fg: #e5e7eb; --muted: #9ca3af; --border: #374151; --row-alt: #1f2937; --card: #1f2937; }
}
* { box-sizing: border-box; }
body { background: var(--bg); color: var(--fg); font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif; margin: 0; padding: 2rem; }
.container { max-width: 1100px; margin: 0 auto; }
h1, h2 { font-weight: 600; }
h1 { font-size: 1.5rem; }
h2 { font-size: 1.15rem; margin-top: 2.5rem; border-bottom: 1px solid var(--border); padding-bottom: 0.4rem; }
p.meta { color: var(--muted); font-size: 0.9rem; }
table { border-collapse: collapse; width: 100%; font-size: 0.85rem; margin: 1rem 0; }
th, td { text-align: left; padding: 0.4rem 0.6rem; border-bottom: 1px solid var(--border); }
tbody tr:nth-child(even) { background: var(--row-alt); }
a { color: var(--accent); }
.chart { max-width: 100%; height: auto; display: block; margin: 0.5rem 0; }
.axis-label { font-size: 9px; fill: var(--muted); }
.axis-line { stroke: var(--border); stroke-width: 1; }
.legend-label { font-size: 10px; fill: var(--fg); }
.heatmap-round-label, .heatmap-id-label { font-size: 10px; fill: var(--fg); text-anchor: middle; }
.heatmap-id-label { text-anchor: start; }
.cell-failed { fill: #ef4444; }
.cell-skipped { fill: #9ca3af; }
.cell-absent { fill: var(--border); }
.hist-bar { fill: var(--accent); }
.trend-line { fill: none; stroke-width: 2; }
.chart-empty, .chart-note { color: var(--muted); font-size: 0.85rem; font-style: italic; }
.dest-charts { display: flex; flex-wrap: wrap; gap: 1.5rem; }
figure { margin: 0; background: var(--card); padding: 0.5rem; border-radius: 6px; }
figcaption { font-size: 0.8rem; color: var(--muted); text-align: center; }
details.dest-section { border: 1px solid var(--border); border-radius: 6px; padding: 0.6rem 1rem; margin: 0.6rem 0; }
details.dest-section summary { cursor: pointer; font-weight: 600; }
footer { color: var(--muted); font-size: 0.8rem; margin-top: 2rem; }
@media print {
  details.dest-section { page-break-before: always; border: none; }
  details.dest-section summary::-webkit-details-marker { display: none; }
  details.dest-section summary { list-style: none; font-size: 1.1rem; }
}
'

render_report() {
    local generated_at total connected failed skipped
    generated_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    total="$(jq '[.per_destination[].attempts] | add // 0' "$AGGREGATE")"
    connected="$(jq '[.per_destination[].connected] | add // 0' "$AGGREGATE")"
    failed="$(jq '[.per_destination[].failed] | add // 0' "$AGGREGATE")"
    skipped="$(jq '[.per_destination[].skipped] | add // 0' "$AGGREGATE")"

    {
        printf '<!doctype html>\n<html><head><meta charset="utf-8">\n'
        printf '<title>Gnosis VPN drain tour report</title>\n'
        printf '<style>%s</style>\n</head><body>\n<div class="container">\n' "$STYLE"
        printf '<h1>Gnosis VPN drain tour report</h1>\n'
        printf '<p class="meta">Generated %s from %s. %s attempts: %s connected, %s failed, %s skipped.</p>\n' \
            "$generated_at" "$(htmlescape_str "$RUN_DIR")" "$total" "$connected" "$failed" "$skipped"
        if [ -f "$RUN_DIR/stop_reason.txt" ]; then
            printf '<p class="meta">Stopped: %s</p>\n' "$(htmlescape_str "$(cat "$RUN_DIR/stop_reason.txt")")"
        fi

        printf '<h2>Comparison</h2>\n'
        render_comparison_table

        printf '<h2>Outcome by round</h2>\n'
        render_heatmap

        printf '<h2>Latency trend</h2>\n'
        render_trend "latency" "Latency (ms)"

        printf '<h2>Throughput trend</h2>\n'
        render_trend "throughput" "Throughput (B/s)"

        printf '<h2>Funding drain timeline</h2>\n'
        render_timeline

        printf '<h2>Destination detail</h2>\n'
        render_destination_sections

        printf '<footer>Raw data: <code>%s</code>, <code>%s</code>. Open in a browser and use Print &rarr; Save as PDF to share; destination sections auto-expand for print and collapse back after.</footer>\n' \
            "$(htmlescape_str "$RUN_DIR/runs.jsonl")" "$(htmlescape_str "$RUN_DIR/metrics.csv")"
        printf '<script>\n'
        printf 'var closedBeforePrint = []\n'
        printf "window.addEventListener('beforeprint', function () {\n"
        printf "    closedBeforePrint = Array.prototype.slice.call(document.querySelectorAll('details.dest-section:not([open])'))\n"
        printf '    closedBeforePrint.forEach(function (d) { d.open = true })\n'
        printf '})\n'
        printf "window.addEventListener('afterprint', function () {\n"
        printf '    closedBeforePrint.forEach(function (d) { d.open = false })\n'
        printf '})\n'
        printf '</script>\n'
        printf '</div>\n</body></html>\n'
    } >"$OUT_FILE"
}

# check_deps <cmd> <apt-pkg> [<cmd> <apt-pkg> ...] -> with --install-deps, apt-get installs any missing command's package and returns; otherwise dies with the apt-get command to run.
check_deps() {
    local missing_cmds=() missing_pkgs=()
    while [ "$#" -gt 0 ]; do
        command -v "$1" >/dev/null 2>&1 || {
            missing_cmds+=("$1")
            missing_pkgs+=("$2")
        }
        shift 2
    done
    [ "${#missing_cmds[@]}" -eq 0 ] && return 0

    if [ "$INSTALL_DEPS" -eq 1 ]; then
        command -v apt-get >/dev/null 2>&1 || die "--install-deps needs apt-get, which was not found"
        printf 'Installing missing dependencies: %s\n' "${missing_pkgs[*]}"
        if [ "${EUID:-1}" -eq 0 ]; then
            apt-get update && apt-get install -y "${missing_pkgs[@]}"
        elif command -v sudo >/dev/null 2>&1; then
            sudo apt-get update && sudo apt-get install -y "${missing_pkgs[@]}"
        else
            die "--install-deps needs sudo (or re-run as root)"
        fi
        return 0
    fi

    if command -v apt-get >/dev/null 2>&1; then
        die "missing ${missing_cmds[*]}. On Debian/Ubuntu: sudo apt-get install -y ${missing_pkgs[*]} (or re-run with --install-deps)"
    fi
    die "missing ${missing_cmds[*]}"
}

main() {
    parse_args "$@"
    [ -n "$RUN_DIR" ] || die "--run-dir is required (see --help)"
    [ -f "$RUN_DIR/runs.jsonl" ] || die "$RUN_DIR/runs.jsonl not found"
    check_deps jq jq awk gawk

    OUT_FILE="${OUT_FILE:-$RUN_DIR/report.html}"
    AGGREGATE="$RUN_DIR/aggregate.json"

    WORK="$(mktemp -d)"
    trap 'rm -rf "$WORK"' EXIT

    write_programs
    jq -s -f "$WORK/aggregate.jq" "$RUN_DIR/runs.jsonl" >"$AGGREGATE"
    render_report

    printf 'wrote %s\n' "$OUT_FILE"
}

main "$@"
