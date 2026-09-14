// Unified side-panel lifecycle (drawer → float → fullscreen). Each side
// panel (Notifications, Monitor, Files, Diff; Browser joins later on its
// own native-webview track) shares one lifecycle: it opens docked as a
// side drawer, then can either float out into an in-app window or expand
// to fill the workspace like a maximized pane. This module owns only the
// small state vocabulary — a per-panel "surface" — that App.tsx drives.

export type PanelId = "notifications" | "monitor" | "files" | "diff" | "tickets" | "queue";

/** Where a panel currently lives. `closed` ⇒ not shown. */
export type Surface = "closed" | "drawer" | "float" | "fullscreen";

/** Sparse map of panel → surface. A missing key means "closed". */
export type PanelSurfaces = Partial<Record<PanelId, Surface>>;

/** Collapse every panel currently docked as a `drawer` except `keep`.
 *  Only one drawer may be open at a time (they share the inline-end dock
 *  + backdrop); `float` and `fullscreen` panels coexist freely. Pure —
 *  returns a new map. */
export function closeOtherDrawers(cur: PanelSurfaces, keep: PanelId): PanelSurfaces {
  const next: PanelSurfaces = { ...cur };
  for (const k of Object.keys(next) as PanelId[]) {
    if (k !== keep && next[k] === "drawer") next[k] = "closed";
  }
  return next;
}
