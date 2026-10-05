import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { t } from "./i18n";
import { createLogger } from "./logger";
import type { Workspace } from "./types";
import { inQueue, type QueueRow } from "./queueModel";
import { QueueRowView, relAge } from "./QueueRow";
import { IntentEditor } from "./IntentEditor";
import {
  clampRailWidth,
  clipPrompt,
  isClosed,
  logIcon,
  logLineText,
  logNewestFirst,
  sessionsForPane,
  sessionTitle,
  LOG_PREVIEW,
  RAIL_DEFAULT_W,
  type SessionContext,
} from "./contextModel";

// Phase 104: the Context Rail — a docked column at the inline-end of the
// main layout (a third `.app` grid column, not a SideDrawer: no backdrop,
// it never covers the panes). It shows ONE thing: the context of the
// FOCUSED pane's Claude session (App's activePaneId) — 🎯 the workspace
// intent, the pane's live row, 📝 the session's first prompt and its
// "where we stand" log from its briefs; earlier sessions of the same pane
// sit behind a toggle. Switching focus switches the card. Data:
// `session_context_list` for the workspace, filtered to the pane,
// refetched on `context:changed`. All agent/user text renders as plain
// text (Solid escapes it) with dir="auto".

const log = createLogger("CONTEXT");

const WIDTH_KEY = "ymux.contextRail.width";
const COLLAPSED_KEY = "ymux.contextRail.collapsed";

/** Per-machine rail prefs in localStorage. Storage can throw (private
 *  window, blocked site data) — fall back to defaults and keep going. */
export function loadRailPrefs(): { width: number; collapsed: boolean } {
  try {
    const w = Number(localStorage.getItem(WIDTH_KEY));
    const c = localStorage.getItem(COLLAPSED_KEY) === "1";
    return { width: w ? clampRailWidth(w) : RAIL_DEFAULT_W, collapsed: c };
  } catch {
    return { width: RAIL_DEFAULT_W, collapsed: false };
  }
}

export function saveRailPrefs(p: { width: number; collapsed: boolean }): void {
  try {
    localStorage.setItem(WIDTH_KEY, String(clampRailWidth(p.width)));
    localStorage.setItem(COLLAPSED_KEY, p.collapsed ? "1" : "0");
  } catch {
    // Not persisted this time; the rail still works.
  }
}

interface Props {
  ws: Workspace | null;
  /** The focused pane — App's `activePaneId`, the same signal keyboard
   *  and focus routing use. The rail follows it. */
  paneId: string | null;
  /** That pane's agent row (App's allPaneAgentRows), if it has one. */
  row: QueueRow | null;
  nowMs: number;
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** Live drag (no persistence) and drag end (persist). */
  onResize: (width: number) => void;
  onResizeEnd: () => void;
  width: number;
  onSaveIntent: (text: string) => void;
  onJumpPane: (paneId: string) => void;
}

export function ContextRail(p: Props) {
  const [sessions, setSessions] = createSignal<SessionContext[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [showEarlier, setShowEarlier] = createSignal(false);
  // Expansion state lives here, keyed by session id: a refetch replaces
  // every SessionContext object (and remounts its card), and a new turn
  // must not snap an expanded log shut.
  const [openPrompts, setOpenPrompts] = createSignal<ReadonlySet<string>>(new Set());
  const [openLogs, setOpenLogs] = createSignal<ReadonlySet<string>>(new Set());
  const flip = (set: ReadonlySet<string>, id: string): ReadonlySet<string> => {
    const next = new Set(set);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    return next;
  };
  const expand = {
    promptOpen: (id: string) => openPrompts().has(id),
    togglePrompt: (id: string) => setOpenPrompts((s) => flip(s, id)),
    logOpen: (id: string) => openLogs().has(id),
    toggleLog: (id: string) => setOpenLogs((s) => flip(s, id)),
  };

  const wsId = () => p.ws?.id ?? null;

  // One fetch per workspace; the focused pane is a client-side filter,
  // so switching focus inside a workspace costs no IPC.
  let fetchSeq = 0;
  const refetch = async () => {
    const id = wsId();
    const mine = ++fetchSeq;
    if (!id) {
      setSessions([]);
      return;
    }
    try {
      const list = await invoke<SessionContext[]>("session_context_list", { wsId: id });
      if (mine !== fetchSeq) return; // a newer fetch (ws switch) won
      setSessions(list);
      setError(null);
    } catch (e) {
      if (mine !== fetchSeq) return;
      log.warn("session_context_list failed", e);
      setError(String(e));
    }
  };

  createEffect(on(wsId, () => void refetch()));
  // A different pane's earlier sessions start collapsed.
  createEffect(on(() => p.paneId, () => setShowEarlier(false), { defer: true }));

  onMount(() => {
    let un: (() => void) | null = null;
    let disposed = false;
    void listen<{ session_id: string; ws_id: string | null }>("context:changed", (e) => {
      if (e.payload.ws_id === wsId()) void refetch();
    }).then((f) => {
      if (disposed) f();
      else un = f;
    });
    onCleanup(() => {
      disposed = true;
      un?.();
    });
  });

  // [current, ...earlier] for the focused pane.
  const paneSessions = createMemo(() => sessionsForPane(sessions(), p.paneId));
  const current = () => paneSessions()[0] ?? null;
  const earlier = () => paneSessions().slice(1);

  // Drag the inline-start edge. In RTL the rail sits on the left, so the
  // pointer delta flips sign.
  const startResize = (e: MouseEvent) => {
    e.preventDefault();
    const rtl = getComputedStyle(document.documentElement).direction === "rtl";
    const startX = e.clientX;
    const startW = p.width;
    const move = (ev: MouseEvent) => {
      const dx = ev.clientX - startX;
      p.onResize(clampRailWidth(rtl ? startW + dx : startW - dx));
    };
    const up = () => {
      window.removeEventListener("mousemove", move);
      window.removeEventListener("mouseup", up);
      p.onResizeEnd();
    };
    window.addEventListener("mousemove", move);
    window.addEventListener("mouseup", up);
  };

  return (
    <aside
      class="context-rail"
      classList={{ "context-rail-collapsed": p.collapsed }}
      aria-label={t("context.title")}
    >
      <Show
        when={!p.collapsed}
        fallback={
          <button
            class="context-rail-strip"
            onClick={p.onToggleCollapsed}
            title={t("context.expand")}
            aria-label={t("context.expand")}
            aria-expanded="false"
          >
            <span aria-hidden="true">🧭</span>
          </button>
        }
      >
        <div
          class="context-rail-resizer"
          onMouseDown={startResize}
          title={t("context.resize")}
          role="separator"
          aria-orientation="vertical"
        />
        <div class="context-rail-head">
          <span class="context-rail-title">🧭 {t("context.title")}</span>
          <button
            class="side-drawer-btn"
            onClick={p.onToggleCollapsed}
            title={t("context.collapse")}
            aria-label={t("context.collapse")}
            aria-expanded="true"
          >
            ⇥
          </button>
        </div>

        <div class="context-rail-body">
          <Show
            when={p.ws}
            fallback={<div class="context-empty">{t("context.noWorkspace")}</div>}
          >
            {(ws) => (
              <IntentEditor
                class="context-intent"
                intent={ws().intent}
                onSave={p.onSaveIntent}
              />
            )}
          </Show>

          <Show when={error()}>
            {(err) => <div class="context-error" dir="auto">{err()}</div>}
          </Show>

          <Show when={p.ws}>
            <Show
              when={p.paneId}
              fallback={<div class="context-empty">{t("context.noPane")}</div>}
            >
              <Show
                when={current()}
                fallback={
                  // No session record for this pane: an agent pane on an
                  // older CLI still gets its live row; anything else (a
                  // plain shell, a Diff / Files pane) gets the hint.
                  <Show
                    when={p.row && inQueue(p.row) ? p.row : null}
                    fallback={<div class="context-empty">{t("context.noSession")}</div>}
                  >
                    {(r) => (
                      <div class="context-card">
                        <QueueRowView row={r()} nowMs={p.nowMs} onClick={() => p.onJumpPane(r().paneId)} />
                      </div>
                    )}
                  </Show>
                }
              >
                {(s) => (
                  <SessionCard s={s()} row={p.row} nowMs={p.nowMs} expand={expand} onJumpPane={p.onJumpPane} />
                )}
              </Show>
              <Show when={earlier().length > 0}>
                <button
                  class="context-link"
                  aria-expanded={showEarlier()}
                  onClick={() => setShowEarlier((v) => !v)}
                >
                  {showEarlier()
                    ? t("context.hideEarlier")
                    : t("context.showEarlier", { n: earlier().length })}
                </button>
                <Show when={showEarlier()}>
                  <For each={earlier()}>
                    {(s) => (
                      <SessionCard s={s} row={null} nowMs={p.nowMs} expand={expand} onJumpPane={p.onJumpPane} />
                    )}
                  </For>
                </Show>
              </Show>
            </Show>
          </Show>
        </div>
      </Show>
    </aside>
  );
}

interface Expand {
  promptOpen: (id: string) => boolean;
  togglePrompt: (id: string) => void;
  logOpen: (id: string) => boolean;
  toggleLog: (id: string) => void;
}

function SessionCard(p: {
  s: SessionContext;
  row: QueueRow | null;
  nowMs: number;
  expand: Expand;
  onJumpPane: (paneId: string) => void;
}) {
  const promptOpen = () => p.expand.promptOpen(p.s.session_id);
  const logOpen = () => p.expand.logOpen(p.s.session_id);
  const prompt = () => (p.s.first_prompt ? clipPrompt(p.s.first_prompt) : null);
  const lines = () => logNewestFirst(p.s.log, logOpen() ? null : LOG_PREVIEW);

  return (
    <div class="context-card" classList={{ "context-card-closed": isClosed(p.s) }}>
      <Show
        when={p.row}
        fallback={
          <div class="context-card-title" dir="auto">
            {isClosed(p.s) ? "✅ " : ""}
            {sessionTitle(p.s)}
          </div>
        }
      >
        {(r) => <QueueRowView row={r()} nowMs={p.nowMs} onClick={() => p.onJumpPane(r().paneId)} />}
      </Show>

      <Show when={prompt()}>
        {(pr) => (
          <button
            class="context-prompt"
            aria-expanded={promptOpen()}
            disabled={!pr().clipped}
            onClick={() => p.expand.togglePrompt(p.s.session_id)}
            title={pr().clipped ? t("context.prompt.toggle") : undefined}
          >
            <span class="context-prompt-label">📝 {t("context.prompt.label")}</span>
            <span class="context-prompt-text" dir="auto">
              {promptOpen() ? p.s.first_prompt : pr().text}
            </span>
          </button>
        )}
      </Show>

      <Show when={p.s.log.length > 0}>
        <ol class="context-log">
          <For each={lines()}>
            {(e) => (
              <li class="context-log-line" classList={{ "context-log-degraded": e.degraded }}>
                <span class="context-log-time">{relAge(e.ts_ms, p.nowMs)}</span>
                <span class="context-log-icon" aria-hidden="true">{logIcon(e)}</span>
                <span class="context-log-text" dir="auto">
                  {logLineText(e) || (e.kind === "closed" ? t("context.log.closed") : "—")}
                </span>
              </li>
            )}
          </For>
        </ol>
        <Show when={p.s.log.length > LOG_PREVIEW}>
          <button
            class="context-link"
            aria-expanded={logOpen()}
            onClick={() => p.expand.toggleLog(p.s.session_id)}
          >
            {logOpen() ? t("context.log.less") : t("context.log.all", { n: p.s.log.length })}
          </button>
        </Show>
      </Show>
    </div>
  );
}
