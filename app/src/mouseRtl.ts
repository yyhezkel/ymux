// v0.4.4-beta.4 (RTL mouse fix): coordinate transform for RTL rows.
//
// xterm.js's SelectionService maps `event.clientX` -> buffer column assuming
// the row is laid out LTR. In beta.3 (Approach C+) we set `dir="rtl"` on rows
// whose block contains Hebrew/Arabic, so the BROWSER paints those rows
// visually mirrored - cell 0 lands at the right edge, cell N-1 at the left -
// but the SelectionService still does `col = floor((clientX - screenLeft) /
// cellWidth)`, so a click at what the user SEES as cell 5 lands on cell
// (cols - 5 - 1) in the buffer. The user's selection and click positioning
// both look "mirrored to the wrong side" on Hebrew/Arabic lines. Same story
// inside Claude's CLI running in ymux.
//
// Fix in beta.4: intercept mouse events on the terminal element in CAPTURE
// phase (before xterm's own bubble-phase handlers run), and for events over
// an RTL row, mirror `clientX` around that row's horizontal midpoint. Then
// dispatch a synthetic MouseEvent with the mirrored coord so the
// SelectionService sees the LTR-equivalent position.
//
// The DOM order of row children is unchanged by `dir="rtl"` - it only flips
// the visual paint order - so `document.getSelection().toString()` and
// `term.getSelection()` already return logical order. Clipboard content is
// correct as soon as the visual selection lines up.
//
// This module is deliberately dependency-free and pure (no DOM globals in the
// hot path) so it can be unit tested under `node --test` without a bundler.

export interface RowRect {
  readonly left: number;
  readonly right: number;
  readonly top: number;
  readonly bottom: number;
  readonly dir: "ltr" | "rtl";
  /** Phase 94: an `ltr-end` row (`data-ymux-align="end"`, see
   *  `applyRowDirections`) is painted LTR but packed against the right edge,
   *  so every cell sits `shift` px to the right of where xterm's column math
   *  expects it. 0 / absent for an ordinary row. */
  readonly shift?: number;
}

/**
 * Find the row rect at the given clientY inside a `.xterm-rows` host. Rows are
 * direct children of the host and their `dir` attribute is set by the RTL
 * Approach C+ pass in terminalInstance.ts (see `applyRowDirections`). Returns
 * `null` if no child rect contains clientY (pointer outside the row area).
 *
 * Iterates in DOM order and stops at the first hit; rows do not overlap
 * vertically in the DOM renderer's grid, so a linear scan is fine (viewport
 * is at most a few dozen rows).
 */
export function findRow(
  rowsHost: Element,
  clientY: number,
): RowRect | null {
  const children = rowsHost.children;
  for (let i = 0; i < children.length; i++) {
    const el = children[i] as HTMLElement;
    const r = el.getBoundingClientRect();
    if (clientY >= r.top && clientY < r.bottom) {
      const dir = el.getAttribute("dir") === "rtl" ? "rtl" : "ltr";
      // Phase 94: a right-packed LTR row — the gap between the row's right
      // edge and its last painted span is how far every cell moved.
      let shift = 0;
      if (dir === "ltr" && el.getAttribute("data-ymux-align") === "end") {
        const last = el.lastElementChild;
        if (last) shift = Math.max(0, r.right - last.getBoundingClientRect().right);
      }
      return {
        left: r.left,
        right: r.right,
        top: r.top,
        bottom: r.bottom,
        dir,
        shift,
      };
    }
  }
  return null;
}

/**
 * Mirror `clientX` around the row's horizontal midpoint if the row is RTL;
 * otherwise pass through. `x' = row.left + (row.right - clientX)`, which is
 * the reflection of clientX about `(row.left + row.right) / 2`.
 *
 * Passing `null` for `row` (no row found under the pointer) also passes
 * through, so callers can pipe every event through this helper without
 * pre-filtering.
 */
export function transformMouseX(
  clientX: number,
  row: RowRect | null,
): number {
  if (!row) return clientX;
  if (row.dir === "rtl") return row.left + (row.right - clientX);
  // Phase 94: an `ltr-end` row is in reading order, just moved right by
  // `shift`; undo the move so xterm's `(x - left) / cellWidth` finds the
  // column the user actually clicked.
  return clientX - (row.shift ?? 0);
}
