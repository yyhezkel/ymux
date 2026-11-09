// The Diff pane's line notes: anchors, sanitizing and the prompt text. Pure,
// and with type-only imports, so `diffNotes.test.ts` runs under `node --test`.
//
// A note is anchored by file + side + line + line text. The pane re-renders
// wholesale on every diff event, so nothing here may refer to a DOM node or a
// row index. Status is derived from the current parse, never stored: when a
// transient error empties the diff, the notes come back as soon as it returns.
//
// The prompt reaches a live shell or agent, and part of it (paths, code lines)
// comes from the repository. Treat all of it as untrusted.

import type { DiffSource } from "./bindings/DiffSource";
import type { LineRef, ParsedDiff } from "./diffModel";

export interface DiffNote extends LineRef {
  id: string;
  /** The line's text when the note was written; compared to detect drift. */
  lineText: string;
  text: string;
}

export type NoteStatus = "current" | "outdated";

/** A pane the user can send to. */
export interface DiffSendTarget {
  paneId: string;
  label: string;
  isAgent: boolean;
}

export type DiffSendResult = "sent" | "not_connected" | "no_bracketed_paste";

export const NOTE_MAX_CHARS = 4000;
const PROMPT_CODE_MAX_CHARS = 200;

export function noteKey(a: LineRef): string {
  return `${a.file}\0${a.side}\0${a.line}`;
}

/** Notes are per diff target: a worktree plus a source. */
export function diffTargetKey(workspaceId: string, cwd: string, source: DiffSource): string {
  return `${workspaceId}\0${cwd}\0${source.kind}\0${source.kind === "ref" ? source.git_ref : ""}`;
}

export interface IndexedLine {
  idx: number;
  text: string;
}

/** Anchor key → the row that holds that anchor in this parse. */
export function indexLines(parsed: ParsedDiff): Map<string, IndexedLine> {
  const out = new Map<string, IndexedLine>();
  parsed.refs.forEach((ref, idx) => {
    if (ref) out.set(noteKey(ref), { idx, text: parsed.lines[idx].text });
  });
  return out;
}

/** Outdated when the anchored line is gone or its text changed. A line that
 *  only moved also reads as outdated: matching on text alone could pick the
 *  wrong copy of a common line such as `}`. */
export function noteStatus(note: DiffNote, index: Map<string, IndexedLine>): NoteStatus {
  return index.get(noteKey(note))?.text === note.lineText ? "current" : "outdated";
}

// C0, DEL and C1 except LF. ESC and the 8-bit CSI are in here, so a pasted
// text cannot close the bracket with its own ESC[201~.
const CONTROLS = /[\u0000-\u0009\u000b-\u001f\u007f-\u009f]/g;
// Directional marks, embeddings, overrides and isolates: they can make the
// preview show text in a different order from the bytes that are sent.
const BIDI_CONTROLS = /[؜‎‏‪-‮⁦-⁩]/g;
const LINE_BREAKS = /\r\n?|\u0085| | /g;

/** Remove every control character. LF is the only one that can stay. */
export function stripControls(s: string): string {
  return s
    .replace(/\t/g, "  ")
    .replace(LINE_BREAKS, "\n")
    .replace(CONTROLS, "")
    .replace(BIDI_CONTROLS, "");
}

/** For a file name or a code line: one line, no controls. */
export function sanitizeInline(s: string): string {
  return stripControls(s).replace(/\n+/g, " ").trim();
}

/** For note text: keeps line breaks, removes the other controls, caps length. */
export function sanitizeNote(s: string): string {
  return Array.from(stripControls(s)).slice(0, NOTE_MAX_CHARS).join("").trim();
}

function capped(s: string): string {
  if (s.length <= PROMPT_CODE_MAX_CHARS) return s;
  return `${Array.from(s).slice(0, PROMPT_CODE_MAX_CHARS).join("")}...`;
}

function describeSource(source: DiffSource): string {
  switch (source.kind) {
    case "working": return "unstaged changes";
    case "head": return "all changes vs HEAD";
    case "ref": return `working tree vs ${sanitizeInline(source.git_ref)}`;
  }
}

export interface PromptContext {
  cwd: string;
  branch: string | null;
  source: DiffSource;
}

/** The exact text the user previews and sends. The frame stays in English:
 *  it is an instruction to an agent, not UI. Returns "" when no note has text.
 *  Never ends with a line break. */
export function buildNotesPrompt(
  notes: { note: DiffNote; status: NoteStatus }[],
  ctx: PromptContext,
): string {
  const blocks: string[] = [];
  for (const { note, status } of notes) {
    const text = sanitizeNote(note.text);
    if (!text) continue;
    const flag = status === "outdated" ? " [outdated: this line changed after the note]" : "";
    const lines = [`${blocks.length + 1}. ${sanitizeInline(note.file)}:${note.line} (${note.side} side)${flag}`];
    const code = capped(sanitizeInline(note.lineText));
    if (code) lines.push(`   code: ${code}`);
    const [first, ...rest] = text.split("\n");
    lines.push(`   note: ${first}`, ...rest.map((l) => `   | ${l}`));
    blocks.push(lines.join("\n"));
  }
  if (blocks.length === 0) return "";
  const where = [sanitizeInline(ctx.cwd), ctx.branch ? `branch ${sanitizeInline(ctx.branch)}` : ""]
    .filter(Boolean)
    .join(", ");
  return [
    `Review notes on a git diff (${describeSource(ctx.source)})${where ? ` in ${where}` : ""}. Address each note.`,
    "",
    ...blocks,
  ].join("\n");
}

/** What goes to `pty_write`: one bracketed paste with no Enter after it.
 *  Line breaks become CR, as xterm's own paste does. Controls are stripped
 *  again here because this is the last step before the PTY. */
export function bracketedPastePayload(text: string): string {
  const body = stripControls(text).replace(/\n+$/, "");
  if (!body) return "";
  return `\x1b[200~${body.replace(/\n/g, "\r")}\x1b[201~`;
}
