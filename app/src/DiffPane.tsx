// Phase 50 / 91.F: the live diff pane.
//
// On mount we subscribe to `diff-pane-updated` FIRST, then call
// `diff_pane_start` (order matters — the first emit is hash-gated and
// never repeats, so a late listener would miss it). The backend bundle
// carries the branch, the changed-file list (incl. untracked), the diff
// text and any error verbatim. A worktree strip along the top switches
// which worktree the pane looks at (`diff_pane_set_cwd`); a file list
// jumps to a file's hunk. Parsing is in `diffModel.ts` (pure, tested).
//
// Line notes: the user notes a diff line and pastes the batch into a pane.
// Anchors and the prompt text are in `diffNotes.ts` (pure, tested). The rows
// are rebuilt on every diff event, so the editor and the send controls live
// in a tray outside the rows and the keyboard cursor is held by anchor.
//
// PR status: `PrStatusCard` under the worktree strip shows the PR of the
// pane's branch and offers "Fix broken checks". It shares the pane picker.

import { createSelector, createSignal, createMemo, createEffect, on, For, onCleanup, onMount, Show } from "solid-js";
import { backend, type UnlistenFn } from "./backend";
import type { DiffSource } from "./bindings/DiffSource";
import type { WorktreeEntry } from "./bindings/WorktreeEntry";
import type { LayoutNode } from "./types";
import { t } from "./i18n";
import { keyEq } from "./shortcuts";
import { TechText } from "./TechText";
import { parseDiff, statusLetter, pathKey, type LineRef } from "./diffModel";
import {
  NOTE_MAX_CHARS,
  buildNotesPrompt,
  diffTargetKey,
  indexLines,
  noteKey,
  noteStatus,
  sanitizeNote,
  type DiffNote,
  type DiffSendResult,
  type DiffSendTarget,
} from "./diffNotes";
import {
  IconGitCompare,
  IconGitBranch,
  IconChevronDown,
  IconClose,
  IconPencil,
  IconPlus,
  IconRefresh,
  IconExternalLink,
  IconTrash,
} from "./icons";
import { createLogger } from "./logger";
import { PaneTargetSelect } from "./PaneTargetSelect";
import { PrStatusCard } from "./PrStatusCard";

const log = createLogger("DIFF");

// Module scope on purpose: a workspace switch unmounts LayoutView and with it
// this pane, and the notes must still be there when the user comes back.
const [notesByTarget, setNotesByTarget] = createSignal<Record<string, DiffNote[]>>({});
let nextNoteId = 1;

interface NoteDraft extends LineRef {
  targetKey: string;
  lineText: string;
  /** The note being edited, or null for a new note. */
  editingId: string | null;
}

interface StatusEntry {
  xy: string;
  path: string;
  orig_path: string | null;
}

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  isActive: boolean;
  onFocus: (paneId: string) => void;
  onClose: (paneId: string) => void;
  workspaceCwd?: string;
  /** Bumped by App after a worktree is created, so the strip re-lists. */
  worktreesVersion: number;
  onOpenWorktree: (workspaceId: string, wt: WorktreeEntry) => void;
  onNewWorktree: (workspaceId: string) => void;
  /** Feed the listing back to App so sidebar cards can show a branch. */
  onWorktreesListed: (workspaceId: string, entries: WorktreeEntry[]) => void;
  /** Connected terminal panes of this workspace. */
  sendTargets: DiffSendTarget[];
  onSendPrompt: (paneId: string, text: string) => Promise<DiffSendResult>;
}

function describeSource(s: DiffSource): string {
  switch (s.kind) {
    case "working": return t("diff.pane.source.working");
    case "head": return t("diff.pane.source.head");
    case "ref": return t("diff.pane.source.ref");
  }
}

export function DiffPane(p: Props) {
  let bodyRef!: HTMLDivElement;
  const initialSource: DiffSource =
    (p.pane.diff_source as DiffSource | null) ?? { kind: "head" };
  const [source, setSource] = createSignal<DiffSource>(initialSource);
  const [diffText, setDiffText] = createSignal<string>("");
  const [files, setFiles] = createSignal<StatusEntry[]>([]);
  const [error, setError] = createSignal<string | null>(null);
  const [cwd, setCwd] = createSignal<string>("");
  const [branch, setBranch] = createSignal<string | null>(null);
  const [truncated, setTruncated] = createSignal<boolean>(false);
  const [busy, setBusy] = createSignal<boolean>(false);
  const [hunkIdx, setHunkIdx] = createSignal<number>(0);
  const [refDraft, setRefDraft] = createSignal<string>(
    initialSource.kind === "ref" ? initialSource.git_ref : "",
  );
  const [refEditing, setRefEditing] = createSignal<boolean>(false);
  const [menuOpen, setMenuOpen] = createSignal<boolean>(false);
  const [worktrees, setWorktrees] = createSignal<WorktreeEntry[]>([]);
  const [wtError, setWtError] = createSignal<string | null>(null);

  const [draft, setDraft] = createSignal<NoteDraft | null>(null);
  const [draftText, setDraftText] = createSignal<string>("");
  const [cursor, setCursor] = createSignal<LineRef | null>(null);
  const [targetPane, setTargetPane] = createSignal<string>("");
  const [sending, setSending] = createSignal<boolean>(false);
  const [sendStatus, setSendStatus] = createSignal<{ ok: boolean; msg: string } | null>(null);
  const [prNonce, setPrNonce] = createSignal<number>(0);
  let draftRef: HTMLTextAreaElement | undefined;

  const parsed = createMemo(() => parseDiff(diffText()));
  const isEmpty = () => !error() && diffText().trim().length === 0 && files().length === 0;

  const lineIndex = createMemo(() => indexLines(parsed()));
  const noteableRows = createMemo(() => parsed().refs.flatMap((r, i) => (r ? [i] : [])));
  const cursorRow = createMemo(() => {
    const c = cursor();
    return c ? (lineIndex().get(noteKey(c))?.idx ?? -1) : -1;
  });
  const isCursorRow = createSelector(cursorRow);

  const targetKey = () => diffTargetKey(p.workspaceId, cwd(), source());
  const notes = (): DiffNote[] => notesByTarget()[targetKey()] ?? [];
  const notesWithStatus = createMemo(() =>
    notes().map((note) => ({ note, status: noteStatus(note, lineIndex()) })),
  );
  const notesOnLine = createMemo(() => {
    const out = new Map<string, DiffNote[]>();
    for (const { note, status } of notesWithStatus()) {
      if (status !== "current") continue;
      const k = noteKey(note);
      out.set(k, [...(out.get(k) ?? []), note]);
    }
    return out;
  });
  const prompt = createMemo(() =>
    buildNotesPrompt(notesWithStatus(), { cwd: cwd(), branch: branch(), source: source() }),
  );
  const validTarget = () => p.sendTargets.some((x) => x.paneId === targetPane());
  const canSend = () => !sending() && !error() && prompt().length > 0 && validTarget();

  const updateNotes = (key: string, fn: (list: DiffNote[]) => DiffNote[]) => {
    setNotesByTarget((all) => ({ ...all, [key]: fn(all[key] ?? []) }));
    setSendStatus(null);
  };

  const closeDraft = () => {
    setDraft(null);
    setDraftText("");
    bodyRef?.focus();
  };

  const openDraft = (next: NoteDraft, text: string) => {
    setDraft(next);
    setDraftText(text);
    queueMicrotask(() => draftRef?.focus());
  };

  const startNoteAt = (idx: number) => {
    const ref = parsed().refs[idx];
    if (!ref) return;
    setCursor(ref);
    openDraft({ ...ref, targetKey: targetKey(), lineText: parsed().lines[idx].text, editingId: null }, "");
  };

  const editNote = (n: DiffNote) =>
    openDraft({ file: n.file, side: n.side, line: n.line, targetKey: targetKey(), lineText: n.lineText, editingId: n.id }, n.text);

  const saveDraft = () => {
    const d = draft();
    const text = sanitizeNote(draftText());
    if (!d || !text) return;
    updateNotes(d.targetKey, (list) =>
      d.editingId
        ? list.map((n) => (n.id === d.editingId ? { ...n, text } : n))
        : [...list, { id: String(nextNoteId++), file: d.file, side: d.side, line: d.line, lineText: d.lineText, text }],
    );
    closeDraft();
  };

  const deleteNote = (id: string) => updateNotes(targetKey(), (list) => list.filter((n) => n.id !== id));

  const send = async () => {
    if (!canSend()) return;
    setSending(true);
    setSendStatus(null);
    try {
      // Rule #1: this text is pane input. Never log it.
      const result = await p.onSendPrompt(targetPane(), prompt());
      setSendStatus(
        result === "sent" ? { ok: true, msg: t("diff.notes.sent") }
        : result === "no_bracketed_paste" ? { ok: false, msg: t("diff.notes.errNoPaste") }
        : { ok: false, msg: t("diff.notes.errNotConnected") },
      );
    } catch (e) {
      log.error("send notes failed", e);
      setSendStatus({ ok: false, msg: t("diff.notes.errSend", { err: String(e) }) });
    } finally {
      setSending(false);
    }
  };

  /** The first line that can take a note, at or after a row. */
  const firstNoteableFrom = (idx: number): number =>
    noteableRows().find((i) => i >= idx) ?? -1;

  const currentHunkHeader = () => parsed().hunks[hunkIdx()]?.headerIdx ?? 0;

  const moveCursor = (delta: number) => {
    const rows = noteableRows();
    if (rows.length === 0) return;
    const at = rows.indexOf(cursorRow());
    const next = at < 0
      ? firstNoteableFrom(currentHunkHeader())
      : rows[Math.max(0, Math.min(rows.length - 1, at + delta))];
    if (next < 0) return;
    setCursor(parsed().refs[next]);
    scrollToLine(next, "nearest");
  };

  const apply = async (next: DiffSource) => {
    setSource(next);
    setBusy(true);
    try {
      await backend.call("diff_pane_set_source", { paneId: p.pane.pane_id, source: next });
    } catch (e) {
      log.error("diff_pane_set_source failed", e);
    } finally {
      setBusy(false);
    }
  };

  const refresh = async () => {
    setBusy(true);
    setPrNonce((n) => n + 1);
    try {
      await backend.call("diff_pane_refresh", { paneId: p.pane.pane_id });
    } catch {
      // diff_pane_refresh emits an error event too; ignore the rejection.
    } finally {
      setBusy(false);
    }
  };

  const listWorktrees = async () => {
    try {
      const list = await backend.call<WorktreeEntry[]>("diff_pane_worktrees", {
        paneId: p.pane.pane_id,
      });
      setWorktrees(list);
      setWtError(null);
      p.onWorktreesListed(p.workspaceId, list);
    } catch (e) {
      setWtError(String(e));
    }
  };

  const selectWorktree = async (wt: WorktreeEntry) => {
    const back = pathKey(wt.path) === pathKey(p.workspaceCwd ?? "");
    try {
      await backend.call("diff_pane_set_cwd", {
        paneId: p.pane.pane_id,
        cwd: back ? null : wt.path,
      });
    } catch (e) {
      log.error("diff_pane_set_cwd failed", e);
    }
  };

  const isCurrentWorktree = (wt: WorktreeEntry) =>
    !!cwd() && pathKey(wt.path) === pathKey(cwd());

  // ↑/↓ (and j/k, Hebrew-layout-safe) jump between hunks. With Shift they
  // move the line cursor, and N opens a note on the cursor's line.
  const onBodyKey = (e: KeyboardEvent) => {
    const total = parsed().hunks.length;
    if (total === 0) return;
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (!e.shiftKey && keyEq(e, "n")) {
      e.preventDefault();
      const row = cursorRow() >= 0 ? cursorRow() : firstNoteableFrom(currentHunkHeader());
      if (row >= 0) startNoteAt(row);
      return;
    }
    let delta = 0;
    if (e.key === "ArrowDown" || keyEq(e, "j")) delta = 1;
    else if (e.key === "ArrowUp" || keyEq(e, "k")) delta = -1;
    else return;
    e.preventDefault();
    if (e.shiftKey) {
      moveCursor(delta);
      return;
    }
    const next = Math.max(0, Math.min(total - 1, hunkIdx() + delta));
    setHunkIdx(next);
    const header = parsed().hunks[next]?.headerIdx;
    scrollToLine(header);
    if (header != null) setCursor(parsed().refs[firstNoteableFrom(header)] ?? null);
  };

  const scrollToLine = (idx: number | undefined, block: ScrollLogicalPosition = "center") => {
    if (idx == null) return;
    const el = bodyRef?.querySelector(`[data-line-idx="${idx}"]`) as HTMLElement | null;
    if (el) el.scrollIntoView({ block, behavior: "smooth" });
  };

  const jumpToFile = (path: string) => {
    const idx = parsed().anchors[path];
    if (idx != null) scrollToLine(idx);
  };

  onMount(() => {
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void (async () => {
      try {
        unlisten = await backend.on<{
          pane_id: string;
          diff_text: string;
          files: StatusEntry[];
          error: string | null;
          cwd: string;
          branch: string | null;
          truncated: boolean;
        }>("diff-pane-updated", (event) => {
          if (event.payload.pane_id !== p.pane.pane_id) return;
          const hadError = !!wtError();
          // Preserve scroll across the wholesale re-render.
          const top = bodyRef?.scrollTop ?? 0;
          setError(event.payload.error);
          setDiffText(event.payload.diff_text);
          setFiles(event.payload.files ?? []);
          setCwd(event.payload.cwd ?? "");
          setBranch(event.payload.branch ?? null);
          setTruncated(!!event.payload.truncated);
          const total = parseDiff(event.payload.diff_text).hunks.length;
          if (hunkIdx() >= total) setHunkIdx(0);
          queueMicrotask(() => {
            if (bodyRef) bodyRef.scrollTop = top;
          });
          // An SSH host that was down when we first listed worktrees is now
          // reachable (a real diff arrived) — retry the strip.
          if (!event.payload.error && hadError) void listWorktrees();
        });
      } catch (e) {
        log.warn("listen failed", e);
      }
      if (disposed) {
        try { unlisten?.(); } catch {}
        return;
      }
      // Start the watcher only after the listener is armed (the first emit
      // is hash-gated and never repeats).
      try { await backend.call("diff_pane_start", { paneId: p.pane.pane_id }); }
      catch (e) { log.error("diff_pane_start failed", e); }
      void listWorktrees();
    })();
    onCleanup(() => {
      disposed = true;
      try { unlisten?.(); } catch {}
      void backend.call("diff_pane_stop", { paneId: p.pane.pane_id }).catch(() => {});
    });
  });

  // Re-list when App signals a worktree was created.
  createEffect(on(() => p.worktreesVersion, () => void listWorktrees(), { defer: true }));

  return (
    <div
      class={`pane diff-pane ${p.isActive ? "active" : ""}`}
      onMouseDown={() => p.onFocus(p.pane.pane_id)}
    >
      <div class="pane-header">
        <span class="pane-conn">
          <IconGitCompare size={14} />{" "}
          <Show when={branch()} fallback={<>diff</>}>
            <span class="diff-pane-branch"><IconGitBranch size={12} /> <TechText text={branch()!} /></span>
          </Show>
        </span>
        <div class="diff-pane-source">
          <button class="ws-header-btn" disabled={busy()} onClick={() => setMenuOpen(!menuOpen())}>
            {describeSource(source())}
            <Show when={source().kind === "ref"}>
              {" "}<TechText text={(source() as { kind: "ref"; git_ref: string }).git_ref} />
            </Show>
            {" "}<IconChevronDown size={13} />
          </button>
          <Show when={menuOpen()}>
            <div class="diff-pane-menu">
              <button onClick={() => { setMenuOpen(false); void apply({ kind: "head" }); }}>
                {t("diff.pane.source.head")}
              </button>
              <button onClick={() => { setMenuOpen(false); void apply({ kind: "working" }); }}>
                {t("diff.pane.source.working")}
              </button>
              <button onClick={() => { setMenuOpen(false); setRefEditing(true); }}>
                {t("diff.pane.source.ref")}…
              </button>
            </div>
          </Show>
        </div>
        <button class="ws-header-btn" disabled={busy()} onClick={() => void refresh()}>
          {t("diff.pane.refresh")}
        </button>
        <button
          class="pane-btn pane-close"
          title={t("common.close")}
          onClick={(e) => { e.stopPropagation(); p.onClose(p.pane.pane_id); }}
        >
          <IconClose size={14} />
        </button>
      </div>

      {/* Worktree strip */}
      <div class="diff-wt-strip">
        <span class="diff-wt-title" title={t("diff.wt.title")}><IconGitBranch size={12} /></span>
        <For each={worktrees()}>
          {(wt) => (
            <span
              class={`diff-wt ${isCurrentWorktree(wt) ? "current" : ""}`}
              title={wt.path}
            >
              <button class="diff-wt-name" onClick={() => void selectWorktree(wt)}>
                <TechText text={wt.branch ?? (wt.is_detached ? wt.head.slice(0, 7) : "—")} />
                <Show when={wt.is_main}><span class="diff-wt-main">{t("diff.wt.main")}</span></Show>
                <Show when={wt.is_locked}><span class="diff-wt-flag" title={t("pf.locked")}>🔒</span></Show>
                <Show when={wt.is_prunable}><span class="diff-wt-flag" title={t("pf.prunable")}>⚠</span></Show>
              </button>
              <button
                class="diff-wt-open"
                title={t("diff.wt.open")}
                onClick={() => p.onOpenWorktree(p.workspaceId, wt)}
              >
                <IconExternalLink size={11} />
              </button>
            </span>
          )}
        </For>
        <Show when={wtError()}>
          <span class="diff-wt-err">{wtError()}</span>
        </Show>
        <span class="diff-wt-actions">
          <button class="diff-wt-add" title={t("pf.newWorktree")} onClick={() => p.onNewWorktree(p.workspaceId)}>
            <IconPlus size={12} />
          </button>
          <button class="diff-wt-add" title={t("diff.wt.reload")} onClick={() => void listWorktrees()}>
            <IconRefresh size={12} />
          </button>
        </span>
      </div>

      <Show when={backend.can("prStatus")}>
        <PrStatusCard
          paneId={p.pane.pane_id}
          cwd={cwd()}
          branch={branch()}
          enabled={!error()}
          refreshNonce={prNonce()}
          sendTargets={p.sendTargets}
          targetPane={targetPane()}
          onTargetPane={setTargetPane}
          onSendPrompt={p.onSendPrompt}
        />
      </Show>

      <Show when={refEditing()}>
        <div class="diff-pane-ref-row">
          <input
            type="text"
            value={refDraft()}
            placeholder="HEAD~1 / main / abc123"
            onInput={(e) => setRefDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const v = refDraft().trim();
                if (v) { setRefEditing(false); void apply({ kind: "ref", git_ref: v }); }
              } else if (e.key === "Escape") {
                setRefEditing(false);
              }
            }}
          />
          <button onClick={() => {
            const v = refDraft().trim();
            if (v) { setRefEditing(false); void apply({ kind: "ref", git_ref: v }); }
          }}>{t("common.save")}</button>
          <button onClick={() => setRefEditing(false)}>{t("common.cancel")}</button>
        </div>
      </Show>

      {/* Changed-file list */}
      <Show when={files().length > 0}>
        <div class="diff-files" title={t("diff.files.title")}>
          <For each={files()}>
            {(f) => (
              <button class="diff-file" onClick={() => jumpToFile(f.path)} title={f.orig_path ? `${f.orig_path} → ${f.path}` : f.path}>
                <span class="diff-file-letter" data-k={statusLetter(f.xy)}>{statusLetter(f.xy)}</span>
                <span class="diff-file-path"><TechText text={f.path} /></span>
              </button>
            )}
          </For>
        </div>
      </Show>

      <div
        class="pane-body diff-pane-body"
        ref={(el) => (bodyRef = el)}
        tabIndex={0}
        aria-label={t("diff.notes.bodyLabel")}
        onKeyDown={onBodyKey}
      >
        <Show when={error()}>
          <p class="diff-pane-msg diff-pane-error">{error()}</p>
        </Show>
        <Show when={!error() && isEmpty()}>
          <p class="diff-pane-msg">{t("diff.pane.empty")}</p>
        </Show>
        <Show when={!error() && !isEmpty()}>
          <pre class="diff-pane-pre">
            <For each={parsed().lines}>
              {(line, idx) => {
                // A parse makes new line objects, so a row never outlives
                // the ref it reads here.
                const ref = parsed().refs[idx()];
                return (
                  <>
                    <div
                      class={`dl dl-${line.kind}`}
                      classList={{ "dl-cursor": isCursorRow(idx()) }}
                      data-line-idx={idx()}
                    >
                      <span class="dl-note-slot">
                        {/* Mouse only: the keyboard path is Shift+arrows and N,
                            so one Tab stop serves the whole diff. */}
                        <Show when={ref}>
                          <button
                            class="dl-note-add"
                            tabIndex={-1}
                            title={t("diff.notes.add")}
                            aria-label={t("diff.notes.add")}
                            onClick={() => startNoteAt(idx())}
                          >
                            <IconPlus size={11} />
                          </button>
                        </Show>
                      </span>
                      <span class="dl-gutter">
                        {line.kind === "add" ? "+" :
                         line.kind === "del" ? "-" :
                         line.kind === "hunk" ? "@" :
                         line.kind === "file" ? "·" : " "}
                      </span>
                      <span class="dl-text">{line.text}</span>
                    </div>
                    <Show when={ref && notesOnLine().get(noteKey(ref))}>
                      {(list) => (
                        <div class="dl-notes">
                          <For each={list()}>
                            {(n) => <div class="dl-note" dir="auto">{n.text}</div>}
                          </For>
                        </div>
                      )}
                    </Show>
                  </>
                );
              }}
            </For>
          </pre>
          <Show when={truncated()}>
            <p class="diff-pane-msg">{t("diff.pane.truncated")}</p>
          </Show>
        </Show>
      </div>

      <div class="sr-only" role="status">
        {(() => {
          const c = cursor();
          const row = cursorRow();
          return c && row >= 0 ? `${c.file}:${c.line} ${parsed().lines[row].text}` : "";
        })()}
      </div>

      <Show when={draft() || notes().length > 0}>
        <section class="diff-notes" aria-label={t("diff.notes.title", { count: notes().length })}>
          <Show when={draft()}>
            {(d) => (
              <div class="diff-note-editor">
                <span class="diff-note-ref" dir="ltr">
                  <TechText text={`${d().file}:${d().line}`} /> ({t(`diff.notes.side.${d().side}`)})
                </span>
                <textarea
                  ref={(el) => (draftRef = el)}
                  dir="auto"
                  rows={3}
                  maxLength={NOTE_MAX_CHARS}
                  aria-label={t("diff.notes.noteLabel")}
                  placeholder={t("diff.notes.placeholder")}
                  value={draftText()}
                  onInput={(e) => setDraftText(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.isComposing) return;
                    if (e.key === "Escape") {
                      e.preventDefault();
                      closeDraft();
                    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
                      e.preventDefault();
                      saveDraft();
                    }
                  }}
                />
                <div class="diff-notes-row">
                  <button disabled={!sanitizeNote(draftText())} onClick={saveDraft}>{t("common.save")}</button>
                  <button onClick={closeDraft}>{t("common.cancel")}</button>
                  <span class="diff-notes-hint">{t("diff.notes.editorHint")}</span>
                </div>
              </div>
            )}
          </Show>

          <Show when={notes().length > 0}>
            <h3 class="diff-notes-title">{t("diff.notes.title", { count: notes().length })}</h3>
            <ul class="diff-notes-list">
              <For each={notesWithStatus()}>
                {(entry) => (
                  <li class="diff-note-item">
                    <button
                      class="diff-note-ref"
                      dir="ltr"
                      title={t("diff.notes.jump")}
                      disabled={!lineIndex().has(noteKey(entry.note))}
                      onClick={() => {
                        setCursor(entry.note);
                        scrollToLine(lineIndex().get(noteKey(entry.note))?.idx);
                      }}
                    >
                      <TechText text={`${entry.note.file}:${entry.note.line}`} /> ({t(`diff.notes.side.${entry.note.side}`)})
                    </button>
                    <Show when={entry.status === "outdated"}>
                      <span class="diff-note-outdated" title={t("diff.notes.outdatedHint")}>
                        {t("diff.notes.outdated")}
                      </span>
                    </Show>
                    <span class="diff-note-text" dir="auto">{entry.note.text}</span>
                    <button
                      class="diff-note-btn"
                      title={t("diff.notes.edit")}
                      aria-label={t("diff.notes.edit")}
                      onClick={() => editNote(entry.note)}
                    >
                      <IconPencil size={12} />
                    </button>
                    <button
                      class="diff-note-btn"
                      title={t("diff.notes.delete")}
                      aria-label={t("diff.notes.delete")}
                      onClick={() => deleteNote(entry.note.id)}
                    >
                      <IconTrash size={12} />
                    </button>
                  </li>
                )}
              </For>
            </ul>

            <span class="diff-notes-label" id={`diff-notes-preview-${p.pane.pane_id}`}>{t("diff.notes.preview")}</span>
            {/* A per-line base direction: a Hebrew note stays readable
                between English frame lines. Focusable so the keyboard can
                scroll it. */}
            <pre
              class="diff-notes-preview"
              tabIndex={0}
              role="region"
              aria-labelledby={`diff-notes-preview-${p.pane.pane_id}`}
            >{prompt()}</pre>

            <div class="diff-notes-row">
              <PaneTargetSelect targets={p.sendTargets} value={targetPane()} onChange={setTargetPane} />
              <button disabled={!canSend()} onClick={() => void send()}>{t("diff.notes.send")}</button>
            </div>
            <p class="diff-notes-hint">{t("diff.notes.sendHint")}</p>
            <div role="status">
              <Show when={sendStatus()}>
                {(s) => (
                  <p class={s().ok ? "diff-notes-hint" : "diff-notes-err"}>
                    {s().msg}{" "}
                    <Show when={s().ok}>
                      <button onClick={() => updateNotes(targetKey(), () => [])}>{t("diff.notes.clear")}</button>
                    </Show>
                  </p>
                )}
              </Show>
            </div>
          </Show>
        </section>
      </Show>
    </div>
  );
}
