import { createEffect, createMemo, createSignal, For, Show, on, onCleanup } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { KillSessionOutcome, SessionSummary, TmuxSessionInfo } from "./types";
import { currentLanguage, t } from "./i18n";
import { IconClose, IconRefresh } from "./icons";
import { createLogger } from "./logger";

const log = createLogger("SESSIONS");

// Phase 90: the active-sessions overview ("סשנים פעילים"). Opened from a
// workspace's right-click menu; lists EVERY multiplexer session on that
// workspace's machine, grouped by directory, and then asks the machine's own
// `claude -p` for a one-line summary + status per session.
//
// Summaries are PULLED, never pushed (Yossi, 2026-09-02, PR #41 comment): every
// chunk is a real `claude -p` over captured screens and a machine holds many
// sessions the user does not care about, so selection is the cost control.
// The list renders instantly from multiplexer metadata; the user ticks rows
// (or a whole directory group) and presses "Summarize selected". The choice
// is per open (v1); persisting it per host is a BACKLOG item. A request
// counter drops a late answer after a refresh so a stale summary never lands
// on a fresh row.
//
// Rule #1: summaries are derived from screen content. They are rendered,
// never logged — `log.*` here only ever sees counts and error kinds.

/** Backend cap is 25 per call; 10 keeps the first summaries arriving sooner. */
const SUMMARY_CHUNK = 10;
/** Mirrors `validate_tmux_rename_target` in lib.rs. */
const RENAME_RE = /^[A-Za-z0-9_-]{1,64}$/;

interface Props {
  open: boolean;
  workspaceId?: string;
  workspaceName?: string;
  /** Windows local workspace: zellij, which refuses rename (docs/ZELLIJ.md §1). */
  isZellij: boolean;
  onClose: () => void;
  /** Open the session on a screen of its own (a child workspace row in the
   *  tree, attached to it). App owns the flow; it needs the whole row for
   *  the display name and the directory. */
  onOpen: (s: TmuxSessionInfo) => Promise<void>;
  /** Resolves to what the backend achieved; `null` when the invoke threw. */
  onKill: (name: string) => Promise<KillSessionOutcome | null>;
  /** Throws with the backend's message on failure. */
  onRename: (oldName: string, newName: string) => Promise<void>;
}

type Group = { key: string | null; title: string; rows: TmuxSessionInfo[] };

const displayName = (s: TmuxSessionInfo): string =>
  s.label ?? s.auto_name ?? s.claude_title ?? s.name;

const lastSegment = (path: string): string => {
  const trimmed = path.replace(/[\\/]+$/, "");
  const idx = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  return idx >= 0 ? trimmed.slice(idx + 1) : trimmed;
};

const fmtAge = (unix: number): string => {
  if (!unix) return "—";
  const sec = Math.max(1, Math.floor(Date.now() / 1000 - unix));
  if (sec < 60) return `${sec}s`;
  if (sec < 3600) return `${Math.floor(sec / 60)}m`;
  if (sec < 86400) return `${Math.floor(sec / 3600)}h`;
  return `${Math.floor(sec / 86400)}d`;
};

export function SessionsOverviewWindow(p: Props) {
  const [rows, setRows] = createSignal<TmuxSessionInfo[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [listError, setListError] = createSignal<string | null>(null);
  const [summaries, setSummaries] = createSignal<Record<string, SessionSummary>>({});
  const [summarizing, setSummarizing] = createSignal(false);
  const [summaryError, setSummaryError] = createSignal<string | null>(null);
  // Row-level UI state, keyed by session name.
  const [killArmed, setKillArmed] = createSignal<string | null>(null);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [renaming, setRenaming] = createSignal<string | null>(null);
  const [renameValue, setRenameValue] = createSignal("");
  const [renameError, setRenameError] = createSignal<string | null>(null);
  // Which rows the next "Summarize selected" covers, and which are in flight.
  const [selected, setSelected] = createSignal<Set<string>>(new Set());
  const [inFlight, setInFlight] = createSignal<Set<string>>(new Set());
  let reqId = 0;
  let disarmTimer: ReturnType<typeof setTimeout> | undefined;

  const errText = (e: unknown): string => (typeof e === "string" ? e : String(e));

  const summarize = async (names: string[], id: number) => {
    if (names.length === 0) return;
    setSummarizing(true);
    setSummaryError(null);
    setInFlight(new Set(names));
    try {
      for (let i = 0; i < names.length; i += SUMMARY_CHUNK) {
        const chunk = names.slice(i, i + SUMMARY_CHUNK);
        const out = await invoke<SessionSummary[]>("sessions_overview_summarize", {
          workspaceId: p.workspaceId,
          names: chunk,
          lang: currentLanguage(),
        });
        if (id !== reqId) return; // a refresh superseded this run
        setSummaries((prev) => {
          const next = { ...prev };
          for (const s of out) next[s.name] = s;
          return next;
        });
        setInFlight((prev) => {
          const next = new Set(prev);
          for (const n of chunk) next.delete(n);
          return next;
        });
      }
    } catch (e) {
      if (id !== reqId) return;
      log.warn("sessions_overview_summarize failed", e);
      setSummaryError(errText(e));
    } finally {
      if (id === reqId) {
        setSummarizing(false);
        setInFlight(new Set());
      }
    }
  };

  // Only rows that can be captured: an EXITED zellij session has no running
  // server to read from.
  const summarizable = (s: TmuxSessionInfo) => !s.exited;
  const isSelected = (name: string) => selected().has(name);
  const toggleSelected = (name: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(name)) next.delete(name);
      else next.add(name);
      return next;
    });
  const setManySelected = (names: string[], on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      for (const n of names) {
        if (on) next.add(n);
        else next.delete(n);
      }
      return next;
    });
  const selectableNames = () => rows().filter(summarizable).map((s) => s.name);
  const allSelected = () => {
    const all = selectableNames();
    return all.length > 0 && all.every((n) => selected().has(n));
  };
  const summarizeSelected = () => {
    const names = selectableNames().filter((n) => selected().has(n));
    if (names.length === 0 || summarizing()) return;
    const id = ++reqId;
    void summarize(names, id);
  };

  const load = async () => {
    const wsId = p.workspaceId;
    if (!wsId) return;
    const id = ++reqId;
    setLoading(true);
    setListError(null);
    setSummaries({});
    setKillArmed(null);
    setRenaming(null);
    setInFlight(new Set());
    setSummarizing(false);
    try {
      const list = await invoke<TmuxSessionInfo[]>("pane_list_tmux_sessions", {
        workspaceId: wsId,
        projectPath: null,
      });
      if (id !== reqId) return;
      setRows(list);
      log.info(`listed ${list.length} sessions`);
      // Keep a selection only for rows that still exist; nothing is summarized
      // until the user asks.
      const live = new Set(list.map((s) => s.name));
      setSelected((prev) => new Set([...prev].filter((n) => live.has(n))));
    } catch (e) {
      if (id !== reqId) return;
      log.error("pane_list_tmux_sessions failed", e);
      setListError(errText(e));
      setRows([]);
    } finally {
      if (id === reqId) setLoading(false);
    }
  };

  const retrySummaries = () => summarizeSelected();

  createEffect(
    on(
      () => [p.open, p.workspaceId] as const,
      ([open, wsId]) => {
        if (open && wsId) void load();
        if (!open) {
          reqId++; // drop anything still in flight
          setRows([]);
          setSummaries({});
          setSelected(new Set());
          setInFlight(new Set());
          setSummarizing(false);
        }
      },
    ),
  );

  // Esc closes — unless an inline rename is open, where Esc cancels that.
  createEffect(() => {
    if (!p.open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      if (renaming()) setRenaming(null);
      else p.onClose();
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });
  onCleanup(() => clearTimeout(disarmTimer));

  // Grouped by directory: the live tmux cwd first, else the cwd ymux recorded
  // when it claimed the session (the only key a zellij row can have). Rows we
  // cannot place go last under one "unknown folder" heading.
  const groups = createMemo<Group[]>(() => {
    const byKey = new Map<string | null, TmuxSessionInfo[]>();
    for (const s of rows()) {
      const key = s.cwd ?? s.owner_cwd ?? null;
      const list = byKey.get(key) ?? [];
      list.push(s);
      byKey.set(key, list);
    }
    const out: Group[] = [];
    const keys = [...byKey.keys()].filter((k): k is string => k !== null).sort();
    for (const k of keys) out.push({ key: k, title: lastSegment(k), rows: byKey.get(k)! });
    const unknown = byKey.get(null);
    if (unknown) out.push({ key: null, title: t("sessions.unknownFolder"), rows: unknown });
    return out;
  });

  const armKill = (name: string) => {
    clearTimeout(disarmTimer);
    setKillArmed(name);
    disarmTimer = setTimeout(() => setKillArmed(null), 3000);
  };

  const doKill = async (s: TmuxSessionInfo) => {
    clearTimeout(disarmTimer);
    setKillArmed(null);
    setBusy(s.name);
    try {
      const out = await p.onKill(s.name);
      const gone =
        out !== null &&
        (out.result === "killed" || out.result === "already_gone" || out.result === "no_session");
      if (gone) {
        setRows((prev) => prev.filter((r) => r.name !== s.name));
      } else if (out?.result !== "attempted") {
        window.alert(t("sessions.killFailed", { name: displayName(s) }));
      }
    } finally {
      setBusy(null);
    }
  };

  const startRename = (s: TmuxSessionInfo) => {
    setRenameError(null);
    setRenameValue(s.name);
    setRenaming(s.name);
  };

  const commitRename = async (s: TmuxSessionInfo) => {
    const next = renameValue().trim();
    if (next === s.name) {
      setRenaming(null);
      return;
    }
    if (!RENAME_RE.test(next)) {
      setRenameError(t("sessions.renameInvalid"));
      return;
    }
    setBusy(s.name);
    try {
      await p.onRename(s.name, next);
      setRenaming(null);
      await load();
    } catch (e) {
      setRenameError(t("sessions.renameFailed", { error: errText(e) }));
    } finally {
      setBusy(null);
    }
  };

  const doOpen = async (s: TmuxSessionInfo) => {
    setBusy(s.name);
    try {
      await p.onOpen(s);
    } catch (e) {
      log.error("open session failed", e);
      window.alert(t("sessions.openFailed", { error: errText(e) }));
    } finally {
      setBusy(null);
    }
  };

  const statusOf = (s: TmuxSessionInfo): SessionSummary["status"] | null =>
    s.exited ? null : (summaries()[s.name]?.status ?? null);

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal sessions-overview" onClick={(e) => e.stopPropagation()}>
          <div class="settings-head">
            <h3>{t("sessions.title", { workspace: p.workspaceName ?? "" })}</h3>
            <span class="sessions-head-note">
              <Show when={summarizing()}>
                <span class="sessions-spinner" aria-hidden="true" /> {t("sessions.summarizing")}
              </Show>
              <Show when={!summarizing() && summaryError()}>
                {t("sessions.summaryFailed", { error: summaryError()! })}{" "}
                <button class="sessions-link" onClick={retrySummaries}>
                  {t("sessions.retry")}
                </button>
              </Show>
              <Show when={!summarizing() && !summaryError() && rows().length > 0 && selected().size === 0}>
                {t("sessions.selectHint")}
              </Show>
            </span>
            <Show when={rows().length > 0}>
              <button
                class="sessions-head-btn"
                disabled={summarizing() || selectableNames().length === 0}
                onClick={() => setManySelected(selectableNames(), !allSelected())}
              >
                {allSelected() ? t("sessions.selectNone") : t("sessions.selectAll")}
              </button>
              <button
                class="sessions-head-btn primary"
                disabled={summarizing() || selected().size === 0}
                onClick={summarizeSelected}
              >
                {t("sessions.summarizeSelected", { n: selected().size })}
              </button>
            </Show>
            <button
              class="feed-x"
              title={t("sessions.refresh")}
              disabled={loading()}
              onClick={() => void load()}
            >
              <IconRefresh />
            </button>
            <button class="feed-x" title={t("common.close")} onClick={p.onClose}>
              <IconClose />
            </button>
          </div>

          <div class="sessions-body">
            <Show when={listError()}>
              <p class="sessions-empty">{t("sessions.listFailed", { error: listError()! })}</p>
            </Show>
            <Show when={!listError() && loading() && rows().length === 0}>
              <p class="sessions-empty">{t("sessions.loading")}</p>
            </Show>
            <Show when={!listError() && !loading() && rows().length === 0}>
              <p class="sessions-empty">{t("sessions.empty")}</p>
            </Show>
            <Show when={rows().length > 0}>
              <table class="ins-an-table sessions-table">
                <thead>
                  <tr>
                    <th class="sessions-col-tick" />
                    <th>{t("sessions.col.name")}</th>
                    <th>{t("sessions.col.state")}</th>
                    <th>{t("sessions.col.windows")}</th>
                    <th>{t("sessions.col.age")}</th>
                    <th>{t("sessions.col.status")}</th>
                    <th class="sessions-col-summary">{t("sessions.col.summary")}</th>
                    <th>{t("sessions.col.actions")}</th>
                  </tr>
                </thead>
                <For each={groups()}>
                  {(g) => (
                    <tbody>
                      <tr class="sessions-group-head">
                        <td class="sessions-col-tick">
                          <input
                            type="checkbox"
                            title={t("sessions.selectGroup")}
                            disabled={summarizing() || !g.rows.some(summarizable)}
                            checked={g.rows.filter(summarizable).every((s) => isSelected(s.name)) && g.rows.some(summarizable)}
                            onChange={(e) =>
                              setManySelected(
                                g.rows.filter(summarizable).map((s) => s.name),
                                e.currentTarget.checked,
                              )
                            }
                          />
                        </td>
                        <td colSpan={7} title={g.key ?? undefined}>
                          <span class="sessions-group-title">{g.title}</span>
                          <Show when={g.key}>
                            <span class="sessions-group-path">{g.key}</span>
                          </Show>
                        </td>
                      </tr>
                      <For each={g.rows}>
                        {(s) => (
                          <tr classList={{ "sessions-row-busy": busy() === s.name, "sessions-row-selected": isSelected(s.name) }}>
                            <td class="sessions-col-tick">
                              <input
                                type="checkbox"
                                disabled={summarizing() || !summarizable(s)}
                                checked={isSelected(s.name)}
                                onChange={() => toggleSelected(s.name)}
                              />
                            </td>
                            <td class="sessions-col-name">
                              <Show
                                when={renaming() === s.name}
                                fallback={
                                  <>
                                    <div class="sessions-name" title={s.name}>
                                      {displayName(s)}
                                    </div>
                                    <Show when={displayName(s) !== s.name}>
                                      <div class="sessions-raw-name">{s.name}</div>
                                    </Show>
                                  </>
                                }
                              >
                                <input
                                  class="sessions-rename-input"
                                  value={renameValue()}
                                  placeholder={t("sessions.renamePlaceholder")}
                                  spellcheck={false}
                                  autofocus
                                  disabled={busy() === s.name}
                                  onInput={(e) => {
                                    setRenameValue(e.currentTarget.value);
                                    setRenameError(null);
                                  }}
                                  onKeyDown={(e) => {
                                    if (e.key === "Enter") {
                                      e.preventDefault();
                                      void commitRename(s);
                                    }
                                  }}
                                />
                                <Show when={renameError()}>
                                  <div class="sessions-rename-error">{renameError()}</div>
                                </Show>
                              </Show>
                            </td>
                            <td>
                              <span
                                class="nc-resume-badge"
                                classList={{ "sessions-badge-attached": s.attached }}
                              >
                                {s.exited
                                  ? t("sessions.state.exited")
                                  : s.attached
                                    ? t("sessions.state.attached")
                                    : t("sessions.state.detached")}
                              </span>
                            </td>
                            <td class="sessions-num">{s.windows}</td>
                            <td class="sessions-num" title={new Date(s.created * 1000).toLocaleString()}>
                              {fmtAge(s.last_attached || s.created)}
                            </td>
                            <td>
                              <Show
                                when={statusOf(s)}
                                fallback={
                                  <Show
                                    when={inFlight().has(s.name)}
                                    fallback={<span class="sessions-dash">—</span>}
                                  >
                                    <span class="sessions-spinner" aria-hidden="true" />
                                  </Show>
                                }
                              >
                                {(st) => (
                                  <span class={`sessions-status ${st()}`}>
                                    {t(`sessions.status.${st()}`)}
                                  </span>
                                )}
                              </Show>
                            </td>
                            <td class="sessions-col-summary">
                              <span class="sessions-summary" title={summaries()[s.name]?.summary}>
                                {summaries()[s.name]?.summary ?? ""}
                              </span>
                            </td>
                            <td>
                              <span class="ports-row-actions sessions-actions">
                                <button
                                  class="primary"
                                  title={t("sessions.open.hint")}
                                  disabled={busy() === s.name}
                                  onClick={() => void doOpen(s)}
                                >
                                  {t("sessions.open")}
                                </button>
                                <Show when={renaming() === s.name} fallback={
                                  <button
                                    title={p.isZellij ? t("sessions.renameZellij") : t("sessions.rename.hint")}
                                    disabled={p.isZellij || busy() === s.name}
                                    onClick={() => startRename(s)}
                                  >
                                    {t("sessions.rename")}
                                  </button>
                                }>
                                  <button
                                    disabled={busy() === s.name}
                                    onClick={() => void commitRename(s)}
                                  >
                                    {t("common.save")}
                                  </button>
                                  <button
                                    disabled={busy() === s.name}
                                    onClick={() => setRenaming(null)}
                                  >
                                    {t("common.cancel")}
                                  </button>
                                </Show>
                                <button
                                  class="danger"
                                  classList={{ armed: killArmed() === s.name }}
                                  disabled={busy() === s.name}
                                  onClick={() =>
                                    killArmed() === s.name ? void doKill(s) : armKill(s.name)
                                  }
                                >
                                  {killArmed() === s.name
                                    ? t("sessions.killConfirm")
                                    : t("sessions.kill")}
                                </button>
                              </span>
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  )}
                </For>
              </table>
            </Show>
          </div>
        </div>
      </div>
    </Show>
  );
}
