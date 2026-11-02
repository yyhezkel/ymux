import { createMemo, createSignal, Show } from "solid-js";
import { backend } from "./backend";
import MarkdownIt from "markdown-it";
import DOMPurify from "dompurify";
import { t } from "./i18n";
import { markdownDoc, closeMarkdown } from "./mdViewerStore";
import { createLogger } from "./logger";

const log = createLogger("MARKDOWN");
import { IconFile, IconClose } from "./icons";
import {
  clampToViewport,
  makeWindowControls,
  ResizeHandles,
  type Geometry,
} from "./floatingWindow";

// Phase GG: read-only Markdown viewer, shown as a floating window (reuses
// the same drag/resize chrome as the File Manager window). Double-clicking
// a `.md` file in the File Manager opens it here instead of the OS app.
//
// Security: `html: false` makes markdown-it drop raw HTML in the source at
// parse time, and DOMPurify scrubs the rendered output as a second layer
// (Rule-of-thumb defense in depth). Links are never auto-navigated — clicks
// are intercepted and only http(s)/mailto are handed to the system opener.
//
// RTL: the body `dir` is chosen by a Hebrew-vs-Latin heuristic, and
// `unicode-bidi: plaintext` on block elements (in App.css) keeps mixed
// Hebrew/English paragraphs readable. Code blocks are forced LTR.

const md = new MarkdownIt({ html: false, linkify: true, breaks: false });

const DEFAULT_GEOMETRY: Geometry = { x: 200, y: 120, w: 820, h: 640 };
const MIN_W = 360;
const MIN_H = 240;

/** Hebrew/Arabic vs Latin code-point count → overall text direction. */
function detectDir(src: string): "rtl" | "ltr" {
  const rtl = (src.match(/[֐-׿؀-ۿ]/g) || []).length;
  const latin = (src.match(/[A-Za-z]/g) || []).length;
  return rtl > latin ? "rtl" : "ltr";
}

export function MarkdownViewer() {
  const [geom, setGeom] = createSignal<Geometry>(
    clampToViewport(DEFAULT_GEOMETRY, MIN_W, MIN_H)
  );
  const { onDragStart, onResizeStart } = makeWindowControls({
    geom,
    setGeom,
    minW: MIN_W,
    minH: MIN_H,
    closeGuardSelector: ".md-window-x",
  });

  const html = createMemo(() => {
    const d = markdownDoc();
    if (!d) return "";
    const rendered = md.render(d.source);
    return DOMPurify.sanitize(rendered, {
      FORBID_TAGS: ["script", "style", "iframe", "object", "embed", "form"],
      FORBID_ATTR: ["onerror", "onload", "onclick", "onmouseover"],
      ADD_ATTR: ["target"],
    });
  });

  const dir = createMemo<"rtl" | "ltr">(() => {
    const d = markdownDoc();
    return d ? detectDir(d.source) : "ltr";
  });

  // Intercept link clicks in the rendered body. http(s)/mailto → system
  // opener; everything else (relative paths, javascript:, etc.) is blocked
  // in this round.
  const onBodyClick = (e: MouseEvent) => {
    const target = e.target as HTMLElement | null;
    const a = target?.closest("a");
    if (!a) return;
    e.preventDefault();
    const href = a.getAttribute("href") ?? "";
    if (/^(https?:\/\/|mailto:)/i.test(href)) {
      void backend.host.openUrl(href).catch((err) => log.warn("openUrl failed", err));
    } else {
      log.warn(`blocked non-external link: ${href}`);
    }
  };

  return (
    <Show when={markdownDoc()}>
      <div
        class="fm-window md-window"
        style={{
          left: `${geom().x}px`,
          top: `${geom().y}px`,
          width: `${geom().w}px`,
          height: `${geom().h}px`,
        }}
      >
        <div class="fm-window-header" onMouseDown={onDragStart}>
          <span class="fm-window-title"><IconFile size={15} /> {markdownDoc()!.title}</span>
          <button
            class="fm-window-x md-window-x"
            onClick={closeMarkdown}
            title={t("common.close")}
            aria-label={t("common.close")}
          >
            <IconClose size={15} />
          </button>
        </div>
        <div class="fm-window-body md-view-body">
          {/* eslint-disable-next-line solid/no-innerhtml -- sanitized above */}
          <div class="md-view" dir={dir()} innerHTML={html()} onClick={onBodyClick} />
        </div>
        <ResizeHandles onStart={onResizeStart} />
      </div>
    </Show>
  );
}
