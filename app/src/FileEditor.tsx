import { createSignal, Show, onMount, onCleanup } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { keyEq } from "./shortcuts";
import { TechText } from "./TechText";
import { IconSearch, IconClose, IconArrowUp, IconArrowDown, IconWarning } from "./icons";

// Phase 17.B: minimal built-in editor. A modal with a monospace
// <textarea>, Save / Cancel / Reload buttons, and an unsaved-changes
// confirmation on close. Syntax highlighting is intentionally out of
// scope — we want this to be a "view the file, fix a typo, save"
// affordance, not a code editor.
//
// Phase 23: direction follows document language (dir="auto" + manual
// override) instead of being locked to RTL by the surrounding UI;
// find/replace bar with Ctrl+F / Ctrl+H, case + regex toggles,
// next/prev navigation, replace one / replace all.

interface FileContents {
  text: string;
  encoding: string;
  is_binary: boolean;
  size: number;
  truncated: boolean;
}

interface Props {
  open: boolean;
  /** Display name shown in the header. */
  filename: string;
  /** Full path shown under the filename + used by the backend. */
  path: string;
  /** "local" → reads/writes via fs. "remote" → SFTP via the
   *  workspace. */
  side: "local" | "remote";
  /** Required when side === "remote". */
  workspaceId?: string;
  onClose: () => void;
  /** Fires after a successful save so the parent (file manager) can
   *  refresh the row's size/mtime in the listing. */
  onSaved?: () => void;
}

type EditorDir = "auto" | "ltr" | "rtl";

export function FileEditor(p: Props) {
  const [contents, setContents] = createSignal<string>("");
  const [original, setOriginal] = createSignal<string>("");
  const [meta, setMeta] = createSignal<FileContents | null>(null);
  const [loading, setLoading] = createSignal(true);
  const [saving, setSaving] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [largeWarn, setLargeWarn] = createSignal(false);

  // Phase 23: text direction. Default mirrors the UI direction —
  // Hebrew/Arabic UI users get RTL out of the box (matching pre-Phase 23
  // behaviour), LTR users get LTR. Switching to "auto" makes the
  // browser pick base direction from the first strong character in the
  // value (per-paragraph thanks to `unicode-bidi: plaintext` on the
  // textarea); "ltr"/"rtl" pin the direction regardless of content.
  const initialDir: EditorDir =
    (document?.documentElement?.dir as EditorDir) === "rtl" ? "rtl" : "ltr";
  const [editorDir, setEditorDir] = createSignal<EditorDir>(initialDir);

  // Phase 23: find & replace state. The bar is hidden by default and
  // pops in on Ctrl+F (find) or Ctrl+H (find + replace).
  const [findOpen, setFindOpen] = createSignal(false);
  const [replaceOpen, setReplaceOpen] = createSignal(false);
  const [findQuery, setFindQuery] = createSignal("");
  const [replaceQuery, setReplaceQuery] = createSignal("");
  const [caseSensitive, setCaseSensitive] = createSignal(false);
  const [useRegex, setUseRegex] = createSignal(false);
  const [matchCount, setMatchCount] = createSignal(0);
  const [matchIdx, setMatchIdx] = createSignal(0); // 1-based for display

  let textareaRef: HTMLTextAreaElement | undefined;
  let findInputRef: HTMLInputElement | undefined;
  let replaceInputRef: HTMLInputElement | undefined;

  const dirty = () => contents() !== original();

  const load = async () => {
    setLoading(true);
    setErr(null);
    try {
      const fc: FileContents =
        p.side === "local"
          ? await backend.call("file_read_local", { path: p.path })
          : await backend.call("file_read_remote", {
              workspaceId: p.workspaceId,
              path: p.path,
            });
      setMeta(fc);
      setContents(fc.text);
      setOriginal(fc.text);
      // Phase 17.B threshold check happens against the actual byte
      // size returned by the backend so we don't second-guess what
      // counts as "large".
      const threshold = await backend.call<number>("file_large_threshold");
      setLargeWarn(fc.size > threshold && !fc.is_binary);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };

  const save = async () => {
    if (meta()?.is_binary) return;
    setSaving(true);
    setErr(null);
    try {
      if (p.side === "local") {
        await backend.call("file_write_local", { path: p.path, text: contents() });
      } else {
        await backend.call("file_write_remote", {
          workspaceId: p.workspaceId,
          path: p.path,
          text: contents(),
        });
      }
      setOriginal(contents());
      p.onSaved?.();
    } catch (e) {
      setErr(String(e));
    } finally {
      setSaving(false);
    }
  };

  const tryClose = () => {
    if (dirty()) {
      if (!window.confirm(t("editor.unsaved_confirm"))) return;
    }
    p.onClose();
  };

  // ─── Phase 23: find & replace primitives ────────────────────────

  /** Build the RegExp the find/replace logic uses. Returns null when
   *  the query is empty or the user-supplied regex doesn't compile. */
  const buildRegex = (): RegExp | null => {
    const q = findQuery();
    if (!q) return null;
    try {
      const flags = caseSensitive() ? "g" : "gi";
      const pat = useRegex() ? q : q.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
      return new RegExp(pat, flags);
    } catch {
      return null;
    }
  };

  /** Recompute total match count + which match (1-based) the current
   *  selection is on. Called after any change to query / flags /
   *  content so the "n of N" counter stays honest. */
  const recountMatches = () => {
    const re = buildRegex();
    if (!re || !textareaRef) {
      setMatchCount(0);
      setMatchIdx(0);
      return;
    }
    const text = contents();
    let count = 0;
    let cursorIdx = 0;
    const pos = textareaRef.selectionStart;
    let m: RegExpExecArray | null;
    re.lastIndex = 0;
    while ((m = re.exec(text)) !== null) {
      count++;
      if (m.index <= pos && cursorIdx === 0) {
        cursorIdx = count;
      }
      // Prevent zero-length-match infinite loops.
      if (m.index === re.lastIndex) re.lastIndex++;
    }
    setMatchCount(count);
    setMatchIdx(cursorIdx);
  };

  const findNext = (direction: 1 | -1 = 1) => {
    const re = buildRegex();
    if (!re || !textareaRef) return;
    const text = contents();
    const start = direction === 1
      ? textareaRef.selectionEnd
      : textareaRef.selectionStart;
    const matches: { i: number; len: number }[] = [];
    let m: RegExpExecArray | null;
    re.lastIndex = 0;
    while ((m = re.exec(text)) !== null) {
      matches.push({ i: m.index, len: m[0].length });
      if (m.index === re.lastIndex) re.lastIndex++;
    }
    if (matches.length === 0) {
      setMatchCount(0);
      setMatchIdx(0);
      return;
    }
    let target: { i: number; len: number } | undefined;
    if (direction === 1) {
      target = matches.find((mm) => mm.i >= start) || matches[0];
    } else {
      // Last match whose end <= start (the cursor); else wrap.
      const before = matches.filter((mm) => mm.i + mm.len <= start);
      target = before.length > 0 ? before[before.length - 1] : matches[matches.length - 1];
    }
    if (!target) return;
    textareaRef.focus();
    textareaRef.setSelectionRange(target.i, target.i + target.len);
    // Scroll the match into view — textarea doesn't auto-scroll on
    // selection programmatically.
    scrollSelectionIntoView();
    setMatchCount(matches.length);
    setMatchIdx(matches.findIndex((mm) => mm.i === target!.i) + 1);
  };

  const scrollSelectionIntoView = () => {
    if (!textareaRef) return;
    // Cheap approximation: place cursor → blur → focus seems to
    // trigger scroll in most webviews. Cleaner: compute line height *
    // line number. Approximation is good enough for v1.
    const ta = textareaRef;
    const lineHeight = parseFloat(getComputedStyle(ta).lineHeight) || 18;
    const before = ta.value.slice(0, ta.selectionStart);
    const line = before.split("\n").length - 1;
    const target = line * lineHeight;
    if (target < ta.scrollTop || target > ta.scrollTop + ta.clientHeight - lineHeight) {
      ta.scrollTop = Math.max(0, target - ta.clientHeight / 2);
    }
  };

  const replaceCurrent = () => {
    if (!textareaRef) return;
    const re = buildRegex();
    if (!re) return;
    const sel = contents().slice(textareaRef.selectionStart, textareaRef.selectionEnd);
    // Only replace if the current selection matches the search.
    // Use a fresh non-global RE for the test.
    const flags = caseSensitive() ? "" : "i";
    const pat = useRegex() ? findQuery() : findQuery().replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    let testRe: RegExp;
    try {
      testRe = new RegExp(`^(?:${pat})$`, flags);
    } catch {
      return;
    }
    if (!testRe.test(sel)) {
      // Cursor not on a match — advance to next match first.
      findNext(1);
      return;
    }
    const start = textareaRef.selectionStart;
    const rep = useRegex() ? sel.replace(testRe, replaceQuery()) : replaceQuery();
    const next = contents().slice(0, start) + rep + contents().slice(textareaRef.selectionEnd);
    setContents(next);
    // Set caret after the replacement and find next.
    queueMicrotask(() => {
      if (!textareaRef) return;
      textareaRef.value = next; // for native sync
      textareaRef.setSelectionRange(start + rep.length, start + rep.length);
      findNext(1);
      recountMatches();
    });
  };

  const replaceAll = () => {
    const re = buildRegex();
    if (!re) return;
    const before = contents();
    let count = 0;
    const after = before.replace(re, () => {
      count++;
      return replaceQuery();
    });
    if (count > 0) {
      setContents(after);
      // Clear match position; recountMatches will repopulate.
      queueMicrotask(() => {
        if (textareaRef) textareaRef.setSelectionRange(0, 0);
        recountMatches();
      });
    }
  };

  const openFindBar = (withReplace: boolean) => {
    setFindOpen(true);
    setReplaceOpen(withReplace);
    // Pre-fill query with current selection if there is one.
    if (textareaRef) {
      const sel = textareaRef.value.slice(textareaRef.selectionStart, textareaRef.selectionEnd);
      if (sel && !sel.includes("\n")) setFindQuery(sel);
    }
    queueMicrotask(() => {
      if (withReplace) replaceInputRef?.focus();
      else findInputRef?.focus();
      findInputRef?.select();
    });
  };
  const closeFindBar = () => {
    setFindOpen(false);
    setReplaceOpen(false);
    textareaRef?.focus();
  };

  // Ctrl+S → save. Ctrl+F → find bar. Ctrl+H → find + replace.
  // Esc → close bar (or modal if bar is already closed).
  const keydown = (e: KeyboardEvent) => {
    if (!p.open) return;
    // Phase 62.B (item G): keyEq → physical-key match so Ctrl+S/F/H work
    // on a Hebrew (non-US) layout too.
    if (e.ctrlKey && !e.shiftKey && !e.altKey && keyEq(e, "s")) {
      e.preventDefault();
      void save();
      return;
    }
    if (e.ctrlKey && !e.shiftKey && !e.altKey && keyEq(e, "f")) {
      e.preventDefault();
      openFindBar(false);
      return;
    }
    if (e.ctrlKey && !e.shiftKey && !e.altKey && keyEq(e, "h")) {
      e.preventDefault();
      openFindBar(true);
      return;
    }
    if (e.key === "F3") {
      e.preventDefault();
      findNext(e.shiftKey ? -1 : 1);
      return;
    }
    if (e.key === "Escape") {
      e.preventDefault();
      if (findOpen()) closeFindBar();
      else tryClose();
    }
  };

  onMount(() => {
    if (p.open) void load();
    window.addEventListener("keydown", keydown);
  });
  onCleanup(() => window.removeEventListener("keydown", keydown));

  const fmtSize = (n: number) => {
    if (n < 1024) return `${n} B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
    return `${(n / 1024 / 1024).toFixed(1)} MB`;
  };

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={tryClose}>
        <div
          class="modal editor-modal"
          onClick={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div class="editor-head">
            <h3>{t("editor.title", { filename: p.filename })}</h3>
            <span class="editor-path" title={p.path}><TechText text={p.path} /></span>
            {/* Phase 23: direction picker. Default "auto" handles
                 most files correctly via UAX #9 first-strong; manual
                 override is for when the heuristic guesses wrong. */}
            <select
              class="editor-dir"
              title={t("editor.dir.title")}
              value={editorDir()}
              onChange={(ev) => setEditorDir(ev.currentTarget.value as EditorDir)}
            >
              <option value="auto">{t("editor.dir.auto")}</option>
              <option value="ltr">LTR</option>
              <option value="rtl">RTL</option>
            </select>
            <button
              class="fm-tool"
              title={t("editor.btn.find")}
              onClick={() => openFindBar(false)}
            >
              <IconSearch size={14} />
            </button>
            <button class="feed-x" title={t("common.close")} onClick={tryClose}>
              <IconClose size={14} />
            </button>
          </div>

          <Show when={findOpen()}>
            <div class="editor-find-bar" dir="ltr">
              <input
                ref={(el) => (findInputRef = el)}
                class="editor-find-input"
                placeholder={t("editor.find.placeholder")}
                value={findQuery()}
                onInput={(ev) => {
                  setFindQuery(ev.currentTarget.value);
                  recountMatches();
                }}
                onKeyDown={(ev) => {
                  if (ev.key === "Enter") {
                    ev.preventDefault();
                    findNext(ev.shiftKey ? -1 : 1);
                  } else if (ev.key === "Escape") {
                    ev.preventDefault();
                    closeFindBar();
                  }
                }}
                spellcheck={false}
              />
              <span class="editor-find-count">
                {matchCount() > 0 ? `${matchIdx()}/${matchCount()}` : "0/0"}
              </span>
              <button
                class="fm-tool"
                title={t("editor.find.prev")}
                onClick={() => findNext(-1)}
              >
                <IconArrowUp size={14} />
              </button>
              <button
                class="fm-tool"
                title={t("editor.find.next")}
                onClick={() => findNext(1)}
              >
                <IconArrowDown size={14} />
              </button>
              <label class="fm-checkbox" title={t("editor.find.case_sensitive")}>
                <input
                  type="checkbox"
                  checked={caseSensitive()}
                  onChange={(ev) => {
                    setCaseSensitive(ev.currentTarget.checked);
                    recountMatches();
                  }}
                />
                <span>Aa</span>
              </label>
              <label class="fm-checkbox" title={t("editor.find.regex")}>
                <input
                  type="checkbox"
                  checked={useRegex()}
                  onChange={(ev) => {
                    setUseRegex(ev.currentTarget.checked);
                    recountMatches();
                  }}
                />
                <span>.*</span>
              </label>
              <button
                class="fm-tool"
                title={replaceOpen() ? t("editor.find.hide_replace") : t("editor.find.show_replace")}
                onClick={() => setReplaceOpen(!replaceOpen())}
              >
                {replaceOpen() ? "−" : "+"}
              </button>
              <button class="feed-x" title={t("common.close")} onClick={closeFindBar}>
                <IconClose size={14} />
              </button>
            </div>
            <Show when={replaceOpen()}>
              <div class="editor-find-bar editor-replace-bar" dir="ltr">
                <input
                  ref={(el) => (replaceInputRef = el)}
                  class="editor-find-input"
                  placeholder={t("editor.replace.placeholder")}
                  value={replaceQuery()}
                  onInput={(ev) => setReplaceQuery(ev.currentTarget.value)}
                  onKeyDown={(ev) => {
                    if (ev.key === "Enter") {
                      ev.preventDefault();
                      if (ev.shiftKey) replaceAll();
                      else replaceCurrent();
                    } else if (ev.key === "Escape") {
                      ev.preventDefault();
                      closeFindBar();
                    }
                  }}
                  spellcheck={false}
                />
                <button class="fm-action" onClick={replaceCurrent} disabled={matchCount() === 0}>
                  {t("editor.replace.one")}
                </button>
                <button class="fm-action" onClick={replaceAll} disabled={matchCount() === 0}>
                  {t("editor.replace.all")}
                </button>
              </div>
            </Show>
          </Show>

          <Show when={loading()}>
            <div class="editor-status">{t("common.loading")}</div>
          </Show>

          <Show when={err()}>
            <div class="editor-status err"><IconWarning size={14} /> {err()}</div>
          </Show>

          <Show when={!loading() && meta()?.is_binary}>
            <div class="editor-status err">
              {t("editor.binary_warning")}
            </div>
          </Show>

          <Show when={!loading() && largeWarn()}>
            <div class="editor-status warn">
              {t("editor.large_file_warning", {
                size: meta() ? fmtSize(meta()!.size) : "?",
              })}
            </div>
          </Show>

          <Show when={!loading() && !meta()?.is_binary}>
            <textarea
              ref={(el) => (textareaRef = el)}
              class="editor-textarea"
              spellcheck={false}
              dir={editorDir()}
              value={contents()}
              onInput={(e) => {
                setContents(e.currentTarget.value);
                if (findOpen()) recountMatches();
              }}
              onSelect={() => recountMatches()}
              autofocus
            />
            <div class="editor-meta">
              {meta()?.encoding ?? "—"} ·{" "}
              {meta() ? fmtSize(meta()!.size) : "0 B"}
              <Show when={dirty()}>
                <span class="editor-dirty"> · {t("editor.dirty")}</span>
              </Show>
            </div>
          </Show>

          <div class="modal-buttons">
            <button onClick={() => void load()} disabled={loading() || saving()}>
              {t("editor.btn.reload")}
            </button>
            <button onClick={tryClose}>{t("editor.btn.cancel")}</button>
            <button
              class="primary"
              disabled={loading() || saving() || !dirty() || meta()?.is_binary}
              onClick={() => void save()}
            >
              {saving() ? t("common.saving") : t("editor.btn.save")}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
