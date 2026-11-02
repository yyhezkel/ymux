// Phase 98: how many Shift+Up/Down keys one wheel event is worth.
//
// The tmux conf scrolls 3 lines per key (`send-keys -X -N 3 scroll-up`, the
// rate the pre-91 WheelUpPane binding used), so a key is a NOTCH, not a line.
// Phase 91.D sent a fixed 3 keys per event instead, which (a) tripped tmux's
// paste detection — see `assume-paste-time` in ymux-tmux.conf — and (b) made
// a touchpad, which streams dozens of tiny events per gesture, run away.
//
// Pure, so it is unit-tested (wheelSteps.test.ts); terminalInstance.ts keeps
// the carry between events.

/** A DOM WheelEvent.deltaMode value: 0 pixels, 1 lines, 2 pages. */
export type WheelDeltaMode = 0 | 1 | 2;

/** Pixels one mouse notch is worth. Chromium/WebView2 reports ~100 per notch
 *  at 100% scale; display scaling moves it, so anything from half a notch up
 *  counts as at least one step rather than vanishing into the carry. */
const NOTCH_PX = 100;
const MIN_NOTCH_PX = NOTCH_PX / 2;
/** Lines one notch is worth in line mode (Firefox-style `deltaMode 1`). */
const NOTCH_LINES = 3;
/** Ceiling per event, so one huge fling cannot queue a screenful of keys. */
const MAX_STEPS = 10;

export interface WheelResult {
  /** Signed key count: negative = up (Shift+Up), positive = down. */
  steps: number;
  /** Sub-notch remainder to pass back in with the next event. */
  carry: number;
}

/**
 * Turn one wheel event into whole steps.
 *
 * - A notch-sized event (a mouse wheel) is always worth at least one step.
 * - Small events (a touchpad) accumulate in `carry` until they add up to half
 *   a notch, then pay out one step at a time.
 * - Reversing direction drops the carry, so a flick back is not eaten by the
 *   leftover of the other direction.
 */
export function wheelSteps(carry: number, deltaY: number, deltaMode: WheelDeltaMode): WheelResult {
  if (deltaY === 0) return { steps: 0, carry };
  const sign = deltaY < 0 ? -1 : 1;
  const px =
    deltaMode === 1 ? (deltaY * NOTCH_PX) / NOTCH_LINES : deltaMode === 2 ? deltaY * NOTCH_PX * 3 : deltaY;
  const base = Math.sign(carry) === sign ? carry : 0;
  const total = base + px;
  const mag = Math.abs(total);
  if (mag < MIN_NOTCH_PX) return { steps: 0, carry: total };
  const whole = Math.min(MAX_STEPS, Math.max(1, Math.round(mag / NOTCH_PX)));
  // A notch-class event settles the account; only sub-notch leftovers of a
  // touchpad stream carry over.
  const rest = Math.abs(px) >= MIN_NOTCH_PX ? 0 : total - sign * whole * NOTCH_PX;
  return { steps: sign * whole, carry: Math.sign(rest) === sign ? rest : 0 };
}
