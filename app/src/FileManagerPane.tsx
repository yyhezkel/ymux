import { createSignal, createEffect, For, Show, onMount, onCleanup, createMemo } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { t } from "./i18n";
import { FileEditor } from "./FileEditor";
import { TechText } from "./TechText";
import { open } from "@tauri-apps/plugin-dialog";
import { saveRemoteFileAs } from "./download";
import { loadFmPaths, saveFmPaths } from "./fmPaths";
import { isMac, isWindows, sep } from "./platform";
import { createLogger } from "./logger";

const log = createLogger("FM");
import { openMarkdown, isMarkdownFile } from "./mdViewerStore";
import { TransferBar } from "./TransferBar";
import {
  IconArrowUp,
  IconRefresh,
  IconPlus,
  IconScissors,
  IconClipboard,
  IconClose,
  IconChevronUp,
  IconChevronDown,
  IconUpload,
  IconDownload,
  IconCopy,
  IconWarning,
  IconFolder,
  IconLink,
  IconFile,
} from "./icons";

// Phase 15.B: dual-column file manager (local + remote SFTP).
//
// Lives inside a layout-pane just like Terminal / Browser. Local
// column always renders; remote column lights up only when the
// workspace has an active SSH session (the backend will return a
// friendly error otherwise — surfaced as a banner).
//
// Phase 23: full-featured polish — new file (not just folder), upload
// from arbitrary disk path via native picker, OS drag-and-drop, copy
// path action, real popup context menu (no more window.prompt).

interface FileEntry {
  name: string;
  is_dir: boolean;
  is_link: boolean;
  size: number;
  modified: number;
  permissions: string;
}

interface Props {
  workspaceId: string;
  /** True if the workspace is an SSH workspace (i.e. the right column
   *  should be visible). When false we show only the local column.    */
  hasSsh: boolean;
  /** Phase 16: True iff a terminal pane in the workspace currently
   *  has an active SSH session. When false (SSH workspace, no
   *  terminal connected yet) the remote column shows a friendly
   *  "connect a terminal first" placeholder instead of an error. */
  hasActiveSession?: boolean;
  /** Phase 80.1: reopen each column at the directory it was last showing
   *  (per workspace) instead of $HOME. Opt-in — Settings → General →
   *  `file_manager_remember_path`. Absent/false keeps the pre-80.1
   *  behavior, which is what an untouched install gets. */
  rememberPath?: boolean;
}

type Side = "local" | "remote";

export function FileManagerPane(p: Props) {
  const [localPath, setLocalPath] = createSignal("");
  const [remotePath, setRemotePath] = createSignal("");
  // Feedback (cut/copy/paste): a single-item clipboard for moving/copying
  // files between locations. Same-side paste (local→local, remote→remote) and
  // local→remote upload are supported; remote→local uses the Download action.
  const [clip, setClip] = createSignal<{
    op: "cut" | "copy";
    side: Side;
    path: string;
    name: string;
  } | null>(null);
  const [localEntries, setLocalEntries] = createSignal<FileEntry[]>([]);
  const [remoteEntries, setRemoteEntries] = createSignal<FileEntry[]>([]);
  const [localSel, setLocalSel] = createSignal<string | null>(null);
  const [remoteSel, setRemoteSel] = createSignal<string | null>(null);
  const [showHidden, setShowHidden] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [status, setStatus] = createSignal<string>("");

  // Phase 62 (item 6): in-pane confirm toast with Confirm/Cancel
  // actions. Replaces window.confirm for destructive (delete, unzip
  // overwrite) and packing (zip) operations — the native dialog is
  // jarring and can render off the floating window.
  type ConfirmReq = {
    title: string;
    detail?: string;
    confirmLabel: string;
    danger: boolean;
    onConfirm: () => void;
  };
  const [confirmToast, setConfirmToast] = createSignal<ConfirmReq | null>(null);
  const askConfirm = (o: ConfirmReq) => setConfirmToast(o);
  // Phase 16/29: toolbar toggle for hiding the local column when the
  // user only cares about remote. Phase 29: default flipped to FALSE
  // — Yossi's workflow is remote-first on SSH workspaces, the local
  // column was just visual noise most of the time. Local-only
  // workspaces are unaffected: the column render is guarded by
  // `!p.hasSsh || showLocal()`, so `!hasSsh` still forces it shown.
  const [showLocal, setShowLocal] = createSignal(false);

  // Phase 29 (B): per-pane sort control. Directories stay grouped
  // first regardless of field; within each group, sort by the chosen
  // field/direction. Name sort is case-insensitive.
  const [sortMode, setSortMode] = createSignal<"name" | "modified">("name");
  const [sortDir, setSortDir] = createSignal<"asc" | "desc">("asc");

  // Phase 29 (C): substring name filter, case-insensitive, applies
  // to BOTH columns. Composes with sort: filter first, then sort.
  const [filterText, setFilterText] = createSignal("");

  // Phase 17.B: built-in editor modal state. When the user clicks
  // Edit on a file row we open the modal targeting that side / path.
  const [editorOpen, setEditorOpen] = createSignal(false);
  const [editorTarget, setEditorTarget] = createSignal<{
    side: Side;
    path: string;
    filename: string;
  } | null>(null);

  // Phase 23: popup context menu — replaces the old window.prompt
  // hack. Stores screen-coordinate position so we can position-fix
  // the menu div over the WebView. `null` ⇒ hidden.
  const [ctxMenu, setCtxMenu] = createSignal<{
    side: Side;
    entry: FileEntry;
    x: number;
    y: number;
  } | null>(null);

  // Phase 23: which column the OS is currently dragging files over,
  // for visual highlight. Null ⇒ no drag in progress.
  const [dragOverSide, setDragOverSide] = createSignal<Side | null>(null);

  // Phase 23: refs to each column DOM node so we can hit-test Tauri's
  // drag-drop event coordinates against their bounding boxes. Tauri
  // gives us window-space physical pixels for `position` — we divide
  // by devicePixelRatio to compare against DOM rects (which are in
  // CSS pixels).
  let localColRef: HTMLDivElement | undefined;
  let remoteColRef: HTMLDivElement | undefined;
  // Phase 23: hidden <input type="file"> — the "Upload from disk"
  // button click()s this to pop the OS file picker. We stash which
  // side initiated the pick so the change handler knows where to put
  // the resulting bytes.
  const openEditor = (side: Side, name: string) => {
    const path = side === "local" ? fullLocal(name) : fullRemote(name);
    setEditorTarget({ side, path, filename: name });
    setEditorOpen(true);
  };

  // Phase 17.B: convenience accessors for the toolbar's "Selected"
  // group. Returns the currently-selected entry on whichever side
  // last received a click, or null when nothing is selected.
  const [focusedSide, setFocusedSide] = createSignal<Side>("local");
  const selectedEntry = createMemo<{ side: Side; entry: FileEntry } | null>(() => {
    const lname = localSel();
    const rname = remoteSel();
    if (focusedSide() === "remote" && rname) {
      const ent = remoteEntries().find((e) => e.name === rname);
      if (ent) return { side: "remote", entry: ent };
    }
    if (lname) {
      const ent = localEntries().find((e) => e.name === lname);
      if (ent) return { side: "local", entry: ent };
    }
    if (rname) {
      const ent = remoteEntries().find((e) => e.name === rname);
      if (ent) return { side: "remote", entry: ent };
    }
    return null;
  });

  // Phase 29 (B+C): derived view of each column's entries after
  // applying filter (substring on name, case-insensitive) and sort
  // (directories first, then by name|modified asc|desc). Used by the
  // <For> renderers below. Pure derivation — the raw signals
  // localEntries / remoteEntries are still the source of truth
  // (refresh writes raw lists into them).
  const cmpEntries = (a: FileEntry, b: FileEntry): number => {
    // Directories always come before files, regardless of sort field.
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
    let cmp: number;
    if (sortMode() === "modified") {
      cmp = (a.modified ?? 0) - (b.modified ?? 0);
      // Tiebreak on name (case-insensitive) so identical timestamps
      // produce a stable order.
      if (cmp === 0) {
        cmp = a.name.toLowerCase().localeCompare(b.name.toLowerCase());
      }
    } else {
      cmp = a.name.toLowerCase().localeCompare(b.name.toLowerCase());
    }
    return sortDir() === "asc" ? cmp : -cmp;
  };
  const applyFilterSort = (entries: FileEntry[]): FileEntry[] => {
    const q = filterText().trim().toLowerCase();
    const filtered = q.length === 0
      ? entries
      : entries.filter((e) => e.name.toLowerCase().includes(q));
    return [...filtered].sort(cmpEntries);
  };
  const localEntriesView = createMemo(() => applyFilterSort(localEntries()));
  const remoteEntriesView = createMemo(() => applyFilterSort(remoteEntries()));

  const refreshLocal = async () => {
    try {
      const list = await invoke<FileEntry[]>("file_list_local", {
        path: localPath(),
        showHidden: showHidden(),
      });
      setLocalEntries(list);
    } catch (e) {
      setErr(`local list: ${String(e)}`);
    }
  };
  // Probe variants of the two refreshers: they return the listing instead of
  // painting it, and swallow the error instead of showing a banner. Used only
  // by the restore path on mount, to answer "does this remembered directory
  // still exist?" without flashing a red error for a stale bookmark.
  const probeLocal = async (path: string): Promise<FileEntry[] | null> => {
    try {
      return await invoke<FileEntry[]>("file_list_local", {
        path,
        showHidden: showHidden(),
      });
    } catch {
      return null;
    }
  };
  const probeRemote = async (path: string): Promise<FileEntry[] | null> => {
    try {
      return await invoke<FileEntry[]>("file_list_remote", {
        workspaceId: p.workspaceId,
        path,
        showHidden: showHidden(),
      });
    } catch {
      return null;
    }
  };

  const refreshRemote = async () => {
    if (!p.hasSsh) return;
    try {
      const list = await invoke<FileEntry[]>("file_list_remote", {
        workspaceId: p.workspaceId,
        path: remotePath(),
        showHidden: showHidden(),
      });
      setRemoteEntries(list);
    } catch (e) {
      // Most common case: no active SSH session yet. Surface and try
      // again next refresh tick.
      setErr(`remote: ${String(e)}`);
      setRemoteEntries([]);
    }
  };

  // Phase 80.1: re-open where the user left off (per workspace, see fmPaths.ts). Writing
  // on every path change rather than on unmount: a pane can vanish with the
  // window (app close, crash) without ever running cleanup, and the last
  // directory the user actually looked at is exactly what we want to keep.
  //
  // Gated on `pathsReady`: onMount resolves the two columns at very different
  // speeds (local is a syscall, remote is an SSH round-trip), and an effect
  // that fires in that gap would persist a half-resolved pair. saveFmPaths
  // merges rather than replaces for the same reason — belt and braces, since
  // this is state the user can only lose silently.
  const [pathsReady, setPathsReady] = createSignal(false);
  createEffect(() => {
    if (!p.rememberPath) return; // opt-in; nothing is written when it's off
    if (!pathsReady()) return;
    const local = localPath();
    const remote = remotePath();
    if (!local && !remote) return;
    saveFmPaths(p.workspaceId, { local, remote });
  });

  // Phase 98: everything that must be torn down is registered HERE, in the
  // component's own scope. An `onCleanup` called after an `await` inside the
  // async onMount below has lost Solid's owner and never runs — so every
  // remount used to leak a drag-drop listener and two document listeners.
  let disposed = false;
  let unlistenDragDrop: (() => void) | undefined;
  onCleanup(() => {
    disposed = true;
    try {
      unlistenDragDrop?.();
    } catch {}
  });

  onMount(() => {
    // Phase 23: dismiss popup context menu on any outside click /
    // scroll / Escape. Capture phase so we beat the row's click
    // handler when the user clicks elsewhere.
    const onDocClick = (e: MouseEvent) => {
      // If they clicked inside one of our menus, that menu's item
      // handler closes it after firing the action; otherwise close
      // immediately. We check all three popup classes in one pass.
      const target = e.target as HTMLElement;
      if (!target?.closest?.(".fm-ctx-menu")) closeCtxMenu();
      if (!target?.closest?.(".fm-bg-menu")) closeBgCtxMenu();
      if (!target?.closest?.(".fm-add-menu") && !target?.closest?.(".fm-add-btn")) closeAddMenu();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        closeCtxMenu();
        closeBgCtxMenu();
        closeAddMenu();
      }
    };
    document.addEventListener("mousedown", onDocClick, true);
    document.addEventListener("keydown", onKey);
    onCleanup(() => {
      document.removeEventListener("mousedown", onDocClick, true);
      document.removeEventListener("keydown", onKey);
    });
  });

  onMount(async () => {
    // `{}` when the setting is off → every branch below falls through to the
    // $HOME path, i.e. exactly the pre-80.1 behavior.
    const saved = p.rememberPath ? loadFmPaths(p.workspaceId) : {};

    // Local: only honor the remembered directory if it still lists — a folder
    // that was deleted (or a disconnected drive) must not strand the pane on
    // an error every time it opens.
    const savedLocal = saved.local ? await probeLocal(saved.local) : null;
    if (saved.local && savedLocal) {
      setLocalPath(saved.local);
      setLocalEntries(savedLocal);
    } else {
      try {
        const home = await invoke<string>("file_home_local");
        setLocalPath(home);
      } catch (e) {
        setLocalPath(isWindows() ? "C:\\" : "/");
      }
      await refreshLocal();
    }

    if (p.hasSsh) {
      // Remote: a failed listing is ambiguous — usually it just means no live
      // SSH session yet, not a missing directory. So we ask for $HOME as the
      // tie-breaker: if that answers, the session is up and the saved path is
      // genuinely the problem, so we move. If it doesn't, we stay on the saved
      // path and let refreshRemote surface the usual "connect first" banner —
      // the user's directory is then already in place for the next refresh.
      const savedRemote = saved.remote ? await probeRemote(saved.remote) : null;
      if (saved.remote && savedRemote) {
        setRemotePath(saved.remote);
        setRemoteEntries(savedRemote);
      } else {
        let home: string | null = null;
        try {
          home = await invoke<string>("file_home_remote", {
            workspaceId: p.workspaceId,
          });
        } catch {
          home = null;
        }
        setRemotePath(home || saved.remote || "/");
        await refreshRemote();
      }
    }
    // Both columns have settled — from here on, every navigation is the user's
    // and worth remembering.
    setPathsReady(true);

    // Phase 23: register OS drag-drop. Tauri 2 emits 'enter' / 'over'
    // / 'drop' / 'leave' phases. We use 'over' to drive the
    // dragOverSide highlight, 'drop' to actually do the upload, and
    // 'leave' to clear the highlight. The webview emits ALL events for
    // the whole window — we hit-test against our column refs to
    // ignore drops outside the file-manager pane.
    //
    // Coordinate space is platform-dependent even though Tauri types it
    // as PhysicalPosition on both. Windows (WebView2) reports physical
    // pixels — ScreenToClient on a DPI-aware HWND — so they must be
    // divided by devicePixelRatio to compare against CSS-pixel DOM rects.
    // macOS (wry's wkwebview backend) passes NSDraggingInfo's
    // draggingLocation straight through, y-flipped and NOT scaled, i.e.
    // logical points already. Dividing there halved every coordinate on a
    // Retina display, so the hit-test missed both columns and the drop was
    // swallowed with no upload and no error.
    try {
      const unlisten = await getCurrentWebview().onDragDropEvent((event) => {
        const payload = event.payload as
          | { type: "enter" | "over"; position: { x: number; y: number } }
          | { type: "drop"; paths: string[]; position: { x: number; y: number } }
          | { type: "leave" };
        if (payload.type === "leave") {
          setDragOverSide(null);
          return;
        }
        const scale = isMac() ? 1 : window.devicePixelRatio || 1;
        const x = payload.position.x / scale;
        const y = payload.position.y / scale;
        const hitLocal = localColRef && pointInRect(x, y, localColRef.getBoundingClientRect());
        const hitRemote = remoteColRef && pointInRect(x, y, remoteColRef.getBoundingClientRect());
        const side: Side | null = hitRemote ? "remote" : hitLocal ? "local" : null;
        if (payload.type === "enter" || payload.type === "over") {
          setDragOverSide(side);
          return;
        }
        // drop
        setDragOverSide(null);
        if (payload.type !== "drop") return;
        const dropPaths = payload.paths;
        if (side === "remote" && dropPaths.length > 0) {
          void dropUploadToRemote(dropPaths);
        } else if (side === "local" && dropPaths.length > 0) {
          // Local → local: copy each dropped file into the displayed local
          // dir. Unshipped-fivefer (#5): uses the backend file_copy_local
          // (std::fs::copy) so binary files and any size work — no more
          // read-as-text limitation. Skip a copy onto itself.
          (async () => {
            for (const host of dropPaths) {
              const basename = host.split(/[\\/]/).filter(Boolean).pop() || "dropped";
              const dest = fullLocal(basename);
              if (dest.toLowerCase() === host.toLowerCase()) continue;
              await wrap(`copy ${basename}`, async () => {
                await invoke("file_copy_local", { src: host, dest });
              });
            }
            await refreshLocal();
          })();
        }
      });
      // The pane may have unmounted while the listener was being set up.
      if (disposed) unlisten();
      else unlistenDragDrop = unlisten;
    } catch (e) {
      // Drag-drop hookup failure is non-fatal — file manager still
      // works without it.
      log.warn("onDragDropEvent failed", e);
    }
  });

  // Hit-test helper: is the point (x,y) inside a DOMRect? Used by the
  // OS drag-drop logic to figure out which column the user is
  // dragging over.
  const pointInRect = (x: number, y: number, r: DOMRect) =>
    x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;

  const fmtSize = (n: number) => {
    if (n < 1024) return `${n}B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)}K`;
    if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)}M`;
    return `${(n / 1024 / 1024 / 1024).toFixed(1)}G`;
  };
  const fmtTime = (ts: number) => {
    if (!ts) return "—";
    const d = new Date(ts * 1000);
    const now = Date.now();
    const sec = (now - d.getTime()) / 1000;
    if (sec < 86400) return d.toLocaleTimeString();
    return d.toLocaleDateString();
  };

  const parentOf = (path: string, sep: string): string => {
    const cleaned = path.replace(/[\\/]+$/, "");
    const idx = Math.max(cleaned.lastIndexOf("/"), cleaned.lastIndexOf("\\"));
    if (idx <= 0) return cleaned.length > 1 ? cleaned[0] + sep : cleaned;
    return cleaned.slice(0, idx) || sep;
  };

  const navIntoLocal = (e: FileEntry) => {
    if (!e.is_dir) return;
    const cur = localPath().replace(/[\\/]+$/, "");
    setLocalPath(`${cur}${sep()}${e.name}`);
    void refreshLocal();
  };
  const navIntoRemote = (e: FileEntry) => {
    if (!e.is_dir) return;
    const cur = remotePath().replace(/\/+$/, "");
    setRemotePath(cur === "" ? `/${e.name}` : `${cur}/${e.name}`);
    void refreshRemote();
  };
  const goUp = (side: Side) => {
    if (side === "local") {
      setLocalPath(parentOf(localPath(), sep()));
      void refreshLocal();
    } else {
      setRemotePath(parentOf(remotePath(), "/") || "/");
      void refreshRemote();
    }
  };

  // Every local-side file op routes through here — upload, rename, mkdir,
  // create, zip/unzip, copy-paste, copy-path, and the local drop target. The
  // separator MUST be the host's: joining with a literal `\` on macOS built
  // `/Users/yossi/Downloads\file.txt`, which std::fs::read then failed on —
  // that was the whole "upload is broken on mac" bug.
  const fullLocal = (name: string): string => {
    const cur = localPath().replace(/[\\/]+$/, "");
    return `${cur}${sep()}${name}`;
  };
  const fullRemote = (name: string): string => {
    const cur = remotePath().replace(/\/+$/, "");
    return cur === "" ? `/${name}` : `${cur}/${name}`;
  };

  // Phase 17: "Open" handlers. For directories we keep the
  // existing navigation behavior (cd into); for files we ask the OS
  // to open with the default app. Remote files are downloaded to a
  // stable temp path first; the backend returns the temp location
  // which we surface in the status line so the user knows where the
  // copy lives.
  const openLocal = async (e: FileEntry) => {
    if (e.is_dir) {
      navIntoLocal(e);
      return;
    }
    const path = fullLocal(e.name);
    // Phase GG: render .md in the in-app viewer instead of the OS app.
    if (isMarkdownFile(e.name)) {
      await wrap(`open ${e.name}`, async () => {
        const fc = await invoke<{ text: string }>("file_read_local", { path });
        openMarkdown(e.name, fc.text);
      });
      return;
    }
    await wrap(`open ${e.name}`, async () => {
      await invoke("file_open_local", { path });
      setStatus(t("fm.toast.opened_local", { file: e.name }));
    });
  };
  const openRemote = async (e: FileEntry) => {
    if (e.is_dir) {
      navIntoRemote(e);
      return;
    }
    const path = fullRemote(e.name);
    // Phase GG: render .md in the in-app viewer (SFTP-fetch the text)
    // instead of downloading to a temp file + opening the OS app.
    if (isMarkdownFile(e.name)) {
      await wrap(`open ${e.name}`, async () => {
        const fc = await invoke<{ text: string }>("file_read_remote", {
          workspaceId: p.workspaceId,
          path,
        });
        openMarkdown(e.name, fc.text);
      });
      return;
    }
    await wrap(`open ${e.name}`, async () => {
      const tempPath = await invoke<string>("file_open_remote", {
        workspaceId: p.workspaceId,
        remotePath: path,
      });
      setStatus(t("fm.toast.opened_remote", { file: e.name, temp: tempPath }));
    });
  };

  const wrap = async <T,>(label: string, fn: () => Promise<T>): Promise<T | null> => {
    setBusy(true);
    setStatus(label);
    setErr(null);
    try {
      const r = await fn();
      setStatus(`${label} ✓`);
      return r;
    } catch (e) {
      setErr(`${label}: ${String(e)}`);
      setStatus("");
      return null;
    } finally {
      setBusy(false);
    }
  };

  const uploadSel = async () => {
    const name = localSel();
    if (!name) return;
    const local = fullLocal(name);
    const remote = fullRemote(name);
    const n = await wrap(`upload ${name}`, () =>
      invoke<number>("file_upload", {
        workspaceId: p.workspaceId,
        localPath: local,
        remotePath: remote,
      })
    );
    if (n != null) {
      setStatus(`uploaded ${name} (${fmtSize(n)}) ✓`);
      await refreshRemote();
    }
  };
  const downloadSel = async () => {
    const name = remoteSel();
    if (!name) return;
    const remote = fullRemote(name);
    // Phase 65 (bug K): always ask where to save (native dialog),
    // pre-filling the local column's current folder. Was silently
    // dropping into the local column path with no prompt.
    setBusy(true);
    setStatus(`download ${name}`);
    setErr(null);
    try {
      const dest = await saveRemoteFileAs(p.workspaceId, remote, name, localPath());
      if (dest) {
        setStatus(`↧ ${name} ✓`);
        await refreshLocal();
      } else {
        setStatus("");
      }
    } catch (e) {
      setErr(`download ${name}: ${String(e)}`);
      setStatus("");
    } finally {
      setBusy(false);
    }
  };
  // Phase 57: zip + unzip the currently selected item. Single-item v1
  // (multi-selection support would need an array selection model);
  // matches the right-click → "compress" affordance in OS file
  // managers. The output zip name is derived from the basename so
  // identical entries can sit side-by-side post-zip without colliding
  // unless the user re-runs the action.
  // Phase 62 (item 6): zip now confirms first (toast). The actual work
  // is in performZip; zipSel just gates it behind the confirm toast.
  const performZip = async (s: { side: Side; entry: FileEntry }) => {
    const name = s.entry.name;
    const outputName = `${name}.zip`;
    if (s.side === "local") {
      const out = await wrap(`zip ${name}`, () =>
        invoke<string>("file_manager_zip_local", {
          cwd: localPath(),
          paths: [name],
          outputName,
        })
      );
      if (out != null) {
        setStatus(t("fm.zip.done", { out: outputName }));
        await refreshLocal();
      }
    } else {
      // Phase 65 (bug 2.5): don't use wrap() here — we need to inspect
      // the error so a missing `zip` on the server becomes a tar offer
      // (with an install hint) instead of a raw top-bar error.
      setBusy(true);
      setStatus(`zip ${name}`);
      setErr(null);
      try {
        await invoke<string>("file_manager_zip_remote", {
          workspaceId: p.workspaceId,
          cwd: remotePath(),
          paths: [name],
          outputName,
        });
        setStatus(t("fm.zip.done", { out: outputName }));
        await refreshRemote();
      } catch (e) {
        const msg = String(e);
        if (isZipMissing(msg)) {
          // `zip` isn't installed on the server — offer the tar.gz
          // fallback + an install hint (the toast detail).
          setStatus("");
          askConfirm({
            title: t("fm.zip.notInstalled.title"),
            detail: t("fm.zip.notInstalled.detail"),
            confirmLabel: t("fm.zip.useTar"),
            danger: false,
            onConfirm: () => void performTarGzRemote(s),
          });
        } else {
          setErr(`zip ${name}: ${msg}`);
          setStatus("");
        }
      } finally {
        setBusy(false);
      }
    }
  };
  // Phase 65 (bug 2.5): true when a remote zip failed because `zip` is
  // not installed (exit 127 / "command not found"), as opposed to a real
  // packing error. Drives the tar.gz fallback offer.
  const isZipMissing = (msg: string): boolean => {
    const low = msg.toLowerCase();
    return (
      low.includes("exit 127") ||
      low.includes("command not found") ||
      low.includes("zip: not found")
    );
  };
  // Phase 65 (bug 2.5): tar.gz fallback for servers without `zip`.
  const performTarGzRemote = async (s: { side: Side; entry: FileEntry }) => {
    const name = s.entry.name;
    const outputName = `${name}.tar.gz`;
    const out = await wrap(`tar ${name}`, () =>
      invoke<string>("file_manager_targz_remote", {
        workspaceId: p.workspaceId,
        cwd: remotePath(),
        paths: [name],
        outputName,
      })
    );
    if (out != null) {
      setStatus(t("fm.zip.done", { out: outputName }));
      await refreshRemote();
    }
  };
  const zipSel = () => {
    const s = selectedEntry();
    if (!s) return;
    askConfirm({
      title: t("fm.confirm.zip.title", {
        name: s.entry.name,
        out: `${s.entry.name}.zip`,
      }),
      confirmLabel: t("fm.zip.button"),
      danger: false,
      onConfirm: () => void performZip(s),
    });
  };

  const performUnzip = async (s: { side: Side; entry: FileEntry }) => {
    const name = s.entry.name;
    if (s.side === "local") {
      const out = await wrap(`unzip ${name}`, () =>
        invoke<string>("file_manager_unzip_local", {
          zipPath: fullLocal(name),
        })
      );
      if (out != null) {
        setStatus(t("fm.unzip.done", { dest: out }));
        await refreshLocal();
      }
    } else {
      const out = await wrap(`unzip ${name}`, () =>
        invoke<string>("file_manager_unzip_remote", {
          workspaceId: p.workspaceId,
          zipPath: fullRemote(name),
        })
      );
      if (out != null) {
        setStatus(t("fm.unzip.done", { dest: out }));
        await refreshRemote();
      }
    }
  };
  const unzipSel = async () => {
    const s = selectedEntry();
    if (!s) return;
    const name = s.entry.name;
    if (!name.toLowerCase().endsWith(".zip")) {
      setErr(t("fm.unzip.error.notZip"));
      return;
    }
    // Phase 60 (smoke-test 3b) → 62: pre-flight — when the destination
    // folder already exists (locally: exists AND non-empty), confirm
    // before the extraction overwrites files in it. The confirm is now
    // the in-pane toast instead of window.confirm. (Per-file
    // Skip/Rename is a dialog component — deferred until needed.)
    const destName = name.replace(/\.zip$/i, "");
    let conflict = false;
    try {
      conflict =
        s.side === "local"
          ? await invoke<boolean>("file_manager_unzip_local_check", {
              zipPath: fullLocal(name),
            })
          : await invoke<boolean>("file_manager_unzip_remote_check", {
              workspaceId: p.workspaceId,
              zipPath: fullRemote(name),
            });
    } catch (e) {
      // Check failure shouldn't block the user — log + proceed (the
      // unzip itself surfaces real errors through wrap()).
      log.warn("unzip pre-flight check failed", e);
    }
    if (conflict) {
      askConfirm({
        title: t("fm.unzip.confirmOverwrite", { dest: destName }),
        confirmLabel: t("fm.unzip.button"),
        danger: true,
        onConfirm: () => void performUnzip(s),
      });
    } else {
      void performUnzip(s);
    }
  };

  // Phase 62 (item 6): delete confirms via the in-pane toast.
  const performDelete = async (side: Side, name: string) => {
    if (side === "local") {
      const path = fullLocal(name);
      await wrap(`delete ${name}`, () => invoke("file_delete_local", { path }));
      await refreshLocal();
    } else {
      const path = fullRemote(name);
      await wrap(`delete ${name}`, () =>
        invoke("file_delete_remote", { workspaceId: p.workspaceId, path })
      );
      await refreshRemote();
    }
  };
  const deleteSel = (side: Side) => {
    const name = side === "local" ? localSel() : remoteSel();
    if (!name) return;
    askConfirm({
      title: t("fm.confirm.delete.title", { name }),
      detail: t("fm.confirm.delete.detail"),
      confirmLabel: t("common.delete"),
      danger: true,
      onConfirm: () => void performDelete(side, name),
    });
  };
  const renameSel = async (side: Side) => {
    const name = side === "local" ? localSel() : remoteSel();
    if (!name) return;
    const next = window.prompt(t("fm.action.rename_prompt", { name }), name);
    if (!next || next === name) return;
    if (side === "local") {
      await wrap(`rename ${name}`, () =>
        invoke("file_rename_local", {
          oldPath: fullLocal(name),
          newPath: fullLocal(next),
        })
      );
      await refreshLocal();
    } else {
      await wrap(`rename ${name}`, () =>
        invoke("file_rename_remote", {
          workspaceId: p.workspaceId,
          oldPath: fullRemote(name),
          newPath: fullRemote(next),
        })
      );
      await refreshRemote();
    }
  };
  const mkdirIn = async (side: Side) => {
    const name = window.prompt(t("fm.action.mkdir_prompt"));
    if (!name) return;
    if (side === "local") {
      await wrap(`mkdir ${name}`, () =>
        invoke("file_mkdir_local", { path: fullLocal(name) })
      );
      await refreshLocal();
    } else {
      await wrap(`mkdir ${name}`, () =>
        invoke("file_mkdir_remote", {
          workspaceId: p.workspaceId,
          path: fullRemote(name),
        })
      );
      await refreshRemote();
    }
  };

  // Phase 23: create an empty file in the given side's current directory.
  // Distinct from mkdir, distinct from Edit (which opens a possibly-large
  // file). Backend refuses to clobber existing paths.
  const createFileIn = async (side: Side) => {
    const name = window.prompt(t("fm.action.create_file_prompt"));
    if (!name) return;
    if (side === "local") {
      await wrap(`create ${name}`, () =>
        invoke("file_create_local", { path: fullLocal(name) })
      );
      await refreshLocal();
    } else {
      await wrap(`create ${name}`, () =>
        invoke("file_create_remote", {
          workspaceId: p.workspaceId,
          path: fullRemote(name),
        })
      );
      await refreshRemote();
    }
  };

  // Feedback (cut/copy/paste): put a file on the internal clipboard.
  const clipSet = (side: Side, name: string, op: "cut" | "copy") => {
    const path = side === "local" ? fullLocal(name) : fullRemote(name);
    setClip({ op, side, path, name });
    setStatus(`${op === "cut" ? "cut" : "copied"} ${name}`);
  };

  // Paste the clipboard entry into `targetSide`'s current directory. Resolves
  // the source/target combo to the right backend op; cut clears the source.
  const pasteInto = async (targetSide: Side) => {
    const c = clip();
    if (!c) return;
    const dest = targetSide === "local" ? fullLocal(c.name) : fullRemote(c.name);
    if (dest === c.path) {
      setErr(t("fm.paste.same_location"));
      return;
    }
    if (c.side === "remote" && targetSide === "local") {
      setErr(t("fm.paste.use_download"));
      return;
    }
    const ok = await wrap(`${c.op === "cut" ? "move" : "copy"} ${c.name}`, async () => {
      if (c.side === "local" && targetSide === "local") {
        await invoke("file_copy_local", { src: c.path, dest });
        if (c.op === "cut") await invoke("file_delete_local", { path: c.path });
      } else if (c.side === "remote" && targetSide === "remote") {
        if (c.op === "cut") {
          await invoke("file_rename_remote", {
            workspaceId: p.workspaceId,
            oldPath: c.path,
            newPath: dest,
          });
        } else {
          await invoke("file_copy_remote", { workspaceId: p.workspaceId, src: c.path, dest });
        }
      } else {
        // local → remote: upload, then delete local on cut.
        await invoke<number>("file_upload", {
          workspaceId: p.workspaceId,
          localPath: c.path,
          remotePath: dest,
        });
        if (c.op === "cut") await invoke("file_delete_local", { path: c.path });
      }
      return true;
    });
    if (ok) {
      if (c.op === "cut") setClip(null);
      await refreshLocal();
      await refreshRemote();
    }
  };

  // Phase 23: copy the absolute path of a file/dir to the system
  // clipboard. Pure-frontend operation — uses navigator.clipboard
  // since Tauri 2 exposes it in WebView2 with the same async API as
  // browsers. Falls back to writing into a temp <textarea> + execCommand
  // for older WebView2 builds that don't grant clipboard-write.
  const copyPathOf = async (side: Side, name: string) => {
    // Empty name ⇒ copy the column's current directory path, not a child.
    const path = name
      ? side === "local"
        ? fullLocal(name)
        : fullRemote(name)
      : side === "local"
      ? localPath()
      : remotePath();
    try {
      await navigator.clipboard.writeText(path);
      setStatus(t("fm.toast.path_copied", { path }));
    } catch {
      const ta = document.createElement("textarea");
      ta.value = path;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      try {
        document.execCommand("copy");
        setStatus(t("fm.toast.path_copied", { path }));
      } catch (e) {
        setErr(`copy: ${String(e)}`);
      } finally {
        document.body.removeChild(ta);
      }
    }
  };

  // Phase 23: pick files from anywhere on disk via OS picker and upload
  // them to the remote (or copy them into the local column if
  // side === "local").
  //
  // Phase 81.E: this used the HTML5 <input type="file">, which only ever
  // hands back a Blob — no real path. The bytes then crossed the Tauri
  // IPC bridge as `Array.from(new Uint8Array(buf))`, i.e. a JSON array of
  // one number per byte. A 60 MB file became a ~250 MB JSON string and
  // wedged the webview outright. The native dialog returns actual
  // filesystem paths instead, so the backend reads the file itself and
  // zero bytes cross the bridge:
  //   remote → file_upload (streams from disk, reports progress)
  //   local  → file_copy_local (also fixes binaries, which the old
  //            f.text() path silently corrupted through UTF-8)
  const pickAndUpload = async (side: Side) => {
    if (side === "remote" && !p.hasSsh) {
      setErr("no remote — cannot upload");
      return;
    }
    const picked = await open({ multiple: true, directory: false });
    if (!picked) return; // cancelled
    const paths = Array.isArray(picked) ? picked : [picked];
    for (const src of paths) {
      const name = src.split(/[\\/]/).filter(Boolean).pop() || "file";
      if (side === "remote") {
        await wrap(`upload ${name}`, () =>
          invoke<number>("file_upload", {
            workspaceId: p.workspaceId,
            localPath: src,
            remotePath: fullRemote(name),
          })
        );
      } else {
        await wrap(`copy ${name}`, () =>
          invoke("file_copy_local", { src, dest: fullLocal(name) })
        );
      }
    }
    if (side === "remote") await refreshRemote();
    else await refreshLocal();
  };

  // Phase 23: upload raw bytes from a Tauri OS drag-drop. Given the
  // host path, slurp via file_upload (which already reads from disk).
  const dropUploadToRemote = async (hostPaths: string[]) => {
    if (!p.hasSsh) {
      setErr("no remote — cannot upload");
      return;
    }
    for (const host of hostPaths) {
      const basename = host.split(/[\\/]/).filter(Boolean).pop() || "dropped";
      const remote = fullRemote(basename);
      await wrap(`upload ${basename}`, () =>
        invoke<number>("file_upload", {
          workspaceId: p.workspaceId,
          localPath: host,
          remotePath: remote,
        })
      );
    }
    await refreshRemote();
  };

  // Phase 23: context-menu helpers.
  const openCtxMenu = (side: Side, entry: FileEntry, ev: MouseEvent) => {
    ev.preventDefault();
    setCtxMenu({ side, entry, x: ev.clientX, y: ev.clientY });
    if (side === "local") {
      setLocalSel(entry.name);
      setFocusedSide("local");
    } else {
      setRemoteSel(entry.name);
      setFocusedSide("remote");
    }
  };
  const closeCtxMenu = () => setCtxMenu(null);

  // Phase 23.B: "Add" dropdown next to the path bar — single ＋ button
  // that opens a small popup with New folder / New file / Upload from
  // disk. Replaces the prior three separate buttons in the column
  // header for a less crowded toolbar.
  const [addMenu, setAddMenu] = createSignal<{ side: Side; x: number; y: number } | null>(null);
  const closeAddMenu = () => setAddMenu(null);
  // Phase 23.B: background context menu for clicks on the empty area
  // of a list (between or below rows). Different from the per-row
  // context menu — offers create / upload actions for the directory
  // as a whole.
  const [bgCtxMenu, setBgCtxMenu] = createSignal<{ side: Side; x: number; y: number } | null>(null);
  const openBgCtxMenu = (side: Side, ev: MouseEvent) => {
    // Only fire when the click is on the list itself, not a row.
    const t = ev.target as HTMLElement;
    if (t?.closest?.(".fm-row")) return;
    ev.preventDefault();
    setBgCtxMenu({ side, x: ev.clientX, y: ev.clientY });
    setFocusedSide(side);
  };
  const closeBgCtxMenu = () => setBgCtxMenu(null);

  const ColumnHeader = (props: { side: Side; path: () => string; setPath: (v: string) => void; refresh: () => void }) => {
    // Anchor the dropdown at the bottom-left of the + button so it
    // doesn't drift if the path input width changes between renders.
    const openAdd = (ev: MouseEvent) => {
      const r = (ev.currentTarget as HTMLElement).getBoundingClientRect();
      setAddMenu({ side: props.side, x: r.left, y: r.bottom + 4 });
    };
    return (
      <div class="fm-col-head">
        <button class="fm-up" title={t("fm.btn.up")} onClick={() => goUp(props.side)}><IconArrowUp size={14} /></button>
        <input
          class="fm-path"
          value={props.path()}
          onChange={(e) => {
            props.setPath(e.currentTarget.value);
            props.refresh();
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              props.setPath((e.target as HTMLInputElement).value);
              props.refresh();
            }
          }}
          spellcheck={false}
        />
        <button class="fm-tool" title={t("fm.btn.refresh")} onClick={props.refresh}><IconRefresh size={14} /></button>
        <button
          class="fm-tool fm-add-btn"
          title={t("fm.btn.add_menu")}
          onClick={openAdd}
        >
          <IconPlus size={14} />
        </button>
      </div>
    );
  };

  const transferDir = createMemo(() => (p.hasSsh ? "Upload ↦ / Download ↤" : ""));
  void transferDir; // currently rendered inline in toolbar

  return (
    <div class="fm-pane">
      {/* Feedback: persistent clipboard bar so Paste is one click from
          anywhere — no need to hunt for a context menu after copy/cut. */}
      <Show when={clip()}>
        <div class="fm-clip-bar">
          <span class="fm-clip-label">
            {clip()!.op === "cut" ? <IconScissors size={13} /> : <IconClipboard size={13} />} {clip()!.name}
            <span class="fm-clip-src">({clip()!.side})</span>
          </span>
          <span class="fm-clip-actions">
            <button class="fm-clip-btn" onClick={() => void pasteInto("local")}>
              {t("fm.paste.to_local")}
            </button>
            <Show when={p.hasSsh}>
              <button class="fm-clip-btn" onClick={() => void pasteInto("remote")}>
                {t("fm.paste.to_remote")}
              </button>
            </Show>
            <button class="fm-clip-btn fm-clip-x" title={t("fm.paste.clear")} onClick={() => setClip(null)}>
              <IconClose size={13} />
            </button>
          </span>
        </div>
      </Show>
      <div class="fm-toolbar">
        <label class="fm-checkbox">
          <input
            type="checkbox"
            checked={showHidden()}
            onChange={(e) => {
              setShowHidden(e.currentTarget.checked);
              void refreshLocal();
              void refreshRemote();
            }}
          />
          <span>{t("fm.checkbox.hidden")}</span>
        </label>
        <Show when={p.hasSsh}>
          <label class="fm-checkbox">
            <input
              type="checkbox"
              checked={showLocal()}
              onChange={(e) => setShowLocal(e.currentTarget.checked)}
            />
            <span>{t("fm.checkbox.show_local")}</span>
          </label>
        </Show>
        {/* Phase 29 (B): sort controls — name vs modified, asc vs
             desc. Directories always sort first regardless. */}
        <label class="fm-checkbox" title={t("fm.sort.label")}>
          <span>{t("fm.sort.label")}:</span>
          <select
            class="fm-sort-select"
            value={sortMode()}
            onChange={(e) =>
              setSortMode(e.currentTarget.value as "name" | "modified")
            }
          >
            <option value="name">{t("fm.sort.name")}</option>
            <option value="modified">{t("fm.sort.modified")}</option>
          </select>
        </label>
        <button
          class="fm-tool"
          title={sortDir() === "asc" ? t("fm.sort.asc") : t("fm.sort.desc")}
          onClick={() => setSortDir(sortDir() === "asc" ? "desc" : "asc")}
        >
          {sortDir() === "asc" ? <IconChevronUp size={14} /> : <IconChevronDown size={14} />}
        </button>
        {/* Phase 29 (C): name-substring filter, applies to both
             columns, composes with sort. */}
        <div class="fm-filter-wrap">
          <input
            class="fm-filter"
            type="text"
            placeholder={t("fm.filter.placeholder")}
            value={filterText()}
            onInput={(e) => setFilterText(e.currentTarget.value)}
          />
          <Show when={filterText().length > 0}>
            <button
              class="fm-filter-x"
              title={t("common.close")}
              onClick={() => setFilterText("")}
            >
              <IconClose size={13} />
            </button>
          </Show>
        </div>
        {/* Phase 29 (D): the selected-actions block is ALWAYS rendered
             (no <Show> gate) so the toolbar has a constant set of
             children. Buttons just toggle `disabled` based on whether
             anything is selected. Combined with the .fm-toolbar CSS
             (flex-wrap: nowrap, fixed min-height, overflow-x: auto)
             this gives the toolbar a provably constant height
             regardless of selection state — selecting a row no longer
             pushes the file list down and breaks the user's
             double-click target. */}
        <span class="fm-sep">|</span>
        <span
          class="fm-selected-label"
          title={selectedEntry()?.entry.name ?? ""}
        >
          {selectedEntry()?.entry.name ?? "—"}
        </span>
        <button
          class="fm-action"
          title={t("fm.action.open.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (!s) return;
            if (s.side === "local") void openLocal(s.entry);
            else void openRemote(s.entry);
          }}
        >
          {t("fm.action.open")}
        </button>
        <button
          class="fm-action"
          title={t("fm.action.edit.tooltip")}
          disabled={busy() || !selectedEntry() || !!selectedEntry()?.entry.is_dir}
          onClick={() => {
            const s = selectedEntry();
            if (!s) return;
            openEditor(s.side, s.entry.name);
          }}
        >
          {t("fm.action.edit")}
        </button>
        {/* Upload + Download are SSH-workspace-only; further gated by
             which side the selection is on. Always rendered in SSH
             workspaces (no Show on selection state) so layout stays
             constant — they grey out when not applicable. */}
        <Show when={p.hasSsh}>
          <button
            class="fm-action"
            title={t("fm.btn.upload.tooltip")}
            disabled={
              busy() ||
              !selectedEntry() ||
              selectedEntry()?.side !== "local" ||
              !!selectedEntry()?.entry.is_dir
            }
            onClick={() => void uploadSel()}
          >
            <IconUpload size={14} />
          </button>
          <button
            class="fm-action"
            title={t("fm.btn.download.tooltip")}
            disabled={
              busy() ||
              !selectedEntry() ||
              selectedEntry()?.side !== "remote" ||
              !!selectedEntry()?.entry.is_dir
            }
            onClick={() => void downloadSel()}
          >
            <IconDownload size={14} />
          </button>
        </Show>
        <button
          class="fm-action"
          title={t("fm.action.rename.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (s) void renameSel(s.side);
          }}
        >
          {t("common.rename")}
        </button>
        <button
          class="fm-action"
          title={t("fm.action.copy_path.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (s) void copyPathOf(s.side, s.entry.name);
          }}
        >
          <IconCopy size={14} />
        </button>
        {/* Phase 57: compress / extract. Zip always enabled when
            something is selected; Unzip only enabled when the
            selection name ends with .zip (case-insensitive). Output
            lands beside the source: zip → <name>.zip in the same
            dir, unzip → <name>/ in the same dir. */}
        <button
          class="fm-action"
          title={t("fm.zip.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => zipSel()}
        >
          {t("fm.zip.button")}
        </button>
        <button
          class="fm-action"
          title={t("fm.unzip.tooltip")}
          disabled={
            busy() ||
            !selectedEntry() ||
            !selectedEntry()!.entry.name.toLowerCase().endsWith(".zip")
          }
          onClick={() => void unzipSel()}
        >
          {t("fm.unzip.button")}
        </button>
        <button
          class="fm-action fm-action-danger"
          title={t("fm.action.delete.tooltip")}
          disabled={busy() || !selectedEntry()}
          onClick={() => {
            const s = selectedEntry();
            if (s) void deleteSel(s.side);
          }}
        >
          {t("common.delete")}
        </button>
        <span class="fm-status">{busy() ? "…" : status()}</span>
        <Show when={err()}>
          <span class="fm-err" title={err()!}><IconWarning size={13} /> {err()}</span>
        </Show>
      </div>
      <div class={`fm-grid ${p.hasSsh && showLocal() ? "fm-grid-dual" : "fm-grid-single"}`}>
        {/* Local column — hidden when the user untoggles "Show local"
            and we have an SSH workspace to focus on. */}
        <Show when={!p.hasSsh || showLocal()}>
          <div
            class={`fm-col ${dragOverSide() === "local" ? "drag-over" : ""}`}
            ref={(el) => (localColRef = el)}
          >
            <ColumnHeader side="local" path={localPath} setPath={setLocalPath} refresh={refreshLocal} />
            <div
              class="fm-list"
              onContextMenu={(ev) => openBgCtxMenu("local", ev)}
            >
              <For each={localEntriesView()}>
                {(e) => (
                  <div
                    class={`fm-row ${localSel() === e.name ? "selected" : ""}`}
                    onClick={() => {
                      setLocalSel(e.name);
                      setFocusedSide("local");
                    }}
                    onDblClick={() => void openLocal(e)}
                    onContextMenu={(ev) => openCtxMenu("local", e, ev)}
                  >
                    <span class="fm-icon">{e.is_dir ? <IconFolder size={14} /> : e.is_link ? <IconLink size={14} /> : <IconFile size={14} />}</span>
                    <span class="fm-name"><TechText text={e.name} /></span>
                    <span class="fm-size">{e.is_dir ? "" : fmtSize(e.size)}</span>
                    <span class="fm-time">{fmtTime(e.modified)}</span>
                  </div>
                )}
              </For>
            </div>
          </div>
        </Show>
        {/* Remote column (SSH workspaces only) */}
        <Show when={p.hasSsh}>
          <div
            class={`fm-col ${dragOverSide() === "remote" ? "drag-over" : ""}`}
            ref={(el) => (remoteColRef = el)}
          >
            <ColumnHeader side="remote" path={remotePath} setPath={setRemotePath} refresh={refreshRemote} />
            <div
              class="fm-list"
              onContextMenu={(ev) => openBgCtxMenu("remote", ev)}
            >
              <Show
                when={remoteEntries().length > 0}
                fallback={
                  <div class="fm-empty">
                    {/* Phase 16: differentiate "SSH workspace, terminal not
                         connected yet" from a true error. The backend
                         returns `no active SSH session` precisely in this
                         shape — surface a friendlier message that points
                         the user at the fix. */}
                    {!p.hasActiveSession
                      ? t("fm.empty.connect_terminal_first")
                      : err()
                      ? t("fm.empty.no_ssh")
                      : t("fm.empty.empty")}
                  </div>
                }
              >
                <For each={remoteEntriesView()}>
                  {(e) => (
                    <div
                      class={`fm-row ${remoteSel() === e.name ? "selected" : ""}`}
                      onClick={() => {
                        setRemoteSel(e.name);
                        setFocusedSide("remote");
                      }}
                      onDblClick={() => void openRemote(e)}
                      onContextMenu={(ev) => openCtxMenu("remote", e, ev)}
                    >
                      <span class="fm-icon">{e.is_dir ? <IconFolder size={14} /> : e.is_link ? <IconLink size={14} /> : <IconFile size={14} />}</span>
                      <span class="fm-name"><TechText text={e.name} /></span>
                      <span class="fm-size">{e.is_dir ? "" : fmtSize(e.size)}</span>
                      <span class="fm-time">{fmtTime(e.modified)}</span>
                    </div>
                  )}
                </For>
              </Show>
            </div>
          </div>
        </Show>
      </div>

      {/* Phase 81: live progress for uploads and downloads. Reads the
           global transferStore, so it also lights up for transfers this
           pane didn't start (terminal drag-drop, OSC 8 link downloads).
           Renders nothing when there is no transfer. */}
      <TransferBar />

      {/* Phase 23: popup context menu. Position-fixed; tracks mouse
           coordinates of the right-click. Outside-click + Escape
           handlers in onMount close it. */}
      <Show when={ctxMenu()}>
        {(() => {
          const m = ctxMenu()!;
          // Clamp menu position so it doesn't bleed off-screen on
          // right-edge clicks.
          const maxX = window.innerWidth - 200;
          const maxY = window.innerHeight - 280;
          const x = Math.min(m.x, maxX);
          const y = Math.min(m.y, maxY);
          const e = m.entry;
          const side = m.side;
          const isLocal = side === "local";
          const fire = (fn: () => void) => () => {
            fn();
            closeCtxMenu();
          };
          return (
            <div
              class="fm-ctx-menu"
              style={{ left: `${x}px`, top: `${y}px` }}
              onClick={(ev) => ev.stopPropagation()}
            >
              <button class="fm-ctx-item" onClick={fire(() => (isLocal ? openLocal(e) : openRemote(e)))}>
                {t("fm.action.open")}
              </button>
              <Show when={!e.is_dir}>
                <button class="fm-ctx-item" onClick={fire(() => openEditor(side, e.name))}>
                  {t("fm.action.edit")}
                </button>
              </Show>
              <Show when={p.hasSsh && isLocal && !e.is_dir}>
                <button class="fm-ctx-item" onClick={fire(() => void uploadSel())}>
                  {t("fm.btn.upload")}
                </button>
              </Show>
              <Show when={p.hasSsh && !isLocal && !e.is_dir}>
                <button class="fm-ctx-item" onClick={fire(() => void downloadSel())}>
                  {t("fm.btn.download")}
                </button>
              </Show>
              <div class="fm-ctx-sep" />
              {/* Feedback: cut / copy / paste to move files between locations. */}
              <button class="fm-ctx-item" onClick={fire(() => clipSet(side, e.name, "copy"))}>
                {t("fm.action.copy")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => clipSet(side, e.name, "cut"))}>
                {t("fm.action.cut")}
              </button>
              <Show when={clip()}>
                <button class="fm-ctx-item" onClick={fire(() => void pasteInto(side))}>
                  {t("fm.action.paste")}
                </button>
              </Show>
              <div class="fm-ctx-sep" />
              <button class="fm-ctx-item" onClick={fire(() => void copyPathOf(side, e.name))}>
                {t("fm.action.copy_path")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => void renameSel(side))}>
                {t("common.rename")}
              </button>
              <div class="fm-ctx-sep" />
              <button class="fm-ctx-item fm-ctx-danger" onClick={fire(() => void deleteSel(side))}>
                {t("common.delete")}
              </button>
            </div>
          );
        })()}
      </Show>

      {/* Phase 23.B: "+" dropdown next to the path bar.
           Shows New folder / New file / Upload from disk. */}
      <Show when={addMenu()}>
        {(() => {
          const m = addMenu()!;
          const maxX = window.innerWidth - 200;
          const maxY = window.innerHeight - 180;
          const x = Math.min(m.x, maxX);
          const y = Math.min(m.y, maxY);
          const side = m.side;
          const fire = (fn: () => void) => () => {
            fn();
            closeAddMenu();
          };
          return (
            <div
              class="fm-ctx-menu fm-add-menu"
              style={{ left: `${x}px`, top: `${y}px` }}
              onClick={(ev) => ev.stopPropagation()}
            >
              <button class="fm-ctx-item" onClick={fire(() => void mkdirIn(side))}>
                <IconFolder size={14} /> {t("fm.btn.new_folder")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => void createFileIn(side))}>
                <IconFile size={14} /> {t("fm.btn.new_file")}
              </button>
              <button
                class="fm-ctx-item"
                disabled={side === "remote" && !p.hasSsh}
                onClick={fire(() => pickAndUpload(side))}
              >
                <IconUpload size={14} /> {side === "remote"
                  ? t("fm.btn.upload_from_disk_remote")
                  : t("fm.btn.upload_from_disk_local")}
              </button>
              <Show when={clip()}>
                <div class="fm-ctx-sep" />
                <button class="fm-ctx-item" onClick={fire(() => void pasteInto(side))}>
                  <IconClipboard size={14} /> {t("fm.action.paste")}
                </button>
              </Show>
            </div>
          );
        })()}
      </Show>

      {/* Phase 23.B: background context menu — right-click on the empty
           area of a list (not a row) opens this with directory-level
           create/upload actions. */}
      <Show when={bgCtxMenu()}>
        {(() => {
          const m = bgCtxMenu()!;
          const maxX = window.innerWidth - 200;
          const maxY = window.innerHeight - 200;
          const x = Math.min(m.x, maxX);
          const y = Math.min(m.y, maxY);
          const side = m.side;
          const fire = (fn: () => void) => () => {
            fn();
            closeBgCtxMenu();
          };
          return (
            <div
              class="fm-ctx-menu fm-bg-menu"
              style={{ left: `${x}px`, top: `${y}px` }}
              onClick={(ev) => ev.stopPropagation()}
            >
              <button class="fm-ctx-item" onClick={fire(() => void mkdirIn(side))}>
                <IconFolder size={14} /> {t("fm.btn.new_folder")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => void createFileIn(side))}>
                <IconFile size={14} /> {t("fm.btn.new_file")}
              </button>
              <button
                class="fm-ctx-item"
                disabled={side === "remote" && !p.hasSsh}
                onClick={fire(() => pickAndUpload(side))}
              >
                <IconUpload size={14} /> {side === "remote"
                  ? t("fm.btn.upload_from_disk_remote")
                  : t("fm.btn.upload_from_disk_local")}
              </button>
              <Show when={clip()}>
                <button class="fm-ctx-item" onClick={fire(() => void pasteInto(side))}>
                  <IconClipboard size={14} /> {t("fm.action.paste")}
                </button>
              </Show>
              <div class="fm-ctx-sep" />
              <button class="fm-ctx-item" onClick={fire(() => void copyPathOf(side, ""))}>
                {t("fm.btn.copy_path_current")}
              </button>
              <button class="fm-ctx-item" onClick={fire(() => (side === "local" ? refreshLocal() : refreshRemote()))}>
                {t("fm.btn.refresh")}
              </button>
            </div>
          );
        })()}
      </Show>

      {/* Phase 62 (item 6): confirm toast for delete / zip / unzip-
           overwrite. Replaces the native window.confirm with an in-pane
           card carrying Cancel + a (danger-styled) confirm action. */}
      <Show when={confirmToast()}>
        {(c) => (
          <div
            class={`fm-confirm-toast ${c().danger ? "danger" : ""}`}
            role="alertdialog"
            aria-modal="false"
          >
            <div class="fm-confirm-body">
              <div class="fm-confirm-title">{c().title}</div>
              <Show when={c().detail}>
                <div class="fm-confirm-detail">{c().detail}</div>
              </Show>
            </div>
            <div class="fm-confirm-actions">
              <button class="fm-action" onClick={() => setConfirmToast(null)}>
                {t("common.cancel")}
              </button>
              <button
                class={`fm-action ${c().danger ? "fm-action-danger" : ""}`}
                onClick={() => {
                  const fn = c().onConfirm;
                  setConfirmToast(null);
                  fn();
                }}
              >
                {c().confirmLabel}
              </button>
            </div>
          </div>
        )}
      </Show>

      <Show when={editorOpen() && editorTarget()}>
        <FileEditor
          open
          filename={editorTarget()!.filename}
          path={editorTarget()!.path}
          side={editorTarget()!.side}
          workspaceId={p.workspaceId}
          onClose={() => setEditorOpen(false)}
          onSaved={() => {
            // After a successful save, refresh the corresponding column
            // so the new size / mtime show up in the listing.
            if (editorTarget()?.side === "local") void refreshLocal();
            else void refreshRemote();
          }}
        />
      </Show>
    </div>
  );
}
