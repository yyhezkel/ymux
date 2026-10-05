import { createEffect, createSignal, onCleanup, onMount, Show } from "solid-js";
import { backend, type UnlistenFn } from "./backend";
import html2canvas from "html2canvas";
import { t } from "./i18n";
import { createLogger } from "./logger";

const log = createLogger("BROWSER");
import type { LayoutNode, SplitDirection } from "./types";
import {
  IconArrowLeft,
  IconRefreshCcw,
  IconHome,
  IconArrowRight,
  IconLink,
  IconWarning,
  IconColumns,
  IconRows,
  IconClose,
} from "./icons";

interface Props {
  workspaceId: string;
  pane: Extract<LayoutNode, { kind: "pane" }>;
  isActive: boolean;
  // Phase 26: pane is waiting on a blocking agent permission request.
  isWaiting?: boolean;
  onFocus: (paneId: string) => void;
  onSplit: (paneId: string, direction: SplitDirection) => void;
  onClose: (paneId: string) => void;
  onNavigate: (paneId: string, url: string) => void;
  onGoBack: (paneId: string) => void;
  onGoHome: (paneId: string) => void;
  onSetForward: (paneId: string, forward: boolean) => void;
  onSetTitle: (paneId: string, title: string) => void;
  onSetAnnotation: (paneId: string, annotation: string) => void;
}

export function BrowserPane(p: Props) {
  // Phase 8 fix v3.1: merge with defaults so fields skipped during JSON
  // serialization (e.g. `history` is omitted by serde when the Vec is empty,
  // `forward_localhost` is omitted when it's the default true) come back as
  // safe values rather than `undefined` — `browser().history.length` was
  // crashing when navigating to a fresh browser pane.
  const browser = () => {
    const b = p.pane.browser;
    return {
      url: b?.url ?? "",
      home_url: b?.home_url,
      history: b?.history ?? [],
      forward_localhost: b?.forward_localhost ?? true,
    };
  };
  const forwardOn = () => browser().forward_localhost ?? true;
  const [urlDraft, setUrlDraft] = createSignal(browser().url);
  // Phase 8.B: the URL the iframe actually loads. Differs from browser().url
  // when localhost forwarding rewrites it to 127.0.0.1:<local_forward_port>.
  const [resolvedUrl, setResolvedUrl] = createSignal(browser().url);
  const [resolveErr, setResolveErr] = createSignal<string | null>(null);
  // Phase 53 (#4.8 / 48-F): the iframe is gone. We now spawn a native
  // child Webview via the backend; this div is the placeholder slot
  // the Webview is positioned over (set_position + set_size on every
  // ResizeObserver tick).
  let iframeRef: HTMLIFrameElement | undefined; // kept for the legacy
  // eval/screenshot bridge (Phase 8.F.1) until 53.C rewires it; on the
  // Webview path this stays undefined and the bridge degrades to error.
  let bodyRef: HTMLDivElement | undefined;
  let slotRef: HTMLDivElement | undefined;
  // True once the backend has confirmed the Webview spawned; gates
  // navigate/resize commands so we don't fire them against a non-
  // existent target.
  const [webviewLive, setWebviewLive] = createSignal(false);

  // Whenever the persisted URL changes (user nav, back, home, CLI), refresh
  // both the address-bar draft and the iframe's resolved src. Track the LAST
  // (url, forward) pair we asked the backend to resolve so the effect doesn't
  // re-fire just because setResolvedUrl flipped resolvedUrl ≠ browser().url
  // (which is the whole point of forwarding — the two are intentionally
  // different after rewrite).
  let lastUrl = browser().url;
  let lastResolvedSource = "";
  let lastResolvedForward = forwardOn();
  createEffect(() => {
    const u = browser().url;
    const f = forwardOn();
    if (u !== lastUrl) {
      lastUrl = u;
      setUrlDraft(u);
    }
    if (u === lastResolvedSource && f === lastResolvedForward) {
      return; // no source change — don't re-resolve
    }
    lastResolvedSource = u;
    lastResolvedForward = f;
    setResolveErr(null);
    if (!u) {
      setResolvedUrl("");
      return;
    }
    backend.call<string>("pane_browser_resolve_url", {
      workspaceId: p.workspaceId,
      paneId: p.pane.pane_id,
      url: u,
    })
      .then((rewritten) => setResolvedUrl(rewritten))
      .catch((err) => {
        // If forwarding fails (e.g. no SSH session), fall back to the raw URL
        // and surface the error in the chrome.
        setResolvedUrl(u);
        setResolveErr(String(err));
      });
  });

  const submitUrl = () => {
    let v = urlDraft().trim();
    if (!v) return;
    // Auto-prepend http:// for localhost (dev servers usually are http) and
    // https:// for everything else missing a scheme.
    if (!/^[a-z][a-z0-9+\-.]*:/i.test(v)) {
      const isLocal = /^(localhost|127\.0\.0\.1)(:|$|\/)/i.test(v);
      v = (isLocal ? "http://" : "https://") + v;
    }
    p.onNavigate(p.pane.pane_id, v);
  };

  // Phase 8.B race fix: a manual reload should ALWAYS bypass the
  // lastResolvedSource cache. If the SSH session came up after the initial
  // resolve attempt failed, the cached resolvedUrl would still be the raw
  // localhost URL (or empty); re-resolving now picks up the live forward.
  const forceResolveAndReload = async () => {
    const u = browser().url;
    if (!u) return;
    setResolveErr(null);
    try {
      const rewritten = await backend.call<string>("pane_browser_resolve_url", {
        workspaceId: p.workspaceId,
        paneId: p.pane.pane_id,
        url: u,
      });
      lastResolvedSource = u;
      lastResolvedForward = forwardOn();
      setResolvedUrl(rewritten);
      // Phase 53: route navigation through the native Webview command
      // rather than mutating iframe.src.
      if (webviewLive() && rewritten) {
        await backend.call("browser_pane_navigate", {
          paneId: p.pane.pane_id,
          url: rewritten,
        }).catch((err) => log.error("browser_pane_navigate failed", err));
      }
    } catch (err) {
      setResolveErr(String(err));
      if (webviewLive() && u) {
        await backend.call("browser_pane_navigate", {
          paneId: p.pane.pane_id,
          url: u,
        }).catch(() => {});
      }
    }
  };

  const reload = () => {
    void forceResolveAndReload();
  };

  // True if `url` shares an origin with the parent webview (then html2canvas
  // can traverse the iframe and `iframe.contentWindow.eval` is allowed).
  const sameOrigin = (url: string): boolean => {
    try {
      return new URL(url, window.location.href).origin === window.location.origin;
    } catch {
      return false;
    }
  };

  const isTunneled = () => {
    const u = browser().url;
    if (!u || !resolvedUrl()) return false;
    return resolvedUrl() !== u;
  };

  // Phase 8.C: tell the backend whenever the iframe finishes loading.
  // Phase 53: still wired but only the legacy iframe code path fires it
  // (currently dead — see slot div below). Native Webview load-completion
  // signaling lands in 53.C alongside the MCP bridge rewire.
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  const handleIframeLoad = () => {
    backend.call("pane_browser_loaded", {
      paneId: p.pane.pane_id,
      url: browser().url || "",
    }).catch(() => {});
  };
  // Silence unused warnings while iframe is dead code (53.C will
  // either fully remove the bridge code or rewire it to the Webview).
  void handleIframeLoad;
  void iframeRef;

  // Phase 53: Webview lifecycle. Spawn on mount, resize on every layout
  // tick (debounced), close on unmount.
  let resizeTimer: number | undefined;
  let lastRect = { x: -1, y: -1, w: -1, h: -1 };
  const debouncedResize = () => {
    if (!slotRef || !webviewLive()) return;
    const r = slotRef.getBoundingClientRect();
    const x = Math.round(r.left);
    const y = Math.round(r.top);
    const w = Math.round(r.width);
    const h = Math.round(r.height);
    if (x === lastRect.x && y === lastRect.y && w === lastRect.w && h === lastRect.h) {
      return;
    }
    lastRect = { x, y, w, h };
    backend.call("browser_pane_resize", {
      paneId: p.pane.pane_id,
      x,
      y,
      w,
      h,
    }).catch((err) => log.error("browser_pane_resize failed", err));
  };
  const queueResize = () => {
    if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
    // 32ms ≈ 30Hz — Yossi's eye won't notice; IPC round-trip survives.
    resizeTimer = window.setTimeout(debouncedResize, 32);
  };

  onMount(() => {
    if (!slotRef) return;
    // First-paint: snapshot the rect, spawn the Webview.
    const r = slotRef.getBoundingClientRect();
    const spawnUrl = resolvedUrl() || browser().url || "about:blank";
    backend.call("browser_pane_spawn", {
      workspaceId: p.workspaceId,
      paneId: p.pane.pane_id,
      url: spawnUrl,
      x: Math.round(r.left),
      y: Math.round(r.top),
      w: Math.round(r.width),
      h: Math.round(r.height),
    })
      .then(() => {
        setWebviewLive(true);
        lastRect = {
          x: Math.round(r.left),
          y: Math.round(r.top),
          w: Math.round(r.width),
          h: Math.round(r.height),
        };
      })
      .catch((err) => log.error("browser_pane_spawn failed", err));

    // Track every layout change that could move/resize the slot.
    const ro = new ResizeObserver(queueResize);
    ro.observe(slotRef);
    // Window resize + scroll (sidebar collapse, etc.) also shift the slot.
    window.addEventListener("resize", queueResize);
    window.addEventListener("scroll", queueResize, true);
    onCleanup(() => {
      ro.disconnect();
      window.removeEventListener("resize", queueResize);
      window.removeEventListener("scroll", queueResize, true);
      if (resizeTimer !== undefined) window.clearTimeout(resizeTimer);
      backend.call("browser_pane_close", {
        paneId: p.pane.pane_id,
      }).catch(() => {});
    });
  });

  // Phase 8.C: serve agent-side requests (eval, screenshot) for THIS pane.
  type BrowserRequest = {
    request_id: string;
    kind: "eval" | "screenshot";
    pane_id: string;
    expression?: string;
  };

  // Phase 8.B race fix: when the workspace's SSH session comes up, the
  // backend emits `pane:browser:resolve-stale`. If we're a browser pane in
  // that workspace, bypass the resolved-URL cache and re-fetch — typically
  // we were sitting on a "no active SSH session" error from a too-early
  // resolve attempt during app startup.
  onMount(() => {
    let cancelledStale = false;
    let unlistenStale: UnlistenFn | undefined;
    backend.on<{ workspace_id: string }>("pane:browser:resolve-stale", (e) => {
      if (cancelledStale) return;
      if (e.payload?.workspace_id !== p.workspaceId) return;
      void forceResolveAndReload();
    }).then((u) => {
      if (cancelledStale) {
        u();
      } else {
        unlistenStale = u;
      }
    });
    onCleanup(() => {
      cancelledStale = true;
      unlistenStale?.();
    });
  });

  onMount(() => {
    let cancelled = false;
    let unlisten: UnlistenFn | undefined;
    backend.on<BrowserRequest>("browser:request", async (e) => {
      if (cancelled) return;
      const r = e.payload;
      if (r.pane_id !== p.pane.pane_id) return;
      try {
        if (r.kind === "eval") {
          const win = iframeRef?.contentWindow;
          if (!win) throw new Error("iframe not ready");
          let result: unknown;
          try {
            // Same-origin access throws SecurityError on cross-origin.
            // eslint-disable-next-line @typescript-eslint/no-implied-eval
            result = (win as unknown as { eval: (s: string) => unknown }).eval(
              r.expression || ""
            );
          } catch (err) {
            const msg = String(err);
            if (
              msg.includes("SecurityError") ||
              msg.includes("Blocked") ||
              msg.includes("cross-origin") ||
              msg.toLowerCase().includes("permission")
            ) {
              throw new Error(
                "cross-origin: WebView2 panes (Phase 8.D) needed for JS eval on arbitrary pages"
              );
            }
            throw err;
          }
          let serialized: unknown;
          try {
            serialized = JSON.parse(JSON.stringify(result));
          } catch {
            serialized = String(result);
          }
          await backend.call("pane_browser_response", {
            requestId: r.request_id,
            ok: { value: serialized },
            err: null,
          });
        } else if (r.kind === "screenshot") {
          if (!bodyRef) throw new Error("pane body not mounted");
          // html2canvas cannot enter cross-origin iframes; ignore them so the
          // capture succeeds with the iframe area shown as the body background.
          // OS-level capture (real iframe pixels) lands in 8.D with WebView2.
          const canvas = await html2canvas(bodyRef, {
            useCORS: true,
            backgroundColor: "#0b0d10",
            logging: false,
            ignoreElements: (el: Element) =>
              el.tagName === "IFRAME" &&
              !!(el as HTMLIFrameElement).src &&
              !sameOrigin((el as HTMLIFrameElement).src),
          });
          const dataUrl = canvas.toDataURL("image/png");
          await backend.call("pane_browser_response", {
            requestId: r.request_id,
            ok: dataUrl,
            err: null,
          });
        }
      } catch (err) {
        await backend.call("pane_browser_response", {
          requestId: r.request_id,
          ok: null,
          err: String(err),
        }).catch(() => {});
      }
    }).then((u) => {
      if (cancelled) {
        u();
      } else {
        unlisten = u;
      }
    });
    onCleanup(() => {
      cancelled = true;
      unlisten?.();
    });
  });

  return (
    <div
      class={`pane browser-pane ${p.isActive ? "active" : ""} ${
        p.isWaiting ? "waiting" : ""
      }`}
      onMouseDown={() => p.onFocus(p.pane.pane_id)}
    >
      <div class="pane-header browser-header">
        <button
          class="pane-btn"
          title={t("browser.btn.back")}
          disabled={browser().history.length === 0}
          onClick={(e) => {
            e.stopPropagation();
            p.onGoBack(p.pane.pane_id);
          }}
        >
          <IconArrowLeft size={14} />
        </button>
        <button
          class="pane-btn"
          title={t("browser.btn.reload")}
          onClick={(e) => {
            e.stopPropagation();
            reload();
          }}
        >
          <IconRefreshCcw size={14} />
        </button>
        <button
          class="pane-btn"
          title={t("browser.btn.home")}
          disabled={!browser().home_url}
          onClick={(e) => {
            e.stopPropagation();
            p.onGoHome(p.pane.pane_id);
          }}
        >
          <IconHome size={14} />
        </button>
        <input
          class="browser-url"
          spellcheck={false}
          placeholder="https://…"
          value={urlDraft()}
          onMouseDown={(e) => e.stopPropagation()}
          onInput={(e) => setUrlDraft(e.currentTarget.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              submitUrl();
            }
          }}
        />
        <button
          class="pane-btn"
          title={t("browser.btn.go")}
          onClick={(e) => {
            e.stopPropagation();
            submitUrl();
          }}
        >
          <IconArrowRight size={14} />
        </button>
        <Show when={isTunneled()}>
          <span
            class="browser-tunnel-badge"
            title={`tunneled via SSH → ${resolvedUrl()}`}
          >
            <IconLink size={13} /> tunneled
          </span>
        </Show>
        <Show when={resolveErr()}>
          <span class="browser-tunnel-err" title={resolveErr()!}>
            <IconWarning size={13} />
          </span>
        </Show>
        <label
          class="browser-forward-toggle"
          title={t("browser.btn.forward_localhost")}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <input
            type="checkbox"
            checked={forwardOn()}
            onChange={(e) =>
              p.onSetForward(p.pane.pane_id, e.currentTarget.checked)
            }
          />
          fwd
        </label>
        <Show when={p.pane.title}>
          <span class="pane-title browser-title" title={p.pane.title!}>
            · {p.pane.title}
          </span>
        </Show>
        <button
          class="pane-btn"
          title={t("browser.btn.split_right")}
          onClick={(e) => {
            e.stopPropagation();
            p.onSplit(p.pane.pane_id, "horizontal");
          }}
        >
          <IconColumns size={14} />
        </button>
        <button
          class="pane-btn"
          title={t("browser.btn.split_down")}
          onClick={(e) => {
            e.stopPropagation();
            p.onSplit(p.pane.pane_id, "vertical");
          }}
        >
          <IconRows size={14} />
        </button>
        <button
          class="pane-btn pane-close"
          title={t("browser.btn.close")}
          onClick={(e) => {
            e.stopPropagation();
            p.onClose(p.pane.pane_id);
          }}
        >
          <IconClose size={14} />
        </button>
      </div>
      <div ref={(el) => (bodyRef = el)} class="pane-body browser-body">
        {/* Phase 8.B race fix: friendly waiting message when the SSH session
            isn't ready yet — beats the iframe's generic "connection refused".
            Cleared as soon as a successful resolve sets resolvedUrl. */}
        <Show when={resolveErr()?.includes("no active SSH session")}>
          <div class="browser-waiting">
            <p>{t("browser.waiting_ssh")}</p>
            <p class="browser-hint">
              Connect a terminal pane in this workspace to enable port forwarding,
              then press ↺ to retry.
            </p>
          </div>
        </Show>
        {/* Phase 53: this div is a sized placeholder, NOT the renderer.
            The native child Webview is positioned over its rect by
            browser_pane_resize. When no URL is set we still need the
            slot present so onMount can spawn the Webview; we just
            spawn it with "about:blank". The X-Frame-Options note that
            used to live here is gone — Webview2 panes don't have that
            limit. */}
        <div
          ref={(el) => (slotRef = el)}
          class="browser-webview-slot"
          data-pane-id={p.pane.pane_id}
        />
        <Show when={!resolvedUrl() && !resolveErr()?.includes("no active SSH session")}>
          <div class="browser-placeholder">
            <p>{t("browser.empty_url")}</p>
          </div>
        </Show>
      </div>
    </div>
  );
}
