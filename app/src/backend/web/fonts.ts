// Hebrew in the browser terminal (WebBackend.init).
//
// The terminal's font stack names Windows fonts (Cascadia Mono, Consolas,
// Courier New). On Android and Linux none exists, `monospace` resolves to a
// font with no Hebrew, and the browser borrows Hebrew glyphs from a
// proportional system font — narrower than a cell, so the row's columns drift
// and spaces show up in the wrong places. Liberation Mono (OFL, unmodified,
// /fonts/liberation-mono-*.ttf) has Hebrew at the same 0.6em advance; it is
// declared here, limited by unicode-range to Hebrew, and sits LAST in the
// stack, so it only ever supplies glyphs the earlier fonts lack. The desktop
// never declares it, so its rendering is unchanged.

import { createLogger } from "../../logger";

const log = createLogger("FONTS");

export const HEBREW_MONO = "YMUX Hebrew Mono";

const HEBREW_RANGE = "U+0590-05FF, U+FB1D-FB4F";

export async function installHebrewMono(): Promise<void> {
  const style = document.createElement("style");
  style.textContent = [400, 700]
    .map(
      (w) =>
        `@font-face { font-family: "${HEBREW_MONO}"; font-style: normal; font-weight: ${w}; ` +
        `font-display: block; src: url("/fonts/liberation-mono-${w}.ttf") format("truetype"); ` +
        `unicode-range: ${HEBREW_RANGE}; }`,
    )
    .join("\n");
  document.head.appendChild(style);
  // Load before the first pane draws: a renderer that caches glyphs would
  // otherwise keep the fallback shapes it drew while the file was in flight.
  try {
    await document.fonts.load(`16px "${HEBREW_MONO}"`, "א");
  } catch (e) {
    log.warn(`hebrew mono font did not load: ${String(e)}`);
  }
}
