import { createSignal, For, Show, createEffect, on, onCleanup, type JSX } from "solid-js";
import { createNarrow } from "./useNarrow";
import {
  IconActivity,
  IconSmartphone,
  IconFile,
  IconSparkles,
  IconBot,
  IconHistory,
  IconRefresh,
  IconClose,
} from "./icons";
import { backend } from "./backend";
import { t, currentLanguage } from "./i18n";
import { formatResetLocal } from "./claudeUsageFmt";
import { MobilePairing } from "./MobilePairing";
import { HygienePanel } from "./HygienePanel";
import { InsightsAnalytics } from "./InsightsAnalytics";
import { InsightsClaudeCost } from "./InsightsClaudeCost";
import { fmtBytes, fmtBps } from "./insightsFmt";
import { PanelSurface } from "./PanelSurface";
import type { Surface } from "./panels";
import type { Geometry } from "./floatingWindow";
import type { ClaudeUsage } from "./types";

// Phase 68.D: Server Insights monitor. Pull-based — fetches the live
// snapshot from the remote `ymux-insights` daemon (via the insights_fetch
// Tauri command, which curls 127.0.0.1:7879 over the workspace SSH session).
// No mock data: if the daemon isn't installed/running the panel says so and
// points at Settings → Add-ons.

interface Snapshot {
  ts: number;
  cpu: { pct: number; per_core: number[]; load: number[] };
  mem: { total: number; used: number; cached: number; swap_used: number };
  disks: { mount: string; total: number; used: number; pct: number }[];
  net: { iface: string; rx_bps: number; tx_bps: number }[];
  docker_running: number;
  docker_total: number;
  top: { pid: number; name: string; cpu: number; rss: number }[];
}
interface DockerContainer {
  id: string;
  name: string;
  image: string;
  state: string;
  status: string;
  cpu_pct: number;
  mem_used: number;
  mem_pct: number;
}

interface Props {
  /** Unified surface: closed | drawer | float | fullscreen (see panels.ts). */
  surface: Surface;
  workspaceId?: string;
  workspaceName?: string;
  /** Local workspaces are served in-process; remote ones over SSH. Only the
   *  copy-the-commands blocks care, so they can name the right paths. */
  local?: boolean;
  onClose: () => void;
  onDrawer: () => void;
  onFloat: () => void;
  onFullscreen: () => void;
  /** Phase 68 (UX): open the Add-ons window to install the daemon. */
  onInstall?: () => void;
}

const DEFAULT_GEOMETRY: Geometry = { x: 180, y: 90, w: 820, h: 620 };
const MIN_W = 460;
const MIN_H = 360;

type InsightsView = "metrics" | "analytics" | "mobile" | "logs" | "health" | "claude";

export function InsightsWindow(p: Props) {
  const tabsNarrow = createNarrow(380);
  const [snap, setSnap] = createSignal<Snapshot | null>(null);
  const [docker, setDocker] = createSignal<DockerContainer[]>([]);
  const [dockerOk, setDockerOk] = createSignal(false);
  const [dockerReason, setDockerReason] = createSignal<string | null>(null);
  // Diagnostics surfaced by the daemon (Phase 68 docker patch): the resolved
  // socket, the daemon version (so we can tell if the server is on an old
  // build), and the daemon's own English hint.
  const [dockerInfo, setDockerInfo] = createSignal<{
    socket: string;
    version: string;
    hint: string;
    detail: string;
  } | null>(null);
  const [err, setErr] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [auto, setAuto] = createSignal(false);
  // Phase 70.C: Metrics ↔ Mobile pairing tabs. Phase 78: + Claude usage.
  const [view, setView] = createSignal<InsightsView>("metrics");
  // Phase 78: Claude subscription usage (session/weekly % + contributing).
  const [usage, setUsage] = createSignal<ClaudeUsage | null>(null);
  const [usageLoading, setUsageLoading] = createSignal(false);
  const [usageErr, setUsageErr] = createSignal<string | null>(null);
  // `usageInFlight` and `usageTriedFor` are plain `let`s, NOT signals, so
  // reading them inside the effect below cannot subscribe it. This is the
  // same trap ClaudeUsageIndicator documents at its `let inFlight`: an
  // effect that reads state its own fetch writes re-triggers itself.
  //
  // Here it was `!usage() && !usageLoading()`. On success it settled,
  // because `usage()` filled — which is why it looked fine for months. On
  // FAILURE `usage()` stays null forever, so the guard reopens the moment
  // `usageLoading` drops and the effect fires again immediately: a tight
  // loop of remote fetches, ~14k in 40 minutes, saturating the IPC channel
  // until unrelated clicks stopped landing. It took a workspace that has
  // an SSH connection but no live session (a pinned project folder before
  // its first connect) to make the fetch fail reliably enough to show.
  //
  // A failed attempt is now remembered per workspace: the Refresh button
  // is the retry, not the render loop.
  let usageInFlight = false;
  let usageTriedFor: string | null = null;
  const refreshClaude = async (force: boolean) => {
    if (!p.workspaceId || usageInFlight) return;
    usageInFlight = true;
    usageTriedFor = p.workspaceId;
    setUsageLoading(true);
    setUsageErr(null);
    try {
      const u = await backend.call<ClaudeUsage>("claude_usage_fetch", {
        workspaceId: p.workspaceId,
        force,
      });
      setUsage(u);
    } catch (e) {
      setUsageErr(String(e instanceof Error ? e.message : e));
    } finally {
      usageInFlight = false;
      setUsageLoading(false);
    }
  };
  // Load usage when the Claude tab is opened (uses the 5-min backend cache;
  // only cold-fetches — ~8s — if nothing is cached yet). `on([...])` locks
  // the dependency list so nothing read in the body can retrigger it.
  createEffect(
    on(
      () => [view(), p.workspaceId] as const,
      ([v, ws]) => {
        if (v !== "claude" || !ws) return;
        if (usageTriedFor === ws) return;
        void refreshClaude(false);
      },
    ),
  );
  // Phase 72.2: daemon log viewer.
  const [logLines, setLogLines] = createSignal<string[]>([]);
  const [logPath, setLogPath] = createSignal("");
  const [logLoading, setLogLoading] = createSignal(false);
  const [logErr, setLogErr] = createSignal<string | null>(null);
  const [logFilter, setLogFilter] = createSignal("");

  const refreshLogs = async () => {
    if (!p.workspaceId) return;
    setLogLoading(true);
    setLogErr(null);
    try {
      const r = await backend.call<string>("insights_fetch", {
        workspaceId: p.workspaceId,
        path: "/logs?tail=400",
      });
      const parsed = JSON.parse(r) as { path: string; lines: string[] };
      setLogLines(parsed.lines ?? []);
      setLogPath(parsed.path ?? "");
    } catch (e) {
      setLogErr(String(e instanceof Error ? e.message : e));
    } finally {
      setLogLoading(false);
    }
  };

  const visibleLogs = () => {
    const f = logFilter().trim().toLowerCase();
    return f ? logLines().filter((l) => l.toLowerCase().includes(f)) : logLines();
  };

  const downloadLogs = () => {
    const blob = new Blob([logLines().join("\n")], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = "insights.log";
    a.click();
    URL.revokeObjectURL(url);
  };

  const isOpen = () => p.surface !== "closed";

  // Load logs when the Logs tab is opened.
  createEffect(() => {
    if (view() === "logs" && p.workspaceId) void refreshLogs();
  });

  const refresh = async () => {
    if (!p.workspaceId) return;
    setLoading(true);
    setErr(null);
    try {
      const cur = await backend.call<string>("insights_fetch", {
        workspaceId: p.workspaceId,
        path: "/current",
      });
      if (!cur.trim()) throw new Error(t("insights.unreachable"));
      setSnap(JSON.parse(cur) as Snapshot);
      try {
        const d = await backend.call<string>("insights_fetch", {
          workspaceId: p.workspaceId,
          path: "/docker",
        });
        const parsed = JSON.parse(d) as {
          available: boolean;
          reason?: string;
          detail?: string;
          socket?: string;
          hint?: string;
          daemon_version?: string;
          containers: DockerContainer[];
        };
        setDockerOk(parsed.available);
        setDockerReason(parsed.available ? null : parsed.reason ?? null);
        setDocker(parsed.containers ?? []);
        setDockerInfo({
          socket: parsed.socket ?? "",
          version: parsed.daemon_version ?? "",
          hint: parsed.hint ?? "",
          detail: parsed.detail ?? "",
        });
      } catch {
        // Empty/failed /docker fetch — likely an old daemon that predates the
        // endpoint, or the daemon is down. Flag it so the panel can advise.
        setDockerOk(false);
        setDockerReason("api_error");
        setDocker([]);
        setDockerInfo(null);
      }
    } catch (e) {
      setErr(String(e instanceof Error ? e.message : e));
      setSnap(null);
    } finally {
      setLoading(false);
    }
  };

  // Refresh on open; optional 5s auto-refresh while focused.
  createEffect(() => {
    if (!isOpen() || !p.workspaceId) return;
    void refresh();
    if (!auto()) return;
    const id = setInterval(() => void refresh(), 5000);
    onCleanup(() => clearInterval(id));
  });

  const dockerAction = async (id: string, action: string) => {
    if (!p.workspaceId) return;
    try {
      await backend.call("insights_docker_action", {
        workspaceId: p.workspaceId,
        containerId: id,
        action,
      });
      await refresh();
    } catch (e) {
      setErr(String(e));
    }
  };

  const bar = (pct: number, danger = false) => (
    <div class="ins-bar">
      <div
        class={`ins-bar-fill ${danger && pct > 80 ? "danger" : ""}`}
        style={{ width: `${Math.min(100, Math.max(0, pct))}%` }}
      />
    </div>
  );

  const titleText = () =>
    `${t("insights.title")}${p.workspaceName ? ` — ${p.workspaceName}` : ""}`;

  const tab = (id: InsightsView, icon: JSX.Element, label: string) => (
    <button
      class={view() === id ? "active" : ""}
      title={label}
      onClick={() => setView(id)}
    >
      {icon}
      <span class="ins-tab-label">{label}</span>
    </button>
  );

  const tabsEl = () => (
    <div class="ins-tabs" classList={{ compact: tabsNarrow.narrow() }} ref={tabsNarrow.ref}>
      {tab("metrics", <IconActivity />, t("insights.tab.metrics"))}
      {tab("analytics", <IconHistory />, t("insights.tab.analytics"))}
      {tab("mobile", <IconSmartphone />, t("insights.tab.mobile"))}
      {tab("logs", <IconFile />, t("insights.tab.logs"))}
      {tab("health", <IconSparkles />, t("insights.tab.health"))}
      {tab("claude", <IconBot />, t("claudeUsage.tab"))}
    </div>
  );

  const metricsControlsEl = () => (
    <Show when={view() === "metrics"}>
      <label class="ins-auto">
        <input type="checkbox" checked={auto()} onChange={(e) => setAuto(e.currentTarget.checked)} />
        <span>{t("insights.auto")}</span>
      </label>
      <button class="ins-refresh" onClick={() => void refresh()} title={t("insights.refresh")}>
        {loading() ? "…" : <IconRefresh />}
      </button>
    </Show>
  );

  const bodyContent = () => (
    <>
      <Show when={view() === "analytics"}>
            <InsightsAnalytics workspaceId={p.workspaceId} workspaceName={p.workspaceName} local={p.local} />
          </Show>
          <Show when={view() === "mobile"}>
            <MobilePairing workspaceId={p.workspaceId} />
          </Show>
          <Show when={view() === "health"}>
            <HygienePanel workspaceId={p.workspaceId} />
          </Show>
          <Show when={view() === "claude"}>
            <div class="ins-claude">
              <div class="ins-logs-bar">
                <button onClick={() => void refreshClaude(true)} disabled={usageLoading()}>
                  {usageLoading() ? "…" : <IconRefresh />} {t("insights.refresh")}
                </button>
              </div>
              <Show when={usageErr()}>
                <div class="ins-docker-err"><div class="ins-docker-err-msg"><IconClose /> {usageErr()}</div></div>
              </Show>
              <Show
                when={usage()}
                fallback={
                  <Show when={!usageLoading()} fallback={<div class="settings-hint">{t("claude_picker.loading")}</div>}>
                    <button class="primary" onClick={() => void refreshClaude(false)}>{t("claudeUsage.fetch")}</button>
                  </Show>
                }
              >
                {(u) => (
                  <>
                    <div class="ins-metrics">
                      <div class="ins-metric">
                        <div class="ins-metric-head"><span>{t("claudeUsage.session")}</span><b>{u().session_pct}%</b></div>
                        {bar(u().session_pct, true)}
                        <div class="ins-metric-sub" title={u().session_reset}>{t("claudeUsage.resetsAt")} {formatResetLocal(u().session_reset, Number(u().fetched_unix), currentLanguage())}</div>
                      </div>
                      <div class="ins-metric">
                        <div class="ins-metric-head"><span>{t("claudeUsage.week")}</span><b>{u().week_pct}%</b></div>
                        {bar(u().week_pct, true)}
                        <div class="ins-metric-sub" title={u().week_reset}>{t("claudeUsage.resetsAt")} {formatResetLocal(u().week_reset, Number(u().fetched_unix), currentLanguage())}</div>
                      </div>
                      <For each={u().models}>
                        {(m) => (
                          <div class="ins-metric">
                            <div class="ins-metric-head"><span>{m.name}</span><b>{m.pct}%</b></div>
                            {bar(m.pct, true)}
                            <div class="ins-metric-sub" title={m.reset}>{t("claudeUsage.resetsAt")} {formatResetLocal(m.reset, Number(u().fetched_unix), currentLanguage())}</div>
                          </div>
                        )}
                      </For>
                    </div>
                    <Show when={u().contributing_24h.length > 0}>
                      <div class="ins-claude-detail">
                        <div class="ins-metric-head"><span>{t("claudeUsage.contributing24h")}</span></div>
                        <pre class="ins-logs-view">{u().contributing_24h.join("\n")}</pre>
                      </div>
                    </Show>
                    <Show when={u().contributing_7d.length > 0}>
                      <div class="ins-claude-detail">
                        <div class="ins-metric-head"><span>{t("claudeUsage.contributing7d")}</span></div>
                        <pre class="ins-logs-view">{u().contributing_7d.join("\n")}</pre>
                      </div>
                    </Show>
                  </>
                )}
              </Show>
              {/* Phase 84.E: the quota bars above say how much allowance is
                  left; this says where it went, in tokens and dollars. */}
              <h4 class="ins-h4">{t("insights.cc.title")}</h4>
              <InsightsClaudeCost
                workspaceId={p.workspaceId}
                workspaceName={p.workspaceName}
                local={p.local}
              />
            </div>
          </Show>
          <Show when={view() === "logs"}>
            <div class="ins-logs">
              <div class="ins-logs-bar">
                <input
                  type="text"
                  placeholder={t("insights.logs.filter")}
                  value={logFilter()}
                  onInput={(e) => setLogFilter(e.currentTarget.value)}
                />
                <button onClick={() => void refreshLogs()} disabled={logLoading()}>
                  {logLoading() ? "…" : "⟳"}
                </button>
                <button onClick={downloadLogs} disabled={logLines().length === 0}>
                  {t("insights.logs.download")}
                </button>
              </div>
              <Show when={logPath()}>
                <div class="settings-hint"><code>{logPath()}</code></div>
              </Show>
              <Show when={logErr()}>
                <div class="ins-docker-err"><div class="ins-docker-err-msg">✗ {logErr()}</div></div>
              </Show>
              <Show
                when={visibleLogs().length > 0}
                fallback={<div class="settings-hint">{logLoading() ? t("insights.logs.loading") : t("insights.logs.empty")}</div>}
              >
                <pre class="ins-logs-view">{visibleLogs().join("\n")}</pre>
              </Show>
            </div>
          </Show>
          <Show when={view() === "metrics"}>
          <Show when={err()}>
            <div class="wizard-test-result err" style="margin:10px">
              <div class="wizard-test-line">✗ {err()}</div>
              <div class="wizard-test-meta">{t("insights.install_hint")}</div>
              <Show when={p.onInstall}>
                <button
                  class="primary"
                  style="margin-top:8px"
                  onClick={() => {
                    p.onClose();
                    p.onInstall!();
                  }}
                >
                  {t("insights.install_btn")}
                </button>
              </Show>
            </div>
          </Show>
          <Show when={snap()}>
            {(s) => (
              <>
                <div class="ins-metrics">
                  <div class="ins-metric">
                    <div class="ins-metric-head"><span>CPU</span><b>{s().cpu.pct}%</b></div>
                    {bar(s().cpu.pct, true)}
                    <div class="ins-metric-sub">load {s().cpu.load.map((l) => l.toFixed(2)).join(" ")}</div>
                  </div>
                  <div class="ins-metric">
                    <div class="ins-metric-head">
                      <span>RAM</span>
                      <b>{fmtBytes(s().mem.used)} / {fmtBytes(s().mem.total)}</b>
                    </div>
                    {bar(s().mem.total ? (s().mem.used / s().mem.total) * 100 : 0, true)}
                    <div class="ins-metric-sub">swap {fmtBytes(s().mem.swap_used)}</div>
                  </div>
                  <For each={s().disks}>
                    {(d) => (
                      <div class="ins-metric">
                        <div class="ins-metric-head"><span>{d.mount}</span><b>{d.pct}%</b></div>
                        {bar(d.pct, true)}
                        <div class="ins-metric-sub">{fmtBytes(d.used)} / {fmtBytes(d.total)}</div>
                      </div>
                    )}
                  </For>
                  <div class="ins-metric">
                    <div class="ins-metric-head"><span>NET</span><b /></div>
                    <For each={s().net}>
                      {(n) => (
                        <div class="ins-metric-sub">{n.iface}: ↓{fmtBps(n.rx_bps)} ↑{fmtBps(n.tx_bps)}</div>
                      )}
                    </For>
                  </div>
                </div>

                <h4 class="ins-h4">
                  🐳 Docker ({s().docker_running}/{s().docker_total})
                  <Show when={dockerOk() && dockerInfo()?.socket}>
                    <span class="settings-hint"> · <code>{dockerInfo()!.socket}</code></span>
                  </Show>
                </h4>
                <Show when={!dockerOk()}>
                  <div class="ins-docker-err">
                    <div class="ins-docker-err-msg">
                      ⚠️ {t(
                        dockerReason() === "permission" ? "insights.dk_reason.permission"
                        : dockerReason() === "not_installed" ? "insights.dk_reason.not_installed"
                        : dockerReason() === "no_socket" ? "insights.dk_reason.no_socket"
                        : dockerReason() === "api_error" ? "insights.dk_reason.api_error"
                        : "insights.no_docker"
                      )}
                    </div>
                    <Show when={dockerInfo()?.hint}>
                      <div class="ins-docker-err-hint">{dockerInfo()?.hint}</div>
                    </Show>
                    <div class="ins-docker-err-diag">
                      <Show when={dockerInfo()?.socket}>
                        <span>socket: <code>{dockerInfo()?.socket}</code></span>
                      </Show>
                      <span>
                        {t("insights.daemon_version")}:{" "}
                        <code>{dockerInfo()?.version || t("insights.daemon_unknown")}</code>
                      </span>
                    </div>
                    {/* No version field at all → the server runs a pre-patch daemon. */}
                    <Show when={p.onInstall && !dockerInfo()?.version}>
                      <div class="ins-docker-err-hint">{t("insights.daemon_outdated")}</div>
                    </Show>
                    <Show when={p.onInstall}>
                      <button class="primary" style="margin-top:8px" onClick={() => p.onInstall?.()}>
                        {t("insights.dk_reinstall")}
                      </button>
                    </Show>
                  </div>
                </Show>
                <Show when={dockerOk() && docker().length === 0}>
                  <div class="settings-hint">{t("insights.dk_no_containers")}</div>
                </Show>
                <div class="ins-docker">
                  <For each={docker()}>
                    {(c) => (
                      <div class={`ins-cont ${(c.cpu_pct > 80 || c.mem_pct > 80) ? "alert" : ""}`}>
                        <span class="ins-cont-name">{c.state === "running" ? "●" : "○"} {c.name}</span>
                        <span class="ins-cont-stat">
                          {c.state === "running"
                            ? `${c.cpu_pct}% cpu · ${fmtBytes(c.mem_used)} (${c.mem_pct}%)`
                            : c.status}
                        </span>
                        <span class="ins-cont-actions">
                          <Show when={c.state === "running"}>
                            <button onClick={() => void dockerAction(c.id, "stop")}>{t("insights.dk.stop")}</button>
                            <button onClick={() => void dockerAction(c.id, "restart")}>{t("insights.dk.restart")}</button>
                          </Show>
                          <Show when={c.state !== "running"}>
                            <button onClick={() => void dockerAction(c.id, "start")}>{t("insights.dk.start")}</button>
                          </Show>
                        </span>
                      </div>
                    )}
                  </For>
                </div>

                <h4 class="ins-h4">{t("insights.top")}</h4>
                <div class="ins-procs">
                  <For each={s().top.slice(0, 8)}>
                    {(pr) => (
                      <span class="ins-proc">{pr.name} <b>{pr.cpu}%</b> {fmtBytes(pr.rss)}</span>
                    )}
                  </For>
                </div>
              </>
            )}
          </Show>
          </Show>
    </>
  );

  return (
    <PanelSurface
      surface={p.surface}
      icon={<IconActivity />}
      title={titleText()}
      drawerStorageKey="ymux.drawer-width.monitor"
      drawerDefaultWidth={600}
      drawerMinWidth={420}
      bodyClass="insights-body"
      floatStorageKey={`ymux.panel-monitor-geometry.${p.workspaceId ?? "none"}`}
      floatDefault={DEFAULT_GEOMETRY}
      floatMinW={MIN_W}
      floatMinH={MIN_H}
      onClose={p.onClose}
      onDrawer={p.onDrawer}
      onFloat={p.onFloat}
      onFullscreen={p.onFullscreen}
      headerActions={() => (
        <>
          {tabsEl()}
          {metricsControlsEl()}
        </>
      )}
      body={bodyContent}
    />
  );
}
