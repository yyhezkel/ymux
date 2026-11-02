// The browser app as a PWA (Phase 114, WEB-DESIGN E): the service worker
// (/sw.js, served by the daemon) and the Web Push subscription it delivers to.
//
// - `startPwa` (WebBackend.init) registers the worker, hands it the device
//   token and language (it cannot read localStorage), and — when permission
//   was already granted — re-sends the subscription, so a daemon that lost
//   its record (or a new VAPID key) heals on the next load.
// - `enablePush` is the one call that asks for permission; browsers want a
//   click behind it, so only the banner (WebPushPrompt.tsx) calls it.
// - A gate decided in the app closes its notification on this device.
//
// Rule #8: the token goes to the worker by postMessage and nowhere else.

import { createSignal } from "solid-js";
import { createLogger } from "../../logger";
import { api, getToken } from "./api";

const log = createLogger("PWA");

export type PushState = "unsupported" | "default" | "denied" | "on" | "error";

const [pushState, setPushState] = createSignal<PushState>("unsupported");
/** Reactive: what the banner shows. */
export { pushState };

let reg: ServiceWorkerRegistration | null = null;
let lang: () => "he" | "en" = () => "en";

function supported(): boolean {
  return (
    typeof window !== "undefined" &&
    window.isSecureContext &&
    "serviceWorker" in navigator &&
    "PushManager" in window &&
    "Notification" in window
  );
}

/** The VAPID key as the BufferSource `pushManager.subscribe` wants. */
function keyBytes(b64url: string): Uint8Array {
  const pad = "=".repeat((4 - (b64url.length % 4)) % 4);
  const raw = atob((b64url + pad).replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

function sameKey(a: ArrayBuffer | null, b: Uint8Array): boolean {
  if (!a) return false;
  const x = new Uint8Array(a);
  return x.length === b.length && x.every((v, i) => v === b[i]);
}

/** Tell the worker who we are (token + language), or that we signed out. */
export function syncWorkerAuth(): void {
  reg?.active?.postMessage({ type: "auth", token: getToken(), lang: lang() });
}

async function subscribe(r: ServiceWorkerRegistration): Promise<void> {
  const { public_key } = await api<{ public_key: string }>("GET", "/api/v2/webpush/key");
  const key = keyBytes(public_key);
  let sub = await r.pushManager.getSubscription();
  if (sub && !sameKey(sub.options.applicationServerKey, key)) {
    // The daemon's key changed (its data dir was reset): the old
    // subscription can never be delivered to again.
    await sub.unsubscribe();
    sub = null;
  }
  sub ??= await r.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
  await api("POST", "/api/v2/webpush/subscriptions", { ...sub.toJSON(), lang: lang() });
}

export async function startPwa(opts: {
  lang: () => "he" | "en";
  onGateResolved: (cb: (requestId: string) => void) => void;
}): Promise<void> {
  lang = opts.lang;
  if (!supported()) {
    setPushState("unsupported");
    return;
  }
  try {
    await navigator.serviceWorker.register("/sw.js", { scope: "/" });
    reg = await navigator.serviceWorker.ready;
  } catch (e) {
    log.warn(`service worker unavailable: ${String(e)}`);
    setPushState("unsupported");
    return;
  }
  syncWorkerAuth();
  opts.onGateResolved((id) => reg?.active?.postMessage({ type: "close", tag: `gate-${id}` }));
  const perm = Notification.permission;
  if (perm !== "granted") {
    setPushState(perm === "denied" ? "denied" : "default");
    return;
  }
  try {
    await subscribe(reg);
    setPushState("on");
  } catch (e) {
    // 503 = a daemon without Web Push; 403 = no shell:attach.
    log.warn(`push subscription not renewed: ${String(e)}`);
    setPushState("error");
  }
}

/** Ask for permission (from a click) and subscribe. */
export async function enablePush(): Promise<PushState> {
  if (!reg) return pushState();
  const perm = await Notification.requestPermission();
  if (perm !== "granted") {
    setPushState(perm === "denied" ? "denied" : "default");
    return pushState();
  }
  try {
    await subscribe(reg);
    setPushState("on");
    await api("POST", "/api/v2/webpush/test");
  } catch (e) {
    log.warn(`push subscription failed: ${String(e)}`);
    setPushState("error");
  }
  return pushState();
}
