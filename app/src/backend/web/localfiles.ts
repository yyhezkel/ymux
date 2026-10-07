// Files from the user's own computer, in a browser (Phase 116, WEB-DESIGN F2).
//
// The desktop hands the UI real local paths: a native open dialog
// (`host.pickPaths`) and OS drag-and-drop (`host.onDragDrop`), then
// `file_upload` / `pane_upload_dropped` read the file at that path. A page
// never sees a path — it gets `File` objects. So this module keeps each File
// under a token shaped like a path, `webfile:<n>/<name>`: the UI's own
// basename logic (`split(/[\\/]/).pop()`) still yields the file's name, and
// the upload handlers in web.ts swap the token back for the File. Nothing
// else in the UI changes.

import type { DragDropPayload, EventCallback, PickPathsOptions, UnlistenFn } from "../types";

const PREFIX = "webfile:";
const files = new Map<string, File>();
let seq = 0;

/** Register a File; returns its path-shaped token. */
export function tokenFor(f: File): string {
  const t = `${PREFIX}${++seq}/${f.name || "file"}`;
  files.set(t, f);
  return t;
}

export const isToken = (p: string): boolean => p.startsWith(PREFIX);

/** The File behind a token (kept: the same pick may be pasted twice). */
export function fileFor(token: string): File {
  const f = files.get(token);
  if (!f) throw new Error("that file is no longer available — pick it again");
  return f;
}

/** host.pickPaths: a file input. Directories cannot be uploaded from here. */
export function pickPaths(opts: PickPathsOptions): Promise<string | string[] | null> {
  if (opts.directory) return Promise.resolve(null);
  return new Promise((resolve) => {
    const input = document.createElement("input");
    input.type = "file";
    input.multiple = !!opts.multiple;
    input.style.display = "none";
    let done = false;
    const finish = (v: string | string[] | null) => {
      if (done) return;
      done = true;
      input.remove();
      resolve(v);
    };
    input.addEventListener("change", () => {
      const picked = [...(input.files ?? [])].map(tokenFor);
      if (picked.length === 0) finish(null);
      else finish(opts.multiple ? picked : picked[0]);
    });
    input.addEventListener("cancel", () => finish(null));
    document.body.appendChild(input);
    input.click();
  });
}

/**
 * host.onDragDrop: OS file drags over the page, in the desktop's payload
 * shape. Positions are in physical pixels like Tauri's on Windows (the
 * consumers divide by devicePixelRatio). Text / URL drags are left to the
 * panes' own HTML5 handlers.
 */
export function onDragDrop(cb: EventCallback<DragDropPayload>): Promise<UnlistenFn> {
  const hasFiles = (e: DragEvent) => !!e.dataTransfer && [...e.dataTransfer.types].includes("Files");
  const dpr = () => window.devicePixelRatio || 1;
  const pos = (e: DragEvent) => ({ x: e.clientX * dpr(), y: e.clientY * dpr() });
  const emit = (payload: DragDropPayload) => cb({ payload });
  const over = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault(); // allow the drop
    emit({ type: e.type === "dragenter" ? "enter" : "over", position: pos(e) });
  };
  const leave = (e: DragEvent) => {
    // Only leaving the window counts; moving between children fires too.
    if (hasFiles(e) && e.relatedTarget === null) emit({ type: "leave" });
  };
  const drop = (e: DragEvent) => {
    if (!hasFiles(e)) return;
    e.preventDefault(); // never navigate to the file
    const paths = [...(e.dataTransfer?.files ?? [])].map(tokenFor);
    emit({ type: "drop", paths, position: pos(e) });
  };
  window.addEventListener("dragenter", over);
  window.addEventListener("dragover", over);
  window.addEventListener("dragleave", leave);
  window.addEventListener("drop", drop);
  return Promise.resolve(() => {
    window.removeEventListener("dragenter", over);
    window.removeEventListener("dragover", over);
    window.removeEventListener("dragleave", leave);
    window.removeEventListener("drop", drop);
  });
}
