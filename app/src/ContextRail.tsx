import { createEffect, createMemo, createSignal, For, on, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { t } from "./i18n";
import { createLogger } from "./logger";
import type { Workspace } from "./types";
import { inQueue, type QueueRow } from "./queueModel";
import { QueueRowView, relAge } from "./QueueRow";
import { IntentEditor } from "./IntentEditor";
import { AgentLight } from "./AgentLight";
import {
  cardGoal,
  clampRailWidth,
  doneCount,
  doneEntries,
  isClosed,
  lastTurn,
  logIcon,
  oneLine,
  sessionsForPane,
  waitingText,
  LOG_PREVIEW,
  RAIL_DEFAULT_W,
  type SessionContext,
} from "./contextModel";

// Phase 104: the Context Rail — a docked column at the inline-end of the
// main layout (a third `.app` grid column, not a SideDrawer: no backdrop,
// it never covers the panes). It shows ONE thing: the context of the
// FOCUSED pane's Claude session (App's activePaneId), as a card modeled on
// tzafrir/human-in-the-loop's task card — 🎯 goal + Done when, Now / Next /
// Waiting on you, the last 3 ✔ deltas, then ▸ N more · ▸ original prompt
// (SessionCard below). The workspace 🎯 intent editor sits above it;
// earlier sessions of the same pane sit behind a toggle. Switching focus
// switches the card. Data: `session_context_list` for the workspace,
// filtered to the pane, refetched on `context:changed`. All agent/user
// text renders as plain text (Solid escapes it) with dir="auto".

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
                  <SessionCard s={s()} row={p.row} nowMs={p.nowMs} expand={expand} />
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
                      <SessionCard s={s} row={null} nowMs={p.nowMs} expand={expand} />
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

/** Wall-clock time of a ✔ line (the age is on the Now line). */
function clock(ms: number): string {
  try {
    return new Date(ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  } catch {
    return "";
  }
}

// The card, modeled on human-in-the-loop's task card: fixed labels, one
// short line each. Layout (Yossi-approved, 2026-10-05):
//   🎯 goal / Done when
//   Now (light + age) / ➜ Next / ❓ Waiting on you
//   ✔ last 3 deltas with clock time
//   ▸ N more · ▸ original prompt
// Full text of every clipped line rides its tooltip.
function SessionCard(p: {
  s: SessionContext;
  row: QueueRow | null;
  nowMs: number;
  expand: Expand;
}) {
  const promptOpen = () => p.expand.promptOpen(p.s.session_id);
  const logOpen = () => p.expand.logOpen(p.s.session_id);
  const goal = () => cardGoal(p.s);
  const doneWhen = () => (p.s.done_when ? oneLine(p.s.done_when) : null);
  const turn = () => lastTurn(p.s);
  const now = () => (turn()?.task ? oneLine(turn()?.task ?? "") : null);
  const next = () => (turn()?.next ? oneLine(turn()?.next ?? "") : null);
  const waiting = () => {
    const w = waitingText(p.s);
    return w ? oneLine(w) : null;
  };
  const done = () => doneEntries(p.s, logOpen() ? null : LOG_PREVIEW);
  const more = () => Math.max(0, doneCount(p.s) - LOG_PREVIEW);

  return (
    <div class="context-card" classList={{ "context-card-closed": isClosed(p.s) }}>
      <Show when={goal()}>
        {(g) => (
          <div class="context-goal">
            <div class="context-field" title={g().full}>
              <span class="context-field-icon" aria-hidden="true">🎯</span>
              <span class="context-goal-text" dir="auto">{g().text}</span>
            </div>
            <Show when={doneWhen()}>
              {(d) => (
                <div class="context-field context-field-sub" title={d().full}>
                  <span class="context-field-label">{t("context.doneWhen")}</span>
                  <span dir="auto">{d().text}</span>
                </div>
              )}
            </Show>
          </div>
        )}
      </Show>

      <Show when={turn()}>
        {(tu) => (
          <div class="context-status">
            <div class="context-field" title={now()?.full ?? ""}>
              <span class="context-field-icon">
                <Show
                  when={p.row?.light}
                  fallback={<span aria-hidden="true">{isClosed(p.s) ? "✅" : logIcon(tu())}</span>}
                >
                  <AgentLight
                    light={p.row?.light ?? null}
                    waitingOnPermission={p.row?.waitingOnPermission ?? false}
                    stateSince={p.row?.stateSince ?? null}
                    nowMs={p.nowMs}
                  />
                </Show>
              </span>
              <span class="context-field-label">{t("context.now")}</span>
              <span class="context-field-text" dir="auto">{now()?.text ?? "—"}</span>
              <span class="context-age">{relAge(tu().ts_ms, p.nowMs)}</span>
            </div>
            <Show when={next()}>
              {(n) => (
                <div class="context-field" title={n().full}>
                  <span class="context-field-icon" aria-hidden="true">➜</span>
                  <span class="context-field-label">{t("context.next")}</span>
                  <span class="context-field-text" dir="auto">{n().text}</span>
                </div>
              )}
            </Show>
            <Show when={waiting()}>
              {(w) => (
                <div class="context-field context-waiting" title={w().full}>
                  <span class="context-field-icon" aria-hidden="true">❓</span>
                  <span class="context-field-label">{t("context.waiting")}</span>
                  <span class="context-field-text" dir="auto">{w().text}</span>
                </div>
              )}
            </Show>
          </div>
        )}
      </Show>

      <Show when={done().length > 0}>
        <ol class="context-log">
          <For each={done()}>
            {(e) => {
              const line = e.kind === "closed" ? null : oneLine(e.delta ?? "");
              return (
                <li
                  class="context-log-line"
                  classList={{ "context-log-degraded": e.degraded }}
                  title={line?.full ?? t("context.log.closed")}
                >
                  <span class="context-log-icon" aria-hidden="true">{e.kind === "closed" ? "✅" : "✔"}</span>
                  <span class="context-log-text" dir="auto">
                    {line ? line.text : t("context.log.closed")}
                  </span>
                  <span class="context-log-time">{clock(e.ts_ms)}</span>
                </li>
              );
            }}
          </For>
        </ol>
      </Show>

      <Show when={more() > 0 || p.s.first_prompt}>
        <div class="context-links">
          <Show when={more() > 0}>
            <button
              class="context-link"
              aria-expanded={logOpen()}
              onClick={() => p.expand.toggleLog(p.s.session_id)}
            >
              {logOpen() ? `▾ ${t("context.log.less")}` : `▸ ${t("context.log.more", { n: more() })}`}
            </button>
          </Show>
          <Show when={p.s.first_prompt}>
            <button
              class="context-link"
              aria-expanded={promptOpen()}
              onClick={() => p.expand.togglePrompt(p.s.session_id)}
            >
              {promptOpen() ? "▾" : "▸"} {t("context.prompt.original")}
            </button>
          </Show>
        </div>
      </Show>
      <Show when={promptOpen() && p.s.first_prompt}>
        {(fp) => <div class="context-prompt-text" dir="auto">{fp()}</div>}
      </Show>
    </div>
  );
}
