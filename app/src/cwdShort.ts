// Phase 91.E: the sidebar card's path line. Pure and import-free on purpose
// so `cwdShort.test.ts` runs under plain `node --test` (the app tsconfig
// excludes *.test.ts; relative imports there need explicit extensions and
// this module has none to need).

/**
 * `/home/yossi/src/x` → `~/src/x`. Collapses the home prefixes ymux meets:
 * `/home/<u>` (any user when `sshUser` is null — a local Linux/WSL pane —
 * or the connection's own user), `/root` for an ssh root login,
 * `/Users/<u>` (macOS) and `<X>:\Users\<u>` (Windows). A path still longer
 * than `maxLen` keeps its last two segments behind an ellipsis; the full
 * path belongs in the row's tooltip.
 */
export function shortenCwd(path: string, sshUser: string | null, maxLen = 34): string {
  let s = path;
  s = s.replace(/^\/home\/([^/]+)(?=\/|$)/, (m, u: string) =>
    sshUser === null || u === sshUser ? "~" : m,
  );
  if (sshUser === "root") s = s.replace(/^\/root(?=\/|$)/, "~");
  s = s.replace(/^\/Users\/[^/]+(?=\/|$)/, "~");
  s = s.replace(/^[A-Za-z]:[\\/]Users[\\/][^\\/]+(?=[\\/]|$)/, "~");
  if (s.length <= maxLen) return s;
  const parts = s.split(/[\\/]+/).filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : s;
}
