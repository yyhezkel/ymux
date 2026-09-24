/* @refresh reload */
// logger.ts must load BEFORE the console monkeypatch below — it captures the
// original console fns so logger output is never forwarded twice.
import { enqueueLog } from "./logger";
import { render } from "solid-js/web";
import { getCurrentWindow } from "@tauri-apps/api/window";
// Global stylesheets live at the entry point so BOTH the main <App> and the
// #4 pop-out window (which bypasses <App>) get xterm's CSS + our theme.
// Previously these were imported inside App.tsx, so a popout webview rendered
// unstyled — blank white screen, invisible terminal.
import "@xterm/xterm/css/xterm.css";
import "./App.css";
import App from "./App";
import { PopoutTerminal } from "./components/PopoutTerminal";
import { PopoutBrowser } from "./components/PopoutBrowser";
import { initPlatform } from "./platform";

// Phase 8.E → unified logging: capture console.error / console.warn as a
// safety net for un-swept or third-party output. Queued through the logger's
// batch (`ui_log_batch`, capped per second), which writes debug.log AND the
// dev ring buffer (`ymux dev console-tail`). Swept code logs through createLogger() instead — it uses
// the original console fns captured in logger.ts, so nothing loops through
// here twice. Original console output is preserved.
{
  const origErr = console.error;
  const origWarn = console.warn;
  const fmt = (args: unknown[]): string =>
    args
      .map((a) => {
        if (typeof a === "string") return a;
        if (a instanceof Error) return `${a.name}: ${a.message}`;
        try {
          return JSON.stringify(a);
        } catch {
          return String(a);
        }
      })
      .join(" ");
  console.error = (...args: unknown[]) => {
    origErr(...(args as []));
    enqueueLog("error", "CONSOLE", fmt(args));
  };
  console.warn = (...args: unknown[]) => {
    origWarn(...(args as []));
    enqueueLog("warn", "CONSOLE", fmt(args));
  };
  window.addEventListener("error", (e) => {
    enqueueLog("error", "CONSOLE", `unhandled: ${e.message} @ ${e.filename}:${e.lineno}`);
  });
  window.addEventListener("unhandledrejection", (e) => {
    enqueueLog("error", "CONSOLE", `unhandled rejection: ${String(e.reason)}`);
  });
}

// Unshipped-fivefer (#4): pop-out terminal windows. Bail to a bare
// full-screen <PopoutTerminal> BEFORE mounting <App>, so none of the
// workspace/settings bootstrap runs in the popout webview.
//
// The session id comes from the window LABEL (`popout-<sid>`). The popout URL
// is a CLEAN `index.html` (no query/fragment) because Tauri's built-app asset
// protocol serves a blank page for any suffixed path — so the label, not the
// URL, carries the id.
let winLabel = "";
try {
  winLabel = getCurrentWindow().label;
} catch {
  // window metadata not ready — treat as the main window
}
const popoutSid = winLabel.startsWith("popout-")
  ? winLabel.slice("popout-".length)
  : null;
// Phase 85.C: the workspace Browser popped out into its own OS window.
// Same label-carries-the-id trick. `browser-popout-<ws>` deliberately does
// NOT start with `popout-`, so the two checks cannot collide (and neither
// can the two capability globs, which are prefix-anchored too).
const popoutBrowserWs = winLabel.startsWith("browser-popout-")
  ? winLabel.slice("browser-popout-".length)
  : null;

// Resolve the host OS BEFORE the first render. PaneView / FileManagerPane
// register their OS drag-drop listeners in onMount and hit-test with
// platform-dependent scaling, and the local file-manager column joins paths
// with a platform-dependent separator — none of which can wait on a promise
// that resolves after mount. Popout windows take the same path (they drag-drop
// too). initPlatform never rejects; a failed probe just keeps the default.
//
// An async IIFE rather than top-level await: Vite's default build target is
// `es2020`, where esbuild refuses TLA outright.
void (async () => {
  await initPlatform();
  if (popoutBrowserWs) {
    render(
      () => <PopoutBrowser workspaceId={popoutBrowserWs} />,
      document.getElementById("root") as HTMLElement,
    );
  } else if (popoutSid) {
    render(
      () => <PopoutTerminal sessionId={popoutSid} />,
      document.getElementById("root") as HTMLElement,
    );
  } else {
    render(() => <App />, document.getElementById("root") as HTMLElement);
  }
})();
