import { createSignal, For, Show, onMount } from "solid-js";
import { backend } from "./backend";
import { IconClose, IconCheck } from "./icons";
import type { SkillInfo } from "./bindings/SkillInfo";

// ymux-tools → Skills. Per-workspace (skills install onto the env's
// ~/.claude/skills/), driven by the skill_* Tauri commands. Modeled on
// AddonsTab so it stays self-contained out of SettingsModal.
export function YmuxToolsTab(p: { workspaceId?: string }) {
  const [skills, setSkills] = createSignal<SkillInfo[]>([]);
  const [installed, setInstalled] = createSignal<Set<string>>(new Set<string>());
  const [busy, setBusy] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);

  const loadInstalled = async () => {
    if (!p.workspaceId) {
      setInstalled(new Set<string>());
      return;
    }
    const inst = await backend.call<string[]>("skills_installed", { workspaceId: p.workspaceId });
    setInstalled(new Set(inst));
  };

  const refresh = async () => {
    setErr(null);
    setLoading(true);
    try {
      setSkills(await backend.call<SkillInfo[]>("skills_list"));
      await loadInstalled();
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };
  onMount(refresh);

  const act = async (cmd: "skill_install" | "skill_uninstall", name: string) => {
    if (!p.workspaceId) return;
    setBusy(name);
    setErr(null);
    try {
      await backend.call(cmd, { workspaceId: p.workspaceId, name });
      await loadInstalled();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section>
      <h4>ymux-tools · Skills</h4>
      <p class="settings-hint">
        Skills in your local registry (config dir → ymux-tools/skills/). Install them
        onto the selected workspace's ~/.claude/skills/ — local or remote over SFTP.
      </p>
      <Show when={!p.workspaceId}>
        <p class="settings-hint">Select a workspace to install skills onto it.</p>
      </Show>
      <Show when={err()}>
        <div class="wizard-test-result err">
          <div class="wizard-test-line">
            <IconClose size={14} /> {err()}
          </div>
        </div>
      </Show>
      <Show when={skills().length === 0 && !loading()}>
        <p class="settings-hint">
          No skills in the registry yet — drop a skill folder into ymux-tools/skills/.
        </p>
      </Show>
      <div class="addons-list">
        <For each={skills()}>
          {(s) => (
            <div class="addons-row">
              <div class="addons-meta">
                <strong>{s.name}</strong>
                <span class="settings-hint">
                  {s.files} file{s.files === 1 ? "" : "s"}
                  <Show when={installed().has(s.name)}>
                    {" · "}
                    <IconCheck size={12} /> installed
                  </Show>
                </span>
              </div>
              <Show when={p.workspaceId}>
                <div class="addons-actions">
                  <Show
                    when={installed().has(s.name)}
                    fallback={
                      <button
                        disabled={busy() === s.name}
                        onClick={() => void act("skill_install", s.name)}
                      >
                        {busy() === s.name ? "…" : "Install"}
                      </button>
                    }
                  >
                    <button
                      disabled={busy() === s.name}
                      onClick={() => void act("skill_install", s.name)}
                    >
                      {busy() === s.name ? "…" : "Reinstall"}
                    </button>
                    <button
                      disabled={busy() === s.name}
                      onClick={() => void act("skill_uninstall", s.name)}
                    >
                      Uninstall
                    </button>
                  </Show>
                </div>
              </Show>
            </div>
          )}
        </For>
      </div>
      <button disabled={loading()} onClick={() => void refresh()}>
        {loading() ? "…" : "Refresh"}
      </button>
    </section>
  );
}
