// Window-title pane name: title, then auto title, then the described
// connection. Dependency-free so node tests load it (no ./types import);
// the caller passes describeConnection. Pure.
export function windowPaneName<C>(
  pane: { title?: string | null; auto_title?: string | null; connection?: C | null },
  describe: (c: C) => string,
): string | null {
  return pane.title || pane.auto_title || (pane.connection ? describe(pane.connection) : null);
}
