// Pop-out wheel-proxy arm decision. Zero imports so node can unit-test it.
// True only when the origin pane is tmux-persisted; any miss or failure → false.
export async function resolvePopoutTmuxArm(
  paneId: string | null,
  list: () => Promise<Record<string, string>>,
  warn: (msg: string, err: unknown) => void,
): Promise<boolean> {
  if (!paneId) return false;
  try {
    const m = await list();
    return !!m?.[paneId];
  } catch (e) {
    warn("popout wheel-proxy arm failed", e);
    return false;
  }
}
