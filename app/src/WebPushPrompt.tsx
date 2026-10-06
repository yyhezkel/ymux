// The browser's "turn on notifications" banner (Phase 114, WEB-DESIGN E).
// index.tsx mounts it next to <App> in a browser tab only; the desktop never
// renders it. Browsers want a click behind a permission request, hence a
// banner and not a prompt on load. "Not now" is remembered per browser.

import { createSignal, Show } from "solid-js";
import { enablePush, pushState } from "./backend/web/pwa";
import { t } from "./i18n";

const DISMISS_KEY = "ymux.web.push.dismissed";

function dismissedBefore(): boolean {
  try {
    return localStorage.getItem(DISMISS_KEY) === "1";
  } catch {
    return false;
  }
}

export function WebPushPrompt() {
  const [dismissed, setDismissed] = createSignal(dismissedBefore());
  const [busy, setBusy] = createSignal(false);
  // The click asked and the browser said no (or a browser-level setting
  // refused without asking — Zen does): say so, instead of vanishing.
  const [blocked, setBlocked] = createSignal(false);
  const dismiss = () => {
    setDismissed(true);
    try {
      localStorage.setItem(DISMISS_KEY, "1");
    } catch {
      /* private mode: dismissed for this tab only */
    }
  };
  const enable = async () => {
    setBusy(true);
    const st = await enablePush();
    setBusy(false);
    if (st === "on") setDismissed(true);
    else if (st === "denied" || st === "default") setBlocked(true);
  };
  return (
    <Show
      when={!dismissed() && (blocked() || pushState() === "default" || pushState() === "error")}
    >
      <div class="web-push-prompt" role="status">
        <span>
          {blocked()
            ? t("web.push.blocked")
            : pushState() === "error"
              ? t("web.push.error")
              : t("web.push.ask")}
        </span>
        <button class="primary" disabled={busy()} onClick={() => void enable()}>
          {blocked() ? t("web.push.retry") : t("web.push.enable")}
        </button>
        <button disabled={busy()} onClick={dismiss}>
          {t("web.push.later")}
        </button>
      </div>
    </Show>
  );
}
