// A browser pane's PTY (Phase 109): one attach socket per connected pane,
// speaking the desktop's own contract so App.tsx / TerminalInstance /
// PopoutTerminal are untouched (WEB-DESIGN §8.2, C2 dropped):
//
//   pane_connect  → opens /api/v2/term/sessions/{name}/attach, returns a sid
//   pty_write     → a binary frame (UTF-8 of the typed string)
//   pty_resize    → {"type":"resize","cols","rows"}
//   binary frames → TextDecoder(stream) → `pty:data` {session_id, data}
//   {"type":"exit"} or a close → `pty:exit` {session_id, reason}
//
// The streaming decoder is `pty_decode.rs`'s job on the desktop: a multibyte
// character split across two frames must not become two U+FFFD.
// Rule #1: nothing here logs bytes — only session ids and close codes.

import { BidiFilter } from "./bidiFilter";
import { wsUrl } from "./api";
import type { EventBus } from "./events";

interface Live {
  ws: WebSocket;
  decoder: TextDecoder;
  ended: boolean;
  /** Phase 117 (F3): smart bidi, per attach (off unless the leaf says so). */
  bidi: BidiFilter;
}

export class PtySessions {
  private live = new Map<string, Live>();
  private seq = 0;

  constructor(private bus: EventBus) {}

  /**
   * Attach to tmux session `name`; resolves with the session id once open.
   * `sid` pins the id — a popout window reuses the opener's, which is the
   * only id PopoutTerminal knows.
   */
  open(name: string, cols: number, rows: number, sid?: string): Promise<string> {
    sid = sid ?? `web_${Date.now().toString(36)}_${(this.seq++).toString(36)}`;
    const ws = new WebSocket(
      wsUrl(`/api/v2/term/sessions/${encodeURIComponent(name)}/attach`, { cols, rows }),
    );
    ws.binaryType = "arraybuffer";
    const entry: Live = { ws, decoder: new TextDecoder("utf-8"), ended: false, bidi: new BidiFilter(false) };
    this.live.set(sid, entry);
    return new Promise<string>((resolve, reject) => {
      let opened = false;
      ws.onopen = () => {
        opened = true;
        resolve(sid);
      };
      ws.onmessage = (ev) => {
        if (typeof ev.data === "string") {
          let f: { type?: string } = {};
          try {
            f = JSON.parse(ev.data) as { type?: string };
          } catch {
            /* the server sends only control frames as text */
          }
          if (f.type === "exit") this.end(sid, "session ended");
          return;
        }
        // Decode first, then filter — the desktop's order (pty_decode → bidi).
        const data = entry.bidi.process(entry.decoder.decode(new Uint8Array(ev.data as ArrayBuffer), { stream: true }));
        if (data) this.bus.emit("pty:data", { session_id: sid, data });
      };
      ws.onclose = (ev) => {
        if (!opened) {
          this.live.delete(sid);
          reject(new Error(ev.code === 1006 ? "could not attach (refused or unreachable)" : `attach closed (${ev.code})`));
          return;
        }
        this.end(sid, ev.code === 1000 ? null : `connection lost (${ev.code})`);
      };
    });
  }

  write(sid: string, data: string): void {
    const e = this.live.get(sid);
    if (e && e.ws.readyState === WebSocket.OPEN) e.ws.send(new TextEncoder().encode(data));
  }

  resize(sid: string, cols: number, rows: number): void {
    const e = this.live.get(sid);
    if (e && e.ws.readyState === WebSocket.OPEN && cols > 0 && rows > 0) {
      e.ws.send(JSON.stringify({ type: "resize", cols, rows }));
    }
  }

  /** Smart bidi on / off for one attach (pane_set_smart_bidi, or the leaf at connect). */
  setBidi(sid: string, on: boolean): void {
    this.live.get(sid)?.bidi.setEnabled(on);
  }

  /** Detach without ending the tmux session (the desktop's pane_disconnect). */
  close(sid: string): void {
    const e = this.live.get(sid);
    if (!e) return;
    e.ended = true; // a user detach is not an exit: no `pty:exit`
    this.live.delete(sid);
    e.ws.close(1000);
  }

  private end(sid: string, reason: string | null): void {
    const e = this.live.get(sid);
    if (!e || e.ended) return;
    e.ended = true;
    this.live.delete(sid);
    const tail = e.decoder.decode();
    if (tail) this.bus.emit("pty:data", { session_id: sid, data: tail });
    try {
      e.ws.close(1000);
    } catch {
      /* already closing */
    }
    this.bus.emit("pty:exit", { session_id: sid, reason });
  }
}
