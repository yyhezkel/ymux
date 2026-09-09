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

export interface ParsedDiff {
  lines: DiffLine[];
  hunks: Hunk[];
  /** path → index into `lines` of that file's `diff --git` line. */
  anchors: Record<string, number>;
}

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
  if (!text) return { lines: out, hunks, anchors };
  const src = text.split("\n");
  let currentFile = "";
  let inHunk = false;
  let hunkStart = -1;
  const closeHunk = (endExclusive: number) => {
    if (hunkStart >= 0) {
      hunks.push({ fileLabel: currentFile, headerIdx: hunkStart, lineSpan: [hunkStart, endExclusive] });
    }
    inHunk = false;
    hunkStart = -1;
  };
  for (let i = 0; i < src.length; i++) {
    const raw = src[i];
    if (raw.startsWith("diff --git ")) {
      closeHunk(out.length);
      currentFile = fileFromGitHeader(raw);
      anchors[currentFile] = out.length;
      out.push({ kind: "file", text: raw });
      continue;
    }
    if (
      raw.startsWith("--- ") || raw.startsWith("+++ ") ||
      raw.startsWith("index ") || raw.startsWith("new file mode") ||
      raw.startsWith("deleted file mode") || raw.startsWith("similarity index") ||
      raw.startsWith("rename from ") || raw.startsWith("rename to ") ||
      raw.startsWith("old mode ") || raw.startsWith("new mode ") ||
      raw.startsWith("Binary files ")
    ) {
      out.push({ kind: "file", text: raw });
      continue;
    }
    if (raw.startsWith("@@")) {
      closeHunk(out.length);
      hunkStart = out.length;
      inHunk = true;
      out.push({ kind: "hunk", text: raw });
      continue;
    }
    if (!inHunk) {
      if (raw.length === 0) continue; // blank line between files
      out.push({ kind: "file", text: raw });
      continue;
    }
    if (raw.startsWith("+")) {
      out.push({ kind: "add", text: raw.slice(1) });
    } else if (raw.startsWith("-")) {
      out.push({ kind: "del", text: raw.slice(1) });
    } else if (raw.startsWith(" ") || raw.length === 0) {
      out.push({ kind: "context", text: raw.length === 0 ? "" : raw.slice(1) });
    } else if (raw === "\\ No newline at end of file") {
      out.push({ kind: "context", text: raw });
    } else {
      out.push({ kind: "context", text: raw });
    }
  }
  closeHunk(out.length);
  return { lines: out, hunks, anchors };
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
 *  `\`→`/`, drop a trailing slash, lowercase a drive letter. */
export function pathKey(path: string): string {
  let p = path.replace(/\\/g, "/").replace(/\/+$/, "");
  if (/^[a-zA-Z]:/.test(p)) p = p[0].toLowerCase() + p.slice(1);
  return p;
}
