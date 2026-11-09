// Command-palette search. Zero imports, so it runs under bare node in
// paletteMatch.test.ts: CommandPalette.tsx has JSX and i18n imports JSON,
// and the test runner can load neither.

/** True when every whitespace-separated token of `query` is a
 *  case-insensitive substring of at least one field. A token can match a
 *  different field than its neighbours, so word order and a mixed-language
 *  query do not matter. An empty query matches everything. */
export function matchesQuery(query: string, fields: readonly string[]): boolean {
  const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (tokens.length === 0) return true;
  const haystacks = fields.map((f) => f.toLowerCase());
  return tokens.every((token) => haystacks.some((h) => h.includes(token)));
}
