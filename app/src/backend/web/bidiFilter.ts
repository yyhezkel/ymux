// Smart bidi for browser panes (Phase 117, WEB-DESIGN F3) — a port of
// app/src-tauri/src/bidi_filter.rs, which the desktop runs in the PTY read
// path. Opt-in per pane (`smart_bidi` on the layout leaf): Latin runs near
// Hebrew/Arabic are wrapped in FSI (U+2068) / PDI (U+2069) isolates so the
// renderer cannot reorder them against the RTL text around them.
//
// What it must not break, as in Rust: escape sequences (CSI / OSC / DCS)
// pass through verbatim and may straddle chunks; a newline resets the RTL
// context; a text run dominated by box-drawing is left alone (Claude Code's
// borders). Input is decoded text (web/pty.ts runs a streaming TextDecoder
// first), so a chunk never splits a character — the Rust contract.
// The tests in bidiFilter.test.ts are the Rust tests, carried over.

const FSI = "⁨";
const PDI = "⁩";
const ESCAPE_BUF_MAX = 64;
const OSC_BUF_MAX = 512;
/** A Latin run is isolated only if Hebrew/Arabic appeared this recently. */
const RTL_CONTEXT_WINDOW = 200;

type State = "normal" | "esc" | "csi" | "osc" | "oscEsc" | "dcs" | "dcsEsc";

const isLatinRunChar = (c: string): boolean => /^[A-Za-z0-9_/.\-\\:+@~]$/.test(c);

const isRtl = (cp: number): boolean => cp >= 0x0590 && cp <= 0x077f;

const isBox = (cp: number): boolean => cp >= 0x2500 && cp <= 0x259f;

export class BidiFilter {
  private state: State = "normal";
  private escapeBuf = "";
  private charsSinceRtl = RTL_CONTEXT_WINDOW;
  enabled: boolean;

  constructor(enabled = false) {
    this.enabled = enabled;
  }

  /** Toggle; resets transient state like Rust's set_pane_enabled. */
  setEnabled(on: boolean): void {
    this.enabled = on;
    this.state = "normal";
    this.escapeBuf = "";
    this.charsSinceRtl = RTL_CONTEXT_WINDOW;
  }

  process(input: string): string {
    if (!this.enabled) return input;
    let out = "";
    let text = "";
    const flushEscape = () => {
      out += this.escapeBuf;
      this.escapeBuf = "";
      this.state = "normal";
    };
    for (const c of input) {
      const b = c.codePointAt(0) ?? 0;
      switch (this.state) {
        case "normal":
          if (b === 0x1b) {
            out += this.flushText(text);
            text = "";
            this.escapeBuf = c;
            this.state = "esc";
          } else if (b < 0x20) {
            // Control bytes flush the run; LF resets the RTL context.
            out += this.flushText(text);
            text = "";
            out += c;
            if (c === "\n") this.charsSinceRtl = RTL_CONTEXT_WINDOW;
          } else {
            text += c;
          }
          break;
        case "esc":
          this.escapeBuf += c;
          if (c === "[") this.state = "csi";
          else if (c === "]") this.state = "osc";
          else if (c === "P") this.state = "dcs";
          else flushEscape(); // a single-byte escape, or an unknown intro
          break;
        case "csi":
          this.escapeBuf += c;
          if ((b >= 0x40 && b <= 0x7e) || this.escapeBuf.length > ESCAPE_BUF_MAX) flushEscape();
          break;
        case "osc":
          this.escapeBuf += c;
          if (b === 0x07) flushEscape();
          else if (b === 0x1b) this.state = "oscEsc";
          else if (this.escapeBuf.length > OSC_BUF_MAX) flushEscape();
          break;
        case "oscEsc":
          this.escapeBuf += c;
          if (b === 0x5c) flushEscape();
          else this.state = "osc";
          break;
        case "dcs":
          this.escapeBuf += c;
          if (b === 0x1b) this.state = "dcsEsc";
          else if (this.escapeBuf.length > OSC_BUF_MAX) flushEscape();
          break;
        case "dcsEsc":
          this.escapeBuf += c;
          if (b === 0x5c) flushEscape();
          else this.state = "dcs";
          break;
      }
    }
    return out + this.flushText(text);
  }

  private flushText(s: string): string {
    if (!s) return "";
    const chars = [...s];
    const box = chars.filter((c) => isBox(c.codePointAt(0) ?? 0)).length;
    if (box * 2 > chars.length) {
      this.charsSinceRtl = Math.min(RTL_CONTEXT_WINDOW, this.charsSinceRtl + chars.length);
      return s;
    }
    let result = "";
    let run = "";
    let runStart = this.charsSinceRtl;
    const writeRun = () => {
      result += runStart < RTL_CONTEXT_WINDOW ? FSI + run + PDI : run;
      run = "";
    };
    for (const c of chars) {
      if (isLatinRunChar(c)) {
        if (!run) runStart = this.charsSinceRtl;
        run += c;
        this.charsSinceRtl = Math.min(RTL_CONTEXT_WINDOW, this.charsSinceRtl + 1);
      } else {
        if (run) writeRun();
        result += c;
        this.charsSinceRtl = isRtl(c.codePointAt(0) ?? 0) ? 0 : Math.min(RTL_CONTEXT_WINDOW, this.charsSinceRtl + 1);
      }
    }
    if (run) writeRun();
    return result;
  }
}
