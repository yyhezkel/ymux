// Persisted per-pane "Claude is running" flag (LayoutNode::Pane.claude_running).
// Pure decisions only; App.tsx owns the invoke and signal calls.
//
// Stale-true trade-off: Claude dying while the app is closed leaves persisted
// true, so a reattach starts in Claude bidi state until a session-end hook or
// a non-restoring connect corrects it.

// Signal to apply before pane_connect; null = leave the signal untouched.
export function tuiSignalOnConnect(
  mode: string | undefined,
  restoring: boolean,
  persisted: boolean | null | undefined,
): boolean | null {
  if (mode === "claude") return true;
  if (!restoring) return false;
  if (persisted === true) return true;
  return null;
}

// Value to persist; null = no transition, skip the invoke.
export function claudeRunningWrite(
  persisted: boolean | null | undefined,
  on: boolean,
): boolean | null {
  return (persisted === true) === on ? null : on;
}
