// Phase 110. Pure on purpose: argv.test.ts pins it.

/**
 * Free-text claude arguments → argv. Whitespace separates; single or double
 * quotes group; nothing is evaluated (no $, globs, escapes) — the daemon runs
 * the result as an argv after `--`, never through a shell (Rule #3).
 */
export function splitArgs(s: string): string[] {
  const out: string[] = [];
  let cur = "";
  let quote: string | null = null;
  let has = false;
  for (const ch of s) {
    if (quote) {
      if (ch === quote) quote = null;
      else cur += ch;
    } else if (ch === "'" || ch === '"') {
      quote = ch;
      has = true;
    } else if (/\s/.test(ch)) {
      if (has || cur) out.push(cur);
      cur = "";
      has = false;
    } else {
      cur += ch;
    }
  }
  if (has || cur) out.push(cur);
  return out;
}
