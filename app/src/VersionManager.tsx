import { createSignal, For, Show, onMount } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { isMac } from "./platform";
import { IconRefresh, IconBadgePlus, IconClose, IconCircle, IconCheck, IconWarning } from "./icons";

// Phase 71 — Settings → Updates version manager: list every published
// release, install any of them (incl. downgrade, with a warning), and pick a
// release channel. Backed by updater_list_versions / updater_install_version.

interface ReleaseInfo {
  version: string;
  tag: string;
  published_at: string | null;
  notes_url: string;
  body_md: string;
  prerelease: boolean;
  nsis_url: string | null;
  nsis_sha256: string | null;
  msi_url: string | null;
  msi_sha256: string | null;
  dmg_url: string | null;
  dmg_sha256: string | null;
}

// Compare X.Y.Z[-suffix]; a release (no suffix) outranks a prerelease.
function cmpSemver(a: string, b: string): number {
  const parse = (v: string): [number[], string] => {
    const [head, ...rest] = v.replace(/^v/, "").split("-");
    return [head.split(".").map((n) => parseInt(n, 10) || 0), rest.join("-")];
  };
  const [an, asfx] = parse(a);
  const [bn, bsfx] = parse(b);
  for (let i = 0; i < Math.max(an.length, bn.length); i++) {
    const d = (an[i] ?? 0) - (bn[i] ?? 0);
    if (d !== 0) return d > 0 ? 1 : -1;
  }
  if (asfx === bsfx) return 0;
  if (asfx === "") return 1; // release > prerelease
  if (bsfx === "") return -1;
  return asfx > bsfx ? 1 : -1;
}

function fmtDate(iso: string | null): string {
  if (!iso) return "";
  return new Date(iso).toLocaleDateString();
}

export function VersionManager(p: {
  channel: string;
  onSetChannel: (c: string) => void;
  skipped: string[];
  onUnskip: (v: string) => void;
}) {
  const [versions, setVersions] = createSignal<ReleaseInfo[]>([]);
  const [loading, setLoading] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [current, setCurrent] = createSignal("");
  const [expanded, setExpanded] = createSignal<string | null>(null);
  const [confirmTarget, setConfirmTarget] = createSignal<ReleaseInfo | null>(null);
  const [backup, setBackup] = createSignal(true);
  const [installing, setInstalling] = createSignal(false);

  const load = async (force: boolean) => {
    setLoading(true);
    setErr(null);
    try {
      const list = await backend.call<ReleaseInfo[]>("updater_list_versions", { force });
      setVersions(list);
    } catch (e) {
      setErr(String(e));
    } finally {
      setLoading(false);
    }
  };

  onMount(async () => {
    try {
      setCurrent(await backend.host.appVersion());
    } catch {
      /* ignore */
    }
    void load(false);
  });

  const visible = () =>
    versions().filter((r) => p.channel === "beta" || !r.prerelease);

  const latest = () => visible()[0]; // API returns newest-first
  const isDowngrade = (r: ReleaseInfo) => current() !== "" && cmpSemver(r.version, current()) < 0;
  const isCurrent = (r: ReleaseInfo) => r.version === current();

  const doInstall = async () => {
    const r = confirmTarget();
    if (!r) return;
    setInstalling(true);
    setErr(null);
    try {
      await backend.call("updater_install_version", {
        version: r.version,
        backupSettings: isDowngrade(r) && backup(),
      });
      // The app exits ~800ms after the installer spawns; nothing more to do.
    } catch (e) {
      setErr(String(e));
      setInstalling(false);
      setConfirmTarget(null);
    }
  };

  return (
    <div class="vm">
      {/* ── Channel + current/latest ── */}
      <div class="vm-top">
        <label class="vm-channel">
          <span>{t("vm.channel")}</span>
          <select value={p.channel} onChange={(e) => p.onSetChannel(e.currentTarget.value)}>
            <option value="stable">{t("vm.channel.stable")}</option>
            <option value="beta">{t("vm.channel.beta")}</option>
          </select>
        </label>
        <button class="vm-refresh" disabled={loading()} onClick={() => void load(true)}>
          {loading() ? "…" : <IconRefresh size={14} />}
        </button>
      </div>
      <p class="settings-hint">
        {t("vm.installed")} <code>{current() || "?"}</code>
        <Show when={latest()}>
          {" · "}
          {t("vm.latest")} <code>{latest()!.version}</code>
          <Show when={current() && latest() && cmpSemver(latest()!.version, current()) > 0}>
            {" "}<IconBadgePlus size={14} />
          </Show>
        </Show>
      </p>

      <Show when={err()}>
        <div class="vm-err">
          <IconClose size={14} /> {err()}
          <button onClick={() => void load(true)}>{t("vm.retry")}</button>
        </div>
      </Show>

      {/* ── Version history ── */}
      <div class="vm-list">
        <For each={visible()}>
          {(r) => (
            <div class={`vm-row ${isCurrent(r) ? "current" : ""}`}>
              <div class="vm-row-head" onClick={() => setExpanded(expanded() === r.version ? null : r.version)}>
                <span class="vm-ver">
                  {isCurrent(r) ? <IconCircle size={14} /> : r === latest() ? <IconCheck size={14} /> : <IconCircle size={14} />} v{r.version}
                  <Show when={r.prerelease}><span class="vm-pre">beta</span></Show>
                </span>
                <span class="vm-date">{fmtDate(r.published_at)}</span>
                <span class="vm-action">
                  <Show
                    when={!isCurrent(r)}
                    fallback={<span class="vm-current-tag">{t("vm.current")}</span>}
                  >
                    <button
                      class={isDowngrade(r) ? "vm-downgrade" : "primary"}
                      disabled={installing() || !(isMac() ? r.dmg_url : r.nsis_url)}
                      onClick={(e) => {
                        e.stopPropagation();
                        setConfirmTarget(r);
                      }}
                    >
                      {isDowngrade(r) ? t("vm.downgrade") : t("vm.install")}
                    </button>
                  </Show>
                </span>
              </div>
              <Show when={expanded() === r.version}>
                <div class="vm-notes">
                  <pre>{r.body_md || t("vm.no_notes")}</pre>
                  <a href={r.notes_url} target="_blank" rel="noreferrer">{t("vm.open_github")}</a>
                </div>
              </Show>
            </div>
          )}
        </For>
        <Show when={!loading() && visible().length === 0 && !err()}>
          <p class="settings-hint">{t("vm.empty")}</p>
        </Show>
      </div>

      {/* ── Skipped versions ── */}
      <Show when={p.skipped.length > 0}>
        <div class="vm-skipped">
          <span class="settings-hint">{t("vm.skipped")}</span>
          <For each={p.skipped}>
            {(v) => (
              <span class="vm-skip-chip">
                {v} <button onClick={() => p.onUnskip(v)} title={t("vm.unskip")}><IconClose size={14} /></button>
              </span>
            )}
          </For>
        </div>
      </Show>

      {/* ── Install / downgrade confirm modal ── */}
      <Show when={confirmTarget()}>
        <div class="vm-confirm-backdrop" onClick={() => !installing() && setConfirmTarget(null)}>
          <div class="vm-confirm" onClick={(e) => e.stopPropagation()}>
            <h4>
              {isDowngrade(confirmTarget()!) ? t("vm.confirm.downgrade_title") : t("vm.confirm.install_title")}
              {" "}v{confirmTarget()!.version}
            </h4>
            <Show when={isDowngrade(confirmTarget()!)}>
              <p class="vm-warn"><IconWarning size={14} /> {t("vm.confirm.downgrade_warn")}</p>
              <label class="settings-checkbox">
                <input type="checkbox" checked={backup()} onChange={(e) => setBackup(e.currentTarget.checked)} />
                <span>{t("vm.confirm.backup")}</span>
              </label>
            </Show>
            <p class="settings-hint">{t("vm.confirm.restart")}</p>
            <div class="vm-confirm-actions">
              <button disabled={installing()} onClick={() => setConfirmTarget(null)}>
                {t("common.cancel")}
              </button>
              <button class="primary" disabled={installing()} onClick={() => void doInstall()}>
                {installing() ? t("vm.installing") : t("vm.confirm.go")}
              </button>
            </div>
          </div>
        </div>
      </Show>
    </div>
  );
}
