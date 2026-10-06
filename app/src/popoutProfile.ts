// Popout RTL profile hand-off: App writes the origin pane's profile under
// popoutProfileKey(sid) before popping out; PopoutTerminal reads it back.
// Pure module -- no storage or Tauri access, so node:test can import it.
import type { RtlProfileKind } from "./types";

export function popoutProfileKey(sessionId: string): string {
  return `ymux.popout.profile.${sessionId}`;
}

export function parsePopoutProfile(raw: string | null): RtlProfileKind {
  return raw === "remote" ? "remote" : "local"; // AI-NOTE: absent/unknown → local, the pre-fix default
}
