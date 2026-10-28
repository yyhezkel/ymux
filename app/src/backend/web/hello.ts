// Daemon hello ships a zero-value brief/run entry (seq 0) for every live
// session; the desktop map holds hook-written entries only. Go bumps seq on
// every hook mutation, so seq > 0 <=> a hook wrote it.
export function touchedEntries(
  m: Record<string, unknown> | null | undefined,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  if (!m) return out;
  for (const [k, v] of Object.entries(m)) {
    if (typeof v !== "object" || v === null) continue;
    const seq = (v as { seq?: unknown }).seq;
    if (typeof seq === "number" && seq > 0) out[k] = v;
  }
  return out;
}
