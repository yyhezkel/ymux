import { createSignal, createEffect, on, onCleanup, For, Show } from "solid-js";
import { backend } from "./backend";
import { t, currentLanguage } from "./i18n";
import { formatResetLocal } from "./claudeUsageFmt";
import { IconBot } from "./icons";
import type { ClaudeUsage } from "./types";

// Phase 78: compact, always-visible Claude subscription-usage indicator.
// When there's room it shows all metrics (session · week · top model) as
// colored % (or bars); when the window is narrow it collapses to the single
// most-critical metric and the rest live in the tooltip — one per line, with
// reset times converted to the viewer's LOCAL timezone.
// It owns its own fetch + auto-refresh: while the active workspace has a LIVE
// (non-headless) connection, it re-fetches every `refreshMinutes` (0 = off).
// Calls are free but ~8s, so we lean on the 5-min backend cache (force:false)
// and never fast-poll.

interface Props {
  workspaceId?: string;
  /** True only when the active workspace has a real terminal session. */
  live: boolean;
  displayMode: "percent" | "bar" | string;
  refreshMinutes: number;
}

interface Metric {
  key: string;
  label: string;
  pct: number;
  reset: string;
}

function color(pct: number): string {
  if (pct > 85) return "var(--w-error)";
  if (pct > 60) return "var(--w-warning, #e0af68)";
  return "var(--w-ok, #9ece6a)";
}

export function ClaudeUsageIndicator(p: Props) {
  const [usage, setUsage] = createSignal<ClaudeUsage | null>(null);

  // Track viewport width so we can decide how many metrics fit.
  const [vw, setVw] = createSignal(
    typeof window !== "undefined" ? window.innerWidth : 1280,
  );
  const onResize = () => setVw(window.innerWidth);
  if (typeof window !== "undefined") {
    window.addEventListener("resize", onResize);
    onCleanup(() => window.removeEventListener("resize", onResize));
  }

  // Bugfix (beta.3): `loading` MUST NOT be read from inside the effect
  // below — a reactive dependency loop caused `fetchUsage` to fire every
  // ~21 s (SSH exec timeout) instead of every `refreshMinutes`, opening
  // a new `claude -p /usage` on the remote each time, orphaning the
  // previous one, exhausting sshd's MaxSessions, and killing the whole
  // SSH connection. `inFlight` is a plain `let` (untracked) so reads
  // inside fetchUsage don't subscribe the effect.
  let inFlight = false;
  const fetchUsage = async (force = false) => {
    if (!p.workspaceId || !p.live || inFlight) return;
    inFlight = true;
    try {
      const u = await backend.call<ClaudeUsage>("claude_usage_fetch", {
        workspaceId: p.workspaceId,
        force,
      });
      setUsage(u);
    } catch {
      // Leave the last-known value in place; the tab surfaces the real error.
    } finally {
      inFlight = false;
    }
  };

  // Fetch on (workspace, live, refreshMinutes) change, then on the
  // auto-refresh cadence. Explicit `on([...])` locks the dependency list
  // so nothing read inside the body can retrigger the effect.
  createEffect(
    on(
      () => [p.workspaceId, p.live, p.refreshMinutes] as const,
      ([ws, live, mins]) => {
        // Every (workspace, live) change starts from a blank pill: the
        // numbers belong to ONE workspace's account (the remote's, the
        // distro's, or the local login), so a failed fetch after a
        // switch must show nothing rather than the previous host's quota.
        setUsage(null);
        if (!ws || !live) return;
        void fetchUsage();
        if (!mins || mins <= 0) return;
        const id = setInterval(() => void fetchUsage(), mins * 60_000);
        onCleanup(() => clearInterval(id));
      },
    ),
  );

  // All metrics, most-critical first is NOT assumed — session/week/top-model in
  // a stable order so the pill doesn't reshuffle on every refresh.
  const metrics = (): Metric[] => {
    const u = usage();
    if (!u) return [];
    const list: Metric[] = [
      { key: "session", label: t("claudeUsage.session"), pct: u.session_pct, reset: u.session_reset },
      { key: "week", label: t("claudeUsage.week"), pct: u.week_pct, reset: u.week_reset },
    ];
    const top = [...u.models].sort((a, b) => b.pct - a.pct)[0];
    if (top) list.push({ key: "model", label: top.name, pct: top.pct, reset: top.reset });
    return list;
  };

  // How many chips fit. Bars are wider than % text, so give them a bit more room.
  const maxChips = (): number => {
    const w = vw();
    const wide = p.displayMode === "bar" ? 1.25 : 1;
    if (w >= 1200 * wide) return 3;
    if (w >= 950 * wide) return 2;
    return 1;
  };

  // Chips actually rendered. When only one fits, show the most-critical (highest
  // %) so the headline never hides a near-limit metric behind a calmer one.
  const visible = (): Metric[] => {
    const all = metrics();
    const n = maxChips();
    if (n >= all.length) return all;
    if (n <= 1) return [[...all].sort((a, b) => b.pct - a.pct)[0]];
    return all.slice(0, n);
  };

  // Tooltip: every metric on its own line, reset time in the LOCAL timezone.
  const tip = (): string => {
    const u = usage();
    if (!u) return "";
    const anchor = Number(u.fetched_unix);
    const line = (label: string, pct: number, reset: string) =>
      `${label} ${pct}% · ${t("claudeUsage.resetsAt")} ${formatResetLocal(reset, anchor, currentLanguage())}`;
    return [
      line(t("claudeUsage.session"), u.session_pct, u.session_reset),
      line(t("claudeUsage.week"), u.week_pct, u.week_reset),
      ...u.models.map((m) => line(m.name, m.pct, m.reset)),
    ].join("\n");
  };

  return (
    <Show when={usage()}>
      {/* Passive readout, not a button: hover shows the tooltip, double-click
          forces a refresh, single click does nothing (the Monitor toolbar
          button opens the panel). */}
      <div
        class="claude-usage-pill"
        title={tip()}
        onDblClick={() => void fetchUsage(true)}
        aria-label={t("claudeUsage.tab")}
      >
        <IconBot class="cu-glyph" size={14} />
        <For each={visible()}>
          {(m) => (
            <span class="cu-metric">
              <span class="cu-label">{m.label}</span>
              <Show
                when={p.displayMode === "bar"}
                fallback={
                  <span class="cu-pct" style={{ color: color(m.pct) }}>
                    {m.pct}%
                  </span>
                }
              >
                <span class="cu-bar">
                  <span
                    class="cu-bar-fill"
                    style={{ width: `${m.pct}%`, background: color(m.pct) }}
                  />
                </span>
              </Show>
            </span>
          )}
        </For>
      </div>
    </Show>
  );
}
