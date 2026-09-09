import { For, Show, onCleanup, onMount } from "solid-js";
import { t } from "./i18n";
import { IconClose, IconFolder, IconGitBranch, IconWarning } from "./icons";
import { TechText } from "./TechText";
import type { Workspace } from "./types";

// Deleting a workspace now takes its whole subtree — pinned project
// folders and the worktree workspaces under them — and with them any
// live remote sessions. `window.confirm` was carrying that: an unstyled
// grey OS box with the danger described in prose nobody reads. Yossi,
// on the first build that shipped it: "delete just deletes everything,
// with no caution button or anything."
//
// So the warning is the dialog, not a sentence inside one: every
// workspace that dies is listed by name, the ones with live sessions
// are marked, and the destructive button is the one you have to aim at
// while Cancel holds focus.
//
// What it deliberately does NOT claim: that anything on the host is
// removed. ymux never created those directories and never deletes
// them, and saying so is the line that makes the rest readable.
//
// Phase 91 is the one exception, and it is named: a session ROW (a
// workspace opened FOR a tmux/zellij session) takes its session with it —
// killed on the host, not detached. The dialog lists those by name.

interface Props {
  /** The workspace the user clicked, first; then its descendants. */
  subtree: Workspace[];
  /** Ids with a live session — rendered as a per-row badge. */
  liveIds: Set<string>;
  /** Phase 91: the tmux/zellij sessions behind the subtree's session rows.
   *  These are KILLED on the host, not merely detached — the one thing in
   *  this dialog that does reach the host, so it is spelled out. */
  sessionNames: string[];
  /** Notes across the whole subtree; 0 hides the line. */
  noteCount: number;
  onConfirm: () => void;
  onClose: () => void;
}

export function ConfirmDeleteWorkspace(p: Props) {
  const target = () => p.subtree[0];
  const descendants = () => p.subtree.slice(1);
  const liveCount = () => p.subtree.filter((w) => p.liveIds.has(w.id)).length;

  onMount(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        p.onClose();
      }
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  return (
    <div class="modal-backdrop" onClick={p.onClose}>
      <div
        class="modal confirm-delete"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
        role="alertdialog"
        aria-modal="true"
      >
        <div class="settings-head">
          <h3 class="confirm-delete-title">
            <IconWarning size={15} /> {t("workspace.delete.title")}
          </h3>
          <button class="feed-x" title={t("common.close")} onClick={p.onClose}>
            <IconClose size={14} />
          </button>
        </div>

        <div class="confirm-delete-body">
          <p class="confirm-delete-lead">
            {t("workspace.delete.lead")}{" "}
            <strong><TechText text={target()?.name ?? ""} /></strong>
          </p>

          <Show when={descendants().length > 0}>
            <p class="confirm-delete-sub">
              {t("workspace.delete.alsoRemoves", { count: descendants().length })}
            </p>
            <ul class="confirm-delete-list">
              <For each={descendants()}>
                {(w) => (
                  <li class="confirm-delete-row" title={w.cwd ?? undefined}>
                    <span class="confirm-delete-icon">
                      {w.is_project_root ? <IconFolder size={13} /> : <IconGitBranch size={13} />}
                    </span>
                    <span class="confirm-delete-name"><TechText text={w.name} /></span>
                    <Show when={p.liveIds.has(w.id)}>
                      <span class="confirm-delete-live">{t("workspace.delete.live")}</span>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </Show>

          <Show when={liveCount() > 0}>
            <p class="confirm-delete-warn">
              <IconWarning size={13} /> {t("workspace.delete.liveWarning", { count: liveCount() })}
            </p>
          </Show>

          <Show when={p.sessionNames.length > 0}>
            <p class="confirm-delete-warn">
              <IconWarning size={13} />{" "}
              {t("workspace.delete.sessionWarning", {
                count: p.sessionNames.length,
                names: p.sessionNames.join(", "),
              })}
            </p>
          </Show>

          <Show when={p.noteCount > 0}>
            <p class="confirm-delete-warn">
              <IconWarning size={13} /> {t("workspace.delete.notesWarning", { count: p.noteCount })}
            </p>
          </Show>

          <p class="confirm-delete-safe">{t("workspace.delete.hostSafe")}</p>
        </div>

        <div class="modal-buttons">
          {/* Cancel holds focus: the destructive action must be aimed at. */}
          <button ref={(el) => queueMicrotask(() => el.focus())} onClick={p.onClose}>
            {t("common.cancel")}
          </button>
          <button class="danger" onClick={p.onConfirm}>
            {t("workspace.delete.confirmButton")}
          </button>
        </div>
      </div>
    </div>
  );
}
