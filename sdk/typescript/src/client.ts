// client.ts — the hand-written ymux-server REST client. Thin, dependency-free
// (uses the built-in fetch), typed against the generated OpenAPI schemas in
// types.gen.ts. The WS side lives in ws.ts.
import type { components } from "./types.gen.js";

type Schemas = components["schemas"];
export type FileEntry = Schemas["FileEntry"];
export type FileList = Schemas["FileListBody"];
export type UploadResult = Schemas["UploadResultBody"];
export type ClientInfo = Schemas["ClientInfo"];
export type LogRead = Schemas["ReadBody"];
export type Version = Schemas["VersionBody"];
export type Health = Schemas["HealthBody"];
export type PairingRedeemResponse = Schemas["PairingRedeemResponse"];
export type Workspace = Schemas["Workspace"];
export type Session = Schemas["Session"];
export type CreateSessionRequest = Schemas["CreateSessionRequest"];
export type SessionCreated = Schemas["SessionCreated"];

export interface YmuxClientOptions {
  /** e.g. "http://127.0.0.1:7879" */
  baseUrl: string;
  /** bearer token; omitted only for the public /healthz + /api/version. */
  token?: string;
  /** override fetch (tests / non-browser runtimes). Defaults to global fetch. */
  fetch?: typeof fetch;
}

export class YmuxApiError extends Error {
  constructor(
    readonly status: number,
    readonly body: string,
  ) {
    super(`ymux-server ${status}: ${body}`);
    this.name = "YmuxApiError";
  }
}

export class YmuxClient {
  private readonly base: string;
  private readonly token?: string;
  private readonly f: typeof fetch;

  constructor(opts: YmuxClientOptions) {
    this.base = opts.baseUrl.replace(/\/$/, "");
    this.token = opts.token;
    this.f = opts.fetch ?? globalThis.fetch.bind(globalThis);
  }

  private auth(headers: Record<string, string> = {}): Record<string, string> {
    return this.token ? { ...headers, Authorization: `Bearer ${this.token}` } : headers;
  }

  private async json<T>(path: string, init?: RequestInit): Promise<T> {
    const res = await this.f(this.base + path, { ...init, headers: this.auth(init?.headers as Record<string, string>) });
    if (!res.ok) throw new YmuxApiError(res.status, await res.text());
    return (await res.json()) as T;
  }

  private postJson<T>(path: string, body: unknown): Promise<T> {
    return this.json<T>(path, { method: "POST", body: JSON.stringify(body), headers: { "Content-Type": "application/json" } });
  }

  // ── pairing ──────────────────────────────────────────────────────────────
  /** Pairing: exchange a one-shot token for a device credential. */
  get pairing() {
    return {
      /** POST /api/pairing/redeem — public; the one-shot token is the credential. */
      redeem: (oneShotToken: string): Promise<PairingRedeemResponse> =>
        this.postJson<PairingRedeemResponse>("/api/pairing/redeem", { one_shot_token: oneShotToken }),
    };
  }

  // ── workspaces ─────────────────────────────────────────────────────────────
  /** Workspace list + per-session access (the mobile stream lives on WorkspaceSocket). */
  get workspaces() {
    const c = this;
    return {
      list: (): Promise<Workspace[]> => c.json<Workspace[]>("/api/v2/workspace/list"),
      getSession: (workspaceId: string, sessionId: string): Promise<Session> =>
        c.json<Session>(`/api/v2/workspace/${encodeURIComponent(workspaceId)}/session/${encodeURIComponent(sessionId)}`),
      /** Sessions under a workspace: `.workspaces.sessions(id).create({ kind })`. */
      sessions: (workspaceId: string) => ({
        create: (request: CreateSessionRequest): Promise<SessionCreated> =>
          c.postJson<SessionCreated>(`/api/v2/workspace/${encodeURIComponent(workspaceId)}/sessions`, request),
      }),
    };
  }

  // ── meta (public) ──────────────────────────────────────────────────────
  health(): Promise<Health> {
    return this.json<Health>("/healthz");
  }
  version(): Promise<Version> {
    return this.json<Version>("/api/version");
  }

  // ── files ──────────────────────────────────────────────────────────────
  listFiles(path = "", depth: 1 | 2 = 1): Promise<FileList> {
    return this.json<FileList>(`/api/v2/files/list?path=${encodeURIComponent(path)}&depth=${depth}`);
  }
  async readFile(path: string, maxBytes?: number): Promise<{ bytes: Uint8Array; truncated: boolean }> {
    const q = maxBytes != null ? `&max_bytes=${maxBytes}` : "";
    const res = await this.f(`${this.base}/api/v2/files/read?path=${encodeURIComponent(path)}${q}`, {
      headers: this.auth(),
    });
    if (!res.ok) throw new YmuxApiError(res.status, await res.text());
    const truncated = res.headers.get("X-Ymux-Truncated") === "true";
    return { bytes: new Uint8Array(await res.arrayBuffer()), truncated };
  }
  async uploadFile(path: string, data: Blob | Uint8Array, filename = "file"): Promise<UploadResult> {
    const form = new FormData();
    const blob = data instanceof Blob ? data : new Blob([data as BlobPart]);
    form.append("file", blob, filename);
    return this.json<UploadResult>(`/api/v2/files/upload?path=${encodeURIComponent(path)}`, { method: "POST", body: form });
  }
  async downloadFile(path: string): Promise<Uint8Array> {
    const res = await this.f(`${this.base}/api/v2/files/download?path=${encodeURIComponent(path)}`, {
      headers: this.auth(),
    });
    if (!res.ok) throw new YmuxApiError(res.status, await res.text());
    return new Uint8Array(await res.arrayBuffer());
  }
  deleteFile(path: string): Promise<{ ok: boolean }> {
    return this.json<{ ok: boolean }>(`/api/v2/files/delete?path=${encodeURIComponent(path)}`, { method: "DELETE" });
  }

  // ── logs ───────────────────────────────────────────────────────────────
  listLogClients(): Promise<{ clients: ClientInfo[] }> {
    return this.json<{ clients: ClientInfo[] }>("/api/v2/logs/list");
  }
  readLog(clientId: string, file = "", tail = 200): Promise<LogRead> {
    return this.json<LogRead>(
      `/api/v2/logs/read?client_id=${encodeURIComponent(clientId)}&file=${encodeURIComponent(file)}&tail=${tail}`,
    );
  }
}
