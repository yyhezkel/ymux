// ymux service worker — Phase 114 (WEB-DESIGN E).
//
// Notifications only. There is deliberately NO fetch handler and no offline
// cache: the app needs its daemon anyway, Chrome no longer requires a fetch
// handler to install a PWA, and a cached shell is how a stale build outlives
// a bundle update.
//
// The daemon pushes (term/webpush.go) a JSON payload:
//   {v, kind: "gate"|"attention"|"done"|"test", lang, request_id, pane_id,
//    session, title, body}
// A gate carries Approve / Deny; the click answers the gate through
// POST /api/v2/feed/{id}/decide without opening the app.
//
// The page hands this worker its device token (pwa.ts → postMessage) — the
// worker cannot read localStorage. It is kept in Cache Storage, same origin as
// the localStorage copy, and never logged (Rule #8). This file is not built by
// vite; it is served as-is from /sw.js.

const STORE = "ymux-sw-v1";
const AUTH_KEY = "/__ymux_sw_auth";

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (e) => e.waitUntil(self.clients.claim()));

self.addEventListener("message", (e) => {
  const d = e.data || {};
  if (d.type === "auth") e.waitUntil(saveAuth(d.token || "", d.lang === "he" ? "he" : "en"));
  if (d.type === "close" && d.tag) e.waitUntil(closeTag(d.tag));
});

async function saveAuth(token, lang) {
  const c = await caches.open(STORE);
  if (!token) return c.delete(AUTH_KEY);
  return c.put(AUTH_KEY, new Response(JSON.stringify({ token, lang })));
}

async function auth() {
  try {
    const c = await caches.open(STORE);
    const r = await c.match(AUTH_KEY);
    return r ? await r.json() : { token: "", lang: "en" };
  } catch {
    return { token: "", lang: "en" };
  }
}

async function closeTag(tag) {
  for (const n of await self.registration.getNotifications({ tag })) n.close();
}

function tagOf(n) {
  return n.kind === "gate" ? "gate-" + n.request_id : n.kind + "-" + (n.pane_id || "");
}

self.addEventListener("push", (e) => {
  let n = {};
  try {
    n = e.data ? e.data.json() : {};
  } catch {
    n = {};
  }
  e.waitUntil(show(n));
});

async function show(n) {
  const he = n.lang === "he";
  const gate = n.kind === "gate";
  const tag = tagOf(n);
  await self.registration.showNotification(n.title || "YMUX", {
    body: n.body || "",
    tag,
    renotify: true,
    icon: "/icons/icon-192.png",
    data: n,
    requireInteraction: gate,
    actions: gate
      ? [
          { action: "allow", title: he ? "אשר" : "Approve" },
          { action: "deny", title: he ? "דחה" : "Deny" },
        ]
      : [],
  });
  // The app in front already shows the card: a push must still show
  // something (userVisibleOnly), so show it and take it straight down.
  if (n.kind !== "test") {
    const wins = await self.clients.matchAll({ type: "window" });
    if (wins.some((w) => w.focused && w.visibilityState === "visible")) await closeTag(tag);
  }
}

self.addEventListener("notificationclick", (e) => {
  const n = e.notification.data || {};
  e.notification.close();
  if ((e.action === "allow" || e.action === "deny") && n.request_id) {
    e.waitUntil(decide(n, e.action));
    return;
  }
  e.waitUntil(focusApp());
});

async function decide(n, decision) {
  const a = await auth();
  let ok = false;
  try {
    const r = await fetch("/api/v2/feed/" + encodeURIComponent(n.request_id) + "/decide", {
      method: "POST",
      headers: { Authorization: "Bearer " + a.token, "Content-Type": "application/json" },
      body: JSON.stringify({ decision }),
    });
    ok = r.ok;
  } catch {
    ok = false;
  }
  // Gone (decided elsewhere, timed out, daemon restarted) or signed out:
  // the app is where the user can see what happened.
  if (!ok) await focusApp();
}

async function focusApp() {
  const wins = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  for (const w of wins) {
    if ("focus" in w) return w.focus();
  }
  return self.clients.openWindow("/");
}

// The push service rotated the subscription: re-subscribe with the same key
// and tell the daemon, so notifications keep arriving without a page load.
self.addEventListener("pushsubscriptionchange", (e) => {
  e.waitUntil(
    (async () => {
      const key = e.oldSubscription && e.oldSubscription.options.applicationServerKey;
      const sub =
        e.newSubscription ||
        (key ? await self.registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key }) : null);
      const a = await auth();
      if (!sub || !a.token) return;
      await fetch("/api/v2/webpush/subscriptions", {
        method: "POST",
        headers: { Authorization: "Bearer " + a.token, "Content-Type": "application/json" },
        body: JSON.stringify({ ...sub.toJSON(), lang: a.lang }),
      });
    })(),
  );
});
