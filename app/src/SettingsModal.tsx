import { createSignal, For, Show, onMount, createMemo, createEffect, onCleanup } from "solid-js";
import type { RtlProfileKind } from "./types";
import { invoke } from "@tauri-apps/api/core";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import {
  Settings,
  PresetEntry,
  FontFamilies,
  FontEntry,
  FontCatalogItem,
  fontUninstall,
  fontCatalog,
  fontInstall,
  UpdateInfo,
  applyTheme,
  resolveThemeMode,
  getPresets,
  applyPreset,
  resetSettings,
  saveSettings,
  listSystemFonts,
  checkForUpdates,
  loadSettings,
  DEFAULT_SHORTCUTS,
  DEFAULT_BRIEF_SETTINGS,
  DEFAULT_CLAUDE_SETTINGS,
  DEFAULT_CLAUDE_USAGE_SETTINGS,
  DEFAULT_HOOK_NOTIFICATIONS,
  DEFAULT_LOGS_SETTINGS,
  HookType,
  HookNotificationSettings,
  INTERACTIVE_HOOKS,
  OBSERVABILITY_HOOKS,
  type RtlProfileFields,
} from "./settings";
import { applyI18nSettings, LANGUAGES, t } from "./i18n";
import { isFontAvailableAsync } from "./fontProbe";
import { isWindows } from "./platform";
import { IconChevronDown, IconChevronRight, IconRefreshCcw } from "./icons";
import { VersionManager } from "./VersionManager";
import {
  SHORTCUT_GROUPS,
  canonicalAccel,
  conflictingAccels,
  formatEvent,
} from "./shortcuts";
import { AddonsTab } from "./AddonsTab";
import { YmuxToolsTab } from "./YmuxToolsTab";
import { createLogger } from "./logger";

const log = createLogger("SETTINGS");

// ─── font availability ─────────────────────────────────────────────────────
//
// The picker offers a curated baseline on top of what is actually installed
// (see `list_system_fonts` in settings.rs), so it can list families this
// machine does not have — `JetBrains Mono` and `Inter` ship with nothing.
// Picking one used to do nothing visible at all: `quoteFamily()` appends a
// CSS fallback chain ending back at the default, so the terminal re-rendered
// identically and the setting looked broken. These mark the gap instead.

/** Option label: family name prefixed with an availability badge. */
function fontLabel(f: FontEntry): string {
  return `${f.installed ? "✅" : "⚠️"} ${f.name}`;
}

/**
 * The currently-selected family, but only when we positively know it is NOT
 * installed. Returns undefined when it is installed OR when it isn't in the
 * list at all — an unlisted family (hand-typed, or supplied by the web-font
 * URL) is unverifiable here, and a false alarm is worse than no alarm.
 */
function missingFamily(list: FontEntry[], selected: string): string | undefined {
  const hit = list.find(
    (f) => f.name.toLowerCase() === selected.trim().toLowerCase(),
  );
  return hit && !hit.installed ? hit.name : undefined;
}

/** Bytes → "5.4 MB", so the user sees the cost before starting a download. */
function formatBytes(n: number): string {
  return n >= 1048576
    ? `${(n / 1048576).toFixed(1)} MB`
    : `${Math.max(1, Math.round(n / 1024))} KB`;
}

/**
 * Shown under a font select whose chosen family isn't installed. When
 * ymux can install that family itself, the notice carries the button —
 * telling the user what's wrong without offering the fix is only half an
 * answer, and hunting down a .ttf is exactly the step that stalls an
 * onboarding call.
 */
/**
 * Free-text family entry, for anything the dropdown can't offer: a font the
 * mono/UI heuristic filed under the wrong list, one delivered by the
 * web-font URL, or simply a family this build's catalog has never heard of.
 *
 * The verdict comes from `isFontAvailableAsync` — the renderer is asked
 * whether it can actually draw the family, which is the only check that
 * survives the Chromium `document.fonts.check()` false-positive.
 */
function CustomFontField(props: {
  label: string;
  onApply: (family: string) => void;
}) {
  const [value, setValue] = createSignal("");
  const [ok, setOk] = createSignal<boolean | null>(null);

  // Probe as the user types, debounced. Probing only on change/blur looks
  // cheaper but traps them: the Use button stays disabled until a verdict
  // exists, so typing a valid name and clicking Use swallows the click —
  // the blur fires the probe, the button enables a moment later, and the
  // press is gone.
  let probeToken = 0;
  let timer: number | undefined;
  const probe = (raw: string) => {
    const token = ++probeToken;
    const family = raw.trim();
    if (!family) {
      setOk(null);
      return;
    }
    void isFontAvailableAsync(family).then((available) => {
      // Drop a verdict the user has already typed past.
      if (token === probeToken) setOk(available);
    });
  };
  const schedule = (raw: string) => {
    window.clearTimeout(timer);
    timer = window.setTimeout(() => probe(raw), 250);
  };
  onCleanup(() => window.clearTimeout(timer));

  return (
    <label>
      <span>{props.label}</span>
      <div style="display:flex; gap:8px; flex:1; align-items:center">
        <input
          type="text"
          style="flex:1"
          placeholder={t("settings.font.custom.placeholder")}
          value={value()}
          onInput={(e) => {
            const raw = e.currentTarget.value;
            setValue(raw);
            setOk(null);
            schedule(raw);
          }}
        />
        <Show when={ok() !== null}>
          <span title={ok() ? undefined : t("settings.font.custom.unavailable")}>
            {ok() ? "✅" : "⚠️"}
          </span>
        </Show>
        <button
          disabled={!value().trim() || ok() !== true}
          onClick={() => props.onApply(value().trim())}
        >
          {t("settings.font.custom.apply")}
        </button>
      </div>
    </label>
  );
}

function FontMissingNotice(props: {
  family: string;
  catalog: FontCatalogItem[];
  busy: boolean;
  onInstall: (item: FontCatalogItem) => void;
}) {
  const item = () =>
    props.catalog.find(
      (c) => c.family.toLowerCase() === props.family.toLowerCase(),
    );
  return (
    <div class="font-missing-notice">
      <div>⚠️ {t("settings.font.missing", { family: props.family })}</div>
      <Show when={item()}>
        {(c) => (
          <div class="font-missing-actions">
            <button
              disabled={props.busy}
              onClick={() => props.onInstall(c())}
            >
              {props.busy
                ? t("settings.font.installing")
                : t("settings.font.install", {
                    size: formatBytes(c().download_bytes),
                  })}
            </button>
            <a href={c().homepage} target="_blank" rel="noreferrer">
              {c().license}
            </a>
          </div>
        )}
      </Show>
    </div>
  );
}

/**
 * The other half of FontMissingNotice: what ymux has already put on this
 * machine, and a way to take it back off.
 *
 * It is a list rather than a button beside the picker, because the picker
 * only ever shows ONE family and the notice above it only renders when that
 * family is MISSING — so an installed font has nowhere to hang a control.
 * Renders nothing at all when nothing is installed.
 */
function FontInstalledList(props: {
  catalog: FontCatalogItem[];
  busy: string | null;
  onUninstall: (item: FontCatalogItem) => void;
}) {
  const installed = () => props.catalog.filter((c) => c.installed);
  return (
    <Show when={installed().length > 0}>
      <div class="font-installed-list">
        <div class="font-installed-title">
          {t("settings.font.installed.title")}
        </div>
        <For each={installed()}>
          {(c) => (
            <div class="font-installed-row">
              <span class="font-installed-name">{c.family}</span>
              <button
                disabled={props.busy !== null}
                onClick={() => props.onUninstall(c)}
              >
                {props.busy === c.id
                  ? t("settings.font.uninstalling")
                  : t("settings.font.uninstall")}
              </button>
            </div>
          )}
        </For>
      </div>
    </Show>
  );
}

interface Props {
  open: boolean;
  settings: Settings;
  onClose: () => void;
  onChange: (next: Settings) => void;
  /** Phase 68.E: active workspace — add-ons are per-remote. */
  activeWorkspaceId?: string;
}

type Tab = "general" | "textLocale" | "appearance" | "shortcuts" | "agentNotif" | "ai" | "system";

// beta.3: sub-tab within the "Hooks & Notifications" card.
type HooksNotifSubTab = "hooks" | "sound";

/** Per-profile fallbacks, matching RtlProfiles::default() in settings.rs.
 *
 *  They are identical today. The two profiles were measured on 2026-08-19 to
 *  need OPPOSITE modes -- raw ConPTY hands over visual-order Hebrew -- and then
 *  zellij went in front of local panes and normalised the stream to logical
 *  order, which converged them. The split still lets a user tune them apart;
 *  the two lines stay separate so that remains a one-word change.
 *
 *  `direction_policy` is `any_rtl` on BOTH: it is the pre-2026-08-19 rule, and
 *  remote panes are known to render Hebrew correctly on it. */
const RTL_FIELD_DEFAULTS: Record<RtlProfileKind, Required<RtlProfileFields>> = {
  local: { rtl_mode: "auto_per_line", auto_direction: true, mirror_arrows_rtl: true, tui_owns_bidi: true, direction_policy: "any_rtl" },
  remote: { rtl_mode: "auto_per_line", auto_direction: true, mirror_arrows_rtl: true, tui_owns_bidi: false, direction_policy: "any_rtl" },
};

export function SettingsModal(p: Props) {
  const [tab, setTab] = createSignal<Tab>("general");
  // Phase 87: canonical accelerators claimed by more than one action. The
  // dispatcher is first-match-wins, so a duplicate leaves the loser silently
  // dead — every member of a colliding group is flagged and the user picks
  // which one to move. Push-to-talk is passed in explicitly because it lives
  // under settings.stt, not settings.shortcuts.
  const shortcutConflicts = createMemo(() =>
    conflictingAccels(p.settings.shortcuts, {
      push_to_talk: p.settings.stt?.push_to_talk_hotkey,
    }),
  );
  // beta.3: sub-tab inside the "Hooks & Notifications" card.
  const [hnSubTab, setHnSubTab] = createSignal<HooksNotifSubTab>("hooks");
  // 2026-08-19: which RTL profile the controls below are editing. Local
  // (native Windows ConPTY) and remote (SSH/WSL, i.e. anything POSIX) were
  // measured to need OPPOSITE modes, so they are configured separately.
  const [rtlProfile, setRtlProfile] = createSignal<RtlProfileKind>("remote");
  const [presets, setPresets] = createSignal<PresetEntry[]>([]);
  const [fonts, setFonts] = createSignal<FontFamilies>({ ui: [], mono: [] });
  const [catalog, setCatalog] = createSignal<FontCatalogItem[]>([]);
  /** Catalog id currently downloading, or null. */
  const [fontBusy, setFontBusy] = createSignal<string | null>(null);
  const [fontNote, setFontNote] = createSignal<string | null>(null);

  /**
   * Install a catalog font, then re-read the picker so the row flips from
   * ⚠️ to ✅ without a restart. The Rust side registers under HKCU and
   * `list_system_fonts` reads that same hive, so the refresh is enough.
   */
  const installFont = async (item: FontCatalogItem) => {
    setFontBusy(item.id);
    setFontNote(null);
    try {
      const r = await fontInstall(item.id);
      if (r.guided) {
        // The silent path was refused (locked-down box). The font is NOT
        // installed yet — say so rather than showing a success message.
        setFontNote(t("settings.font.install.guided", { family: item.family }));
        log.warn("font install fell back to guided", r.fallback_reason);
      } else {
        setFontNote(
          t("settings.font.install.ok", {
            family: item.family,
            count: r.installed.length,
          }),
        );
      }
      setFonts(await listSystemFonts());
      // The catalog carries the installed flag, so it has to be re-read
      // too or the Remove row will not appear until Settings is reopened.
      setCatalog(await fontCatalog());
    } catch (e) {
      log.warn("fontInstall failed", e);
      setFontNote(t("settings.font.install.failed", { error: String(e) }));
    } finally {
      setFontBusy(null);
    }
  };

  /**
   * Remove a catalog font. Confirmed first: this deletes files, and the
   * only way back is a multi-MB download.
   *
   * A partial result is the expected outcome, not an error — Windows will
   * not delete a font file that a running application has open — so the
   * `failed` list gets its own message rather than being folded into the
   * success one.
   */
  const uninstallFont = async (item: FontCatalogItem) => {
    if (!window.confirm(t("settings.font.uninstall.confirm", { family: item.family }))) {
      return;
    }
    setFontBusy(item.id);
    setFontNote(null);
    try {
      const r = await fontUninstall(item.id);
      if (r.failed.length > 0) {
        setFontNote(
          t("settings.font.uninstall.partial", {
            family: item.family,
            count: r.removed.length,
            failed: r.failed.length,
          }),
        );
        log.warn("font uninstall left faces behind", r.failed);
      } else {
        setFontNote(
          t("settings.font.uninstall.ok", {
            family: item.family,
            count: r.removed.length,
          }),
        );
      }
      setFonts(await listSystemFonts());
      setCatalog(await fontCatalog());
    } catch (e) {
      log.warn("fontUninstall failed", e);
      setFontNote(t("settings.font.uninstall.failed", { error: String(e) }));
    } finally {
      setFontBusy(null);
    }
  };
  const [advanced, setAdvanced] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [lastSaved, setLastSaved] = createSignal<number>(0);
  const [updateInfo, setUpdateInfo] = createSignal<UpdateInfo | null>(null);
  const [checking, setChecking] = createSignal(false);
  // Phase 38/39: resolved debug.log path + "Copied" flash + live tail.
  const [logPath, setLogPath] = createSignal<string>("");
  const [logCopied, setLogCopied] = createSignal(false);
  const [logTail, setLogTail] = createSignal<string>("");
  // Unified logging: component filter ([HOOK] / [SRV:METRICS] / [SSH] / …).
  // "" = all. Filtering fetches a deeper tail so a sparse tag still shows
  // meaningful history.
  const [logFilter, setLogFilter] = createSignal<string>("");
  const refreshLogTail = async () => {
    try {
      setLogTail(
        await invoke<string>("read_log_tail", { n: logFilter() ? 2000 : 200 }),
      );
    } catch (e) {
      log.warn("read_log_tail failed", e);
    }
  };
  // Distinct component tags discovered in the fetched tail. Line shape:
  // `[ts] [LEVEL] [TAG] msg` — the tag is the third bracket group.
  const LOG_TAG_RE = /^\[[^\]]+\] \[[A-Z ]{5}\] \[([^\]]+)\]/;
  const logTags = createMemo<string[]>(() => {
    const tags = new Set<string>();
    for (const line of logTail().split("\n")) {
      const m = LOG_TAG_RE.exec(line);
      if (m) tags.add(m[1]);
    }
    return [...tags].sort();
  });
  const filteredLogTail = createMemo<string>(() => {
    const tag = logFilter();
    if (!tag) return logTail();
    const needle = `] [${tag}] `;
    return logTail()
      .split("\n")
      .filter((l) => l.includes(needle))
      .join("\n");
  });
  // Phase 75: clear the debug log now, then refresh the viewer.
  const clearLogs = async () => {
    try {
      await invoke("clear_debug_log_cmd");
      await refreshLogTail();
    } catch (e) {
      log.warn("clear_debug_log_cmd failed", e);
    }
  };
  // Phase 48-C: /doctor snapshot — paste-friendly JSON for bug reports.
  const [doctorJson, setDoctorJson] = createSignal<string>("");
  const runDoctor = async () => {
    try {
      const snapshot = await invoke<unknown>("doctor");
      setDoctorJson(JSON.stringify(snapshot, null, 2));
    } catch (e) {
      setDoctorJson(`error: ${String(e)}`);
    }
  };

  // Debounced save: live-preview every change locally, persist 500ms after
  // the last edit so a slider drag doesn't write 60 files/sec.
  let saveTimer: number | null = null;
  const queueSave = (next: Settings) => {
    p.onChange(next);
    applyTheme(next);
    applyI18nSettings(next.i18n);
    if (saveTimer) clearTimeout(saveTimer);
    setSaving(true);
    saveTimer = window.setTimeout(async () => {
      try {
        await saveSettings(next);
        setLastSaved(Date.now());
      } catch (e) {
        log.error("settings_save failed", e);
      } finally {
        setSaving(false);
      }
    }, 500);
  };

  /** Read one RTL field for the profile currently being edited, falling back
   *  to the deprecated flat field and then to that profile's own default —
   *  local and remote deliberately differ. */
  const rtlField = <K extends keyof RtlProfileFields>(k: K): RtlProfileFields[K] => {
    const prof = p.settings.terminal.rtl?.[rtlProfile()];
    const flat = (p.settings.terminal as Record<string, unknown>)[k as string] as
      | RtlProfileFields[K]
      | undefined;
    const fallback = RTL_FIELD_DEFAULTS[rtlProfile()][k];
    return (prof?.[k] ?? flat ?? fallback) as RtlProfileFields[K];
  };

  /**
   * Write one RTL field into the profile being edited, leaving the other
   * profile untouched — that separation is the whole point.
   *
   * 2026-08-19: writes the COMPLETE profile, not just the changed key.
   *
   * A partial object round-trips through Rust's `RtlProfile`, where every
   * field carries `#[serde(default)]` — so an absent key comes back as the
   * TYPE's default, not the PROFILE's. `tui_owns_bidi` defaults to false on
   * the type and true for local, so the first time Yossi touched any RTL
   * control the local switch silently turned itself off and stayed off. His
   * log: `setting=0`, with `explicit=1` right above it — the signal was
   * winning and this gate was quietly refusing.
   *
   * `rtlField` already resolves profile -> deprecated flat field ->
   * per-profile default, so seeding from it writes exactly what the UI is
   * currently showing. Nothing can be lost by omission any more.
   */
  const setRtlField = <K extends keyof RtlProfileFields>(k: K, v: RtlProfileFields[K]) => {
    const cur = p.settings.terminal.rtl ?? {};
    const kind = rtlProfile();
    const complete: Required<RtlProfileFields> = {
      rtl_mode: rtlField("rtl_mode") as Required<RtlProfileFields>["rtl_mode"],
      auto_direction: rtlField("auto_direction") as boolean,
      mirror_arrows_rtl: rtlField("mirror_arrows_rtl") as boolean,
      tui_owns_bidi: rtlField("tui_owns_bidi") as boolean,
      direction_policy: rtlField(
        "direction_policy",
      ) as Required<RtlProfileFields>["direction_policy"],
    };
    update("terminal", {
      ...p.settings.terminal,
      rtl: { ...cur, [kind]: { ...complete, [k]: v } },
    });
  };

  const update = <K extends keyof Settings>(k: K, v: Settings[K]) =>
    queueSave({ ...p.settings, [k]: v });

  const setTheme = (patch: Partial<Settings["theme"]>) => {
    const next = { ...p.settings, theme: { ...p.settings.theme, ...patch, preset: "custom" } };
    queueSave(next);
  };

  const setAnsi = (patch: Partial<Settings["theme"]["ansi"]>) =>
    setTheme({ ansi: { ...p.settings.theme.ansi, ...patch } });

  onMount(async () => {
    try { setPresets(await getPresets()); } catch (e) { log.warn("getPresets failed", e); }
    try { setFonts(await listSystemFonts()); } catch (e) { log.warn("listSystemFonts failed", e); }
    try { setCatalog(await fontCatalog()); } catch (e) { log.warn("fontCatalog failed", e); }
    // Phase 38: resolve the debug.log path for the Logs section.
    try { setLogPath(await invoke<string>("log_dir_path")); } catch (e) { log.warn("log_dir_path failed", e); }
  });

  // Phase 38: Logs section actions.
  const onOpenLogFolder = () => {
    if (!logPath()) return;
    void revealItemInDir(logPath()).catch((e) => log.warn("revealItemInDir failed", e));
  };
  const onCopyLogPath = async () => {
    if (!logPath()) return;
    try {
      await navigator.clipboard.writeText(logPath());
      setLogCopied(true);
      setTimeout(() => setLogCopied(false), 1500);
    } catch (e) {
      log.warn("clipboard write failed", e);
    }
  };

  // Phase 39: poll the log tail every 5s while the Logs tab is open;
  // stop when the user navigates away or closes the modal.
  createEffect(() => {
    if (!p.open || tab() !== "system") return;
    void refreshLogTail();
    const id = setInterval(() => void refreshLogTail(), 5000);
    onCleanup(() => clearInterval(id));
  });

  // Redesign pass 4: the four redesign directions ship light+dark variants
  // as separate presets in the engine, but surface as ONE card each — the
  // appearance toggle above picks which variant actually applies.
  const REDESIGN_BASES = ["industry", "broadsheet", "modernist", "classical"];
  const redesignBase = (id: string): string | null => {
    const base = id.replace(/-dark$/, "");
    return REDESIGN_BASES.includes(base) ? base : null;
  };
  const effectiveMode = () => resolveThemeMode(p.settings.theme_mode ?? "system");

  // Swatches preview the variant the toggle would actually apply.
  const cardTheme = (pr: PresetEntry) => {
    const base = redesignBase(pr.id);
    if (base && effectiveMode() === "dark") {
      const dark = presets().find((x) => x.id === `${base}-dark`);
      if (dark) return dark.theme;
    }
    return pr.theme;
  };

  const onPickPreset = async (id: string) => {
    const base = redesignBase(id);
    const resolved = base
      ? effectiveMode() === "dark"
        ? `${base}-dark`
        : base
      : id;
    try {
      const next = await applyPreset(resolved);
      p.onChange(next);
      applyTheme(next);
      setLastSaved(Date.now());
    } catch (e) {
      log.error("apply preset failed", e);
    }
  };

  // Mode click: persist immediately (not via the debounced queue) so the
  // follow-up preset-variant apply on the backend can't race it, then swap
  // an active redesign preset to the variant matching the new polarity.
  const onPickMode = async (m: NonNullable<Settings["theme_mode"]>) => {
    const next = { ...p.settings, theme_mode: m };
    p.onChange(next);
    applyTheme(next);
    try {
      await saveSettings(next);
      setLastSaved(Date.now());
    } catch (e) {
      log.error("settings_save failed", e);
      return;
    }
    const base = redesignBase(next.theme.preset);
    if (base) {
      const variant = resolveThemeMode(m) === "dark" ? `${base}-dark` : base;
      if (variant !== next.theme.preset) await onPickPreset(variant);
    }
  };

  const onResetAll = async () => {
    if (!window.confirm("Reset ALL settings to defaults?")) return;
    try {
      const next = await resetSettings();
      p.onChange(next);
      applyTheme(next);
      setLastSaved(Date.now());
    } catch (e) {
      log.error("reset failed", e);
    }
  };

  const onCheckUpdates = async () => {
    setChecking(true);
    try {
      const info = await checkForUpdates();
      setUpdateInfo(info);
      // v0.2.3: re-pull settings so the "Last check" line in this
      // modal reflects the timestamp the backend just wrote. Without
      // this, the modal shows the stale value it loaded on open.
      try {
        const fresh = await loadSettings();
        p.onChange(fresh);
      } catch (e) {
        log.warn("refresh settings after check failed", e);
      }
    } catch (e) {
      log.error("check updates failed", e);
    } finally {
      setChecking(false);
    }
  };

  const savedAge = createMemo(() => {
    if (saving()) return "saving…";
    if (!lastSaved()) return "";
    const sec = Math.floor((Date.now() - lastSaved()) / 1000);
    if (sec < 5) return t("settings.saved");
    return "";
  });

  return (
    <Show when={p.open}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div
          class="modal settings-modal"
          onClick={(e) => e.stopPropagation()}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <div class="settings-head">
            <h3>{t("settings.title")}</h3>
            <span class="settings-saved-flag">{savedAge()}</span>
            <button class="feed-x" title={t("common.close")} onClick={p.onClose}>×</button>
          </div>

          <div class="settings-body">
            <nav class="settings-tabs">
              <For each={["general", "textLocale", "appearance", "shortcuts", "agentNotif", "ai", "system"] as Tab[]}>
                {(name) => (
                  <button
                    class={`settings-tab ${tab() === name ? "active" : ""}`}
                    onClick={() => setTab(name)}
                  >
                    {t(`settings.tab.${name}`)}
                  </button>
                )}
              </For>
              <div class="settings-tabs-spacer" />
              <button class="settings-tab danger" onClick={onResetAll}>
                {t("settings.reset_all")}
              </button>
            </nav>

            <div class="settings-pane">
              {/* ── Theme ────────────────────────────────────────────── */}
              {/* Phase 49.A: General tab — workspace-lifecycle settings
                  (auto-destroy of empty workspaces). Kept separate from
                  the Terminal tab since these are not terminal-specific. */}
              <Show when={tab() === "general"}>
                <section>
                  <h4>{t("settings.tab.general")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.auto_connect_on_workspace_select !== false}
                      onChange={(e) => update("auto_connect_on_workspace_select", e.currentTarget.checked)}
                    />
                    <span>{t("settings.autoConnect.label")}</span>
                  </label>
                  {/* Phase 80: opt-in session restore. Off by default — it
                      makes startup reach for the network on its own. Hint text
                      lives in i18n (settings.restoreSessions.hint); inline hint
                      paragraphs were purged in the Phase F settings refactor. */}
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.restore_sessions_on_start === true}
                      onChange={(e) => update("restore_sessions_on_start", e.currentTarget.checked)}
                    />
                    <span>{t("settings.restoreSessions.label")}</span>
                  </label>
                  {/* Phase 80.1: file manager reopens where it was left. */}
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.file_manager_remember_path === true}
                      onChange={(e) => update("file_manager_remember_path", e.currentTarget.checked)}
                    />
                    <span>{t("settings.fmRememberPath.label")}</span>
                  </label>
                  {/* Unshipped-fivefer (#3): browser session persistence.
                      WebView2-only — WKWebView and WebKitGTK ignore the
                      profile-folder env var, so off Windows this would be a
                      switch that does nothing. Disable rather than lie. */}
                  <label class="settings-checkbox" title={isWindows() ? undefined : t("settings.windowsOnly")}>
                    <input
                      type="checkbox"
                      disabled={!isWindows()}
                      checked={isWindows() && p.settings.persist_browser_sessions !== false}
                      onChange={(e) => update("persist_browser_sessions", e.currentTarget.checked)}
                    />
                    <span>
                      {t("settings.persistBrowser.label")}
                      <Show when={!isWindows()}>
                        {" "}
                        <span class="nc-optional">{t("settings.windowsOnly")}</span>
                      </Show>
                    </span>
                  </label>
                  <label>
                    <span>{t("settings.autoDestroy.label")}</span>
                    <input
                      type="number"
                      min="1"
                      max="90"
                      placeholder={t("settings.autoDestroy.disabled")}
                      value={p.settings.auto_destroy_empty_workspaces_days ?? ""}
                      onInput={(e) => {
                        const raw = e.currentTarget.value.trim();
                        const n = raw === "" ? null : Math.min(90, Math.max(1, parseInt(raw, 10) || 0)) || null;
                        update("auto_destroy_empty_workspaces_days", n ?? undefined);
                      }}
                    />
                  </label>
                </section>
                {/* BRIEF: Briefing-card triggers. All opt-in (=== true).
                    Writes always carry the COMPLETE group object — the
                    setRtlField lesson: a partial sub-object makes Rust's
                    per-field serde defaults resurrect TYPE defaults, not
                    this group's. */}
                <section>
                  <h4>{t("settings.brief.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.brief?.entry_card_on_return === true}
                      onChange={(e) =>
                        update("brief", {
                          ...DEFAULT_BRIEF_SETTINGS,
                          ...p.settings.brief,
                          entry_card_on_return: e.currentTarget.checked,
                        })
                      }
                    />
                    <span>{t("settings.brief.entryOnReturn.label")}</span>
                  </label>
                  <label>
                    <span>{t("settings.brief.absenceMinutes.label")}</span>
                    <input
                      type="number"
                      min="1"
                      max="720"
                      value={p.settings.brief?.absence_minutes ?? DEFAULT_BRIEF_SETTINGS.absence_minutes}
                      onInput={(e) => {
                        const n = Math.min(720, Math.max(1, parseInt(e.currentTarget.value, 10)
                          || DEFAULT_BRIEF_SETTINGS.absence_minutes));
                        update("brief", {
                          ...DEFAULT_BRIEF_SETTINGS,
                          ...p.settings.brief,
                          absence_minutes: n,
                        });
                      }}
                    />
                  </label>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.brief?.entry_card_on_idle === true}
                      onChange={(e) =>
                        update("brief", {
                          ...DEFAULT_BRIEF_SETTINGS,
                          ...p.settings.brief,
                          entry_card_on_idle: e.currentTarget.checked,
                        })
                      }
                    />
                    <span>{t("settings.brief.entryOnIdle.label")}</span>
                  </label>
                  <label>
                    <span>{t("settings.brief.idleMinutes.label")}</span>
                    <input
                      type="number"
                      min="1"
                      max="720"
                      value={p.settings.brief?.idle_minutes ?? DEFAULT_BRIEF_SETTINGS.idle_minutes}
                      onInput={(e) => {
                        const n = Math.min(720, Math.max(1, parseInt(e.currentTarget.value, 10)
                          || DEFAULT_BRIEF_SETTINGS.idle_minutes));
                        update("brief", {
                          ...DEFAULT_BRIEF_SETTINGS,
                          ...p.settings.brief,
                          idle_minutes: n,
                        });
                      }}
                    />
                  </label>
                </section>
                <section>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.terminal.use_ymux_tmux_config ?? true}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, use_ymux_tmux_config: e.currentTarget.checked })}
                    />
                    <span>{t("settings.terminal.use_ymux_tmux_config.label")}</span>
                  </label>
                </section>
                {/* Phase 81: tmux session picker scope — shared (all
                    sessions on the server, the multi-machine default) vs
                    local (only sessions this machine created). */}
                <section>
                  <h4>{t("settings.sessions.visibility.label")}</h4>
                  <For each={[
                    ["shared", "settings.sessions.visibility.shared"],
                    ["local", "settings.sessions.visibility.local"],
                  ] as const}>
                    {([id, labelKey]) => (
                      <label class="settings-radio" style="grid-template-columns: none !important; display: flex !important; align-items: flex-start; gap: 8px;">
                        <input
                          type="radio"
                          name="session-visibility"
                          value={id}
                          checked={(p.settings.session_visibility ?? "shared") === id}
                          onChange={() => update("session_visibility", id)}
                        />
                        <span style="flex:1">{t(labelKey)}</span>
                      </label>
                    )}
                  </For>
                </section>
                <section>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.terminal.auto_reset_on_connect ?? true}
                      onChange={(e) => update("terminal", { ...p.settings.terminal, auto_reset_on_connect: e.currentTarget.checked })}
                    />
                    <span>{t("settings.terminal.auto_reset_on_connect.label")}</span>
                  </label>
                </section>
              </Show>

              <Show when={tab() === "textLocale"}>
                <section>
                  <h4>{t("settings.language.title")}</h4>
                  <label>
                    <span>{t("settings.language.label")}</span>
                    <select
                      value={p.settings.i18n.language}
                      onChange={(e) =>
                        update("i18n", { ...p.settings.i18n, language: e.currentTarget.value })
                      }
                    >
                      <For each={LANGUAGES}>
                        {(l) => <option value={l.id}>{l.label}</option>}
                      </For>
                    </select>
                  </label>
                  <label>
                    <span>{t("settings.language.direction")}</span>
                    <div class="settings-radio-row">
                      <For each={["auto", "ltr", "rtl"] as const}>
                        {(d) => (
                          <label class="settings-radio">
                            <input
                              type="radio"
                              name="dir"
                              value={d}
                              checked={p.settings.i18n.direction === d}
                              onChange={() =>
                                update("i18n", { ...p.settings.i18n, direction: d })
                              }
                            />
                            <span>{t(`settings.language.dir.${d}`)}</span>
                          </label>
                        )}
                      </For>
                    </div>
                  </label>
                </section>
                <section>
                  <h4>{t("settings.font.title")}</h4>
                  <label>
                    <span>{t("settings.font.ui")}</span>
                    <div style="display:flex; gap:8px; flex:1">
                      <select
                        style="flex:1"
                        value={p.settings.font.ui_family}
                        onChange={(e) => update("font", { ...p.settings.font, ui_family: e.currentTarget.value })}
                      >
                        <For each={fonts().ui}>
                          {(f) => (
                            <option value={f.name}>{fontLabel(f)}</option>
                          )}
                        </For>
                      </select>
                      <input
                        type="number"
                        style="width:70px"
                        min="8"
                        max="32"
                        value={p.settings.font.ui_size_pt}
                        onInput={(e) => {
                          const n = parseInt(e.currentTarget.value);
                          if (!Number.isNaN(n) && n >= 8 && n <= 32) {
                            update("font", { ...p.settings.font, ui_size_pt: n });
                          }
                        }}
                      />
                    </div>
                  </label>
                  <Show when={missingFamily(fonts().ui, p.settings.font.ui_family)}>
                    {(family) => (
                      <FontMissingNotice
                        family={family()}
                        catalog={catalog()}
                        busy={fontBusy() !== null}
                        onInstall={installFont}
                      />
                    )}
                  </Show>
                  <CustomFontField
                    label={t("settings.font.custom.ui")}
                    onApply={(family) =>
                      update("font", { ...p.settings.font, ui_family: family })
                    }
                  />
                  <label>
                    <span>{t("settings.font.terminal")}</span>
                    <div style="display:flex; gap:8px; flex:1">
                      <select
                        style="flex:1"
                        value={p.settings.font.terminal_family}
                        onChange={(e) => update("font", { ...p.settings.font, terminal_family: e.currentTarget.value })}
                      >
                        <For each={fonts().mono}>
                          {(f) => (
                            <option value={f.name}>{fontLabel(f)}</option>
                          )}
                        </For>
                      </select>
                      <input
                        type="number"
                        style="width:70px"
                        min="8"
                        max="32"
                        value={p.settings.font.terminal_size_pt}
                        onInput={(e) => {
                          const n = parseInt(e.currentTarget.value);
                          if (!Number.isNaN(n) && n >= 8 && n <= 32) {
                            update("font", { ...p.settings.font, terminal_size_pt: n });
                          }
                        }}
                      />
                    </div>
                  </label>
                  <Show when={missingFamily(fonts().mono, p.settings.font.terminal_family)}>
                    {(family) => (
                      <FontMissingNotice
                        family={family()}
                        catalog={catalog()}
                        busy={fontBusy() !== null}
                        onInstall={installFont}
                      />
                    )}
                  </Show>
                  <CustomFontField
                    label={t("settings.font.custom.terminal")}
                    onApply={(family) =>
                      update("font", { ...p.settings.font, terminal_family: family })
                    }
                  />
                  <FontInstalledList
                    catalog={catalog()}
                    busy={fontBusy()}
                    onUninstall={uninstallFont}
                  />
                  <Show when={fontNote()}>
                    {(note) => <p class="settings-hint">{note()}</p>}
                  </Show>
                  <label>
                    <span>{t("settings.font.web.url")}</span>
                    <input
                      type="text"
                      placeholder="https://fonts.googleapis.com/css2?family=Iosevka&display=swap"
                      value={p.settings.font.web_font_url ?? ""}
                      onChange={(e) =>
                        update("font", { ...p.settings.font, web_font_url: e.currentTarget.value || null })
                      }
                    />
                  </label>
                </section>
                <section>
                  <h4>{t("settings.terminal.rtl.title")}</h4>
                  {/* Reuses .settings-mode-toggle, the pill control already
                      used for the theme mode in this same modal. */}
                  <div class="settings-mode-toggle" style="margin-bottom:8px">
                    <For each={["local", "remote"] as const}>
                      {(k) => (
                        <button
                          class={`settings-mode-btn ${rtlProfile() === k ? "active" : ""}`}
                          onClick={() => setRtlProfile(k)}
                        >
                          {t(`settings.terminal.rtl.profile.${k}`)}
                        </button>
                      )}
                    </For>
                  </div>
                  <p class="settings-hint">{t(`settings.terminal.rtl.profile.${rtlProfile()}.hint`)}</p>
                  <For each={[
                    ["auto_per_line", "settings.terminal.rtl.auto.label", "settings.terminal.rtl.auto.desc"],
                    ["force_rtl", "settings.terminal.rtl.force.label", "settings.terminal.rtl.force.desc"],
                    ["bidi_reorder", "settings.terminal.rtl.bidi.label", "settings.terminal.rtl.bidi.desc"],
                    ["off", "settings.terminal.rtl.off.label", "settings.terminal.rtl.off.desc"],
                  ] as const}>
                    {([id, labelKey, descKey]) => (
                      <label class="settings-radio" style="grid-template-columns: none !important; display: flex !important; align-items: flex-start; gap: 8px;">
                        <input
                          type="radio"
                          name="rtl-mode"
                          value={id}
                          checked={rtlField("rtl_mode") === id}
                          onChange={() => setRtlField("rtl_mode", id)}
                        />
                        <span style="flex:1" title={t(descKey)}>
                          <strong>{t(labelKey)}</strong>
                        </span>
                      </label>
                    )}
                  </For>
                  <label class="settings-checkbox" style="margin-top:8px">
                    <input
                      type="checkbox"
                      checked={rtlField("auto_direction") as boolean}
                      onChange={(e) => setRtlField("auto_direction", e.currentTarget.checked)}
                    />
                    <span>{t("settings.terminal.auto_direction.label")}</span>
                  </label>
                  <label class="settings-checkbox" style="margin-top:8px">
                    <input
                      type="checkbox"
                      checked={rtlField("mirror_arrows_rtl") as boolean}
                      onChange={(e) => setRtlField("mirror_arrows_rtl", e.currentTarget.checked)}
                    />
                    <span>{t("settings.terminal.mirror_arrows_rtl.label")}</span>
                  </label>
                  <label class="settings-checkbox" style="margin-top:8px">
                    <input
                      type="checkbox"
                      checked={rtlField("tui_owns_bidi") as boolean}
                      onChange={(e) => setRtlField("tui_owns_bidi", e.currentTarget.checked)}
                    />
                    <span>{t("settings.terminal.tui_owns_bidi.label")}</span>
                  </label>
                  <p class="settings-hint">{t("settings.terminal.tui_owns_bidi.hint")}</p>
                  {/* 2026-08-19: the RTL_DOMINANCE vote, per profile and
                      opt-in. It exists because a TUI status bar is positional
                      and flipping its row mirrors a layout the TUI already
                      placed; it is OFF by default because, shipped globally,
                      it broke remote panes that read fine on the older rule. */}
                  <label class="settings-checkbox" style="margin-top:8px">
                    <input
                      type="checkbox"
                      checked={rtlField("direction_policy") === "tui_dominance"}
                      onChange={(e) =>
                        setRtlField(
                          "direction_policy",
                          e.currentTarget.checked ? "tui_dominance" : "any_rtl",
                        )
                      }
                    />
                    <span>{t("settings.terminal.direction_policy.label")}</span>
                  </label>
                  <p class="settings-hint">{t("settings.terminal.direction_policy.hint")}</p>
                </section>
              </Show>

              <Show when={tab() === "appearance"}>
                {/* Design Pass 01 (#2): dark/light/system appearance axis,
                    above the presets. Presets set colours; this sets polarity. */}
                <section>
                  <h4>{t("settings.theme.appearance")}</h4>
                  <div class="settings-mode-toggle">
                    <For each={["dark", "light", "system"] as const}>
                      {(m) => (
                        <button
                          class={`settings-mode-btn ${(p.settings.theme_mode ?? "system") === m ? "active" : ""}`}
                          onClick={() => void onPickMode(m)}
                        >
                          {m === "dark" ? "🌙 " : m === "light" ? "☀ " : "🖥 "}
                          {t(`settings.theme.mode.${m}`)}
                        </button>
                      )}
                    </For>
                  </div>
                </section>
                <section>
                  <h4>{t("settings.theme.preset")}</h4>
                  <div class="settings-preset-grid">
                    {/* Redesign pass 4: hide the redesign -dark twins — one
                        card per direction; the appearance toggle picks the
                        variant (and the swatches preview it live). */}
                    <For each={presets().filter((pr) => !(pr.id.endsWith("-dark") && redesignBase(pr.id)))}>
                      {(pr) => (
                        <button
                          class={`settings-preset-card ${
                            p.settings.theme.preset === pr.id ||
                            (redesignBase(pr.id) && p.settings.theme.preset === `${pr.id}-dark`)
                              ? "active"
                              : ""
                          }`}
                          onClick={() => onPickPreset(pr.id)}
                          title={pr.label}
                        >
                          <div
                            class="settings-preset-swatches"
                            style={{ background: cardTheme(pr).background }}
                          >
                            <span style={{ background: cardTheme(pr).surface }} />
                            <span style={{ background: cardTheme(pr).accent }} />
                            <span style={{ background: cardTheme(pr).success }} />
                            <span style={{ background: cardTheme(pr).warning }} />
                            <span style={{ background: cardTheme(pr).error }} />
                          </div>
                          <span class="settings-preset-label">{pr.label}</span>
                        </button>
                      )}
                    </For>
                  </div>
                </section>
                <section>
                  <h4>{t("settings.theme.base_colors")}</h4>
                  <div class="settings-color-grid">
                    <ColorRow label="Accent" value={p.settings.theme.accent} onInput={(v) => setTheme({ accent: v })} />
                    <ColorRow label="Background" value={p.settings.theme.background} onInput={(v) => setTheme({ background: v })} />
                    <ColorRow label="Surface" value={p.settings.theme.surface} onInput={(v) => setTheme({ surface: v })} />
                    <ColorRow label="Border" value={p.settings.theme.border} onInput={(v) => setTheme({ border: v })} />
                    <ColorRow label="Text primary" value={p.settings.theme.text_primary} onInput={(v) => setTheme({ text_primary: v })} />
                    <ColorRow label="Text secondary" value={p.settings.theme.text_secondary} onInput={(v) => setTheme({ text_secondary: v })} />
                    <ColorRow label="Success" value={p.settings.theme.success} onInput={(v) => setTheme({ success: v })} />
                    <ColorRow label="Warning" value={p.settings.theme.warning} onInput={(v) => setTheme({ warning: v })} />
                    <ColorRow label="Error" value={p.settings.theme.error} onInput={(v) => setTheme({ error: v })} />
                  </div>
                </section>
                <section>
                  <h4>
                    <button class="settings-disclose" onClick={() => setAdvanced(!advanced())}>
                      {advanced() ? <IconChevronDown size={13} /> : <IconChevronRight size={13} />} ANSI palette (xterm 16)
                    </button>
                  </h4>
                  <Show when={advanced()}>
                    <div class="settings-color-grid">
                      <For each={Object.keys(p.settings.theme.ansi) as (keyof Settings["theme"]["ansi"])[]}>
                        {(k) => (
                          <ColorRow
                            label={k.replace(/_/g, " ")}
                            value={p.settings.theme.ansi[k]}
                            onInput={(v) => setAnsi({ [k]: v } as any)}
                          />
                        )}
                      </For>
                    </div>
                  </Show>
                </section>
              </Show>

              <Show when={tab() === "shortcuts"}>
                <section>
                  <h4>{t("settings.shortcuts.title")}</h4>
                  <p class="settings-hint">{t("settings.shortcuts.hint")}</p>
                  <For each={SHORTCUT_GROUPS}>
                    {(group) => (
                      <>
                        <h5 class="settings-shortcut-group">
                          {t(`settings.shortcuts.group.${group.key}`)}
                        </h5>
                        <For each={group.ids}>
                          {(key) => (
                            <ShortcutRow
                              label={t(`settings.shortcuts.${key}`)}
                              value={(p.settings.shortcuts ?? DEFAULT_SHORTCUTS)[key]}
                              defaultValue={DEFAULT_SHORTCUTS[key]}
                              conflict={shortcutConflicts().has(
                                canonicalAccel((p.settings.shortcuts ?? DEFAULT_SHORTCUTS)[key]) ?? "",
                              )}
                              onChange={(v) =>
                                update("shortcuts", {
                                  ...(p.settings.shortcuts ?? DEFAULT_SHORTCUTS),
                                  [key]: v,
                                } as Settings["shortcuts"])
                              }
                            />
                          )}
                        </For>
                      </>
                    )}
                  </For>
                  <label class="settings-checkbox" style="margin-top: 12px;">
                    <input
                      type="checkbox"
                      checked={(p.settings.shortcuts ?? DEFAULT_SHORTCUTS).copy_on_select_with_ctrl_c}
                      onChange={(e) =>
                        update("shortcuts", {
                          ...(p.settings.shortcuts ?? DEFAULT_SHORTCUTS),
                          copy_on_select_with_ctrl_c: e.currentTarget.checked,
                        } as Settings["shortcuts"])
                      }
                    />
                    <span>{t("settings.shortcuts.ctrl_c_copy")}</span>
                  </label>

                  {/* Phase 87: bindings that are NOT rebindable, listed so they
                      stop being invisible. Ctrl+1..9 is a numeric family (and
                      9 means "last"), Escape is a contextual dismissal, and the
                      editor / browser ones are scoped to a focused component
                      and never reach the global handler. */}
                  <h5 class="settings-shortcut-group">{t("settings.shortcuts.fixed.title")}</h5>
                  <p class="settings-hint">{t("settings.shortcuts.fixed.hint")}</p>
                  <For each={[
                    ["Ctrl+1 … Ctrl+9", "settings.shortcuts.fixed.tab_jump"],
                    ["Escape", "settings.shortcuts.fixed.escape"],
                    ["Ctrl+S / Ctrl+F / Ctrl+H", "settings.shortcuts.fixed.editor"],
                    ["Ctrl+T / Ctrl+W / Ctrl+Tab", "settings.shortcuts.fixed.browser"],
                  ] as const}>
                    {([accel, labelKey]) => (
                      <div class="settings-shortcut-row">
                        <span class="settings-shortcut-label">{t(labelKey)}</span>
                        <span class="settings-shortcut-fixed">{accel}</span>
                        <span />
                      </div>
                    )}
                  </For>
                </section>
              </Show>

              <Show when={tab() === "agentNotif"}>
                <section>
                  <h4>{t("settings.hooks.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.hooks.policy_enabled ?? true}
                      onChange={(e) => update("hooks", { ...p.settings.hooks, policy_enabled: e.currentTarget.checked })}
                    />
                    <span>{t("settings.hooks.policy_enabled")}</span>
                  </label>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.hooks.auto_install ?? true}
                      onChange={(e) => update("hooks", { ...p.settings.hooks, auto_install: e.currentTarget.checked })}
                    />
                    <span>{t("settings.hooks.auto_install")}</span>
                  </label>
                  {/* Phase 66.F: user-editable policy lists. One pattern per
                      line; blanks dropped on blur. Enforced desktop-side by
                      the feed.push engine (the CLI static fallback keeps the
                      built-ins). dir=ltr — patterns are shell text. */}
                  <label class="modal-textarea-label">
                    <span>{t("settings.hooks.custom_block")}</span>
                    <textarea
                      rows="4"
                      dir="ltr"
                      placeholder={"npm publish\nterraform destroy"}
                      value={(p.settings.hooks.custom_block ?? []).join("\n")}
                      onChange={(e) =>
                        update("hooks", {
                          ...p.settings.hooks,
                          custom_block: e.currentTarget.value
                            .split("\n")
                            .map((s) => s.trim())
                            .filter((s) => s.length > 0),
                        })
                      }
                    />
                  </label>
                  <label class="modal-textarea-label">
                    <span>{t("settings.hooks.custom_gate")}</span>
                    <textarea
                      rows="4"
                      dir="ltr"
                      placeholder={"kubectl delete\ngit rebase"}
                      value={(p.settings.hooks.custom_gate ?? []).join("\n")}
                      onChange={(e) =>
                        update("hooks", {
                          ...p.settings.hooks,
                          custom_gate: e.currentTarget.value
                            .split("\n")
                            .map((s) => s.trim())
                            .filter((s) => s.length > 0),
                        })
                      }
                    />
                  </label>
                </section>
                <section>
                  <h4>{t("settings.notifications.toasts.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.notifications.toast_enabled}
                      onChange={(e) => update("notifications", { ...p.settings.notifications, toast_enabled: e.currentTarget.checked })}
                    />
                    <span>Show OS toast notifications (workspace events, updates)</span>
                  </label>
                  {/* cmux-A A1: pane pulse on OSC 9/99/777 activity. */}
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.notifications.pane_pulse_on_activity ?? true}
                      onChange={(e) => update("notifications", { ...p.settings.notifications, pane_pulse_on_activity: e.currentTarget.checked })}
                    />
                    <span>{t("notifications.pane_pulse_label")}</span>
                  </label>
                </section>
                {(() => {
                  const getHN = (): HookNotificationSettings =>
                    p.settings.hook_notifications ?? DEFAULT_HOOK_NOTIFICATIONS;
                  const isEnabled = (ty: HookType) =>
                    getHN().enabled_types.includes(ty);
                  const isSound = (ty: HookType) => getHN().sound_types.includes(ty);
                  const toggleEnabled = (ty: HookType, next: boolean) => {
                    const cur = getHN();
                    const set = new Set(cur.enabled_types);
                    if (next) set.add(ty);
                    else set.delete(ty);
                    // Turning a hook OFF also drops it from sound_types — a
                    // sound-only entry with no processing would never fire.
                    const soundSet = new Set(cur.sound_types);
                    if (!next) soundSet.delete(ty);
                    update("hook_notifications", {
                      ...cur,
                      enabled_types: [...set],
                      sound_types: [...soundSet],
                    });
                  };
                  const toggleSound = (ty: HookType, next: boolean) => {
                    const cur = getHN();
                    const set = new Set(cur.sound_types);
                    if (next) set.add(ty);
                    else set.delete(ty);
                    update("hook_notifications", {
                      ...cur,
                      sound_types: [...set],
                    });
                  };
                  const category = (ty: HookType): "blocking" | "passive" =>
                    ty === "pre-tool-use" ? "blocking" : "passive";
                  const rowLabel = (ty: HookType) =>
                    t(`hooksNotif.type.${ty}.label`);
                  const rowHint = (ty: HookType) => t(`hooksNotif.type.${ty}.hint`);

                  const HookRow = (
                    ty: HookType,
                    mode: "enable" | "sound",
                  ) => {
                    const checked = mode === "enable" ? isEnabled(ty) : isSound(ty);
                    const parentOff = mode === "sound" && !isEnabled(ty);
                    const masterOff =
                      mode === "sound" && !getHN().sound_master;
                    const disabled = parentOff || masterOff;
                    const onChange = mode === "enable" ? toggleEnabled : toggleSound;
                    return (
                      <label
                        class={`settings-checkbox hooksNotif-row ${
                          disabled ? "hooksNotif-row-dim" : ""
                        }`}
                      >
                        <input
                          type="checkbox"
                          checked={checked && !disabled}
                          disabled={disabled}
                          onChange={(e) => onChange(ty, e.currentTarget.checked)}
                        />
                        <span class="hooksNotif-row-label">
                          <span class="hooksNotif-row-name">{rowLabel(ty)}</span>
                          <span class="hooksNotif-row-hint">{rowHint(ty)}</span>
                        </span>
                        <span
                          class={`hooksNotif-badge hooksNotif-badge-${category(ty)}`}
                        >
                          {category(ty) === "blocking"
                            ? t("hooksNotif.badge.blocking")
                            : t("hooksNotif.badge.passive")}
                        </span>
                        {ty === "session-start" && (
                          <span class="hooksNotif-deprecated">
                            {t("hooksNotif.deprecated")}
                          </span>
                        )}
                      </label>
                    );
                  };
                  return (
                    <section>
                      <h4>{t("hooksNotif.title")}</h4>
                      <div class="hooksNotif-tabs">
                        <button
                          class={`hooksNotif-tab ${hnSubTab() === "hooks" ? "active" : ""}`}
                          onClick={() => setHnSubTab("hooks")}
                        >
                          {t("hooksNotif.tab.hooks")}
                        </button>
                        <button
                          class={`hooksNotif-tab ${hnSubTab() === "sound" ? "active" : ""}`}
                          onClick={() => setHnSubTab("sound")}
                        >
                          {t("hooksNotif.tab.sound")}
                        </button>
                      </div>

                      <Show when={hnSubTab() === "hooks"}>
                        <div class="hooksNotif-group">
                          <h5 class="hooksNotif-group-head">
                            {t("hooksNotif.group.interactive")}
                          </h5>
                          <For each={INTERACTIVE_HOOKS}>
                            {(ty) => HookRow(ty, "enable")}
                          </For>
                        </div>
                        <div class="hooksNotif-group">
                          <h5 class="hooksNotif-group-head">
                            {t("hooksNotif.group.observability")}
                          </h5>
                          <For each={OBSERVABILITY_HOOKS}>
                            {(ty) => HookRow(ty, "enable")}
                          </For>
                        </div>
                      </Show>

                      <Show when={hnSubTab() === "sound"}>
                        <label class="settings-checkbox">
                          <input
                            type="checkbox"
                            checked={getHN().sound_master}
                            onChange={(e) =>
                              update("hook_notifications", {
                                ...getHN(),
                                sound_master: e.currentTarget.checked,
                              })
                            }
                          />
                          <span>{t("hooksNotif.sound_master")}</span>
                        </label>
                        <div class="hooksNotif-group">
                          <h5 class="hooksNotif-group-head">
                            {t("hooksNotif.group.interactive")}
                          </h5>
                          <For each={INTERACTIVE_HOOKS}>
                            {(ty) => HookRow(ty, "sound")}
                          </For>
                        </div>
                        <div class="hooksNotif-group">
                          <h5 class="hooksNotif-group-head">
                            {t("hooksNotif.group.observability")}
                          </h5>
                          <For each={OBSERVABILITY_HOOKS}>
                            {(ty) => HookRow(ty, "sound")}
                          </For>
                        </div>
                      </Show>
                    </section>
                  );
                })()}
              </Show>

              <Show when={tab() === "ai"}>
                <section>
                  <h4>{t("settings.claude.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS).auto_summarize_on_stop}
                      onChange={(e) =>
                        update("claude", {
                          ...(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS),
                          auto_summarize_on_stop: e.currentTarget.checked,
                        } as Settings["claude"])
                      }
                    />
                    <span>{t("settings.claude.auto_on_stop")}</span>
                  </label>
                  <label>
                    <span>{t("settings.claude.history_count")}</span>
                    <input
                      type="number"
                      min="5"
                      max="50"
                      value={(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS).summary_history_count}
                      onChange={(e) =>
                        update("claude", {
                          ...(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS),
                          summary_history_count:
                            Math.max(5, Math.min(50, parseInt(e.currentTarget.value) || 10)),
                        } as Settings["claude"])
                      }
                    />
                  </label>
                  <label class="modal-textarea-label">
                    <span>{t("settings.claude.summary_prompt")}</span>
                    <textarea
                      rows="3"
                      value={(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS).summary_prompt}
                      onChange={(e) =>
                        update("claude", {
                          ...(p.settings.claude ?? DEFAULT_CLAUDE_SETTINGS),
                          summary_prompt: e.currentTarget.value,
                        } as Settings["claude"])
                      }
                    />
                  </label>

                  {/* Phase 78: usage-indicator display + auto-refresh (one row). */}
                  <h4 style="margin-top:18px">{t("claudeUsage.settings.title")}</h4>
                  <div class="claude-usage-settings-row">
                    <label class="settings-checkbox">
                      <input
                        type="checkbox"
                        checked={(p.settings.claude_usage ?? DEFAULT_CLAUDE_USAGE_SETTINGS).show_top_indicator}
                        onChange={(e) =>
                          update("claude_usage", {
                            ...(p.settings.claude_usage ?? DEFAULT_CLAUDE_USAGE_SETTINGS),
                            show_top_indicator: e.currentTarget.checked,
                          } as Settings["claude_usage"])
                        }
                      />
                      <span>{t("claudeUsage.settings.show")}</span>
                    </label>
                    <select
                      value={(p.settings.claude_usage ?? DEFAULT_CLAUDE_USAGE_SETTINGS).display_mode}
                      onChange={(e) =>
                        update("claude_usage", {
                          ...(p.settings.claude_usage ?? DEFAULT_CLAUDE_USAGE_SETTINGS),
                          display_mode: e.currentTarget.value,
                        } as Settings["claude_usage"])
                      }
                    >
                      <option value="percent">{t("claudeUsage.settings.modePercent")}</option>
                      <option value="bar">{t("claudeUsage.settings.modeBar")}</option>
                    </select>
                    <label class="claude-usage-refresh">
                      <span>{t("claudeUsage.settings.autoRefresh")}</span>
                      <input
                        type="number"
                        min="0"
                        max="120"
                        value={(p.settings.claude_usage ?? DEFAULT_CLAUDE_USAGE_SETTINGS).auto_refresh_minutes}
                        onChange={(e) =>
                          update("claude_usage", {
                            ...(p.settings.claude_usage ?? DEFAULT_CLAUDE_USAGE_SETTINGS),
                            auto_refresh_minutes: Math.max(0, Math.min(120, parseInt(e.currentTarget.value) || 0)),
                          } as Settings["claude_usage"])
                        }
                      />
                    </label>
                  </div>
                </section>
                <section>
                  <h4>{t("settings.stt.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.stt?.enabled ?? false}
                      onChange={(e) =>
                        update("stt", {
                          ...(p.settings.stt ?? {
                            enabled: false,
                            backend: "webspeech",
                            local_endpoint: null,
                            language: "auto",
                            push_to_talk_hotkey: "Ctrl+Shift+M",
                          }),
                          enabled: e.currentTarget.checked,
                        })
                      }
                    />
                    <span>{t("settings.stt.enable")}</span>
                  </label>
                  <label>
                    <span>{t("settings.stt.backend.label")}</span>
                    <select
                      value={p.settings.stt?.backend ?? "webspeech"}
                      onChange={(e) =>
                        update("stt", {
                          ...(p.settings.stt ?? {
                            enabled: false,
                            backend: "webspeech",
                            local_endpoint: null,
                            language: "auto",
                            push_to_talk_hotkey: "Ctrl+Shift+M",
                          }),
                          backend: e.currentTarget.value as "webspeech" | "local",
                        })
                      }
                    >
                      <option value="webspeech">{t("settings.stt.backend.webspeech")}</option>
                      <option value="local">{t("settings.stt.backend.local")}</option>
                    </select>
                  </label>
                  <Show when={(p.settings.stt?.backend ?? "webspeech") === "local"}>
                    <label>
                      <span>{t("settings.stt.endpoint.label")}</span>
                      <input
                        type="text"
                        value={p.settings.stt?.local_endpoint ?? ""}
                        placeholder={t("settings.stt.endpoint.placeholder")}
                        onInput={(e) =>
                          update("stt", {
                            ...(p.settings.stt ?? {
                              enabled: false,
                              backend: "local",
                              local_endpoint: null,
                              language: "auto",
                              push_to_talk_hotkey: "Ctrl+Shift+M",
                            }),
                            local_endpoint:
                              e.currentTarget.value.trim() === ""
                                ? null
                                : e.currentTarget.value,
                          })
                        }
                      />
                    </label>
                  </Show>
                  <label>
                    <span>{t("settings.stt.language.label")}</span>
                    <select
                      value={p.settings.stt?.language ?? "auto"}
                      onChange={(e) =>
                        update("stt", {
                          ...(p.settings.stt ?? {
                            enabled: false,
                            backend: "webspeech",
                            local_endpoint: null,
                            language: "auto",
                            push_to_talk_hotkey: "Ctrl+Shift+M",
                          }),
                          language: e.currentTarget.value,
                        })
                      }
                    >
                      <option value="auto">{t("settings.stt.language.auto")}</option>
                      <option value="he-IL">עברית</option>
                      <option value="en-US">English (US)</option>
                      <option value="ar-SA">العربية</option>
                      <option value="ru-RU">Русский</option>
                    </select>
                  </label>
                  {/* Phase 87: this was a free-text box — the accelerator had
                      to be TYPED, and a typo produced a hotkey that silently
                      never fired. Same click-to-record picker as the Shortcuts
                      tab now. The binding still lives in settings.stt rather
                      than settings.shortcuts, but conflictingAccels() is fed
                      it so a clash with a table binding is still flagged. */}
                  <ShortcutRow
                    label={t("settings.stt.hotkey.label")}
                    value={p.settings.stt?.push_to_talk_hotkey ?? "Ctrl+Shift+M"}
                    defaultValue="Ctrl+Shift+M"
                    conflict={shortcutConflicts().has(
                      canonicalAccel(p.settings.stt?.push_to_talk_hotkey ?? "Ctrl+Shift+M") ?? "",
                    )}
                    onChange={(v) =>
                      update("stt", {
                        ...(p.settings.stt ?? {
                          enabled: false,
                          backend: "webspeech",
                          local_endpoint: null,
                          language: "auto",
                          push_to_talk_hotkey: "Ctrl+Shift+M",
                        }),
                        push_to_talk_hotkey: v,
                      })
                    }
                  />
                </section>
              </Show>

              <Show when={tab() === "system"}>
                <section>
                  <h4>{t("settings.updates.title")}</h4>
                  <label class="settings-checkbox">
                    <input
                      type="checkbox"
                      checked={p.settings.updates.check_on_startup}
                      onChange={(e) => update("updates", { ...p.settings.updates, check_on_startup: e.currentTarget.checked })}
                    />
                    <span>{t("settings.updates.check_on_startup")}</span>
                  </label>
                  <label>
                    <span>{t("settings.updates.manifest_url")}</span>
                    <input
                      type="text"
                      value={p.settings.updates.manifest_url ?? ""}
                      onChange={(e) => update("updates", { ...p.settings.updates, manifest_url: e.currentTarget.value || null })}
                    />
                  </label>
                  <button class="primary" disabled={checking()} onClick={onCheckUpdates}>
                    {checking() ? "Checking…" : "Check now"}
                  </button>
                  <Show when={updateInfo()}>
                    <div class="settings-update-result">
                      <p>
                        {t("settings.updates.current")} <code>{updateInfo()!.current_version}</code>
                        {" · "}{t("settings.updates.latest")} <code>{updateInfo()!.latest_version ?? "—"}</code>
                      </p>
                      <Show when={updateInfo()!.error}>
                        <p class="settings-update-err">{t("settings.updates.error", { msg: updateInfo()!.error ?? "" })}</p>
                      </Show>
                      <Show when={updateInfo()!.available}>
                        <p class="settings-update-ok">{t("settings.updates.available")}</p>
                      </Show>
                    </div>
                  </Show>

                  {/* Phase 71: version history + install/downgrade + channel. */}
                  <hr class="modal-sep" />
                  <h4>{t("vm.history")}</h4>
                  <VersionManager
                    channel={p.settings.updates.channel}
                    onSetChannel={(c) => update("updates", { ...p.settings.updates, channel: c })}
                    skipped={p.settings.updates.skipped_versions}
                    onUnskip={(v) =>
                      update("updates", {
                        ...p.settings.updates,
                        skipped_versions: p.settings.updates.skipped_versions.filter((x) => x !== v),
                      })
                    }
                  />
                </section>
                <section>
                  <h4>{t("settings.logs.recent")}</h4>
                  {/* Component filter — tags discovered from the tail itself. */}
                  <div class="settings-logs-row">
                    <span class="settings-logs-label">{t("settings.logs.filter")}</span>
                    <select
                      value={logFilter()}
                      onChange={(e) => {
                        setLogFilter(e.currentTarget.value);
                        void refreshLogTail();
                      }}
                    >
                      <option value="">{t("settings.logs.filterAll")}</option>
                      <For each={logTags()}>
                        {(tag) => <option value={tag}>{tag}</option>}
                      </For>
                    </select>
                  </div>
                  <pre class="settings-logs-viewer">{filteredLogTail()}</pre>
                  <div class="settings-logs-actions">
                    <button onClick={() => void refreshLogTail()}>
                      {t("settings.logs.refresh")}
                    </button>
                  </div>
                  <hr class="modal-sep" />
                  <div class="settings-logs-row">
                    <span class="settings-logs-label">{t("settings.updates.logs.path")}</span>
                    <code class="settings-logs-path">{logPath()}</code>
                  </div>
                  <div class="settings-logs-actions">
                    <button onClick={onOpenLogFolder} disabled={!logPath()}>
                      {t("settings.updates.logs.openFolder")}
                    </button>
                    <button onClick={() => void onCopyLogPath()} disabled={!logPath()}>
                      {logCopied() ? t("settings.updates.logs.copied") : t("settings.updates.logs.copyPath")}
                    </button>
                  </div>
                  {/* Unified logging: level threshold + remote sync. */}
                  <hr class="modal-sep" />
                  <div class="settings-logs-row">
                    <span class="settings-logs-label">{t("settings.logs.level")}</span>
                    <select
                      value={p.settings.logs?.level ?? "info"}
                      onChange={(e) =>
                        update("logs", {
                          ...(p.settings.logs ?? DEFAULT_LOGS_SETTINGS),
                          level: e.currentTarget.value === "debug" ? "debug" : "info",
                        })
                      }
                    >
                      <option value="info">{t("settings.logs.levelInfo")}</option>
                      <option value="debug">{t("settings.logs.levelDebug")}</option>
                    </select>
                  </div>
                  <div class="settings-logs-row">
                    <label>
                      <input
                        type="checkbox"
                        checked={p.settings.logs?.remote_sync ?? true}
                        onChange={(e) =>
                          update("logs", {
                            ...(p.settings.logs ?? DEFAULT_LOGS_SETTINGS),
                            remote_sync: e.currentTarget.checked,
                          })
                        }
                      />{" "}
                      {t("settings.logs.remoteSync")}
                    </label>
                  </div>
                  {/* Phase 75: retention + clear. */}
                  <hr class="modal-sep" />
                  <div class="settings-logs-row">
                    <span class="settings-logs-label">{t("settings.logs.retention")}</span>
                    <input
                      type="number"
                      min="0"
                      max="365"
                      class="settings-logs-retention"
                      value={p.settings.logs?.retention_days ?? 7}
                      onChange={(e) =>
                        update("logs", {
                          ...(p.settings.logs ?? DEFAULT_LOGS_SETTINGS),
                          retention_days: Math.max(0, Math.min(365, parseInt(e.currentTarget.value || "0", 10) || 0)),
                        })
                      }
                    />
                  </div>
                  <div class="settings-logs-actions">
                    <button onClick={() => void clearLogs()}>{t("settings.logs.clear")}</button>
                  </div>
                  {/* Phase 48-C: /doctor diagnostic snapshot for bug reports. */}
                  <hr class="modal-sep" />
                  <div class="settings-logs-actions">
                    <button onClick={() => void runDoctor()}>Run Doctor</button>
                  </div>
                  <Show when={doctorJson()}>
                    <pre class="settings-logs-viewer">{doctorJson()}</pre>
                  </Show>
                </section>
                <AddonsTab workspaceId={p.activeWorkspaceId} />
                <YmuxToolsTab workspaceId={p.activeWorkspaceId} />
              </Show>
            </div>
          </div>
        </div>
      </div>
    </Show>
  );
}

function ColorRow(p: { label: string; value: string; onInput: (v: string) => void }) {
  return (
    <div class="settings-color-row">
      <input
        type="color"
        value={p.value}
        onInput={(e) => p.onInput(e.currentTarget.value)}
      />
      <input
        type="text"
        class="settings-color-text"
        value={p.value}
        onInput={(e) => p.onInput(e.currentTarget.value)}
      />
      <span>{p.label}</span>
    </div>
  );
}

function ShortcutRow(p: {
  label: string;
  value: string;
  defaultValue: string;
  onChange: (v: string) => void;
  /** True when another action is bound to the same combination. */
  conflict?: boolean;
}) {
  const [recording, setRecording] = createSignal(false);
  return (
    <div class="settings-shortcut-row">
      <span class="settings-shortcut-label">{p.label}</span>
      <input
        type="text"
        classList={{
          "settings-shortcut-input": true,
          "settings-shortcut-input--conflict": !!p.conflict && !recording(),
        }}
        title={p.conflict ? t("settings.shortcuts.conflict") : undefined}
        value={recording() ? t("settings.shortcuts.recording") : p.value}
        readOnly
        onFocus={() => setRecording(true)}
        onBlur={() => setRecording(false)}
        onKeyDown={(e) => {
          if (!recording()) return;
          // Phase 87: the window keydown listener in App.tsx is bubble-phase,
          // so without this the combination being RECORDED also fires the
          // action it is bound to — which since the shortcut table grew means
          // recording Ctrl+Shift+W closes the active pane and Ctrl+Enter
          // maximizes it. preventDefault alone does not stop propagation.
          e.stopPropagation();
          // Esc cancels the recording without committing.
          if (e.key === "Escape") {
            e.preventDefault();
            (e.currentTarget as HTMLInputElement).blur();
            return;
          }
          const formatted = formatEvent(e);
          if (formatted) {
            e.preventDefault();
            p.onChange(formatted);
            (e.currentTarget as HTMLInputElement).blur();
          }
        }}
      />
      <button
        class="settings-shortcut-reset"
        type="button"
        title={t("common.reset")}
        onClick={() => p.onChange(p.defaultValue)}
      >
        <IconRefreshCcw />
      </button>
    </div>
  );
}
