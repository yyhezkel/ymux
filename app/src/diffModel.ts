// Phase 91.F: the Diff pane's pure model — unified-diff parsing, the
// file-list status letter, and path normalization. Import-free on purpose
// so `diffModel.test.ts` runs under plain `node --test` (relative imports
// there need explicit extensions and this module has none to need).

export type DiffLine =
  | { kind: "context"; text: string }
  | { kind: "add"; text: string }
  | { kind: "del"; text: string }
  | { kind: "hunk"; text: string } // @@ header
  | { kind: "file"; text: string }; // "diff --git ..." / "+++ ..." / "--- ..."

export interface Hunk {
  fileLabel: string;
  headerIdx: number; // index into `lines` of the @@ header
  lineSpan: [number, number]; // [start, endExclusive) in `lines`
}

export type DiffSide = "old" | "new";

/** Where a content line lives in a file. A deleted line is on the old side;
 *  an added or context line is on the new side. */
export interface LineRef {
  file: string;
  side: DiffSide;
  line: number;
}

export interface ParsedDiff {
  lines: DiffLine[];
  hunks: Hunk[];
  /** path → index into `lines` of that file's `diff --git` line. */
  anchors: Record<string, number>;
  /** Parallel to `lines`. Null for a header or for a line outside a hunk's
   *  declared count (combined `@@@` hunks have no count, so no refs). */
  refs: (LineRef | null)[];
}

const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/** Pull the destination path out of a `diff --git a/<p> b/<p>` header.
 *  The naive `/ b\/(\S+)/` broke on paths with spaces and on renames; this
 *  uses the identical-path symmetry (`a/P b/P` → the two halves match) and
 *  falls back to the last ` b/` for a rename (`a/old b/new`). */
export function fileFromGitHeader(raw: string): string {
  const rest = raw.slice("diff --git ".length);
  // Common case: both prefixes present and paths equal → `a/P b/P`.
  if (rest.startsWith("a/") && rest.includes(" b/")) {
    const p = (rest.length - 5) / 2; // len = 2 + P + 1 + 2 + P
    if (Number.isInteger(p) && p > 0) {
      const first = rest.slice(2, 2 + p);
      const second = rest.slice(2 + p + 1 + 2);
      if (first === second) return first;
    }
    // Rename or --noprefix off: take everything after the last " b/".
    const i = rest.lastIndexOf(" b/");
    if (i >= 0) return rest.slice(i + 3);
  }
  return rest;
}

export function parseDiff(text: string): ParsedDiff {
  const out: DiffLine[] = [];
  const hunks: Hunk[] = [];
  const anchors: Record<string, number> = {};
  const refs: (LineRef | null)[] = [];
  if (!text) return { lines: out, hunks, anchors, refs };
  const src = text.split("\n");
  let currentFile = "";
  let inHunk = false;
  let hunkStart = -1;
  // Line numbers, and how many lines the current @@ header still promises.
  let oldNo = 0;
  let newNo = 0;
  let oldLeft = 0;
  let newLeft = 0;
  const push = (line: DiffLine, ref: LineRef | null = null) => {
    out.push(line);
    refs.push(ref);
  };
  const ref = (side: DiffSide, line: number): LineRef | null =>
    currentFile ? { file: currentFile, side, line } : null;
  const closeHunk = (endExclusive: number) => {
    if (hunkStart >= 0) {
      hunks.push({ fileLabel: currentFile, headerIdx: hunkStart, lineSpan: [hunkStart, endExclusive] });
    }
    inHunk = false;
    hunkStart = -1;
  };
  for (let i = 0; i < src.length; i++) {
    // Phase 91.H: a CRLF working tree (Windows, core.autocrlf) puts a
    // literal CR at the end of every content line; it must not reach the
    // DOM under `white-space: pre`.
    const raw = src[i].endsWith("\r") ? src[i].slice(0, -1) : src[i];
    if (raw.startsWith("diff --git ")) {
      closeHunk(out.length);
      currentFile = fileFromGitHeader(raw);
      anchors[currentFile] = out.length;
      oldLeft = newLeft = 0;
      push({ kind: "file", text: raw });
      continue;
    }
    // Inside a hunk's declared count, a deleted `-- x` is the raw line
    // `--- x` and an added `++ x` is `+++ x`: content, not a file header.
    if (
      oldLeft === 0 && newLeft === 0 && (
      raw.startsWith("--- ") || raw.startsWith("+++ ") ||
      raw.startsWith("index ") || raw.startsWith("new file mode") ||
      raw.startsWith("deleted file mode") || raw.startsWith("similarity index") ||
      raw.startsWith("rename from ") || raw.startsWith("rename to ") ||
      raw.startsWith("old mode ") || raw.startsWith("new mode ") ||
      raw.startsWith("Binary files "))
    ) {
      push({ kind: "file", text: raw });
      continue;
    }
    if (raw.startsWith("@@")) {
      closeHunk(out.length);
      hunkStart = out.length;
      inHunk = true;
      const m = HUNK_HEADER.exec(raw);
      oldNo = m ? Number(m[1]) : 0;
      oldLeft = m ? Number(m[2] ?? "1") : 0;
      newNo = m ? Number(m[3]) : 0;
      newLeft = m ? Number(m[4] ?? "1") : 0;
      push({ kind: "hunk", text: raw });
      continue;
    }
    if (!inHunk) {
      if (raw.length === 0) continue; // blank line between files
      push({ kind: "file", text: raw });
      continue;
    }
    if (raw.startsWith("+")) {
      const r = newLeft > 0 ? ref("new", newNo) : null;
      if (newLeft > 0) { newNo++; newLeft--; }
      push({ kind: "add", text: raw.slice(1) }, r);
    } else if (raw.startsWith("-")) {
      const r = oldLeft > 0 ? ref("old", oldNo) : null;
      if (oldLeft > 0) { oldNo++; oldLeft--; }
      push({ kind: "del", text: raw.slice(1) }, r);
    } else if (raw.startsWith(" ") || raw.length === 0) {
      const counted = oldLeft > 0 && newLeft > 0;
      const r = counted ? ref("new", newNo) : null;
      if (counted) { oldNo++; newNo++; oldLeft--; newLeft--; }
      push({ kind: "context", text: raw.length === 0 ? "" : raw.slice(1) }, r);
    } else {
      // Includes "\ No newline at end of file": no file line behind it.
      push({ kind: "context", text: raw });
    }
  }
  closeHunk(out.length);
  return { lines: out, hunks, anchors, refs };
}

/** The single letter the file list shows for a porcelain XY status.
 *  `??` → `?`; otherwise the first non-space of the two-char code
 *  (index status, else worktree status). */
export function statusLetter(xy: string): string {
  if (xy === "??") return "?";
  const t = xy.trim();
  return t.length > 0 ? t[0] : "?";
}

/** Normalize a path for prefix/equality comparison across git (which
 *  emits `/`) and a workspace cwd (which may be a Windows `\` path):
 *  `\`→`/`, drop a trailing slash, and — for a drive-letter path — fold
 *  the WHOLE key to lowercase, because NTFS is case-insensitive and git
 *  reports the on-disk casing while the picker recorded the user's.
 *  POSIX paths stay case-sensitive. Mirror of `norm_path` in lib.rs. */
export function pathKey(path: string): string {
  const p = path.replace(/\\/g, "/").replace(/\/+$/, "");
  return /^[a-zA-Z]:(\/|$)/.test(p) ? p.toLowerCase() : p;
}
