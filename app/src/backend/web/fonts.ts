// The browser terminal's monospace font (WebBackend.init).
//
// The terminal's font stack names Windows fonts (Cascadia Mono, Consolas,
// Courier New). Elsewhere none exists, and the generic `monospace` is not
// reliably monospaced: on a box without mono fonts it resolves to a
// proportional face (measured: i 4.7px, W 14.9px at 17px), xterm's DOM
// renderer pads every glyph to the cell with letter-spacing, and the whole
// terminal reads "C l a u d e" — while Hebrew, absent from that face, comes
// from yet another one. Liberation Mono (OFL, unmodified,
// /fonts/liberation-mono-*.ttf) covers Latin and Hebrew at one 0.6em advance,
// so it is declared here and sits in the stack BEFORE the generic families:
// a configured or Windows font still wins, and nothing falls to `monospace`.
// The desktop never declares the family, so there it resolves to nothing.

import { createLogger } from "../../logger";

const log = createLogger("FONTS");

export const WEB_MONO = "YMUX Mono";

export async function installWebMono(): Promise<void> {
  const style = document.createElement("style");
  style.textContent = [400, 700]
    .map(
      (w) =>
        `@font-face { font-family: "${WEB_MONO}"; font-style: normal; font-weight: ${w}; ` +
        `font-display: block; src: url("/fonts/liberation-mono-${w}.ttf") format("truetype"); }`,
    )
    .join("\n");
  document.head.appendChild(style);
  // Load before the first pane draws: xterm measures its cell once, from
  // whatever font is ready at that moment.
  try {
    await document.fonts.load(`16px "${WEB_MONO}"`, "Wא");
  } catch (e) {
    log.warn(`web mono font did not load: ${String(e)}`);
  }
}
