import { createSignal, For, Show, onMount } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import { IconClose, IconWarning, IconCircle } from "./icons";

// Phase 76 — Monitor "Cleanup" tab. Surfaces the two server-side leaks Yossi
// hit (duplicate ymux port-watchers, orphaned claude sessions) from the
// daemon's /hygiene endpoint, and reaps the safe ones via /hygiene/kill.

interface PortWatcher {
  pid: number;
  workspace: string;
  etime_sec: number;
  cpu_time_sec: number;
  duplicate: boolean;
  // Phase 86: ppid=1 and its SSH channel is gone — the desktop that launched
  // it is no longer listening. Reaped together with duplicates.
  orphan: boolean;
}
interface OrphanSession {
  pid: number;
  session_id: string;
  resume: string;
  etime_sec: number;
  cpu_pct: number;
}
interface Hygiene {
  port_watchers: PortWatcher[];
  duplicate_count: number;
  orphan_sessions: OrphanSession[];
}

function fmtDur(s: number): string {
  if (s >= 86400) return `${Math.floor(s / 86400)}d ${Math.floor((s % 86400) / 3600)}h`;
  if (s >= 3600) return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
  return `${Math.floor(s / 60)}m`;
}

export function HygienePanel(p: { workspaceId?: string }) {
  const [data, setData] = createSignal<Hygiene | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [note, setNote] = createSignal<string | null>(null);

  const refresh = async () => {
    if (!p.workspaceId) return;
    setLoading(true);
    setErr(null);
    try {
      const raw = await invoke<string>("insights_fetch", {
        workspaceId: p.workspaceId,
        path: "/hygiene",
      });
      setData(JSON.parse(raw) as Hygiene);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };
  onMount(refresh);

  const kill = async (pids: number[]) => {
    if (!p.workspaceId || pids.length === 0) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      const raw = await invoke<string>("insights_hygiene_kill", {
        workspaceId: p.workspaceId,
        pids,
      });
      const r = JSON.parse(raw) as { killed: number[] };
      setNote(t("hygiene.killed", { n: String(r.killed?.length ?? 0) }));
      await refresh();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  // Phase 86: orphans (ppid=1, SSH gone) are reaped with the duplicates.
  const stale = () => (data()?.port_watchers ?? []).filter((w) => w.duplicate || w.orphan);
  const dupPids = () => stale().map((w) => w.pid);

  return (
    <div class="hyg-tab">
      <Show when={err()}>
        <div class="wizard-test-result err" style="margin:0 0 10px">
          <div class="wizard-test-line"><IconClose size={14} /> {err()}</div>
        </div>
      </Show>
      <Show when={note()}>
        <div class="settings-hint hyg-note">{note()}</div>
      </Show>

      <div class="hyg-bar">
        <button disabled={loading()} onClick={() => void refresh()}>
          {loading() ? "…" : t("hygiene.refresh")}
        </button>
      </div>

      {/* ── Duplicate port-watchers ── */}
      <h4 class="ins-h4">{t("hygiene.watchers")}</h4>
      <Show
        when={data()}
        fallback={<div class="settings-hint">{loading() ? "…" : t("hygiene.no_data")}</div>}
      >
        <Show
          when={stale().length > 0}
          fallback={<div class="settings-hint">{t("hygiene.no_dups")}</div>}
        >
          <div class="hyg-alert">
            <span>{t("hygiene.dups", { n: String(stale().length) })}</span>
            <button class="primary" disabled={busy()} onClick={() => void kill(dupPids())}>
              {t("hygiene.kill_dups")}
            </button>
          </div>
        </Show>
        <div class="hyg-list">
          <For each={data()!.port_watchers ?? []}>
            {(w) => (
              <div class={`hyg-row${w.duplicate || w.orphan ? " dup" : ""}`}>
                <span class="hyg-main">
                  {w.duplicate || w.orphan ? <IconWarning size={14} /> : <IconCircle size={14} />}{" "}
                  {w.workspace || "?"}
                  {w.orphan ? <span class="settings-hint"> · {t("hygiene.orphan")}</span> : null}
                </span>
                <span class="hyg-meta settings-hint">
                  pid {w.pid} · {t("hygiene.uptime")} {fmtDur(w.etime_sec)} · cpu {w.cpu_time_sec}s
                </span>
              </div>
            )}
          </For>
        </div>
      </Show>

      {/* ── Orphan claude sessions (alert only; user decides) ── */}
      <h4 class="ins-h4">{t("hygiene.orphans")}</h4>
      <Show
        when={(data()?.orphan_sessions?.length ?? 0) > 0}
        fallback={<div class="settings-hint">{t("hygiene.no_orphans")}</div>}
      >
        <div class="settings-hint hyg-orphan-hint">{t("hygiene.orphan_hint")}</div>
        <div class="hyg-list">
          <For each={data()!.orphan_sessions ?? []}>
            {(o) => (
              <div class="hyg-row dup">
                <span class="hyg-main"><IconWarning size={14} /> {o.session_id || o.resume || `pid ${o.pid}`}</span>
                <span class="hyg-meta settings-hint">
                  pid {o.pid} · {t("hygiene.uptime")} {fmtDur(o.etime_sec)} · cpu {o.cpu_pct}%
                </span>
                <button class="hyg-kill" disabled={busy()} onClick={() => void kill([o.pid])}>
                  {t("hygiene.kill")}
                </button>
              </div>
            )}
          </For>
        </div>
      </Show>
    </div>
  );
}
