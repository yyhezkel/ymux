import { createSignal, createMemo, createEffect, on, onCleanup, For, Show } from "solid-js";
import { createNarrow } from "./useNarrow";
import { backend } from "./backend";
import { IconClipboard, IconRefresh, IconTerminal } from "./icons";
import { t, currentLanguage } from "./i18n";
import { fmtBytes, fmtBps, fmtPct, fmtSpan } from "./insightsFmt";
import { createLogger } from "./logger";
import { buildAnalyticsReport } from "./insightsReport";
import type { AnPoint, AnReport } from "./insightsReport";
import { copyText } from "./clipboardText";
import { buildCommands } from "./insightsCommands";

// InsightsAnalytics — the Monitor's Analytics tab: what the server has been
// DOING, as opposed to the Metrics tab's what-it-is-doing-right-now.
//
// The daemon already stored the answer and nobody read it: the sampler writes
// `samples` / `disk_samples` / `docker_samples` to SQLite on every 5s tick and
// sweeps to a 7-day window. This tab is a thin view over the `/analytics`
// endpoint that rolls those three tables up in SQL.
//
// Three rules this panel keeps on purpose:
//  * No polling. This is an analysis screen, not a monitor — it loads when the
//    tab opens and when Refresh is pressed. The Metrics tab owns the live view
//    (and even there auto-refresh is opt-in). Each fetch is a curl over the
//    workspace SSH session; a background poll here would be rude.
//  * All aggregation server-side, one round trip. Never pull raw rows and sum
//    them here — a 7-day window is ~120k samples.
//  * Colour means status, not category. Every bar is the accent colour because
//    a bar encodes MAGNITUDE; red/amber are reserved for "this is a problem"
//    (disk filling up, a container that keeps dying). Keeps it from turning
//    into a rainbow where nothing stands out.

const log = createLogger("MONITOR");

type RangeId = "1h" | "6h" | "24h" | "7d";
const RANGES: { id: RangeId; seconds: number }[] = [
  { id: "1h", seconds: 3600 },
  { id: "6h", seconds: 6 * 3600 },
  { id: "24h", seconds: 24 * 3600 },
  { id: "7d", seconds: 7 * 86400 },
];

type MetricId = "cpu" | "mem" | "load" | "rx" | "tx";
interface MetricDef {
  id: MetricId;
  labelKey: string;
  pick: (p: AnPoint) => number;
  fmt: (v: number) => string;
  /** Percent metrics keep a fixed 0-100 scale so two ranges stay comparable. */
  fixedMax?: number;
}
const METRICS: MetricDef[] = [
  { id: "cpu", labelKey: "insights.an.metric.cpu", pick: (p) => p.cpu, fmt: fmtPct, fixedMax: 100 },
  { id: "mem", labelKey: "insights.an.metric.mem", pick: (p) => p.mem_pct, fmt: fmtPct, fixedMax: 100 },
  { id: "load", labelKey: "insights.an.metric.load", pick: (p) => p.load, fmt: (v) => v.toFixed(2) },
  { id: "rx", labelKey: "insights.an.metric.rx", pick: (p) => p.rx_bps, fmt: fmtBps },
  { id: "tx", labelKey: "insights.an.metric.tx", pick: (p) => p.tx_bps, fmt: fmtBps },
];

// Sparkline geometry. viewBox units — the <svg> itself is width:100%, so this
// is responsive with no resize listener and no charting library.
const W = 800;
const H = 130;
const PAD = 10;

/** Disk above this share of capacity is called out in amber. */
const DISK_WARN_PCT = 85;
/** A container running for less than this share of the window is called out. */
const UPTIME_WARN_PCT = 95;

interface Props {
  workspaceId?: string;
  /** Used to title the copyable report; the panel header shows it separately. */
  workspaceName?: string;
  /** Chooses which paths the copyable command block names. */
  local?: boolean;
}

export function InsightsAnalytics(p: Props) {
  // Panel width, NOT viewport width: this panel lives in a drawer or a float
  // that is routinely 440px wide on a 2560px monitor, so a media query would
  // be measuring the wrong box. Below the threshold the secondary table
  // columns drop out (the tables still scroll — this just stops a 6-column
  // table from forcing that scroll on every row).
  const narrow = createNarrow(560);
  const [range, setRange] = createSignal<RangeId>("24h");
  const [metric, setMetric] = createSignal<MetricId>("cpu");
  const [rep, setRep] = createSignal<AnReport | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [hover, setHover] = createSignal<number | null>(null);
  // null = idle, otherwise which button landed ("report" / "cmd" / "fail").
  const [copied, setCopied] = createSignal<string | null>(null);
  let svgRef: SVGSVGElement | undefined;
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(copiedTimer));

  const rangeSeconds = () => RANGES.find((r) => r.id === range())?.seconds ?? 86400;

  const load = async () => {
    if (!p.workspaceId || loading()) return;
    setLoading(true);
    setErr(null);
    setHover(null);
    const since = Math.floor(Date.now() / 1000) - rangeSeconds();
    try {
      const raw = await backend.call<string>("insights_fetch", {
        workspaceId: p.workspaceId,
        path: `/analytics?since=${since}&points=120`,
      });
      const parsed = JSON.parse(raw) as AnReport;
      setRep(parsed);
      log.debug(
        `analytics range=${range()} buckets=${parsed.series?.length ?? 0} samples=${parsed.totals?.samples ?? 0}`,
      );
    } catch (e) {
      const msg = String(e instanceof Error ? e.message : e);
      setErr(msg);
      setRep(null);
    } finally {
      setLoading(false);
    }
  };

  // Load on mount and whenever the range changes. `on([...])` pins the
  // dependencies so nothing the fetch itself writes can retrigger this —
  // the same self-retriggering trap the Claude tab documents.
  createEffect(
    on(
      () => [range(), p.workspaceId] as const,
      () => void load(),
    ),
  );

  // ── Derived ──────────────────────────────────────────────────────────

  /** The local workspace shim answers with a marker instead of a report. */
  const localOnly = () => rep()?.unavailable === "local";
  /** A daemon predating the endpoint has no route for it — curl gets a 404. */
  const oldDaemon = () => /HTTP 40[34]/.test(err() ?? "");
  const series = () => rep()?.series ?? [];
  const hasData = () => !localOnly() && series().length > 0;

  const activeMetric = () => METRICS.find((m) => m.id === metric()) ?? METRICS[0];

  const values = createMemo(() => series().map((pt) => activeMetric().pick(pt)));

  const chartMax = createMemo(() => {
    const fixed = activeMetric().fixedMax;
    if (fixed) return fixed;
    // 0.01 floor: an all-zero window (fresh daemon, idle box) must not divide
    // by zero and collapse the chart onto the baseline. 1.1 leaves headroom
    // so the peak isn't glued to the top edge.
    return Math.max(Math.max(...values(), 0), 0.01) * 1.1;
  });

  const xAt = (i: number) => PAD + (i * (W - 2 * PAD)) / Math.max(1, series().length - 1);
  const yAt = (v: number) => H - PAD - (v / chartMax()) * (H - 2 * PAD);

  const linePts = createMemo(() =>
    values()
      .map((v, i) => `${xAt(i).toFixed(1)},${yAt(v).toFixed(1)}`)
      .join(" "),
  );
  const areaPts = createMemo(() => {
    const pts = linePts();
    if (!pts) return "";
    return `${PAD},${H - PAD} ${pts} ${xAt(series().length - 1).toFixed(1)},${H - PAD}`;
  });

  const timeFmt = createMemo(() => {
    const opts: Intl.DateTimeFormatOptions = { hour: "2-digit", minute: "2-digit" };
    // Past a day the clock alone is ambiguous — add the date.
    if (rangeSeconds() > 86400) {
      opts.day = "2-digit";
      opts.month = "2-digit";
    }
    return new Intl.DateTimeFormat(currentLanguage(), opts);
  });
  const fmtTime = (unix: number) => timeFmt().format(new Date(unix * 1000));

  const periodFmt = createMemo(() => {
    const daily = (rep()?.period_s ?? 3600) >= 86400;
    const opts: Intl.DateTimeFormatOptions = daily
      ? { day: "2-digit", month: "2-digit" }
      : { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" };
    return new Intl.DateTimeFormat(currentLanguage(), opts);
  });
  const fmtPeriod = (unix: number) => periodFmt().format(new Date(unix * 1000));

  /** Coverage: how much of the requested window actually has samples. */
  const coverage = () => {
    const r = rep();
    if (!r || !r.totals.samples) return 0;
    const span = r.until - r.since;
    if (span <= 0) return 0;
    return Math.min(100, ((r.totals.last_ts - r.totals.first_ts) / span) * 100);
  };

  // ── Chart interaction ────────────────────────────────────────────────

  // clientX → viewBox units → nearest bucket index. getBoundingClientRect is
  // what makes this work at any rendered width without a resize listener.
  const onMove = (e: MouseEvent) => {
    const n = series().length;
    if (!svgRef || n === 0) return;
    const box = svgRef.getBoundingClientRect();
    if (box.width === 0) return;
    const vx = ((e.clientX - box.left) / box.width) * W;
    const step = (W - 2 * PAD) / Math.max(1, n - 1);
    const idx = Math.round((vx - PAD) / step);
    setHover(Math.min(n - 1, Math.max(0, idx)));
  };

  const chartAriaLabel = () => {
    const r = rep();
    if (!r) return "";
    return t("insights.an.chart_desc", {
      metric: t(activeMetric().labelKey),
      from: fmtTime(r.since),
      to: fmtTime(r.until),
      max: activeMetric().fmt(Math.max(...values(), 0)),
    });
  };

  // ── Copy for Claude ──────────────────────────────────────────────────

  // The point of storing seven days of metrics is being able to ask what
  // happened. This flattens exactly what is on screen into pasteable text —
  // WYSIWYG on purpose: the button copies the range you are looking at, it
  // does not quietly fetch a different one.
  const copyForClaude = async () => {
    const r = rep();
    if (!r || localOnly()) return;
    const ok = await copyText(
      buildAnalyticsReport(r, {
        intro: t("insights.an.copy_intro"),
        name: p.workspaceName ?? "",
        rangeLabel: t(`insights.an.range.${range()}`),
      }),
    );
    setCopied(ok ? "report" : "fail");
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => setCopied(null), 2500);
    log.debug(`analytics report copied=${ok} range=${range()}`);
  };

  // The report answers the questions we thought to ask; this hands over the
  // paths and the schema so an assistant with a shell can ask its own.
  const copyCommands = async () => {
    const ok = await copyText(
      buildCommands("metrics", { intro: t("insights.an.cmd_intro"), local: !!p.local }),
    );
    setCopied(ok ? "cmd" : "fail");
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => setCopied(null), 2500);
  };

  // ── Bits ─────────────────────────────────────────────────────────────

  const tile = (label: string, value: string, sub: string, warn = false) => (
    <div class="ins-metric" classList={{ "ins-an-warn": warn }}>
      <div class="ins-metric-head">
        <span>{label}</span>
        <b class="ins-an-num">{value}</b>
      </div>
      <div class="ins-metric-sub">{sub}</div>
    </div>
  );

  /** The bar lives INSIDE the cell — no separate chart to keep in sync. */
  const barCell = (frac: number, warn = false) => (
    <td class="ins-an-barcell">
      <div
        class="ins-an-bar"
        classList={{ warn }}
        // 1% floor: a near-zero row must still be visible as a row.
        style={{ width: `${Math.max(1, Math.min(100, frac * 100))}%` }}
      />
    </td>
  );

  return (
    <div class="ins-an" classList={{ narrow: narrow.narrow() }} ref={narrow.ref}>
      <div class="ins-an-filters">
        <div class="ins-an-seg" role="group" aria-label={t("insights.an.range")}>
          <For each={RANGES}>
            {(r) => (
              <button
                class={range() === r.id ? "active" : ""}
                aria-pressed={range() === r.id}
                onClick={() => setRange(r.id)}
              >
                {t(`insights.an.range.${r.id}`)}
              </button>
            )}
          </For>
        </div>
        <div class="ins-an-actions">
          <button
            class="ins-an-copy"
            disabled={!hasData()}
            title={t("insights.an.copy_hint")}
            onClick={() => void copyForClaude()}
          >
            <IconClipboard />
            <span>{copied() === "report" ? t("insights.an.copied") : t("insights.an.copy")}</span>
          </button>
          <button
            class="ins-an-copy"
            title={t("insights.an.cmd_hint")}
            onClick={() => void copyCommands()}
          >
            <IconTerminal />
            <span>{copied() === "cmd" ? t("insights.an.copied") : t("insights.cc.cmd")}</span>
          </button>
          <button class="ins-refresh" onClick={() => void load()} title={t("insights.refresh")}>
            {loading() ? "…" : <IconRefresh />}
          </button>
        </div>
      </div>

      <Show when={copied() === "fail"}>
        <div class="settings-hint">{t("insights.an.copy_failed")}</div>
      </Show>

      <Show when={err()}>
        <div class="ins-docker-err">
          <div class="ins-docker-err-msg">✗ {err()}</div>
          <Show when={oldDaemon()}>
            <div class="ins-docker-err-hint">{t("insights.an.old_daemon")}</div>
          </Show>
        </div>
      </Show>

      <Show when={localOnly()}>
        <div class="ins-docker-err">
          <div class="ins-docker-err-msg">{t("insights.an.local_title")}</div>
          <div class="ins-docker-err-hint">{t("insights.an.local_hint")}</div>
        </div>
      </Show>

      <Show
        when={hasData() ? rep() : null}
        fallback={
          <Show when={!err() && !localOnly()}>
            <div class="settings-hint">
              {loading() ? t("insights.logs.loading") : t("insights.an.empty")}
            </div>
          </Show>
        }
      >
        {(r) => (
          <>
              {/* ── Stat tiles ───────────────────────────────────────── */}
              <div class="ins-metrics">
                {tile(
                  t("insights.an.tile.cpu"),
                  fmtPct(r().totals.cpu_avg),
                  t("insights.an.tile.cpu_sub", { peak: fmtPct(r().totals.cpu_max) }),
                )}
                {tile(
                  t("insights.an.tile.busy"),
                  fmtPct(r().totals.busy_pct),
                  t("insights.an.tile.busy_sub"),
                  r().totals.busy_pct >= 25,
                )}
                {tile(
                  t("insights.an.tile.mem"),
                  fmtPct(r().totals.mem_pct_avg),
                  t("insights.an.tile.mem_sub", {
                    peak: fmtBytes(r().totals.mem_used_max),
                    total: fmtBytes(r().totals.mem_total),
                  }),
                )}
                {tile(
                  t("insights.an.tile.load"),
                  r().totals.load_avg.toFixed(2),
                  t("insights.an.tile.load_sub", { peak: r().totals.load_max.toFixed(2) }),
                )}
                {tile(
                  t("insights.an.tile.net"),
                  `↓${fmtBytes(r().totals.rx_bytes)} ↑${fmtBytes(r().totals.tx_bytes)}`,
                  t("insights.an.tile.net_sub", {
                    rx: fmtBps(r().totals.rx_avg_bps),
                    tx: fmtBps(r().totals.tx_avg_bps),
                  }),
                )}
                {tile(
                  t("insights.an.tile.samples"),
                  String(r().totals.samples),
                  t("insights.an.tile.samples_sub", {
                    span: fmtSpan(r().totals.last_ts - r().totals.first_ts),
                    pct: fmtPct(coverage()),
                  }),
                  coverage() < 50,
                )}
              </div>

              {/* ── Sparkline ────────────────────────────────────────── */}
              <h4 class="ins-h4">{t("insights.an.trend")}</h4>
              <div class="ins-an-seg ins-an-metrics" role="group" aria-label={t("insights.an.metric")}>
                <For each={METRICS}>
                  {(m) => (
                    <button
                      class={metric() === m.id ? "active" : ""}
                      aria-pressed={metric() === m.id}
                      onClick={() => setMetric(m.id)}
                    >
                      {t(m.labelKey)}
                    </button>
                  )}
                </For>
              </div>

              <div class="ins-an-chart">
                <svg
                  ref={svgRef}
                  viewBox={`0 0 ${W} ${H}`}
                  preserveAspectRatio="none"
                  role="img"
                  aria-label={chartAriaLabel()}
                  onMouseMove={onMove}
                  onMouseLeave={() => setHover(null)}
                >
                  <polygon class="ins-an-area" points={areaPts()} />
                  <polyline class="ins-an-line" points={linePts()} />
                  <Show when={hover() !== null && series()[hover()!]}>
                    <line
                      class="ins-an-cursor"
                      x1={xAt(hover()!)}
                      x2={xAt(hover()!)}
                      y1={PAD}
                      y2={H - PAD}
                    />
                    <circle
                      class="ins-an-dot"
                      cx={xAt(hover()!)}
                      cy={yAt(values()[hover()!])}
                      r="4"
                    />
                  </Show>
                </svg>
                <Show when={hover() !== null && series()[hover()!]}>
                  {(pt) => (
                    <div
                      class="ins-an-tip"
                      style={{
                        // Flip the tooltip to the other side past the midpoint
                        // so it never runs off the panel edge.
                        left: `${(xAt(hover()!) / W) * 100}%`,
                        transform:
                          xAt(hover()!) > W / 2 ? "translateX(-100%)" : "translateX(0)",
                      }}
                    >
                      <b>{activeMetric().fmt(values()[hover()!])}</b>
                      <span>{fmtTime(pt().t)}</span>
                    </div>
                  )}
                </Show>
              </div>
              <div class="ins-an-axis">
                <span>{fmtTime(r().since)}</span>
                <span>{t("insights.an.bucket", { span: fmtSpan(r().bucket_s) })}</span>
                <span>{fmtTime(r().until)}</span>
              </div>

              {/* A sparkline alone is unreadable to a screen reader — the same
                  numbers have to exist as text. */}
              <details class="ins-an-details">
                <summary>{t("insights.an.as_table")}</summary>
                <div class="ins-an-scroll">
                  <table class="ins-an-table">
                    <thead>
                      <tr>
                        <th>{t("insights.an.col.time")}</th>
                        <th>{t(activeMetric().labelKey)}</th>
                        <th>{t("insights.an.col.samples")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={series()}>
                        {(pt) => (
                          <tr>
                            <td class="ins-an-num">{fmtTime(pt.t)}</td>
                            <td class="ins-an-num">{activeMetric().fmt(activeMetric().pick(pt))}</td>
                            <td class="ins-an-num">{pt.n}</td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </details>

              {/* ── Rollup: by period ────────────────────────────────── */}
              <Show when={r().by_period.length > 0}>
                <h4 class="ins-h4">
                  {r().period_s >= 86400 ? t("insights.an.by_day") : t("insights.an.by_hour")}
                </h4>
                <div class="ins-an-scroll">
                  <table class="ins-an-table">
                    <thead>
                      <tr>
                        <th>{t("insights.an.col.period")}</th>
                        <th>{t("insights.an.col.cpu_avg")}</th>
                        <th aria-label={t("insights.an.col.share")} />
                        <th>{t("insights.an.col.cpu_peak")}</th>
                        <th class="ins-an-opt ins-an-dim">{t("insights.an.col.ram_avg")}</th>
                        <th class="ins-an-opt ins-an-dim">{t("insights.an.col.load")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={r().by_period}>
                        {(row) => (
                          <tr>
                            <td class="ins-an-num">{fmtPeriod(row.t)}</td>
                            <td class="ins-an-num">{fmtPct(row.cpu_avg)}</td>
                            {barCell(row.cpu_avg / 100)}
                            <td class="ins-an-num">{fmtPct(row.cpu_max)}</td>
                            <td class="ins-an-num ins-an-opt ins-an-dim">{fmtPct(row.mem_pct_avg)}</td>
                            <td class="ins-an-num ins-an-opt ins-an-dim">{row.load_avg.toFixed(2)}</td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>

              {/* ── Rollup: by disk ──────────────────────────────────── */}
              <Show when={r().by_disk.length > 0}>
                <h4 class="ins-h4">{t("insights.an.by_disk")}</h4>
                <div class="ins-an-scroll">
                  <table class="ins-an-table">
                    <thead>
                      <tr>
                        <th>{t("insights.an.col.mount")}</th>
                        <th>{t("insights.an.col.used")}</th>
                        <th aria-label={t("insights.an.col.share")} />
                        <th>{t("insights.an.col.growth")}</th>
                        <th class="ins-an-opt ins-an-dim">{t("insights.an.col.total")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={r().by_disk}>
                        {(d) => (
                          <tr>
                            <td class="ins-an-num">{d.mount}</td>
                            <td class="ins-an-num">{fmtPct(d.pct_last)}</td>
                            {barCell(d.pct_last / 100, d.pct_last >= DISK_WARN_PCT)}
                            <td
                              class="ins-an-num"
                              classList={{ "ins-an-warn-text": d.growth_bytes > 0 }}
                            >
                              {d.growth_bytes > 0 ? "+" : ""}
                              {fmtBytes(d.growth_bytes)}
                            </td>
                            <td class="ins-an-num ins-an-opt ins-an-dim">
                              {fmtBytes(d.used_last)} / {fmtBytes(d.total)}
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>

              {/* ── Rollup: by container ─────────────────────────────── */}
              <Show when={r().by_container.length > 0}>
                <h4 class="ins-h4">{t("insights.an.by_container")}</h4>
                <div class="ins-an-scroll">
                  <table class="ins-an-table">
                    <thead>
                      <tr>
                        <th>{t("insights.an.col.container")}</th>
                        <th>{t("insights.an.col.cpu_avg")}</th>
                        <th aria-label={t("insights.an.col.share")} />
                        <th>{t("insights.an.col.cpu_peak")}</th>
                        <th class="ins-an-opt ins-an-dim">{t("insights.an.col.ram_avg")}</th>
                        <th class="ins-an-opt ins-an-dim">{t("insights.an.col.uptime")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={r().by_container}>
                        {(c) => {
                          const top = Math.max(
                            ...r().by_container.map((x) => x.cpu_avg),
                            0.01,
                          );
                          return (
                            <tr>
                              <td class="ins-an-num">{c.name}</td>
                              <td class="ins-an-num">{fmtPct(c.cpu_avg)}</td>
                              {barCell(c.cpu_avg / top)}
                              <td class="ins-an-num">{fmtPct(c.cpu_max)}</td>
                              <td class="ins-an-num ins-an-opt ins-an-dim">{fmtBytes(c.mem_avg)}</td>
                              <td
                                class="ins-an-num"
                                classList={{
                                  "ins-an-warn-text": c.uptime_pct < UPTIME_WARN_PCT,
                                  "ins-an-dim": c.uptime_pct >= UPTIME_WARN_PCT,
                                  "ins-an-opt": true,
                                }}
                              >
                                {fmtPct(c.uptime_pct)}
                              </td>
                            </tr>
                          );
                        }}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>
          </>
        )}
      </Show>
    </div>
  );
}
