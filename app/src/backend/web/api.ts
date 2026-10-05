// HTTP to the daemon that served this page (Phase 109). Same origin, so the
// paths are relative and the page's CSP (`connect-src 'self'`) holds.
//
// The device token comes from the pairing flow (WebLogin.tsx) and rides as
// `Authorization: Bearer`, or as `?token=` on a WebSocket (a browser cannot
// set headers on the handshake). Rule #8: it is never logged.

const TOKEN_KEY = "ymux.web.token";
const DEVICE_KEY = "ymux.web.device";

export function getToken(): string {
  try {
    return localStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

export function setToken(token: string, deviceId: string): void {
  try {
    localStorage.setItem(TOKEN_KEY, token);
    localStorage.setItem(DEVICE_KEY, deviceId);
  } catch {
    // private mode — the session still works until the tab closes
  }
}

export function forgetToken(): void {
  try {
    localStorage.removeItem(TOKEN_KEY);
    localStorage.removeItem(DEVICE_KEY);
  } catch {
    /* nothing to forget */
  }
}

export function deviceId(): string {
  try {
    return localStorage.getItem(DEVICE_KEY) ?? "";
  } catch {
    return "";
  }
}

/** An HTTP failure, carrying the status so callers can branch on 409 / 403. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly body: unknown = null,
  ) {
    super(message);
  }
}

/** Called once on any 401: the token is dead, so the page goes back to login. */
let onUnauthorized: () => void = () => {};
export function setUnauthorizedHandler(fn: () => void): void {
  onUnauthorized = fn;
}

export async function api<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Authorization: `Bearer ${getToken()}` };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  const r = await fetch(path, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  let parsed: unknown = null;
  try {
    parsed = text ? JSON.parse(text) : null;
  } catch {
    parsed = null;
  }
  if (r.status === 401) {
    onUnauthorized();
    throw new ApiError(401, "not signed in");
  }
  if (!r.ok) {
    const msg = typeof parsed === "string" ? parsed : text.trim() || `HTTP ${r.status}`;
    throw new ApiError(r.status, msg, parsed);
  }
  return parsed as T;
}

/** `ws(s)://<this host><path>?token=…` for the daemon's sockets. */
export function wsUrl(path: string, params: Record<string, string | number> = {}): string {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const q = new URLSearchParams({ token: getToken() });
  for (const [k, v] of Object.entries(params)) q.set(k, String(v));
  return `${proto}//${location.host}${path}?${q.toString()}`;
}
