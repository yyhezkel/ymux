import { createSignal, For, onMount, Show } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import { t } from "./i18n";
import { IconClock, IconClose, IconFolder, IconWarning } from "./icons";

// Remote directory browser, over the workspace's live SSH session.
//
// The markup, CSS classes (`dir-picker-*`, `claude-picker`), i18n keys
// (`connect.dirPicker.*`) and the localStorage recents were all lifted
// out of PaneView, where this dialog had been sitting UNREACHABLE: its
// guard was `dirPicker() && !dirPickForNewConn()`, and the only caller
// of `openDirPicker` set `dirPickForNewConn` first. So it was fully
// built, styled and translated in four languages, and never rendered.
//
// SSH only, and workspace-scoped rather than connection-scoped, because
// `file_list_remote` resolves the SFTP handle from a LIVE session for
// that workspace (file_manager.rs). There is no local variant here on
// purpose — callers use the native dialog for Local, which is what the
// rest of the app already does.
//
// Two variants, one list body. Modal (default): backdrop dialog with its
// own header and footer, used by the App pin-folder flow. Inline: bare
// `nc-*` section for the new-connection wizard's browse view, whose footer
// belongs to PaneView; the current path is reported through
// `onPathChange` so that footer can enable "Use this". The union makes an
// inline picker without `onPathChange` a type error.

type Props = {
  workspaceId: string;
  /** Where to start. Defaults to the remote `$HOME`, then `/`. */
  startPath?: string;
  onPick: (dir: string) => void;
} & (
  | { inline?: false; onClose: () => void }
  | { inline: true; onPathChange: (path: string) => void }
);

// Per-variant class names for the shared list body.
const MODAL_CLS = {
  recent: "dir-picker-recent",
  recentRow: "dir-picker-recent-row",
  err: "status-line err",
  list: "dir-picker-list",
  row: "dir-picker-row",
  empty: "dir-picker-empty",
};
const INLINE_CLS = {
  recent: "nc-recent",
  recentRow: "nc-recent-row",
  err: "nc-muted err",
  list: "nc-dir-list",
  row: "nc-dir-item",
  empty: "nc-dir-empty",
};

const recentsKey = (workspaceId: string) => `ymux.recent-dirs.${workspaceId}`;

export function loadRecentDirs(workspaceId: string): string[] {
  try {
    const raw = localStorage.getItem(recentsKey(workspaceId));
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((d): d is string => typeof d === "string") : [];
  } catch {
    return [];
  }
}

export function pushRecentDir(workspaceId: string, dir: string): void {
  const next = [dir, ...loadRecentDirs(workspaceId).filter((d) => d !== dir)].slice(0, 8);
  try {
    localStorage.setItem(recentsKey(workspaceId), JSON.stringify(next));
  } catch {
    // A full or disabled localStorage must not break the picker.
  }
}

/** `/a/b` → `/a`; `/a` → `/`. POSIX only — the remote side always is. */
export function parentDir(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const idx = trimmed.lastIndexOf("/");
  if (idx <= 0) return "/";
  return trimmed.slice(0, idx);
}

export function joinDir(path: string, name: string): string {
  return path === "/" ? `/${name}` : `${path.replace(/\/+$/, "")}/${name}`;
}

export function DirPicker(p: Props) {
  const [path, setPath] = createSignal("/");
  const [dirs, setDirs] = createSignal<string[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [error, setError] = createSignal<string | null>(null);
  const [recents, setRecents] = createSignal<string[]>([]);

  const navigate = async (to: string) => {
    setPath(to);
    if (p.inline) p.onPathChange(to);
    setDirs([]);
    setLoading(true);
    setError(null);
    try {
      const list = await invoke<{ name: string; is_dir: boolean }[]>("file_list_remote", {
        workspaceId: p.workspaceId,
        path: to,
        showHidden: false,
      });
      setDirs(
        list
          .filter((e) => e.is_dir)
          .map((e) => e.name)
          .sort((a, b) => a.localeCompare(b)),
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  onMount(() => {
    setRecents(loadRecentDirs(p.workspaceId));
    void (async () => {
      if (p.startPath) {
        await navigate(p.startPath);
        return;
      }
      let start = "/";
      try {
        start = await invoke<string>("file_home_remote", { workspaceId: p.workspaceId });
      } catch {
        // No $HOME (or no session yet) — "/" is still a valid place to
        // start, and navigate() surfaces the real error if there is one.
      }
      await navigate(start || "/");
    })();
  });

  const choose = (dir: string) => {
    // inline consumer owns recents (PaneView.chooseDir)
    if (!p.inline) pushRecentDir(p.workspaceId, dir);
    p.onPick(dir);
  };

  const cls = p.inline ? INLINE_CLS : MODAL_CLS;

  const recentBlock = (
    <Show when={recents().length > 0}>
      <div class={cls.recent}>
        <Show when={!p.inline}>
          <div class="dir-picker-recent-label">{t("connect.dirPicker.recent")}</div>
        </Show>
        <For each={recents()}>
          {(d) => (
            <button class={cls.recentRow} title={d} onClick={() => choose(d)}>
              <IconClock size={14} /> {d}
            </button>
          )}
        </For>
      </div>
    </Show>
  );

  const errorRow = (
    <Show when={error()}>
      <p class={cls.err}><IconWarning size={13} /> {error()}</p>
    </Show>
  );

  const list = (
    <ul class={cls.list}>
      <Show when={path() !== "/"}>
        <li class={`${cls.row} up`} onClick={() => void navigate(parentDir(path()))}>
          <IconFolder size={14} /> ..
        </li>
      </Show>
      <For each={dirs()}>
        {(name) => (
          <li class={cls.row} onClick={() => void navigate(joinDir(path(), name))}>
            <IconFolder size={14} /> {name}
          </li>
        )}
      </For>
      <Show when={!loading() && dirs().length === 0 && !error()}>
        <li class={cls.empty}>{t("connect.dirPicker.empty")}</li>
      </Show>
    </ul>
  );

  if (p.inline) {
    return (
      <div class="nc-section">
        <div class="nc-browse-path" title={path()}>{path()}</div>
        {recentBlock}
        {errorRow}
        {list}
      </div>
    );
  }

  const onClose = p.onClose;
  return (
    <div class="modal-backdrop" onClick={onClose}>
      <div
        class="modal claude-picker"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div class="settings-head">
          <h3>{t("connect.dirPicker.title")}</h3>
          <button class="feed-x" title={t("common.close")} onClick={onClose}>
            <IconClose size={14} />
          </button>
        </div>
        <div class="dir-picker-path" title={path()}>{path()}</div>
        {recentBlock}
        <div class="claude-picker-body">
          <Show when={loading()}>
            <p class="status-line">{t("connect.dirPicker.loading")}</p>
          </Show>
          {errorRow}
          {list}
        </div>
        <div class="modal-buttons">
          <button onClick={onClose}>{t("common.cancel")}</button>
          <button class="primary" disabled={loading()} onClick={() => choose(path())}>
            {t("connect.dirPicker.useThis")}
          </button>
        </div>
      </div>
    </div>
  );
}
