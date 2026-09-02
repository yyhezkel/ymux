import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { t } from "./i18n";
import { IconClose, IconFolder, IconGitBranch, IconWarning } from "./icons";
import type { Connection, Workspace, WorktreeEntry } from "./types";

// Two small dialogs over the same chrome:
//
//   mode "pin"      — type a repo path (WSL only; see below)
//   mode "worktree" — create a new worktree inside a pinned folder
//
// Both talk to a host that is usually NOT this machine, so neither one
// guesses whether a path is valid: the backend probe is the validation
// step and its message comes back verbatim. A directory that exists but
// has no git repo is NOT an error — it pins as a plain folder (demoted,
// no worktree list) and can be promoted later via "Check for a git
// repository" in the sidebar.
//
// On "pin": SSH workspaces do NOT come here — they get the real remote
// browser (DirPicker.tsx, over SFTP), and Local gets the native dialog.
// This mode is the WSL fallback, which has neither: `file_list_remote`
// is SFTP-only and `WSL_CAPS.fileTransfer` is false, so there is nothing
// to browse with and the path is typed. Deleting the mode outright, as
// the plan first called for, would leave WSL no way to pin at all.

export type ProjectFolderModalMode =
  | { kind: "pin"; workspaceId: string; connection: Connection | null }
  | { kind: "worktree"; workspace: Workspace };

interface Props {
  mode: ProjectFolderModalMode;
  onClose: () => void;
  /** Fired after a successful pin/create so the caller can reload. */
  onDone: () => void;
}

export function ProjectFolderModal(p: Props) {
  const [path, setPath] = createSignal("");
  const [branch, setBranch] = createSignal("");
  const [base, setBase] = createSignal("main");
  const [target, setTarget] = createSignal("");
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);

  const isPin = () => p.mode.kind === "pin";
  const isLocal = () =>
    p.mode.kind === "pin" &&
    (p.mode.connection === null || p.mode.connection.type === "local");

  // Suggest a sibling directory, mirroring the backend's default so the
  // user sees the real destination before confirming rather than after.
  const suggestTarget = (branchName: string): string => {
    if (p.mode.kind !== "worktree") return "";
    const safe = branchName
      .replace(/[^A-Za-z0-9\-_./]/g, "-")
      .replace(/\//g, "-");
    if (!safe) return "";
    const norm = (p.mode.workspace.cwd ?? "").replace(/\\/g, "/").replace(/\/+$/, "");
    const i = norm.lastIndexOf("/");
    if (i === -1) return `${norm}-${safe}`;
    const parent = norm.slice(0, i);
    const bn = norm.slice(i + 1);
    return parent ? `${parent}/${bn}-${safe}` : `/${bn}-${safe}`;
  };

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !busy()) {
        e.preventDefault();
        e.stopPropagation();
        p.onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  const browse = async () => {
    const picked = await openDialog({ directory: true, multiple: false });
    if (typeof picked === "string") setPath(picked);
  };

  const submitPin = async () => {
    if (p.mode.kind !== "pin") return;
    const value = path().trim();
    if (!value) return;
    setBusy(true);
    setError(null);
    try {
      // Validate before persisting: a path that does not exist (or a
      // host with no live session) fails here instead of landing a dead
      // row in the sidebar. "No git there" is an answer, not a failure —
      // the folder pins demoted.
      const isRepo = await invoke<boolean>("project_folder_probe", {
        path: value,
        connection: p.mode.connection,
      });
      await invoke("workspace_pin_project_folder", {
        parentWorkspaceId: p.mode.workspaceId,
        path: value,
        name: null,
        isProjectRoot: isRepo,
      });
      p.onDone();
      p.onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const submitWorktree = async () => {
    if (p.mode.kind !== "worktree") return;
    const b = branch().trim();
    const bb = base().trim();
    if (!b || !bb) return;
    setBusy(true);
    setError(null);
    try {
      await invoke<WorktreeEntry[]>("workspace_create_project_worktree", {
        workspaceId: p.mode.workspace.id,
        branchName: b,
        baseBranch: bb,
        targetPath: target().trim() || null,
      });
      p.onDone();
      p.onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const title = () =>
    isPin()
      ? t("pf.dialog.title")
      : t("pf.wt.title", {
          folder: p.mode.kind === "worktree" ? p.mode.workspace.name : "",
        });

  return (
    <div class="ticket-modal-backdrop" onClick={() => !busy() && p.onClose()}>
      <div
        class="ticket-modal pf-modal"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label={title()}
      >
        <div class="ticket-modal-header">
          <span class="ticket-modal-title">
            {isPin() ? <IconFolder size={14} /> : <IconGitBranch size={14} />} {title()}
          </span>
          <button
            class="ticket-modal-x"
            onClick={p.onClose}
            title={t("common.close")}
            aria-label={t("common.close")}
          >
            <IconClose size={14} />
          </button>
        </div>

        <div class="ticket-modal-body">
          <Show when={isPin()}>
            <div class="ticket-modal-field">
              <span class="ticket-modal-label">{t("pf.dialog.pathLabel")}</span>
              <div class="pf-modal-row">
                <input
                  class="pane-meta-title"
                  value={path()}
                  spellcheck={false}
                  placeholder="/home/you/src/project"
                  ref={(el) => queueMicrotask(() => el.focus())}
                  onInput={(e) => setPath(e.currentTarget.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void submitPin();
                  }}
                />
                <Show when={isLocal()}>
                  <button class="ticket-modal-cancel" onClick={() => void browse()}>
                    {t("pf.dialog.browse")}
                  </button>
                </Show>
              </div>
              <div class="pane-meta-hint">{t("pf.dialog.pathHint")}</div>
            </div>
          </Show>

          <Show when={!isPin()}>
            <div class="ticket-modal-field">
              <span class="ticket-modal-label">{t("pf.wt.branch")}</span>
              <input
                class="pane-meta-title"
                value={branch()}
                spellcheck={false}
                placeholder="feature/x"
                ref={(el) => queueMicrotask(() => el.focus())}
                onInput={(e) => {
                  const v = e.currentTarget.value;
                  setBranch(v);
                  // Keep the suggestion live until the user edits the
                  // target themselves.
                  if (target() === "" || target() === suggestTarget(branch())) {
                    setTarget(suggestTarget(v));
                  }
                }}
              />
            </div>
            <div class="ticket-modal-field">
              <span class="ticket-modal-label">{t("pf.wt.base")}</span>
              <input
                class="pane-meta-title"
                value={base()}
                spellcheck={false}
                onInput={(e) => setBase(e.currentTarget.value)}
              />
            </div>
            <div class="ticket-modal-field">
              <span class="ticket-modal-label">{t("pf.wt.target")}</span>
              <input
                class="pane-meta-title"
                value={target()}
                spellcheck={false}
                placeholder={suggestTarget(branch())}
                onInput={(e) => setTarget(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void submitWorktree();
                }}
              />
              <div class="pane-meta-hint">{t("pf.wt.targetHint")}</div>
            </div>
          </Show>

          <Show when={error()}>
            <div class="ticket-modal-err" role="alert">
              <IconWarning size={13} /> {error()}
            </div>
          </Show>
        </div>

        <div class="ticket-modal-footer">
          <button class="ticket-modal-cancel" onClick={p.onClose} disabled={busy()}>
            {t("common.cancel")}
          </button>
          <button
            class="ticket-modal-save"
            disabled={busy() || (isPin() ? !path().trim() : !branch().trim() || !base().trim())}
            onClick={() => void (isPin() ? submitPin() : submitWorktree())}
          >
            {busy()
              ? isPin()
                ? t("pf.dialog.checking")
                : t("pf.wt.creating")
              : isPin()
                ? t("pf.dialog.pin")
                : t("pf.wt.create")}
          </button>
        </div>
      </div>
    </div>
  );
}
