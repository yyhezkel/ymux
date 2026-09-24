// Unified frontend logger. Every module logs through `createLogger(tag)`
// instead of raw `console.*`; lines reach BOTH the devtools console and the
// single local debug.log (via the `ui_log_batch` Tauri command) tagged
// `[UI:TAG]`.
//
// Lines are QUEUED and shipped in one invoke at most once a second, capped at
// MAX_PER_FLUSH lines (the overflow is counted, not sent). 2026-09-23: one
// invoke per line meant a chatty call site turned logging itself into a steady
// IPC stream — on macOS every invoke is a WebKit custom-scheme load, and a
// 0.5.0 build sustained ~63 of them a second until the GPU hung.
//
// Level filtering is double-gated: we skip the IPC below the threshold here
// (cheap), and the backend filters again through the same global — the
// backend is authoritative, so popout windows that never load settings still
// behave correctly with the default.
//
// IMPORTANT: this module must be imported before the console monkeypatch in
// index.tsx — it captures the ORIGINAL console fns so logger output is never
// forwarded twice.
import { invoke } from "@tauri-apps/api/core";

export type LogLevel = "debug" | "info" | "warn" | "error";

const LEVEL_ORDER: Record<LogLevel, number> = {
  debug: 0,
  info: 1,
  warn: 2,
  error: 3,
};

// Captured before index.tsx monkeypatches console.error/warn to forward
// stray (un-swept / third-party) output to ui_log.
const orig = {
  debug: console.debug.bind(console),
  info: console.info.bind(console),
  warn: console.warn.bind(console),
  error: console.error.bind(console),
};

let currentLevel: LogLevel = "info";

/** Called from App.tsx when settings load / change. */
export function setLoggerLevel(level: LogLevel): void {
  currentLevel = level;
}

/** Compact, metadata-only rendering of an attached error/value. Never dump
 *  whole objects — keep debug.log free of payload content (CLAUDE.md Rule 1
 *  spirit: log what happened, not what flowed through). */
function describe(err: unknown): string {
  if (err === undefined) return "";
  if (err instanceof Error) return ` — ${err.name}: ${err.message}`;
  if (typeof err === "string") return ` — ${err}`;
  try {
    return ` — ${JSON.stringify(err)}`;
  } catch {
    return ` — ${String(err)}`;
  }
}

function emit(tag: string, level: LogLevel, msg: string, err?: unknown): void {
  const text = `${msg}${describe(err)}`;
  // Always visible in devtools regardless of the persisted threshold —
  // the threshold governs what lands in debug.log.
  orig[level](`[${tag}] ${text}`);
  if (LEVEL_ORDER[level] < LEVEL_ORDER[currentLevel]) return;
  enqueueLog(level, tag, text);
}

interface QueuedLine {
  level: LogLevel;
  tag: string;
  message: string;
}

const FLUSH_MS = 1000;
const MAX_PER_FLUSH = 100;
let queue: QueuedLine[] = [];
let dropped = 0;
let flushTimer: ReturnType<typeof setTimeout> | null = null;

function flushLogs(): void {
  if (flushTimer !== null) {
    clearTimeout(flushTimer);
    flushTimer = null;
  }
  if (queue.length === 0 && dropped === 0) return;
  const entries = queue;
  queue = [];
  if (dropped > 0) {
    entries.push({
      level: "warn",
      tag: "LOG",
      message: `dropped ${dropped} UI log line(s): over ${MAX_PER_FLUSH} per ${FLUSH_MS}ms`,
    });
    dropped = 0;
  }
  invoke("ui_log_batch", { entries }).catch(() => {});
}

/** Queue one debug.log line. Also the sink for index.tsx's console
 *  forwarder, so third-party warnings share the same cap. */
export function enqueueLog(level: LogLevel, tag: string, message: string): void {
  if (queue.length >= MAX_PER_FLUSH) dropped++;
  else queue.push({ level, tag, message });
  if (flushTimer === null) flushTimer = setTimeout(flushLogs, FLUSH_MS);
}

if (typeof window !== "undefined") {
  window.addEventListener("pagehide", flushLogs);
}

export interface Logger {
  debug(msg: string, err?: unknown): void;
  info(msg: string, err?: unknown): void;
  warn(msg: string, err?: unknown): void;
  error(msg: string, err?: unknown): void;
}

export function createLogger(tag: string): Logger {
  return {
    debug: (msg, err?) => emit(tag, "debug", msg, err),
    info: (msg, err?) => emit(tag, "info", msg, err),
    warn: (msg, err?) => emit(tag, "warn", msg, err),
    error: (msg, err?) => emit(tag, "error", msg, err),
  };
}
