// Host-OS awareness for the frontend.
//
// Before the macOS port the frontend had none — no navigator.platform check,
// no plugin-os, nothing. Two Windows-only assumptions were baked in as
// literals and both broke on mac:
//   1. local file-manager paths joined with a hardcoded `\`
//   2. drag-drop positions divided by devicePixelRatio (WebView2 reports
//      physical pixels; wry's macOS backend reports logical points)
//
// Resolved ONCE from the Rust side (`host_platform`, std::env::consts::OS)
// rather than sniffing the user agent, and awaited in index.tsx BEFORE the
// first render — the drag-drop listeners register in onMount, so an
// unresolved promise there would mean the first drop uses the wrong answer.
// After init() the accessors are plain synchronous reads.

import { backend } from "./backend";

type HostOs = "windows" | "macos" | "linux";

// Windows is the safe default for the pre-init window: it's what every call
// site assumed before this module existed, so a failed probe degrades to the
// old behavior instead of a new one.
let hostOs: HostOs = "windows";

export async function initPlatform(): Promise<void> {
  try {
    const os = await backend.call<string>("host_platform");
    if (os === "macos" || os === "linux" || os === "windows") hostOs = os;
  } catch {
    // Probe failed (command missing / IPC not ready) — keep the default.
  }
}

export function isWindows(): boolean {
  return hostOs === "windows";
}

export function isMac(): boolean {
  return hostOs === "macos";
}

/** Path separator for LOCAL paths. Remote paths are always POSIX. */
export function sep(): string {
  return hostOs === "windows" ? "\\" : "/";
}

/** Test seam — lets the path helpers be exercised for both platforms. */
export function __setHostOsForTests(os: HostOs): void {
  hostOs = os;
}
