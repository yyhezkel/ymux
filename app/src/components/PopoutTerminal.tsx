import { onCleanup, onMount } from "solid-js";
import { backend, type UnlistenFn } from "../backend";
import { TerminalInstance, setTerminalFontSize } from "../terminalInstance";

// Ctrl+wheel font zoom — pop-out windows only (the grid stays Settings-driven).
// All open popouts share one zoom level, synced via the `popout:zoom` event and
// persisted in localStorage so a fresh popout opens at the last size.
const POPOUT_FONT_KEY = "ymux.popout.font_size_pt";
const MIN_PT = 8;
const MAX_PT = 32;
const STEP_PT = 1;
const clampPt = (n: number) => Math.min(MAX_PT, Math.max(MIN_PT, n));
function readPopoutFontPt(): number {
  const raw = Number(localStorage.getItem(POPOUT_FONT_KEY));
  return Number.isFinite(raw) && raw > 0 ? clampPt(Math.round(raw)) : 13;
}

// Unshipped-fivefer (#4): the pop-out terminal window.
//
// index.tsx early-bails here when the window LABEL is `popout-<sid>`, so this
// renders in a FRESH webview with none of App.tsx's workspace/settings
// bootstrap. It reuses TerminalInstance (same onData→pty_write + resize
// contract as an in-grid pane) and taps the app-wide `pty:data` / `pty:exit`
// streams filtered to its own session.
//
// Ownership while open: this window drives input + resize (pty_write /
// pty_resize); the origin pane in the main window detaches to a read-only
// mirror (App.tsx), so there's no SIGWINCH tug-of-war over the PTY.
//
// Lifecycle: `pty:exit` for our session → notice + self-close. The main
// window re-attaches when it receives `popout:closed` (emitted by the Rust
// side on window Destroyed).

interface PtyDataEvent {
  session_id: string;
  data: string;
}
interface PtyExitEvent {
  session_id: string;
  reason?: string | null;
}

export function PopoutTerminal(props: { sessionId: string }) {
  let hostRef!: HTMLDivElement;
  let ti: TerminalInstance | null = null;
  const unlistens: UnlistenFn[] = [];

  let sizePt = readPopoutFontPt();
  let onWheel: ((e: WheelEvent) => void) | null = null;

  onMount(() => {
    ti = new TerminalInstance(`popout-${props.sessionId}`);
    hostRef.appendChild(ti.container);
    ti.container.style.display = "block";
    // attach() binds onData→pty_write and pushes an initial pty_resize, so
    // this window becomes the resize authority immediately.
    ti.attach(props.sessionId);
    // Apply the remembered popout zoom (independent of the grid's settings).
    setTerminalFontSize(sizePt);
    requestAnimationFrame(() => ti?.fitAndResize(true));
    ti.focus();

    // Ctrl+wheel zoom. Capture phase + non-passive so we beat xterm's own
    // viewport wheel handler and can preventDefault — a plain (no-Ctrl) wheel
    // falls through untouched to normal scrollback.
    onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      e.stopPropagation();
      const next = clampPt(sizePt + (e.deltaY < 0 ? STEP_PT : -STEP_PT));
      if (next === sizePt) return;
      sizePt = next;
      setTerminalFontSize(sizePt);
      try {
        localStorage.setItem(POPOUT_FONT_KEY, String(sizePt));
      } catch {
        // quota/private mode — zoom still applies for this session
      }
      void backend.emit("popout:zoom", sizePt); // equalize every open popout
    };
    ti.container.addEventListener("wheel", onWheel, {
      capture: true,
      passive: false,
    });

    void (async () => {
      // Cross-popout equalize: match the latest wheel-set size.
      unlistens.push(
        await backend.on<number>("popout:zoom", (e) => {
          const pt = clampPt(Math.round(e.payload));
          if (pt === sizePt) return;
          sizePt = pt;
          setTerminalFontSize(sizePt);
        }),
      );
      unlistens.push(
        await backend.on<PtyDataEvent>("pty:data", (e) => {
          if (e.payload.session_id === props.sessionId) {
            ti?.writeData(e.payload.data);
          }
        }),
      );
      unlistens.push(
        await backend.on<PtyExitEvent>("pty:exit", (e) => {
          if (e.payload.session_id !== props.sessionId) return;
          ti?.notice(
            `[session ended${e.payload.reason ? ` (${e.payload.reason})` : ""}]`,
          );
          // Let the notice land, then close the window. Rust's Destroyed
          // handler emits popout:closed so the main pane cleans up.
          setTimeout(() => {
            void backend.host.closeWindow();
          }, 1200);
        }),
      );
    })();
  });

  onCleanup(() => {
    if (ti && onWheel) {
      ti.container.removeEventListener("wheel", onWheel, { capture: true });
    }
    for (const u of unlistens) {
      try {
        u();
      } catch {
        // best-effort
      }
    }
    ti?.dispose();
    ti = null;
  });

  return <div ref={hostRef!} class="popout-terminal-host" />;
}
