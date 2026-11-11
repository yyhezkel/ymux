// The PR card of the Diff pane: the PR of the pane's branch and its checks,
// from `gh pr view` on the workspace's host (`diff_pane_pr_status`), plus
// "Fix broken checks", which pastes one prompt into a pane the user picks.
//
// It polls only while it is mounted and the window is visible: once at
// mount, then every minute, with a back-off on failure (`nextPollSecs`).
// A cwd or branch change and the refresh button fetch at once.
//
// Rule #1: gh output is repository content. Log the state kind and counts
// only. The prompt is pane input and is never logged.

import { createEffect, createMemo, createSignal, on, onCleanup, onMount, Show } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { IconCircleCheck, IconCircleDot, IconExternalLink, IconGitCompare, IconRefresh, IconWarning } from "./icons";
import { createLogger } from "./logger";
import type { DiffSendResult, DiffSendTarget } from "./diffNotes";
import { PaneTargetSelect } from "./PaneTargetSelect";
import {
  buildFixChecksPrompt,
  failingChecks,
  nextPollSecs,
  rollupCounts,
  safeUrl,
  type PrStatus,
} from "./prChecks";

const log = createLogger("PR");

interface Props {
  paneId: string;
  /** The pane's cwd and branch. A change fetches at once. */
  cwd: string;
  branch: string | null;
  /** False while the Diff pane shows an error (not a repo, no session). */
  enabled: boolean;
  /** Bumped by the Diff pane's own Refresh button. */
  refreshNonce: number;
  sendTargets: DiffSendTarget[];
  targetPane: string;
  onTargetPane: (paneId: string) => void;
  onSendPrompt: (paneId: string, text: string) => Promise<DiffSendResult>;
}

export function PrStatusCard(p: Props) {
  const [status, setStatus] = createSignal<PrStatus | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [fixOpen, setFixOpen] = createSignal(false);
  const [sending, setSending] = createSignal(false);
  const [sendStatus, setSendStatus] = createSignal<{ ok: boolean; msg: string } | null>(null);

  let seq = 0;
  let failures = 0;
  let lastFetch = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;

  const pr = () => {
    const s = status();
    return s?.kind === "ok" ? s.pr : null;
  };
  const errMsg = () => {
    const s = status();
    return s?.kind === "error" ? s.message : undefined;
  };
  const counts = createMemo(() => rollupCounts(pr()?.statusCheckRollup ?? []));
  const failing = createMemo(() => failingChecks(pr()?.statusCheckRollup ?? []));
  const prompt = createMemo(() => {
    const x = pr();
    return x ? buildFixChecksPrompt(x, failing()) : "";
  });
  const canFix = () => pr()?.state === "OPEN" && failing().length > 0;
  const validTarget = () => p.sendTargets.some((x) => x.paneId === p.targetPane);
  const canSend = () => !sending() && prompt().length > 0 && validTarget();

  const schedule = () => {
    clearTimeout(timer);
    if (disposed) return;
    timer = setTimeout(tick, nextPollSecs(status(), failures) * 1000);
  };

  const fetchNow = async () => {
    if (disposed || !p.enabled) return;
    const my = ++seq;
    setLoading(true);
    let next: PrStatus;
    try {
      next = await backend.call<PrStatus>("diff_pane_pr_status", { paneId: p.paneId });
    } catch (e) {
      // The command's own errors (no such pane, no cwd). Not gh output.
      next = { kind: "error", message: String(e) };
    }
    if (my !== seq || disposed) return;
    lastFetch = Date.now();
    failures = next.kind === "error" || next.kind === "timeout" || next.kind === "no_session" ? failures + 1 : 0;
    setStatus(next);
    setLoading(false);
    const c = next.kind === "ok" ? rollupCounts(next.pr.statusCheckRollup) : null;
    log.debug(c ? `pane=${p.paneId} kind=ok pass=${c.pass} fail=${c.fail} pending=${c.pending}` : `pane=${p.paneId} kind=${next.kind}`);
    schedule();
  };

  // A hidden window skips the tick; becoming visible fetches if one is due.
  function tick() {
    if (document.hidden) {
      schedule();
      return;
    }
    void fetchNow();
  }

  const onVisibility = () => {
    if (document.hidden || !p.enabled) return;
    if (Date.now() - lastFetch >= nextPollSecs(status(), failures) * 1000) void fetchNow();
  };

  onMount(() => document.addEventListener("visibilitychange", onVisibility));
  onCleanup(() => {
    disposed = true;
    clearTimeout(timer);
    document.removeEventListener("visibilitychange", onVisibility);
  });

  // Fetch when the pane gets a cwd, the branch or worktree changes, or the
  // user refreshes. A new branch has a different PR, so drop the old one.
  createEffect(
    on(
      () => [p.enabled, p.cwd, p.branch, p.refreshNonce] as const,
      ([enabled], prev) => {
        if (!enabled || !p.cwd) {
          clearTimeout(timer);
          return;
        }
        if (prev && (prev[1] !== p.cwd || prev[2] !== p.branch)) {
          setStatus(null);
          setFixOpen(false);
          setSendStatus(null);
        }
        failures = 0;
        void fetchNow();
      },
    ),
  );

  const openPr = () => {
    const u = safeUrl(pr()?.url);
    if (u) void backend.host.openUrl(u).catch((e: unknown) => log.warn("open PR url failed", e));
  };

  const send = async () => {
    if (!canSend()) return;
    setSending(true);
    setSendStatus(null);
    try {
      // Rule #1: this text is pane input. Never log it.
      const result = await p.onSendPrompt(p.targetPane, prompt());
      setSendStatus(
        result === "sent" ? { ok: true, msg: t("diff.notes.sent") }
        : result === "no_bracketed_paste" ? { ok: false, msg: t("diff.notes.errNoPaste") }
        : { ok: false, msg: t("diff.notes.errNotConnected") },
      );
    } catch (e) {
      log.error("send fix prompt failed", e);
      setSendStatus({ ok: false, msg: t("diff.notes.errSend", { err: String(e) }) });
    } finally {
      setSending(false);
    }
  };

  const quietText = (s: PrStatus): string => {
    switch (s.kind) {
      case "ok": return "";
      case "gh_missing": return t("diff.pr.ghMissing");
      case "not_authenticated": return t("diff.pr.notAuth");
      case "no_pr": return t("diff.pr.noPr");
      case "not_github": return t("diff.pr.notGithub");
      case "timeout": return t("diff.pr.timeout");
      case "no_session": return t("diff.pr.noSession");
      case "error": return t("diff.pr.error");
    }
  };

  const stateLabel = (state: string, draft: boolean): string => {
    if (draft && state === "OPEN") return t("diff.pr.state.draft");
    switch (state) {
      case "OPEN": return t("diff.pr.state.open");
      case "MERGED": return t("diff.pr.state.merged");
      case "CLOSED": return t("diff.pr.state.closed");
      default: return state;
    }
  };

  return (
    <Show when={p.enabled && p.cwd}>
      <div class="diff-pr" role="group" aria-label={t("diff.pr.label")} aria-busy={loading()}>
        <span class="diff-pr-title"><IconGitCompare size={12} /> {t("diff.pr.short")}</span>
        <Show
          when={pr()}
          fallback={
            <span class="diff-pr-quiet" title={errMsg()}>
              {status() ? quietText(status()!) : t("diff.pr.loading")}
            </span>
          }
        >
          {(x) => (
            <>
              <button class="diff-pr-link" disabled={!safeUrl(x().url)} title={t("diff.pr.open")} onClick={openPr}>
                #{x().number} <IconExternalLink size={11} />
              </button>
              <span class="diff-pr-name" dir="auto" title={x().title}>{x().title}</span>
              <span class={`diff-pr-state is-${x().isDraft && x().state === "OPEN" ? "draft" : x().state.toLowerCase()}`}>
                {stateLabel(x().state, x().isDraft)}
              </span>
              <span class="diff-pr-counts" aria-label={t("diff.pr.countsLabel", { pass: counts().pass, fail: counts().fail, pending: counts().pending })}>
                <span class="diff-pr-pass" title={t("diff.pr.passing")}><IconCircleCheck size={12} /> {counts().pass}</span>
                <span class="diff-pr-fail" title={t("diff.pr.failing")}><IconWarning size={12} /> {counts().fail}</span>
                <span class="diff-pr-pending" title={t("diff.pr.pending")}><IconCircleDot size={12} /> {counts().pending}</span>
              </span>
              <Show when={canFix()}>
                <button class="ws-header-btn" aria-expanded={fixOpen()} onClick={() => setFixOpen(!fixOpen())}>
                  {t("diff.pr.fix")}
                </button>
              </Show>
            </>
          )}
        </Show>
        <button
          class="diff-wt-add diff-pr-refresh"
          title={t("diff.pr.refresh")}
          aria-label={t("diff.pr.refresh")}
          disabled={loading()}
          onClick={() => { failures = 0; void fetchNow(); }}
        >
          <IconRefresh size={12} />
        </button>
      </div>

      <Show when={fixOpen() && canFix()}>
        <section class="diff-notes" aria-label={t("diff.pr.fix")}>
          <span class="diff-notes-label" id={`diff-pr-preview-${p.paneId}`}>{t("diff.notes.preview")}</span>
          <pre
            class="diff-notes-preview"
            tabIndex={0}
            role="region"
            aria-labelledby={`diff-pr-preview-${p.paneId}`}
          >{prompt()}</pre>
          <div class="diff-notes-row">
            <PaneTargetSelect targets={p.sendTargets} value={p.targetPane} onChange={p.onTargetPane} />
            <button disabled={!canSend()} onClick={() => void send()}>{t("diff.notes.send")}</button>
          </div>
          <p class="diff-notes-hint">{t("diff.pr.fixHint")}</p>
          <div role="status">
            <Show when={sendStatus()}>
              {(s) => <p class={s().ok ? "diff-notes-hint" : "diff-notes-err"}>{s().msg}</p>}
            </Show>
          </div>
        </section>
      </Show>
    </Show>
  );
}
