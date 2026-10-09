import { createSignal, createMemo, createEffect, on, onCleanup, For, Show } from "solid-js";
import { backend } from "./backend";
import { createNarrow } from "./useNarrow";
import { IconClipboard, IconRefresh, IconTerminal } from "./icons";
import { t, currentLanguage } from "./i18n";
import { fmtSpan } from "./insightsFmt";
import { copyText } from "./clipboardText";
import { createLogger } from "./logger";
import { buildClaudeUsageReport, type ClaudeUsageReport, type ClaudeTokenCounts } from "./insightsReport";
import { buildCommands } from "./insightsCommands";
import {
  blendedRates,
  cacheHitRatio,
  costAtBlended,
  costOf,
  fmtTokens,
  fmtUsd,
  PRICING_AS_OF,
  type BlendedRates,
} from "./claudePricing";

// InsightsClaudeCost — what Claude Code actually spent, from the transcripts it
// already writes. Sits under the quota bars in the Monitor's Claude tab: those
// answer "how much of my allowance is left", this answers "where did it go".
//
// The cost column is an ESTIMATE and the UI says so in three places, because
// the failure mode is somebody reading it as a bill. Claude Code on a
// subscription is not billed per token; these are API list prices applied to
// real token counts. That makes the number right for comparing projects,
// models and sessions against each other, and wrong for predicting a charge.
//
// Same house rules as the Analytics tab: no polling, one round trip, server
// does the aggregation, one accent colour for every bar.

const log = createLogger("MONITOR");

type RangeId = "24h" | "7d" | "30d";
const RANGES: { id: RangeId; seconds: number }[] = [
  { id: "24h", seconds: 24 * 3600 },
  { id: "7d", seconds: 7 * 86400 },
  { id: "30d", seconds: 30 * 86400 },
];

interface DayRow extends ClaudeTokenCounts {
  t: number;
}

interface Props {
  workspaceId?: string;
  workspaceName?: string;
  /** Local workspaces read the desktop's own ~/.claude; remote go over SSH. */
  local?: boolean;
}

export function InsightsClaudeCost(p: Props) {
  const narrow = createNarrow(560);
  const [range, setRange] = createSignal<RangeId>("7d");
  const [rep, setRep] = createSignal<ClaudeUsageReport | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [copied, setCopied] = createSignal<string | null>(null);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(copiedTimer));

  const rangeSeconds = () => RANGES.find((r) => r.id === range())?.seconds ?? 7 * 86400;

  const load = async () => {
    if (!p.workspaceId || loading()) return;
    setLoading(true);
    setErr(null);
    const since = Math.floor(Date.now() / 1000) - rangeSeconds();
    try {
      const raw = await backend.call<string>("insights_fetch", {
        workspaceId: p.workspaceId,
        path: `/claude-usage?since=${since}`,
      });
      const parsed = JSON.parse(raw) as ClaudeUsageReport;
      setRep(parsed);
      // Rule #1: counts only. Never a project path or a session id.
      log.debug(
        `claude usage range=${range()} files=${parsed.scanned_files} calls=${parsed.totals?.calls ?? 0}`,
      );
    } catch (e) {
      setErr(String(e instanceof Error ? e.message : e));
      setRep(null);
    } finally {
      setLoading(false);
    }
  };

  createEffect(
    on(
      () => [range(), p.workspaceId] as const,
      () => void load(),
    ),
  );

  const oldDaemon = () => /HTTP 40[34]/.test(err() ?? "");
  const hasData = () => (rep()?.totals?.calls ?? 0) > 0;

  // ── Pricing ──────────────────────────────────────────────────────────
  //
  // Only `by_model` knows its model. Everything else (a day, a project, a
  // session) can mix several, so those rows are priced with a token-weighted
  // blend of the models actually seen in this window, and marked with a "≈".

  const rates = createMemo<BlendedRates>(() => {
    const r = rep();
    const at = r?.until ?? Math.floor(Date.now() / 1000);
    return blendedRates(r?.by_model ?? [], at);
  });

  const totalCost = () => costAtBlended(rep()?.totals ?? emptyTokens(), rates());
  const blendedOf = (tok: ClaudeTokenCounts) => costAtBlended(tok, rates());

  /** True when at least one model in the window has no published price. */
  const anyUnpriced = () =>
    (rep()?.by_model ?? []).some(
      (m) => !costOf(m, m.key, m.speed, m.ended || rep()!.until).priced,
    );

  // Local days, rolled up from the server's hourly buckets. Doing it here
  // rather than server-side is the only way a day boundary lands where the
  // reader's clock says it does.
  const byDay = createMemo<DayRow[]>(() => {
    const acc = new Map<number, DayRow>();
    for (const b of rep()?.series ?? []) {
      const d = new Date(b.t * 1000);
      d.setHours(0, 0, 0, 0);
      const key = Math.floor(d.getTime() / 1000);
      const cur = acc.get(key) ?? { t: key, ...emptyTokens(), calls: 0 };
      cur.calls += b.calls;
      cur.in_tokens += b.in_tokens;
      cur.out_tokens += b.out_tokens;
      cur.cache_read += b.cache_read;
      cur.cache_write += b.cache_write;
      cur.cache_write_5m += b.cache_write_5m;
      cur.cache_write_1h += b.cache_write_1h;
      acc.set(key, cur);
    }
    return [...acc.values()].sort((a, b) => b.t - a.t);
  });

  const dayFmt = createMemo(
    () =>
      new Intl.DateTimeFormat(currentLanguage(), {
        weekday: "short",
        day: "2-digit",
        month: "2-digit",
      }),
  );
  const stampFmt = createMemo(
    () =>
      new Intl.DateTimeFormat(currentLanguage(), {
        day: "2-digit",
        month: "2-digit",
        hour: "2-digit",
        minute: "2-digit",
      }),
  );

  // ── Copy actions ─────────────────────────────────────────────────────

  const flash = (what: string) => {
    setCopied(what);
    clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => setCopied(null), 2500);
  };

  const copyReport = async () => {
    const r = rep();
    if (!r) return;
    const ok = await copyText(
      buildClaudeUsageReport(r, {
        intro: t("insights.cc.copy_intro"),
        name: p.workspaceName ?? "",
        rangeLabel: t(`insights.cc.range.${range()}`),
        pricingAsOf: PRICING_AS_OF,
        totalCost: fmtUsd(totalCost()),
        costOf: (tok, model, speed, at) =>
          model
            ? fmtUsd(costOf(tok, model, speed, at ?? r.until).usd)
            : `~${fmtUsd(blendedOf(tok))}`,
      }),
    );
    flash(ok ? "report" : "fail");
  };

  const copyCommands = async () => {
    const ok = await copyText(
      buildCommands("claude", { intro: t("insights.cc.cmd_intro"), local: !!p.local }),
    );
    flash(ok ? "cmd" : "fail");
  };

  // ── Bits ─────────────────────────────────────────────────────────────

  const tile = (label: string, value: string, sub: string) => (
    <div class="ins-metric">
      <div class="ins-metric-head">
        <span>{label}</span>
        <b class="ins-an-num">{value}</b>
      </div>
      <div class="ins-metric-sub">{sub}</div>
    </div>
  );

  const barCell = (frac: number) => (
    <td class="ins-an-barcell">
      <div class="ins-an-bar" style={{ width: `${Math.max(1, Math.min(100, frac * 100))}%` }} />
    </td>
  );

  /** Rollup table shared by day / model / project / session. */
  const rollup = (
    title: string,
    keyHeader: string,
    rows: { key: string; label: string; sub?: string; tok: ClaudeTokenCounts; usd: number }[],
    approx: boolean,
  ) => {
    const top = Math.max(...rows.map((r) => r.usd), 1e-9);
    return (
      <Show when={rows.length > 0}>
        <h4 class="ins-h4">{title}</h4>
        <div class="ins-an-scroll">
          <table class="ins-an-table">
            <thead>
              <tr>
                <th>{keyHeader}</th>
                <th>{t("insights.cc.col.cost")}</th>
                <th aria-label={t("insights.an.col.share")} />
                <th class="ins-an-opt ins-an-dim">{t("insights.cc.col.calls")}</th>
                <th class="ins-an-opt ins-an-dim">{t("insights.cc.col.in")}</th>
                <th class="ins-an-opt ins-an-dim">{t("insights.cc.col.out")}</th>
                <th class="ins-an-opt ins-an-dim">{t("insights.cc.col.cache")}</th>
              </tr>
            </thead>
            <tbody>
              <For each={rows}>
                {(r) => (
                  <tr>
                    <td class="ins-cc-key" title={r.sub || r.label}>
                      {r.label}
                      <Show when={r.sub}>
                        <span class="ins-an-dim ins-cc-sub">{r.sub}</span>
                      </Show>
                    </td>
                    <td class="ins-an-num">
                      {approx ? "~" : ""}
                      {fmtUsd(r.usd)}
                    </td>
                    {barCell(r.usd / top)}
                    <td class="ins-an-num ins-an-opt ins-an-dim">{r.tok.calls}</td>
                    <td class="ins-an-num ins-an-opt ins-an-dim">{fmtTokens(r.tok.in_tokens)}</td>
                    <td class="ins-an-num ins-an-opt ins-an-dim">{fmtTokens(r.tok.out_tokens)}</td>
                    <td class="ins-an-num ins-an-opt ins-an-dim">
                      {fmtTokens(r.tok.cache_read)} / {fmtTokens(r.tok.cache_write)}
                    </td>
                  </tr>
                )}
              </For>
            </tbody>
          </table>
        </div>
      </Show>
    );
  };

  const shortPath = (s: string) => {
    const parts = s.split(/[\\/]/).filter(Boolean);
    return parts.length > 2 ? "…/" + parts.slice(-2).join("/") : s;
  };

  return (
    <div class="ins-an ins-cc" classList={{ narrow: narrow.narrow() }} ref={narrow.ref}>
      <div class="ins-an-filters">
        <div class="ins-an-seg" role="group" aria-label={t("insights.an.range")}>
          <For each={RANGES}>
            {(r) => (
              <button
                class={range() === r.id ? "active" : ""}
                aria-pressed={range() === r.id}
                onClick={() => setRange(r.id)}
              >
                {t(`insights.cc.range.${r.id}`)}
              </button>
            )}
          </For>
        </div>
        <div class="ins-an-actions">
          <button
            class="ins-an-copy"
            disabled={!hasData()}
            title={t("insights.cc.copy_hint")}
            onClick={() => void copyReport()}
          >
            <IconClipboard />
            <span>{copied() === "report" ? t("insights.an.copied") : t("insights.an.copy")}</span>
          </button>
          <button
            class="ins-an-copy"
            title={t("insights.cc.cmd_hint")}
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
            <div class="ins-docker-err-hint">{t("insights.cc.old_daemon")}</div>
          </Show>
        </div>
      </Show>

      <Show
        when={hasData() ? rep() : null}
        fallback={
          <Show when={!err()}>
            <div class="settings-hint">
              {loading()
                ? t("insights.logs.loading")
                : t("insights.cc.empty", { files: String(rep()?.scanned_files ?? 0) })}
            </div>
          </Show>
        }
      >
        {(r) => (
          <>
            {/* The disclaimer leads, not trails. Somebody who reads one line
                of this panel has to read the one that says it is not a bill. */}
            <div class="ins-cc-note">
              {t("insights.cc.estimate_note", { asOf: PRICING_AS_OF })}
            </div>
            <Show when={anyUnpriced()}>
              <div class="ins-docker-err">
                <div class="ins-docker-err-hint">{t("insights.cc.unpriced")}</div>
              </div>
            </Show>

            <div class="ins-metrics">
              {tile(
                t("insights.cc.tile.cost"),
                `~${fmtUsd(totalCost())}`,
                t("insights.cc.tile.cost_sub", { calls: String(r().totals.calls) }),
              )}
              {tile(
                t("insights.cc.tile.out"),
                fmtTokens(r().totals.out_tokens),
                t("insights.cc.tile.out_sub", { input: fmtTokens(r().totals.in_tokens) }),
              )}
              {tile(
                t("insights.cc.tile.cache"),
                `${cacheHitRatio(r().totals).toFixed(1)}%`,
                t("insights.cc.tile.cache_sub", {
                  read: fmtTokens(r().totals.cache_read),
                  write: fmtTokens(r().totals.cache_write),
                }),
              )}
              {tile(
                t("insights.cc.tile.subagent"),
                r().totals.calls > 0
                  ? `${((r().sidechain.calls / r().totals.calls) * 100).toFixed(0)}%`
                  : "0%",
                t("insights.cc.tile.subagent_sub", {
                  cost: `~${fmtUsd(blendedOf(r().sidechain))}`,
                }),
              )}
              {tile(
                t("insights.cc.tile.scope"),
                `${r().sessions} / ${r().projects}`,
                t("insights.cc.tile.scope_sub", {
                  files: String(r().scanned_files),
                  took: fmtSpan(Math.round(r().took_ms / 1000)),
                }),
              )}
            </div>

            {rollup(
              t("insights.cc.by_model"),
              t("insights.cc.col.model"),
              r().by_model.map((m) => ({
                key: m.key + (m.speed ?? ""),
                label: m.speed && m.speed !== "standard" ? `${m.key} · ${m.speed}` : m.key,
                tok: m,
                usd: costOf(m, m.key, m.speed, m.ended || r().until).usd,
              })),
              false,
            )}

            {rollup(
              t("insights.cc.by_day"),
              t("insights.an.col.period"),
              byDay().map((d) => ({
                key: String(d.t),
                label: dayFmt().format(new Date(d.t * 1000)),
                tok: d,
                usd: blendedOf(d),
              })),
              true,
            )}

            {rollup(
              t("insights.cc.by_project"),
              t("insights.cc.col.project"),
              r().by_project.map((x) => ({
                key: x.key,
                label: shortPath(x.key),
                sub: x.key,
                tok: x,
                usd: blendedOf(x),
              })),
              true,
            )}

            {rollup(
              t("insights.cc.by_session"),
              t("insights.cc.col.session"),
              r().by_session.map((x) => ({
                key: x.key,
                label: x.key.slice(0, 8),
                sub: x.project
                  ? `${shortPath(x.project)} · ${stampFmt().format(new Date((x.started ?? 0) * 1000))}`
                  : undefined,
                tok: x,
                usd: blendedOf(x),
              })),
              true,
            )}
          </>
        )}
      </Show>
    </div>
  );
}

function emptyTokens(): ClaudeTokenCounts {
  return {
    calls: 0,
    in_tokens: 0,
    out_tokens: 0,
    cache_read: 0,
    cache_write: 0,
    cache_write_5m: 0,
    cache_write_1h: 0,
  };
}
