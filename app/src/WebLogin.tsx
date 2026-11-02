// The browser's sign-in (Phase 109). Rendered by index.tsx instead of <App>
// when the WebBackend has no usable token. It runs the Phase 96 request-access
// flow the diagnostic page proved: the page asks the daemon for access and
// shows a code, ymux on the desktop shows the same code (plus this browser's
// IP and User-Agent) and the human approves, the page redeems its one-shot
// token for a device token. A device also needs `shell:attach` granted — the
// "no-shell" mode says so instead of looping.
//
// Rule #8: the tokens never reach a log or the DOM.

import { createSignal, onCleanup, Show } from "solid-js";
import { setToken, forgetToken, deviceId } from "./backend/web/api";
import { t } from "./i18n";

type Phase = "idle" | "waiting" | "error";

export function WebLogin(p: { mode: "login" | "no-shell" }) {
  const [phase, setPhase] = createSignal<Phase>("idle");
  const [code, setCode] = createSignal("");
  const [left, setLeft] = createSignal(0);
  const [msg, setMsg] = createSignal("");
  let poll: number | undefined;
  let tick: number | undefined;
  const stop = () => {
    window.clearInterval(poll);
    window.clearInterval(tick);
  };
  onCleanup(stop);

  const request = async () => {
    setMsg("");
    try {
      const r = await fetch("/api/pairing/request", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ device_name: `browser · ${navigator.userAgent.slice(0, 48)}` }),
      });
      if (r.status === 503) return fail(t("web.login.no_desktop"));
      if (r.status === 429) return fail(t("web.login.rate_limited"));
      if (!r.ok) return fail(`HTTP ${r.status}`);
      const b = (await r.json()) as { code: string; one_shot_token: string; expires_at: number };
      setCode(b.code);
      setPhase("waiting");
      tick = window.setInterval(() => setLeft(Math.max(0, b.expires_at - Math.floor(Date.now() / 1000))), 500);
      poll = window.setInterval(() => void check(b.one_shot_token), 2000);
    } catch (e) {
      fail(e instanceof Error ? e.message : String(e));
    }
  };

  const check = async (oneShot: string) => {
    try {
      const r = await fetch(`/api/pairing/request/status?one_shot_token=${encodeURIComponent(oneShot)}`);
      const s = ((await r.json()) as { status?: string }).status;
      if (s === "requested") return; // still waiting on a human
      stop();
      if (s !== "pending") return fail(s === "expired" ? t("web.login.expired") : t("web.login.denied"));
      const rr = await fetch("/api/pairing/redeem", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ one_shot_token: oneShot }),
      });
      if (!rr.ok) return fail(`HTTP ${rr.status}`);
      const b = (await rr.json()) as { long_term_token: string; device_id?: string };
      setToken(b.long_term_token, b.device_id ?? "");
      location.reload();
    } catch (e) {
      stop();
      fail(e instanceof Error ? e.message : String(e));
    }
  };

  const fail = (m: string) => {
    stop();
    setMsg(m);
    setPhase("error");
  };

  return (
    <div class="web-login">
      <div class="web-login-card">
        <h1>ymux</h1>
        <Show
          when={p.mode === "login"}
          fallback={
            <>
              <p>{t("web.login.no_shell")}</p>
              <p class="web-login-hint">
                {t("web.login.device")} <code>{deviceId() || "—"}</code>
              </p>
              <button
                onClick={() => {
                  forgetToken();
                  location.reload();
                }}
              >
                {t("web.login.forget")}
              </button>
            </>
          }
        >
          <Show when={phase() !== "waiting"}>
            <p>{t("web.login.intro")}</p>
            <button class="primary" onClick={() => void request()}>
              {t("web.login.request")}
            </button>
          </Show>
          <Show when={phase() === "waiting"}>
            <p>{t("web.login.waiting")}</p>
            <div class="web-login-code">{code()}</div>
            <p class="web-login-hint">{t("web.login.expires", { s: left() })}</p>
          </Show>
          <Show when={msg()}>
            <p class="web-login-err">{msg()}</p>
          </Show>
        </Show>
      </div>
    </div>
  );
}
