// Formatters shared by the Monitor's tabs. Extracted from InsightsWindow so
// the Analytics tab can reuse them without importing back into its own parent
// (that import would be a cycle).

/** 1024-based size, e.g. `1.4 GB`. Sub-KB values stay integral. */
export function fmtBytes(n: number): string {
  if (!n) return "0 B";
  const neg = n < 0;
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  let v = Math.abs(n);
  while (v >= 1024 && i < u.length - 1) {
    v /= 1024;
    i++;
  }
  return `${neg ? "-" : ""}${v.toFixed(v < 10 && i > 0 ? 1 : 0)} ${u[i]}`;
}

export const fmtBps = (n: number) => `${fmtBytes(n)}/s`;

/** Percent with one decimal below 10, none above — keeps columns from dancing. */
export const fmtPct = (n: number) => `${n < 10 ? n.toFixed(1) : Math.round(n)}%`;

/** Compact duration for a window span: `45m`, `6h`, `2d 3h`. */
export function fmtSpan(seconds: number): string {
  if (seconds < 60) return `${Math.max(0, Math.round(seconds))}s`;
  const m = Math.round(seconds / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 48) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`;
  const d = Math.floor(h / 24);
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`;
}
