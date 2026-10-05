// The browser's event plumbing (Phase 109).
//
// `EventBus` is what `backend.on` subscribes to — the same event names and
// payloads the desktop's Rust emits, so App.tsx's listeners need no change.
// Two sources feed it: the daemon's events socket (`/api/v2/events`, frames
// `{type, data}` with the desktop's names, Phase 101) and the WebBackend
// itself (`pty:data` / `pty:exit` from the attach sockets, translated events).
//
// The socket's first frame is `hello`, the hydration snapshot; the WebBackend
// answers the boot reads (`pane_agent_states`, `feed_list`, …) from it. When
// the socket drops it reconnects with backoff, and the fresh hello is
// announced as `backend:resync` so the app can re-seed.

import { wsUrl } from "./api";
import type { BackendEvent, EventCallback, UnlistenFn } from "../types";

export class EventBus {
  private subs = new Map<string, Set<EventCallback<unknown>>>();

  on<T>(event: string, cb: EventCallback<T>): UnlistenFn {
    let set = this.subs.get(event);
    if (!set) this.subs.set(event, (set = new Set()));
    const c = cb as EventCallback<unknown>;
    set.add(c);
    return () => set?.delete(c);
  }

  emit(event: string, payload: unknown): void {
    const set = this.subs.get(event);
    if (!set) return;
    const ev: BackendEvent<unknown> = { payload };
    for (const cb of [...set]) {
      try {
        cb(ev);
      } catch (e) {
        // A throwing listener must not starve the others (the desktop's
        // `listen` isolates them the same way).
        console.error(`listener for ${event} threw`, e);
      }
    }
  }
}

export interface Hello {
  pane_agent_states: Record<string, unknown>;
  pane_briefs: Record<string, unknown>;
  panes: Record<string, { session: string; policy: string }>;
  feed: unknown[] | null;
  pane_status: Record<string, string>;
  notifications: unknown[] | null;
  ports: unknown[];
}

type Frame = { type: string; data?: unknown };

/** The daemon events socket, kept open for the page's life. */
export class EventsSocket {
  private ws: WebSocket | null = null;
  private backoffMs = 1000;
  private first: Promise<Hello>;
  private resolveFirst: (h: Hello) => void = () => {};
  private rejectFirst: (e: Error) => void = () => {};
  private gotFirst = false;
  latest: Hello | null = null;

  constructor(
    private lang: () => string,
    private onFrame: (type: string, data: unknown) => void,
    private onHello: (h: Hello, isFirst: boolean) => void,
  ) {
    this.first = new Promise<Hello>((res, rej) => {
      this.resolveFirst = res;
      this.rejectFirst = rej;
    });
  }

  /** Opens the socket; resolves with the first hello. */
  start(): Promise<Hello> {
    this.connect();
    return this.first;
  }

  send(frame: unknown): boolean {
    if (this.ws?.readyState !== WebSocket.OPEN) return false;
    this.ws.send(JSON.stringify(frame));
    return true;
  }

  private connect(): void {
    const ws = new WebSocket(wsUrl("/api/v2/events", { lang: this.lang() }));
    this.ws = ws;
    ws.onmessage = (ev) => {
      if (typeof ev.data !== "string") return;
      let f: Frame;
      try {
        f = JSON.parse(ev.data) as Frame;
      } catch {
        return;
      }
      if (f.type === "hello") {
        this.backoffMs = 1000;
        const h = f.data as Hello;
        this.latest = h;
        const isFirst = !this.gotFirst;
        this.gotFirst = true;
        this.onHello(h, isFirst);
        if (isFirst) this.resolveFirst(h);
        return;
      }
      this.onFrame(f.type, f.data ?? null);
    };
    ws.onclose = (ev) => {
      if (this.ws !== ws) return;
      this.ws = null;
      // 1008 / 4401-style refusals and a refused handshake never reach
      // `hello`; before the first one, fail the boot instead of spinning.
      if (!this.gotFirst) {
        this.rejectFirst(new Error(`events socket closed before hello (code ${ev.code})`));
        return;
      }
      const wait = this.backoffMs;
      this.backoffMs = Math.min(this.backoffMs * 2, 15000);
      setTimeout(() => this.connect(), wait);
    };
  }
}
