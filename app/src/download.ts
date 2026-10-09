import { backend } from "./backend";

// Phase 65 (bug K): "always ask where to save" downloads. Opens a native
// Save dialog (Tauri dialog plugin), defaulting to the last folder the
// user saved to (or a caller-supplied dir), then SFTP-pulls the remote
// file to the chosen path via the existing `file_download` command.

const LAST_DIR_KEY = "ymux.last-download-dir";

function lastDir(): string | null {
  try {
    return localStorage.getItem(LAST_DIR_KEY);
  } catch {
    return null;
  }
}
function rememberDir(dest: string): void {
  // Strip the filename to keep just the folder.
  const dir = dest.replace(/[\\/][^\\/]*$/, "");
  if (!dir) return;
  try {
    localStorage.setItem(LAST_DIR_KEY, dir);
  } catch {
    // quota / private mode — best-effort
  }
}

/**
 * Prompt for a destination, then download `remotePath` there.
 * Returns the saved local path, or `null` if the user cancelled the
 * dialog (callers should treat null as "no error, just aborted").
 * `defaultDir` pre-selects a folder (e.g. the File Manager's current
 * local column); otherwise the last-used download folder is used.
 */
export async function saveRemoteFileAs(
  workspaceId: string,
  remotePath: string,
  suggestedName: string,
  defaultDir?: string,
): Promise<string | null> {
  // Phase 110: a browser has no local paths and no Save dialog — the file
  // goes to the browser's own download, and there is no path to return.
  if (backend.kind === "web") {
    await backend.call("web_download", { remotePath, name: suggestedName });
    return suggestedName;
  }
  const dir = defaultDir ?? lastDir() ?? "";
  const defaultPath = dir
    ? `${dir.replace(/[\\/]+$/, "")}/${suggestedName}`
    : suggestedName;
  const dest = await backend.host.savePath({ defaultPath });
  if (!dest) return null; // cancelled
  await backend.call("file_download", {
    workspaceId,
    remotePath,
    localPath: dest,
  });
  rememberDir(dest);
  return dest;
}
