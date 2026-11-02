// The File Manager's remote side in a browser (Phase 110, WEB-DESIGN C6).
//
// The desktop reaches a server's files over SFTP with absolute paths. A
// browser has the daemon's Files API instead (`/api/v2/files/*`, vault
// server-go § files): sandboxed to one root (--files-root, $HOME by default),
// paths relative to it. This bridge translates: the root's absolute path is
// learned once from `list("/")`'s `cwd`, absolute paths under it become
// root-relative, and anything outside it is refused with a plain message
// (the sandbox is the daemon's to enforce; this only keeps the error legible).
//
// The API has list, read, write (upload), delete (recursive on request),
// download, and since Phase 116 (F2) mkdir, rename, copy, archive and unzip;
// paths the daemon returns are root-relative and come back absolute here.

import { ApiError, api, getToken } from "./api";

/** The desktop's `FileEntry` (FileManagerPane.tsx). */
export interface FmEntry {
  name: string;
  is_dir: boolean;
  is_link: boolean;
  size: number;
  modified: number;
  permissions: string;
}

/** The desktop's `FileContents` (FileEditor.tsx). */
export interface FmContents {
  text: string;
  encoding: string;
  is_binary: boolean;
  size: number;
  truncated: boolean;
}

/** Same cap the editor warns above on the desktop. */
export const LARGE_FILE_BYTES = 1 << 20;

export class FilesBridge {
  private root = "";

  /** The files root as an absolute path (the browser's "home"). */
  async home(): Promise<string> {
    if (!this.root) {
      const r = await api<{ cwd: string }>("GET", "/api/v2/files/list?path=%2F");
      this.root = r.cwd.replace(/\/+$/, "") || "/";
    }
    return this.root;
  }

  /** Absolute → root-relative ("/" is the root itself). */
  async rel(p: string): Promise<string> {
    const h = await this.home();
    const clean = p.replace(/\/+$/, "") || "/";
    if (clean === h) return "/";
    if (h === "/") return clean;
    if (clean.startsWith(`${h}/`)) return clean.slice(h.length);
    throw new Error(`outside the folder this browser can reach (${h})`);
  }

  async list(p: string): Promise<FmEntry[]> {
    const r = await api<{ cwd: string; entries: { name: string; type: string; size: number; modified: number }[] }>(
      "GET",
      `/api/v2/files/list?path=${encodeURIComponent(await this.rel(p))}`,
    );
    return r.entries.map((e) => ({
      name: e.name,
      is_dir: e.type === "dir",
      is_link: false,
      size: e.size,
      modified: e.modified,
      permissions: "",
    }));
  }

  async read(p: string): Promise<FmContents> {
    const r = await this.raw("GET", `/api/v2/files/read?path=${encodeURIComponent(await this.rel(p))}&max_bytes=${LARGE_FILE_BYTES * 4}`);
    const bytes = new Uint8Array(await r.arrayBuffer());
    // The desktop's heuristic: a NUL in the first 8 KB means binary.
    const is_binary = bytes.subarray(0, 8192).includes(0);
    return {
      text: is_binary ? "" : new TextDecoder("utf-8").decode(bytes),
      encoding: "utf-8",
      is_binary,
      size: bytes.length,
      truncated: r.headers.get("X-Ymux-Truncated") === "true",
    };
  }

  async write(p: string, data: Blob | string): Promise<void> {
    const form = new FormData();
    form.append("file", typeof data === "string" ? new Blob([data], { type: "text/plain" }) : data, "upload");
    await this.raw("POST", `/api/v2/files/upload?path=${encodeURIComponent(await this.rel(p))}`, form);
  }

  async remove(p: string): Promise<void> {
    await api("DELETE", `/api/v2/files/delete?path=${encodeURIComponent(await this.rel(p))}`);
  }

  /** The desktop's recursive delete (the UI confirms first). */
  async removeTree(p: string): Promise<void> {
    await api("DELETE", `/api/v2/files/delete?path=${encodeURIComponent(await this.rel(p))}&recursive=true`);
  }

  async mkdir(p: string): Promise<void> {
    await api("POST", "/api/v2/files/mkdir", { path: await this.rel(p) });
  }

  async rename(from: string, to: string): Promise<void> {
    await api("POST", "/api/v2/files/rename", { from: await this.rel(from), to: await this.rel(to) });
  }

  async copy(from: string, to: string): Promise<void> {
    await api("POST", "/api/v2/files/copy", { from: await this.rel(from), to: await this.rel(to) });
  }

  /** Pack names (relative to cwd) into cwd/output; the archive's absolute path. */
  async archive(cwd: string, names: string[], output: string, format: "zip" | "targz"): Promise<string> {
    const r = await api<{ path: string }>("POST", "/api/v2/files/archive", { cwd: await this.rel(cwd), names, output, format });
    return this.abs(r.path);
  }

  /** Extract a .zip into <dir>/<stem>/; that folder's absolute path. */
  async unzip(p: string): Promise<string> {
    const r = await api<{ path: string }>("POST", "/api/v2/files/unzip", { path: await this.rel(p) });
    return this.abs(r.path);
  }

  /** Whether p exists (its parent lists it). */
  async exists(p: string): Promise<boolean> {
    const clean = p.replace(/\/+$/, "");
    const i = clean.lastIndexOf("/");
    const parent = i > 0 ? clean.slice(0, i) : "/";
    const name = clean.slice(i + 1);
    try {
      return (await this.list(parent)).some((e) => e.name === name);
    } catch {
      return false;
    }
  }

  /** Root-relative (from the daemon) → absolute. */
  async abs(rel: string): Promise<string> {
    const h = await this.home();
    if (rel === "/" || rel === "") return h;
    return h === "/" ? rel : `${h}${rel.startsWith("/") ? "" : "/"}${rel}`;
  }

  /** Hands a remote file to the browser's own download (no local paths here). */
  async download(p: string, name: string): Promise<void> {
    const r = await this.raw("GET", `/api/v2/files/download?path=${encodeURIComponent(await this.rel(p))}`);
    const url = URL.createObjectURL(await r.blob());
    try {
      const a = document.createElement("a");
      a.href = url;
      a.download = name || p.split("/").pop() || "download";
      document.body.appendChild(a);
      a.click();
      a.remove();
    } finally {
      setTimeout(() => URL.revokeObjectURL(url), 30_000);
    }
  }

  private async raw(method: string, path: string, body?: BodyInit): Promise<Response> {
    const r = await fetch(path, { method, headers: { Authorization: `Bearer ${getToken()}` }, body });
    if (!r.ok) {
      const text = (await r.text()).trim();
      let msg = text || `HTTP ${r.status}`;
      try {
        const j = JSON.parse(text) as { detail?: string; title?: string };
        msg = j.detail || j.title || msg; // huma's problem+json
      } catch {
        /* plain text */
      }
      throw new ApiError(r.status, msg);
    }
    return r;
  }
}
