// Phase 50 / 91.F: the live diff pane.
//
// On mount we subscribe to `diff-pane-updated` FIRST, then call
// `diff_pane_start` (order matters — the first emit is hash-gated and
// never repeats, so a late listener would miss it). The backend bundle
// carries the branch, the changed-file list (incl. untracked), the diff
// text and any error verbatim. A worktree strip along the top switches
// which worktree the pane looks at (`diff_pane_set_cwd`); a file list
// jumps to a file's hunk. Parsing is in `diffModel.ts` (pure, tested).

import { createSignal, createMemo, createEffect, on, For, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { DiffSource } from "./bindings/DiffSource";
import type { WorktreeEntry } from "./bindings/WorktreeEntry";
import type { LayoutNode } from "./types";
import { t } from "./i18n";
import { keyEq } from "./shortcuts";
import { TechText } from "./TechText";
import { parseDiff, statusLetter, pathKey } from "./diffModel";
import {
  IconGitCompare,
  IconGitBranch,
  IconChevronDown,
  IconClose,
  IconPlus,
  IconRefresh,
  IconExternalLink,
} from "./icons";
import { createLogger } from "./logger";

const log = createLogger("DIFF");

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

  const parsed = createMemo(() => parseDiff(diffText()));
  const isEmpty = () => !error() && diffText().trim().length === 0 && files().length === 0;

  const apply = async (next: DiffSource) => {
    setSource(next);
    setBusy(true);
    try {
      await invoke("diff_pane_set_source", { paneId: p.pane.pane_id, source: next });
    } catch (e) {
      log.error("diff_pane_set_source failed", e);
    } finally {
      setBusy(false);
    }
  };

  const refresh = async () => {
    setBusy(true);
    try {
      await invoke("diff_pane_refresh", { paneId: p.pane.pane_id });
    } catch {
      // diff_pane_refresh emits an error event too; ignore the rejection.
    } finally {
      setBusy(false);
    }
  };

  const listWorktrees = async () => {
    try {
      const list = await invoke<WorktreeEntry[]>("diff_pane_worktrees", {
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
      await invoke("diff_pane_set_cwd", {
        paneId: p.pane.pane_id,
        cwd: back ? null : wt.path,
      });
    } catch (e) {
      log.error("diff_pane_set_cwd failed", e);
    }
  };

  const isCurrentWorktree = (wt: WorktreeEntry) =>
    !!cwd() && pathKey(wt.path) === pathKey(cwd());

  // ↑/↓ (and j/k, Hebrew-layout-safe) jump between hunks.
  const onBodyKey = (e: KeyboardEvent) => {
    const total = parsed().hunks.length;
    if (total === 0) return;
    let delta = 0;
    if (e.key === "ArrowDown" || keyEq(e, "j")) delta = 1;
    else if (e.key === "ArrowUp" || keyEq(e, "k")) delta = -1;
    else return;
    e.preventDefault();
    const next = Math.max(0, Math.min(total - 1, hunkIdx() + delta));
    setHunkIdx(next);
    scrollToLine(parsed().hunks[next]?.headerIdx);
  };

  const scrollToLine = (idx: number | undefined) => {
    if (idx == null) return;
    const el = bodyRef?.querySelector(`[data-line-idx="${idx}"]`) as HTMLElement | null;
    if (el) el.scrollIntoView({ block: "center", behavior: "smooth" });
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
        unlisten = await listen<{
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
      try { await invoke("diff_pane_start", { paneId: p.pane.pane_id }); }
      catch (e) { log.error("diff_pane_start failed", e); }
      void listWorktrees();
    })();
    onCleanup(() => {
      disposed = true;
      try { unlisten?.(); } catch {}
      void invoke("diff_pane_stop", { paneId: p.pane.pane_id }).catch(() => {});
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
              {(line, idx) => (
                <div class={`dl dl-${line.kind}`} data-line-idx={idx()}>
                  <span class="dl-gutter">
                    {line.kind === "add" ? "+" :
                     line.kind === "del" ? "-" :
                     line.kind === "hunk" ? "@" :
                     line.kind === "file" ? "·" : " "}
                  </span>
                  <span class="dl-text">{line.text}</span>
                </div>
              )}
            </For>
          </pre>
          <Show when={truncated()}>
            <p class="diff-pane-msg">{t("diff.pane.truncated")}</p>
          </Show>
        </Show>
      </div>
    </div>
  );
}
