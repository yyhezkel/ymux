import { createSignal, For, Show, onMount } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { IconClose, IconCheck, IconArrowUp } from "./icons";
import type { AddonStatus } from "./bindings/AddonStatus";

// Phase 68.E: Settings → Add-ons. Per-workspace (add-ons live on the
// remote), driven by the addon_* Tauri commands (68.B). Self-contained so
// it doesn't bloat SettingsModal's component.
export function AddonsTab(p: { workspaceId?: string }) {
  const [rows, setRows] = createSignal<AddonStatus[]>([]);
  const [busy, setBusy] = createSignal<string | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [logs, setLogs] = createSignal<{ id: string; text: string } | null>(null);

  const refresh = async () => {
    if (!p.workspaceId) {
      setRows([]);
      return;
    }
    setErr(null);
    setLoading(true);
    try {
      setRows(await backend.call<AddonStatus[]>("addon_list", { workspaceId: p.workspaceId }));
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };
  onMount(refresh);

  const act = async (cmd: "addon_install" | "addon_uninstall" | "addon_update", id: string) => {
    if (!p.workspaceId) return;
    setBusy(id);
    setErr(null);
    try {
      const s = await backend.call<AddonStatus>(cmd, { workspaceId: p.workspaceId, id });
      setRows((prev) => prev.map((r) => (r.id === id ? s : r)));
      if (s.last_error) setErr(s.last_error);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const viewLogs = async (id: string) => {
    if (!p.workspaceId) return;
    try {
      const text = await backend.call<string>("addon_logs", { workspaceId: p.workspaceId, id });
      setLogs({ id, text: text.trim() || "(empty)" });
    } catch (e) {
      setErr(String(e));
    }
  };

  return (
    <section>
      <h4>{t("settings.addons.title")}</h4>
      <Show when={!p.workspaceId}>
        <p class="settings-hint">{t("settings.addons.no_workspace")}</p>
      </Show>
      <Show when={p.workspaceId}>
        <p class="settings-hint">{t("settings.addons.hint")}</p>
        <Show when={err()}>
          <div class="wizard-test-result err">
            <div class="wizard-test-line"><IconClose size={14} /> {err()}</div>
          </div>
        </Show>
        <div class="addons-list">
          <For each={rows()}>
            {(r) => (
              <div class="addons-row">
                <div class="addons-meta">
                  <strong>{r.id}</strong>
                  <span class="settings-hint">
                    <Show when={r.installed} fallback={t("settings.addons.not_installed")}>
                      {r.installed_version ?? <IconCheck size={14} />}
                      <Show when={r.update_available}>
                        {" → "}{r.available_version} <IconArrowUp size={14} />
                      </Show>
                    </Show>
                  </span>
                </div>
                <div class="addons-actions">
                  <Show when={!r.installed}>
                    <button disabled={busy() === r.id} onClick={() => void act("addon_install", r.id)}>
                      {busy() === r.id ? "…" : t("settings.addons.install")}
                    </button>
                  </Show>
                  <Show when={r.installed && r.update_available}>
                    <button disabled={busy() === r.id} onClick={() => void act("addon_update", r.id)}>
                      {t("settings.addons.update")}
                    </button>
                  </Show>
                  <Show when={r.installed}>
                    {/* Reinstall is always available while installed — a safety
                        net if detect wrongly reports installed, or to push a
                        rebuilt daemon without the uninstall→install dance. */}
                    <button disabled={busy() === r.id} onClick={() => void act("addon_install", r.id)}>
                      {busy() === r.id ? "…" : t("settings.addons.reinstall")}
                    </button>
                    <button disabled={busy() === r.id} onClick={() => void act("addon_uninstall", r.id)}>
                      {t("settings.addons.uninstall")}
                    </button>
                  </Show>
                  <button onClick={() => void viewLogs(r.id)}>{t("settings.addons.logs")}</button>
                </div>
              </div>
            )}
          </For>
        </div>
        <button disabled={loading()} onClick={() => void refresh()}>
          {loading() ? "…" : t("settings.addons.refresh")}
        </button>
        <Show when={logs()}>
          <pre class="addons-logs">{logs()!.text}</pre>
        </Show>
      </Show>
    </section>
  );
}
