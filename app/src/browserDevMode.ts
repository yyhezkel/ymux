// Browser Dev Mode — right-click an element in the workspace browser to
// capture it as a ticket.
//
// Everything specific to Dev Mode lives here rather than in
// BrowserWindow.tsx, so the browser component keeps its existing shape:
// it gains one signal, one toolbar button and one re-inject effect, and
// nothing about tabs / port+path / navigation changes.
//
// How the capture gets back to the app
// ------------------------------------
// The workspace browser is a NATIVE child webview pointed at an external
// (tunnelled) page. It has no Tauri IPC — deliberately, since injecting
// it would expose the app's command surface to whatever service the user
// happens to be viewing. Tauri 2.10's WebviewBuilder has no `on_ipc`
// either. The one channel that exists is navigation, so the injected
// script hands its payload over as a URL:
//
//     location.href = "ymux-ticket:<base64url of JSON>"
//
// `workspace_browser.rs::handle_ticket_navigation` decodes it, emits
// `browser:ticket-captured`, and cancels the navigation. Any other URL
// is passed straight through, unchanged.
//
// The script is injected on demand (Dev Mode on) and removed again on
// Dev Mode off — normal browsing never carries any of it.

import { invoke } from "@tauri-apps/api/core";
import { createSignal } from "solid-js";
import { createLogger } from "./logger";

const log = createLogger("TICKETS");

/** Per-workspace Dev Mode flag. Mirrors how the browser already keeps
 *  geometry / port / path / tabs — localStorage, not the Workspace
 *  schema, so enabling Dev Mode needs no workspaces.json migration. */
const DEVMODE_KEY = (workspaceId: string) =>
  `ymux.workspace-browser-devmode.${workspaceId}`;

export function loadDevMode(workspaceId: string): boolean {
  try {
    return localStorage.getItem(DEVMODE_KEY(workspaceId)) === "1";
  } catch {
    return false;
  }
}

export function saveDevMode(workspaceId: string, on: boolean): void {
  try {
    localStorage.setItem(DEVMODE_KEY(workspaceId), on ? "1" : "0");
  } catch {
    /* private mode / quota — Dev Mode just won't persist */
  }
}

/** Per-workspace project override for tickets. Empty/absent means "let
 *  the backend derive it" — which is the normal case. Kept here (not on
 *  the Workspace schema) for the same reason as the Dev Mode flag: no
 *  workspaces.json migration for a per-machine preference. The backend
 *  still owns the resolution ladder; this is only the top rung. */
const PROJECT_OVERRIDE_KEY = (workspaceId: string) =>
  `ymux.tickets-project.${workspaceId}`;

export function loadProjectOverride(workspaceId: string): string | null {
  try {
    const v = localStorage.getItem(PROJECT_OVERRIDE_KEY(workspaceId));
    return v && v.trim() ? v : null;
  } catch {
    return null;
  }
}

export function saveProjectOverride(workspaceId: string, path: string | null): void {
  try {
    if (path && path.trim()) {
      localStorage.setItem(PROJECT_OVERRIDE_KEY(workspaceId), path.trim());
    } else {
      localStorage.removeItem(PROJECT_OVERRIDE_KEY(workspaceId));
    }
  } catch {
    /* private mode / quota */
  }
}

/** Shape the injected script produces, and therefore the shape the Rust
 *  bridge round-trips to `browser:ticket-captured`. */
export interface ElementCapture {
  url: string;
  xpath: string;
  selector: string;
  html: string;
  style: Record<string, unknown>;
  /** PNG data-url of the element, or null when the snapshot could not be
   *  taken (tainted canvas, zero-size element, too large, timeout). A
   *  missing screenshot never blocks the capture. */
  shot: string | null;
  /** Why `shot` is null; null when a shot exists (or the capture came
   *  from a script predating this field). */
  shot_error: ShotError | null;
}

/** Reasons a snapshot can be missing. Mirrored by the injected script
 *  and by the `tickets.modal.shot.reason.*` i18n keys. */
export const SHOT_ERRORS = [
  "zero-size",
  "timeout",
  "tainted",
  "load-failed",
  "too-large",
  "blank",
  "error",
] as const;
export type ShotError = (typeof SHOT_ERRORS)[number];

/** Payload of the `browser:ticket-captured` Tauri event. */
export interface TicketCaptureEvent {
  workspace_id: string;
  capture: ElementCapture;
}

/** Narrow an unknown event payload into an ElementCapture. The capture
 *  crosses a JSON boundary from an untrusted page, so nothing is assumed
 *  (Rule #5 — narrow, never `any`). */
export function parseCapture(value: unknown): ElementCapture | null {
  if (typeof value !== "object" || value === null) return null;
  const o = value as Record<string, unknown>;
  const str = (k: string): string => (typeof o[k] === "string" ? (o[k] as string) : "");
  const style =
    typeof o.style === "object" && o.style !== null
      ? (o.style as Record<string, unknown>)
      : {};
  // A capture with no selector AND no xpath tells us nothing about which
  // element was hit — treat it as malformed rather than opening an empty
  // ticket (the Phase 54 failure mode).
  const selector = str("selector");
  const xpath = str("xpath");
  if (!selector && !xpath) return null;
  // Only accept a PNG data-url — the page could put anything here, and
  // this string ends up in an <img src>.
  const rawShot = typeof o.shot === "string" ? o.shot : "";
  const shot = rawShot.startsWith("data:image/png;base64,") ? rawShot : null;
  // Allowlist: the reason picks an i18n key, so anything unknown is null.
  const shotError = SHOT_ERRORS.find((e) => e === o.shot_error) ?? null;
  return { url: str("url"), xpath, selector, html: str("html"), style, shot, shot_error: shotError };
}

/** The capture awaiting a description, or null when no ticket modal is
 *  open. Module-level so App.tsx can fold it into `anyModalOpen()` (the
 *  native browser webview paints above HTML and must be hidden while the
 *  modal is up) without the browser component having to route it. */
const [pendingCapture, setPendingCapture] = createSignal<PendingCapture | null>(null);
export { pendingCapture, setPendingCapture };

export interface PendingCapture {
  workspaceId: string;
  capture: ElementCapture;
}

/** Render a capture as Markdown.
 *
 *  Shared by the modal's "copy for later" path (when there is nowhere to
 *  write) and by the panel's copy action, so a ticket reads the same
 *  whether it was saved or only copied. `TicketsPanel` adds the fields a
 *  saved ticket has and a fresh capture does not (id, status, dates). */
export function captureToMarkdown(capture: ElementCapture, description: string): string {
  let style = "{}";
  try {
    style = JSON.stringify(capture.style, null, 2);
  } catch {
    /* keep "{}" */
  }
  return [
    "# Ticket (not saved)",
    "",
    `- URL: ${capture.url}`,
    "",
    "## Description",
    "",
    description.trim() || "_(none)_",
    "",
    "## Element",
    "",
    `- Selector: \`${capture.selector}\``,
    `- XPath: \`${capture.xpath}\``,
    "",
    "```html",
    capture.html,
    "```",
    "",
    "## Computed style",
    "",
    "```json",
    style,
    "```",
    "",
  ].join("\n");
}

/** Cap on captured markup. Keeps the sentinel URL small and bounds how
 *  much page content can land in a ticket file. */
const HTML_MAX = 4096;

/** Snapshot limits. The PNG rides the same `ymux-ticket:` URL as the
 *  rest of the capture, so it has to stay sane: oversized elements are
 *  scaled down, and a data-url past the byte cap is dropped rather than
 *  risking the navigation. */
const SHOT_MAX_W = 1000;
const SHOT_MAX_H = 700;
const SHOT_MAX_BYTES = 512 * 1024;
/** Give up on a snapshot that hasn't rasterized in this long. */
const SHOT_TIMEOUT_MS = 1500;
/** Stop inlining styles past this many nodes — a huge subtree would
 *  produce an SVG bigger than the screenshot is worth. */
const SHOT_MAX_NODES = 400;

/** The injected inspect script.
 *
 *  Self-contained ES5 (the target page can be anything, including one
 *  with a strict parser or an old framework). Idempotent: re-running it
 *  tears down a previous instance first. Everything it adds is reachable
 *  from `window.__ymuxTicket.stop()`.
 */
export function inspectScript(): string {
  return `
(function(){
  if (window.__ymuxTicket) { try { window.__ymuxTicket.stop(); } catch(e){} }
  var HTML_MAX = ${HTML_MAX};
  var hi = document.createElement('div');
  hi.setAttribute('data-ymux-ticket-ui','1');
  hi.style.cssText = 'position:fixed;z-index:2147483647;pointer-events:none;'
    + 'border:2px solid #e0567a;background:rgba(224,86,122,0.12);'
    + 'border-radius:2px;display:none;transition:none;';
  var tip = document.createElement('div');
  tip.setAttribute('data-ymux-ticket-ui','1');
  tip.style.cssText = 'position:fixed;z-index:2147483647;pointer-events:none;'
    + 'background:#e0567a;color:#fff;font:11px/1.4 ui-monospace,Menlo,Consolas,monospace;'
    + 'padding:1px 5px;border-radius:2px;display:none;max-width:60vw;'
    + 'overflow:hidden;text-overflow:ellipsis;white-space:nowrap;';
  function mount(){
    if (document.body) { document.body.appendChild(hi); document.body.appendChild(tip); }
  }
  mount();

  function cssEscape(s){ return String(s).replace(/[^a-zA-Z0-9_-]/g, '\\\\$&'); }

  function xpathOf(el){
    var parts = [];
    while (el && el.nodeType === 1 && parts.length < 64) {
      var i = 0, sib = el.previousSibling;
      while (sib) {
        if (sib.nodeType === 1 && sib.nodeName === el.nodeName) i++;
        sib = sib.previousSibling;
      }
      parts.unshift(el.nodeName.toLowerCase() + '[' + (i + 1) + ']');
      el = el.parentNode;
    }
    return '/' + parts.join('/');
  }

  // Shortest selector that still resolves to this exact element:
  // #id when unique, else a tag.class chain walked up until unique.
  function selectorOf(el){
    if (!el || el.nodeType !== 1) return '';
    if (el.id) {
      var byId = '#' + cssEscape(el.id);
      try { if (document.querySelectorAll(byId).length === 1) return byId; } catch(e){}
    }
    var parts = [], node = el;
    while (node && node.nodeType === 1 && parts.length < 8) {
      var seg = node.nodeName.toLowerCase();
      if (node.id) { seg = '#' + cssEscape(node.id); parts.unshift(seg); break; }
      var cls = (typeof node.className === 'string' ? node.className : '').trim();
      if (cls) {
        var list = cls.split(/\\s+/).slice(0, 3).map(cssEscape);
        if (list.length) seg += '.' + list.join('.');
      }
      var parent = node.parentNode;
      if (parent && parent.nodeType === 1) {
        var same = 0, idx = 0, k;
        for (k = 0; k < parent.children.length; k++) {
          if (parent.children[k].nodeName === node.nodeName) {
            same++;
            if (parent.children[k] === node) idx = same;
          }
        }
        if (same > 1) seg += ':nth-of-type(' + idx + ')';
      }
      parts.unshift(seg);
      var candidate = parts.join(' > ');
      try { if (document.querySelectorAll(candidate).length === 1) return candidate; } catch(e){}
      node = parent;
    }
    return parts.join(' > ');
  }

  function b64url(str){
    var bytes = new TextEncoder().encode(str);
    var bin = '';
    for (var i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
    return btoa(bin).replace(/\\+/g, '-').replace(/\\//g, '_').replace(/=+$/, '');
  }

  // ── element snapshot ────────────────────────────────────────────
  // SVG <foreignObject> rasterization. No library is injected into the
  // page. The catch is that a cloned node inside foreignObject inherits
  // NOTHING from the document's stylesheets, so computed styles have to
  // be inlined by hand — without that the snapshot is unstyled black
  // text on transparent, which is worse than no snapshot.
  //
  // Every failure reports a reason via done(null, reason); success is
  // done(url, null). Known limits, all of which degrade to a reason
  // rather than to a wrong ticket: cross-origin images/fonts taint the canvas and toDataURL
  // throws; very large subtrees are skipped; anything slow times out.
  var SHOT_PROPS = [
    'display','position','width','height','margin','padding','border',
    'border-radius','box-sizing','background-color','background-image',
    'color','font','font-family','font-size','font-weight','font-style',
    'line-height','letter-spacing','text-align','text-decoration',
    'text-transform','white-space','vertical-align','opacity','overflow',
    'flex-direction','justify-content','align-items','gap','list-style',
    'box-shadow','outline','visibility'
  ];

  function inlineStyles(src, dst, budget){
    if (budget.n > ${SHOT_MAX_NODES}) return;
    budget.n++;
    if (src.nodeType === 1 && dst.nodeType === 1) {
      var cs = window.getComputedStyle(src);
      var decl = '';
      for (var i = 0; i < SHOT_PROPS.length; i++) {
        var v = cs.getPropertyValue(SHOT_PROPS[i]);
        if (v) decl += SHOT_PROPS[i] + ':' + v + ';';
      }
      dst.setAttribute('style', decl);
      // Inputs don't serialize their live value into markup.
      if (dst.tagName === 'INPUT' && 'value' in src) dst.setAttribute('value', src.value);
    }
    var sc = src.childNodes, dc = dst.childNodes;
    for (var j = 0; j < sc.length && j < dc.length; j++) {
      inlineStyles(sc[j], dc[j], budget);
    }
  }

  function snapshot(el, done){
    var finished = false;
    function finish(v, reason){ if (!finished) { finished = true; done(v, reason); } }
    setTimeout(function(){ finish(null, 'timeout'); }, ${SHOT_TIMEOUT_MS});
    try {
      var r = el.getBoundingClientRect();
      var w = Math.ceil(r.width), h = Math.ceil(r.height);
      if (!w || !h) return finish(null, 'zero-size');
      var scale = Math.min(1, ${SHOT_MAX_W} / w, ${SHOT_MAX_H} / h);
      var clone = el.cloneNode(true);
      inlineStyles(el, clone, { n: 0 });
      // Neutralize the element's own offset — we want it drawn at 0,0.
      clone.style.margin = '0';
      clone.style.position = 'static';
      var xml = new XMLSerializer().serializeToString(clone);
      var svg = '<svg xmlns="http://www.w3.org/2000/svg" width="' + w + '" height="' + h + '">'
        + '<foreignObject x="0" y="0" width="100%" height="100%">'
        + '<div xmlns="http://www.w3.org/1999/xhtml">' + xml + '</div>'
        + '</foreignObject></svg>';
      var img = new Image();
      img.onload = function(){
        var url;
        try {
          var c = document.createElement('canvas');
          c.width = Math.max(1, Math.round(w * scale));
          c.height = Math.max(1, Math.round(h * scale));
          var ctx = c.getContext('2d');
          ctx.drawImage(img, 0, 0, c.width, c.height);
          // Alpha scan: a fully transparent canvas is a failed render,
          // not a snapshot. getImageData also throws on a tainted canvas.
          var px = ctx.getImageData(0, 0, c.width, c.height).data;
          var seen = false;
          for (var k = 3; k < px.length; k += 4) {
            if (px[k] !== 0) { seen = true; break; }
          }
          if (!seen) return finish(null, 'blank');
          url = c.toDataURL('image/png');
        } catch (e) { return finish(null, 'tainted'); }
        if (url.length > ${SHOT_MAX_BYTES}) return finish(null, 'too-large');
        finish(url, null);
      };
      img.onerror = function(){ finish(null, 'load-failed'); };
      img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(svg);
    } catch (e) { finish(null, 'error'); }
  }

  // Build the capture payload for an element and snapshot it. The
  // snapshot is best-effort: done(payload) always fires once, and a null
  // shot just means this ticket has no image. It never loses the capture.
  function capture(el, done){
    var r = el.getBoundingClientRect();
    var cs = window.getComputedStyle(el);
    var payload = {
      url: location.href,
      xpath: xpathOf(el),
      selector: selectorOf(el),
      html: (el.outerHTML || '').slice(0, HTML_MAX),
      style: {
        color: cs.color,
        background: cs.backgroundColor,
        font: cs.font || (cs.fontSize + ' ' + cs.fontFamily),
        display: cs.display,
        bbox: { x: r.left, y: r.top, w: r.width, h: r.height }
      },
      shot: null,
      shot_error: null
    };
    snapshot(el, function(shot, reason){
      payload.shot = shot;
      payload.shot_error = shot ? null : (reason || 'error');
      done(payload);
    });
  }

  function targetFrom(e){
    var el = e.target;
    if (!el || el.nodeType !== 1 || el.getAttribute('data-ymux-ticket-ui')) {
      el = document.elementFromPoint(e.clientX, e.clientY);
    }
    return (el && el.nodeType === 1) ? el : null;
  }

  function onMove(e){
    var el = targetFrom(e);
    if (!el) { hi.style.display = 'none'; tip.style.display = 'none'; return; }
    var r = el.getBoundingClientRect();
    hi.style.display = 'block';
    hi.style.left = r.left + 'px';
    hi.style.top = r.top + 'px';
    hi.style.width = r.width + 'px';
    hi.style.height = r.height + 'px';
    tip.style.display = 'block';
    tip.textContent = selectorOf(el);
    tip.style.left = r.left + 'px';
    tip.style.top = (r.top > 18 ? r.top - 18 : r.bottom + 2) + 'px';
  }

  function onCtx(e){
    var el = targetFrom(e);
    if (!el) return;
    e.preventDefault();
    e.stopPropagation();
    // Freeze the highlight while rasterizing so the click feels handled.
    tip.textContent = 'ymux: capturing…';
    capture(el, function(payload){
      try {
        location.href = 'ymux-ticket:' + b64url(JSON.stringify(payload));
      } catch (err) { /* nothing we can surface from in here */ }
    });
  }

  document.addEventListener('mousemove', onMove, true);
  document.addEventListener('contextmenu', onCtx, true);

  window.__ymuxTicket = { capture: capture, stop: function(){
    document.removeEventListener('mousemove', onMove, true);
    document.removeEventListener('contextmenu', onCtx, true);
    if (hi.parentNode) hi.parentNode.removeChild(hi);
    if (tip.parentNode) tip.parentNode.removeChild(tip);
    window.__ymuxTicket = null;
  }};
})();
`;
}

/** Removes everything `inspectScript` added. Safe to run when nothing
 *  is installed. */
export function teardownScript(): string {
  return "if (window.__ymuxTicket) { try { window.__ymuxTicket.stop(); } catch(e){} }";
}

/** Inject (or remove) the inspect script in a workspace's browser
 *  webview. Never throws — a browser that isn't open yet just has no
 *  webview to talk to, which is not an error worth surfacing. */
export async function applyDevMode(workspaceId: string, on: boolean): Promise<void> {
  try {
    await invoke("workspace_browser_eval", {
      workspaceId,
      js: on ? inspectScript() : teardownScript(),
    });
  } catch (e) {
    log.debug("devmode eval skipped (no webview yet?)", e);
  }
}
