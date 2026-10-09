import { createSignal, For, Show, onMount } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { isWindows } from "./platform";
import { createLogger } from "./logger";

const log = createLogger("WORKSPACE");
import type { CreateWorkspaceInput, EnvVar } from "./types";
import { WorkspaceExtrasFields } from "./WorkspaceExtrasFields";

// Phase 80 (unified setup wizard): "local → existing shell" — a plain
// cmd/PowerShell/Git-Bash/WSL workspace, exactly the local create wizard
// that used to live on CreateWorkspaceModal's Local tab. No smart setup;
// the smart-install sibling path is LocalSetupFlow ("local → new").

// Phase 13.A wizard data shapes — mirror the Rust definitions in
// src-tauri/src/local_wizard.rs.
interface ShellInfo {
  id: string;
  label: string;
  command: string;
  available: boolean;
}

interface RecentPathSuggestion {
  path: string;
  kind: "recent" | "default";
}

interface Props {
  onCreate: (input: CreateWorkspaceInput) => void;
  onClose: () => void;
}

export function QuickLocalCreateFlow(p: Props) {
  const [name, setName] = createSignal("");
  const [color, setColor] = createSignal("#7aa2f7");
  const [setupCmd, setSetupCmd] = createSignal("");
  const [teardownCmd, setTeardownCmd] = createSignal("");
  const [envRows, setEnvRows] = createSignal<EnvVar[]>([]);

  // Phase 12.C local PTY mini-wizard state. `wizardMode` toggles between
  // "wizard" (shell dropdown + cwd combobox seeded from history) and
  // "custom" (a single free-text command field). The cwd signal is
  // separate from `shell` so the wizard preserves the user's directory
  // when they flip between shells.
  const [wizardMode, setWizardMode] = createSignal<"wizard" | "custom">("wizard");
  const [shellId, setShellId] = createSignal<string>("powershell");
  const [shell, setShell] = createSignal("");
  const [cwd, setCwd] = createSignal<string>("");
  const [detectedShells, setDetectedShells] = createSignal<ShellInfo[]>([]);
  const [recentPaths, setRecentPaths] = createSignal<RecentPathSuggestion[]>([]);

  onMount(() => {
    void (async () => {
      try {
        const shells = await backend.call<ShellInfo[]>("detect_local_shells");
        setDetectedShells(shells);
        // macOS port: the "powershell" seed only exists on Windows. If the
        // seeded id isn't in the detected list, default to the first
        // available shell (the Rust side hoists $SHELL to the top).
        if (!shells.some((s) => s.id === shellId() && s.available)) {
          const first = shells.find((s) => s.available) ?? shells[0];
          if (first) setShellId(first.id);
        }
      } catch (e) {
        log.warn("detect_local_shells failed", e);
      }
      try {
        const paths = await backend.call<RecentPathSuggestion[]>("list_recent_paths");
        setRecentPaths(paths);
      } catch (e) {
        log.warn("list_recent_paths failed", e);
      }
    })();
  });

  // t() returns the key itself when there's no entry — zsh/bash/fish
  // are proper nouns with no i18n row, so fall back to the Rust label.
  const shellLabel = (s: ShellInfo): string => {
    const key = `ws.create.shell.${s.id}`;
    const tr = t(key);
    return tr === key ? s.label : tr;
  };

  const cleanedEnv = (): EnvVar[] =>
    envRows().filter((r) => r.key.trim() !== "");

  const submit = () => {
    if (!name().trim()) return;
    // Phase 12.C: pick the shell from the active mode. Wizard mode maps
    // shellId → the detected command string (or a sensible default if
    // detection failed). Custom mode passes the typed `shell` straight
    // through. The cwd lands at the workspace level so any local pane
    // in the workspace picks it up.
    let cmd: string | undefined;
    if (wizardMode() === "wizard") {
      const found = detectedShells().find((s) => s.id === shellId());
      cmd = found?.command;
    } else {
      cmd = shell().trim() || undefined;
    }
    const workspaceCwd = cwd().trim() || undefined;
    p.onCreate({
      name: name().trim(),
      // ts-rs binding renders Option<String> as `string | null`, so
      // the absent value is null, not undefined.
      connection: { type: "local", shell: cmd ?? null },
      color: color(),
      cwd: workspaceCwd,
      setup_command: setupCmd() || undefined,
      teardown_command: teardownCmd() || undefined,
      env: cleanedEnv().length ? cleanedEnv() : undefined,
    });
    // Phase 12.C: bump the chosen cwd into the recent-paths history so
    // it shows up in the combobox next time. Best-effort — silently
    // ignore RPC failures.
    if (workspaceCwd) {
      backend.call("record_recent_path", { path: workspaceCwd }).catch(() => {});
    }
    p.onClose();
  };

  return (
    <>
      <label>
        <span>{t("ws.create.field.name")}</span>
        <input
          autofocus
          value={name()}
          onInput={(e) => setName(e.currentTarget.value)}
          placeholder={t(
            isWindows()
              ? "ws.create.field.name.placeholder"
              : "ws.create.field.name.placeholder.posix",
          )}
        />
      </label>

      <label>
        <span>{t("ws.create.field.color")}</span>
        <input
          type="color"
          value={color()}
          onInput={(e) => setColor(e.currentTarget.value)}
        />
      </label>

      {/* Phase 12.C: mode toggle. Wizard = shell dropdown + cwd
          combobox; Custom = single free-text command. */}
      <div class="local-wizard-tabs">
        <button
          type="button"
          class={`local-wizard-tab ${wizardMode() === "wizard" ? "active" : ""}`}
          onClick={() => setWizardMode("wizard")}
        >
          {t("ws.create.mode.wizard")}
        </button>
        <button
          type="button"
          class={`local-wizard-tab ${wizardMode() === "custom" ? "active" : ""}`}
          onClick={() => setWizardMode("custom")}
        >
          {t("ws.create.mode.custom")}
        </button>
      </div>
      <Show when={wizardMode() === "wizard"}>
        <label>
          <span>{t("ws.create.shell.label")}</span>
          <select
            value={shellId()}
            onChange={(e) => setShellId(e.currentTarget.value)}
          >
            <For each={detectedShells()}>
              {(s) => (
                <option value={s.id} disabled={!s.available}>
                  {shellLabel(s)}
                  {!s.available ? " " + t("ws.create.shell.not_installed") : ""}
                </option>
              )}
            </For>
          </select>
        </label>
      </Show>
      <Show when={wizardMode() === "custom"}>
        <label>
          <span>{t("ws.create.custom_cmd.label")}</span>
          <input
            value={shell()}
            onInput={(e) => setShell(e.currentTarget.value)}
            placeholder={t(
              isWindows()
                ? "ws.create.custom_cmd.placeholder"
                : "ws.create.custom_cmd.placeholder.posix",
            )}
          />
        </label>
      </Show>
      <label>
        <span>{t("ws.create.cwd.label")}</span>
        <input
          type="text"
          list="setup-recent-paths"
          value={cwd()}
          onInput={(e) => setCwd(e.currentTarget.value)}
          placeholder={t("ws.create.cwd.placeholder")}
        />
      </label>
      {/* Distinct datalist id — the edit modal used to own
          "ymux-recent-paths"; a collision would cross-wire the
          combobox suggestions if both ever mount. */}
      <datalist id="setup-recent-paths">
        <For each={recentPaths()}>
          {(rp) => (
            <option value={rp.path}>
              {rp.kind === "recent"
                ? t("ws.create.cwd.recent")
                : t("ws.create.cwd.defaults")}
            </option>
          )}
        </For>
      </datalist>

      <hr class="modal-sep" />

      <WorkspaceExtrasFields
        setupCmd={setupCmd}
        setSetupCmd={setSetupCmd}
        teardownCmd={teardownCmd}
        setTeardownCmd={setTeardownCmd}
        envRows={envRows}
        setEnvRows={setEnvRows}
      />

      <div class="modal-buttons">
        <button onClick={p.onClose}>{t("common.cancel")}</button>
        <button
          class="primary"
          disabled={!name().trim()}
          onClick={submit}
        >
          {t("ws.create.btn.create")}
        </button>
      </div>
    </>
  );
}
