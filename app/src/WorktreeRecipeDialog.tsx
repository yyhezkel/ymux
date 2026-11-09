import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import { Portal } from "solid-js/web";
import { backend } from "./backend";
import { t } from "./i18n";
import { IconClose, IconWarning } from "./icons";
import type { WorktreeRecipePreview, WorktreeRecipeResult } from "./types";

// The trust prompt for a repo's `.ymux/worktree.json`, and the outcome of
// running it. It mounts after the worktree exists and renders nothing while it
// reads the recipe, so a repo with no recipe shows no dialog. The worktree
// stays in place on every path out of here: Skip, a refused recipe, or a
// failing setup. The backend enforces the approval; this dialog only asks.

type Phase =
  | { kind: "loading" }
  | { kind: "prompt"; preview: WorktreeRecipePreview }
  | { kind: "running" }
  | { kind: "result"; result: WorktreeRecipeResult }
  | { kind: "error"; message: string };

interface Props {
  workspaceId: string;
  branch: string;
  /** The dialog is done. Called exactly once. */
  onFinished: () => void;
}

function isClean(r: WorktreeRecipeResult): boolean {
  return (
    r.copies.every((c) => c.status === "copied") &&
    (r.setup_status === "none" || r.setup_status === "succeeded")
  );
}

export function WorktreeRecipeDialog(p: Props) {
  const [phase, setPhase] = createSignal<Phase>({ kind: "loading" });

  const run = async (sha256: string, approve: boolean) => {
    setPhase({ kind: "running" });
    try {
      const result = await backend.call<WorktreeRecipeResult>("worktree_recipe_run", {
        workspaceId: p.workspaceId,
        branch: p.branch,
        sha256,
        approve,
      });
      if (isClean(result)) p.onFinished();
      else setPhase({ kind: "result", result });
    } catch (e) {
      setPhase({ kind: "error", message: String(e) });
    }
  };

  onMount(() => {
    void (async () => {
      try {
        const preview = await backend.call<WorktreeRecipePreview | null>(
          "worktree_recipe_preview",
          { workspaceId: p.workspaceId, branch: p.branch },
        );
        if (!preview) p.onFinished();
        else if (preview.approved) void run(preview.sha256, false);
        else setPhase({ kind: "prompt", preview });
      } catch (e) {
        setPhase({ kind: "error", message: String(e) });
      }
    })();

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      const kind = phase().kind;
      if (kind === "loading" || kind === "running") return;
      e.preventDefault();
      e.stopPropagation();
      p.onFinished();
    };
    window.addEventListener("keydown", onKey, true);
    onCleanup(() => window.removeEventListener("keydown", onKey, true));
  });

  const preview = () => {
    const ph = phase();
    return ph.kind === "prompt" ? ph.preview : null;
  };
  const result = () => {
    const ph = phase();
    return ph.kind === "result" ? ph.result : null;
  };
  const errorMessage = () => {
    const ph = phase();
    return ph.kind === "error" ? ph.message : null;
  };

  const title = () => {
    switch (phase().kind) {
      case "prompt":
        return t("wt.recipe.title");
      case "running":
        return t("wt.recipe.running");
      default:
        return t("wt.recipe.resultTitle");
    }
  };

  const setupSummary = (r: WorktreeRecipeResult): string | null => {
    switch (r.setup_status) {
      case "failed":
        return r.setup_exit_code === null
          ? t("wt.recipe.setupNoExit")
          : t("wt.recipe.setupFailed", { code: r.setup_exit_code });
      case "succeeded":
        return t("wt.recipe.setupOk");
      case "not_run":
        return t("wt.recipe.setupNotRun");
      case "unsupported":
        return t("wt.recipe.setupUnsupported");
      default:
        return null;
    }
  };

  const focusOnMount = (el: HTMLElement) => queueMicrotask(() => el.focus());

  return (
    <Show when={phase().kind !== "loading"}>
      <Portal>
        <div class="ticket-modal-backdrop" onClick={(e) => e.stopPropagation()}>
          <div
            class="ticket-modal pf-modal"
            onClick={(e) => e.stopPropagation()}
            role="dialog"
            aria-modal="true"
            aria-label={title()}
          >
            <div class="ticket-modal-header">
              <span class="ticket-modal-title">
                <IconWarning size={14} /> {title()}
              </span>
              <Show when={phase().kind !== "running"}>
                <button
                  class="ticket-modal-x"
                  onClick={p.onFinished}
                  title={t("common.close")}
                  aria-label={t("common.close")}
                >
                  <IconClose size={14} />
                </button>
              </Show>
            </div>

            <div class="ticket-modal-body">
              <Show when={preview()}>
                {(pv) => (
                  <>
                    <div class="ticket-modal-err" role="alert">
                      {t("wt.recipe.warning")}
                    </div>
                    <div class="ticket-modal-field">
                      <span class="ticket-modal-label">{t("wt.recipe.host")}</span>
                      <code class="wt-recipe-code" dir="ltr">{pv().host}</code>
                    </div>
                    <div class="ticket-modal-field">
                      <span class="ticket-modal-label">{t("wt.recipe.repo")}</span>
                      <code class="wt-recipe-code" dir="ltr">{pv().repo}</code>
                    </div>
                    <div class="ticket-modal-field">
                      <span class="ticket-modal-label">{t("wt.recipe.worktree")}</span>
                      <code class="wt-recipe-code" dir="ltr">{pv().worktree}</code>
                    </div>
                    <div class="ticket-modal-field">
                      <span class="ticket-modal-label">{t("wt.recipe.files")}</span>
                      <Show
                        when={pv().copy.length > 0}
                        fallback={<div class="pane-meta-hint">{t("wt.recipe.none")}</div>}
                      >
                        <ul class="wt-recipe-list" dir="ltr">
                          <For each={pv().copy}>{(f) => <li><code>{f}</code></li>}</For>
                        </ul>
                      </Show>
                    </div>
                    <div class="ticket-modal-field">
                      <span class="ticket-modal-label">{t("wt.recipe.command")}</span>
                      <Show
                        when={pv().setup}
                        fallback={<div class="pane-meta-hint">{t("wt.recipe.none")}</div>}
                      >
                        <pre class="wt-recipe-code" dir="ltr">{pv().setup}</pre>
                        <Show when={!pv().setup_supported}>
                          <div class="pane-meta-hint">{t("wt.recipe.windowsNote")}</div>
                        </Show>
                      </Show>
                    </div>
                    <div class="ticket-modal-field">
                      <span class="ticket-modal-label">SHA-256</span>
                      <code class="wt-recipe-code" dir="ltr">{pv().sha256}</code>
                    </div>
                  </>
                )}
              </Show>

              <Show when={phase().kind === "running"}>
                <div class="pane-meta-hint" role="status">{t("wt.recipe.runningHint")}</div>
              </Show>

              <Show when={errorMessage()}>
                {(msg) => (
                  <>
                    <div class="ticket-modal-err" role="alert">
                      <IconWarning size={13} /> {msg()}
                    </div>
                    <div class="pane-meta-hint">{t("wt.recipe.kept")}</div>
                  </>
                )}
              </Show>

              <Show when={result()}>
                {(r) => (
                  <>
                    <Show when={r().copies.length > 0}>
                      <ul class="wt-recipe-list" dir="ltr">
                        <For each={r().copies}>
                          {(c) => (
                            <li>
                              <code>{c.path}</code> — <bdi>{t(`wt.recipe.status.${c.status}`)}</bdi>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                    <Show when={setupSummary(r())}>
                      {(s) => (
                        <div
                          class={r().setup_status === "succeeded" ? "pane-meta-hint" : "ticket-modal-err"}
                          role={r().setup_status === "failed" ? "alert" : undefined}
                        >
                          {s()}
                        </div>
                      )}
                    </Show>
                    <Show when={r().setup_error}>
                      {(msg) => <div class="ticket-modal-err" role="alert">{msg()}</div>}
                    </Show>
                    <Show when={r().setup_output}>
                      {(o) => <pre class="wt-recipe-code wt-recipe-output" dir="ltr">{o()}</pre>}
                    </Show>
                    <div class="pane-meta-hint">{t("wt.recipe.kept")}</div>
                  </>
                )}
              </Show>
            </div>

            <div class="ticket-modal-footer">
              <Show when={preview()}>
                {(pv) => (
                  <>
                    <button class="ticket-modal-cancel" ref={focusOnMount} onClick={p.onFinished}>
                      {t("wt.recipe.skip")}
                    </button>
                    <button class="ticket-modal-save" onClick={() => void run(pv().sha256, true)}>
                      {t("wt.recipe.approve")}
                    </button>
                  </>
                )}
              </Show>
              <Show when={result() || errorMessage()}>
                <button class="ticket-modal-save" ref={focusOnMount} onClick={p.onFinished}>
                  {t("common.close")}
                </button>
              </Show>
            </div>
          </div>
        </div>
      </Portal>
    </Show>
  );
}
