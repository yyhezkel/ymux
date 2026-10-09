import { createSignal } from "solid-js";
import { backend } from "./backend";

// Phase 81.B: global store for in-flight SFTP transfers.
//
// A module signal rather than prop-threading (same shape as
// mdViewerStore): transfers are started from several places —
// the File Manager, terminal drag-drop, OSC 8 link downloads — and a
// single listener registered once at the App root feeds them all into
// one list that <TransferBar> renders.
//
// The backend is the source of truth: rows are created by the first
// `fm-transfer-progress` tick (forced at 0 %, so a row shows before any
// byte moves) and closed by `fm-transfer-done`.

export type TransferDirection = "upload" | "download";

export type TransferState = "active" | "canceling" | "done" | "canceled" | "error";

export interface Transfer {
  id: string;
  direction: TransferDirection;
  /** Basename, shown in the progress row. */
  name: string;
  /** Remote-side path. */
  path: string;
  done: number;
  /** 0 when the size is unknown — render an indeterminate bar. */
  total: number;
  speedBps: number;
  state: TransferState;
  error?: string;
}

/** Wire shape of `fm-transfer-progress` (snake_case, straight from Rust). */
interface ProgressPayload {
  id: string;
  direction: TransferDirection;
  name: string;
  path: string;
  done: number;
  total: number;
  speed_bps: number;
}

/** Wire shape of `fm-transfer-done`. */
interface DonePayload {
  id: string;
  direction: TransferDirection;
  name: string;
  ok: boolean;
  canceled: boolean;
  error: string | null;
}

/** How long a finished-cleanly row lingers before it disappears. */
const CLEAR_DELAY_MS = 2500;

const [transfers, setTransfers] = createSignal<Transfer[]>([]);

/** Reactive accessor for every transfer the UI should currently show. */
export const transferList = transfers;

/** True while anything is still moving — drives the bar's visibility. */
export function hasActiveTransfers(): boolean {
  return transfers().some((t) => t.state === "active" || t.state === "canceling");
}

function upsert(p: ProgressPayload): void {
  setTransfers((list) => {
    const i = list.findIndex((t) => t.id === p.id);
    if (i === -1) {
      return [
        ...list,
        {
          id: p.id,
          direction: p.direction,
          name: p.name,
          path: p.path,
          done: p.done,
          total: p.total,
          speedBps: p.speed_bps,
          state: "active",
        },
      ];
    }
    const cur = list[i];
    // A late tick can land after fm-transfer-done (they race across the
    // IPC bridge); never resurrect a row that already finished.
    if (cur.state !== "active" && cur.state !== "canceling") return list;
    const next = [...list];
    next[i] = { ...cur, done: p.done, total: p.total, speedBps: p.speed_bps };
    return next;
  });
}

function finish(p: DonePayload): void {
  const state: TransferState = p.canceled ? "canceled" : p.ok ? "done" : "error";
  setTransfers((list) => {
    const i = list.findIndex((t) => t.id === p.id);
    // A transfer that failed before its first progress tick (no SSH
    // session, permission denied) has no row yet — create one so the
    // error is still visible.
    if (i === -1) {
      if (state === "done") return list;
      return [
        ...list,
        {
          id: p.id,
          direction: p.direction,
          name: p.name,
          path: "",
          done: 0,
          total: 0,
          speedBps: 0,
          state,
          error: p.error ?? undefined,
        },
      ];
    }
    const next = [...list];
    next[i] = { ...next[i], state, error: p.error ?? undefined };
    return next;
  });
  // Clean finishes clear themselves; errors stay until dismissed so the
  // user actually gets to read them.
  if (state !== "error") {
    setTimeout(() => dismissTransfer(p.id), CLEAR_DELAY_MS);
  }
}

/** Drop a row from the list. Does not touch the backend. */
export function dismissTransfer(id: string): void {
  setTransfers((list) => list.filter((t) => t.id !== id));
}

/**
 * Ask the backend to abort a transfer. The row flips to "canceling"
 * immediately for feedback; the real state change arrives with
 * `fm-transfer-done { canceled: true }` once the streaming loop notices
 * between chunks and cleans up the partial file.
 */
export async function cancelTransfer(id: string): Promise<void> {
  setTransfers((list) =>
    list.map((t) => (t.id === id && t.state === "active" ? { ...t, state: "canceling" } : t)),
  );
  try {
    await backend.call("fm_transfer_cancel", { transferId: id });
  } catch {
    // Already finished between the click and the call — the done event
    // has the truth, so there is nothing to report here.
  }
}

let started = false;

/**
 * Subscribe to the transfer events. Call once from the App root; repeat
 * calls are no-ops so a re-mount can't double-count ticks.
 */
export async function initTransferListener(): Promise<void> {
  if (started) return;
  started = true;
  await backend.on<ProgressPayload>("fm-transfer-progress", (e) => upsert(e.payload));
  await backend.on<DonePayload>("fm-transfer-done", (e) => finish(e.payload));
}

// ─── formatting helpers (shared by the bar and any future tray) ──────────

/** "12.4 MB" — matches the File Manager's existing size column style. */
export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let v = n / 1024;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u++;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[u]}`;
}

/** "3.2 MB/s". Zero means "not measured yet". */
export function fmtSpeed(bps: number): string {
  if (bps <= 0) return "";
  return `${fmtBytes(bps)}/s`;
}

/** "0:42" / "1:05:03" remaining. Empty when it can't be estimated. */
export function fmtEta(t: Transfer): string {
  if (t.total <= 0 || t.speedBps <= 0) return "";
  const left = Math.max(0, t.total - t.done);
  const secs = Math.round(left / t.speedBps);
  if (secs >= 3600) {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    return `${h}:${String(m).padStart(2, "0")}:${String(secs % 60).padStart(2, "0")}`;
  }
  return `${Math.floor(secs / 60)}:${String(secs % 60).padStart(2, "0")}`;
}

/** 0–100, or null when the total is unknown (indeterminate bar). */
export function percent(t: Transfer): number | null {
  if (t.total <= 0) return null;
  return Math.min(100, Math.floor((t.done / t.total) * 100));
}
