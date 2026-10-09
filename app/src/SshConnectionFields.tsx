import { createSignal, For, Show, onMount } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { createLogger } from "./logger";

const log = createLogger("SETUP");
import { IconCheck, IconClose } from "./icons";
import type { Connection } from "./types";

// Phase 80 (unified setup wizard): the SSH connection form extracted from
// CreateWorkspaceModal so it can be shared by the edit modal and the
// wizard's "server → existing → I have a key" flow. The parent owns the
// form state (via createSshFormState) so it can hydrate from an existing
// workspace or read the built Connection on submit; everything transient
// (host picker, test-connection inputs/result) stays component-local.

// Phase 13.A wizard data shapes — mirror the Rust definitions in
// src-tauri/src/connect_wizard.rs.
export interface SshConfigHost {
  alias: string;
  hostname?: string | null;
  user?: string | null;
  port?: number | null;
  identity_file?: string | null;
  proxy_command?: string | null;
  proxy_jump?: string | null;
}

export interface DetectedKey {
  path: string;
  filename: string;
  modified_iso: string | null;
  size_bytes: number;
  fingerprint: string | null;
  key_type: string | null;
  perms_ok: boolean;
  perms_error: string | null;
}

export interface PermsResult {
  ok: boolean;
  error?: string | null;
}

export interface TestResult {
  ok: boolean;
  stage: string;
  method?: string | null;
  server_version?: string | null;
  message?: string | null;
  hint?: string | null;
  elapsed_ms: number;
}

export interface SshFormState {
  host: () => string;
  setHost: (v: string) => void;
  user: () => string;
  setUser: (v: string) => void;
  port: () => number;
  setPort: (v: number) => void;
  keyPath: () => string;
  setKeyPath: (v: string) => void;
  // Phase 37: SSH auth mode. "key" → use key_path; "password" → save no
  // credential, prompt interactively at every connect (never persisted).
  authMode: () => "key" | "password";
  setAuthMode: (v: "key" | "password") => void;
  keyMode: () => "auto" | "detected" | "custom";
  setKeyMode: (v: "auto" | "detected" | "custom") => void;
  keyPerms: () => PermsResult | null;
  setKeyPerms: (v: PermsResult | null) => void;
}

export function createSshFormState(): SshFormState {
  const [host, setHost] = createSignal("");
  const [user, setUser] = createSignal("");
  const [port, setPort] = createSignal(22);
  const [keyPath, setKeyPath] = createSignal("");
  const [authMode, setAuthMode] = createSignal<"key" | "password">("key");
  const [keyMode, setKeyMode] = createSignal<"auto" | "detected" | "custom">("auto");
  const [keyPerms, setKeyPerms] = createSignal<PermsResult | null>(null);
  return {
    host, setHost,
    user, setUser,
    port, setPort,
    keyPath, setKeyPath,
    authMode, setAuthMode,
    keyMode, setKeyMode,
    keyPerms, setKeyPerms,
  };
}

// Phase 37: build the SSH Connection from the current form state.
// In "password" auth mode key_path is null — the workspace saves no
// credential and is prompted interactively at connect (never saved).
export function buildSshConnection(s: SshFormState): Connection | null {
  if (!s.host().trim() || !s.user().trim()) return null;
  return {
    type: "ssh",
    host: s.host().trim(),
    user: s.user().trim(),
    port: s.port(),
    key_path: s.authMode() === "key" ? s.keyPath() || null : null,
  };
}

interface Props {
  state: SshFormState;
  // Show the "import from ~/.ssh/config" button + host picker (create
  // flows). Edit mode passes false, matching the old !isEdit() gate.
  showImport: boolean;
  // Phase 34: open the SSH-key-setup HelpPane as a side-by-side split.
  onOpenSshHelp?: () => void;
  // Create flows seed the workspace name from the imported host alias.
  onImportName?: (alias: string) => void;
}

export function SshConnectionFields(p: Props) {
  const s = p.state;

  // Phase 13.A wizard state (component-local; reloaded on every mount,
  // which under the unified wizard means every time the form opens).
  const [sshHosts, setSshHosts] = createSignal<SshConfigHost[]>([]);
  const [sshHostsLoaded, setSshHostsLoaded] = createSignal(false);
  const [showHostPicker, setShowHostPicker] = createSignal(false);
  const [detectedKeys, setDetectedKeys] = createSignal<DetectedKey[]>([]);
  const [testPassword, setTestPassword] = createSignal("");
  const [testPassphrase, setTestPassphrase] = createSignal("");
  const [testing, setTesting] = createSignal(false);
  const [testResult, setTestResult] = createSignal<TestResult | null>(null);

  onMount(() => {
    void (async () => {
      try {
        const hosts = await backend.call<SshConfigHost[]>("parse_ssh_config");
        setSshHosts(hosts);
        setSshHostsLoaded(true);
      } catch (e) {
        log.warn("parse_ssh_config failed", e);
        setSshHostsLoaded(true);
      }
      try {
        const keys = await backend.call<DetectedKey[]>("list_ssh_keys");
        setDetectedKeys(keys);
      } catch (e) {
        log.warn("list_ssh_keys failed", e);
      }
    })();
  });

  // Re-check perms when keyPath changes via the detected dropdown.
  const refreshPermsFor = async (path: string) => {
    if (!path) {
      s.setKeyPerms(null);
      return;
    }
    try {
      const r = await backend.call<PermsResult>("check_key_permissions", { path });
      s.setKeyPerms(r);
    } catch (e) {
      log.warn("check_key_permissions failed", e);
    }
  };

  const onPickDetectedKey = async (path: string) => {
    s.setKeyPath(path);
    s.setKeyMode(path ? "detected" : "auto");
    await refreshPermsFor(path);
  };

  const onFixPerms = async () => {
    const path = s.keyPath();
    if (!path) return;
    try {
      const r = await backend.call<PermsResult>("fix_key_permissions", { path });
      s.setKeyPerms(r);
    } catch (e) {
      log.error("fix_key_permissions failed", e);
      s.setKeyPerms({ ok: false, error: String(e) });
    }
  };

  const onImportHost = (h: SshConfigHost) => {
    if (h.hostname) s.setHost(h.hostname);
    else if (h.alias) s.setHost(h.alias);
    if (h.user) s.setUser(h.user);
    if (h.port) s.setPort(h.port);
    if (h.identity_file) {
      s.setKeyPath(h.identity_file);
      s.setKeyMode("custom");
      void refreshPermsFor(h.identity_file);
    }
    p.onImportName?.(h.alias);
    setShowHostPicker(false);
  };

  const onTestConnect = async () => {
    if (!s.host().trim() || !s.user().trim()) {
      setTestResult({
        ok: false,
        stage: "validation",
        message: "user + host required",
        elapsed_ms: 0,
      });
      return;
    }
    setTesting(true);
    setTestResult(null);
    try {
      const r = await backend.call<TestResult>("test_ssh_connect", {
        host: s.host().trim(),
        user: s.user().trim(),
        port: s.port(),
        keyPath: s.keyPath() || null,
        keyPassphrase: testPassphrase() || null,
        password: testPassword() || null,
      });
      setTestResult(r);
    } catch (e) {
      setTestResult({
        ok: false,
        stage: "rpc",
        message: String(e),
        elapsed_ms: 0,
      });
    } finally {
      setTesting(false);
    }
  };

  return (
    <>
      <Show when={p.showImport}>
        <div class="wizard-row">
          <button
            class="wizard-import"
            type="button"
            disabled={!sshHostsLoaded() || sshHosts().length === 0}
            onClick={() => setShowHostPicker(true)}
            title={
              sshHosts().length === 0
                ? t("wizard.import_no_hosts")
                : t("wizard.import_hosts_tooltip", { n: sshHosts().length })
            }
          >
            {t("wizard.import_btn")}
            <Show when={sshHosts().length > 0}>
              <span class="wizard-pill">{sshHosts().length}</span>
            </Show>
          </button>
        </div>
      </Show>
      {/* Phase 42: user / host / port sit in a 2-col grid so the
          modal can be wider without leaving acres of empty space. */}
      <div class="ws-form-grid">
        <label>
          <span>{t("ws.create.field.user")}</span>
          <input
            value={s.user()}
            onInput={(e) => s.setUser(e.currentTarget.value)}
            placeholder={t("ws.create.field.user.placeholder")}
          />
        </label>
        <label>
          <span>{t("ws.create.field.host")}</span>
          <input
            value={s.host()}
            onInput={(e) => s.setHost(e.currentTarget.value)}
            placeholder={t("ws.create.field.host.placeholder")}
          />
        </label>
        <label>
          <span>{t("ws.create.field.port")}</span>
          <input
            type="number"
            value={s.port()}
            onInput={(e) => s.setPort(parseInt(e.currentTarget.value) || 22)}
          />
        </label>
      </div>
      {/* Phase 37: auth-mode chooser. "Password" mode saves no
          credential — the user is prompted interactively at every
          connect and the password is never persisted. */}
      <label>
        <span class="ws-key-label">
          {t("ws.create.auth.label")}
          {/* Phase 34: single contextual entry point to the
              SSH-key-setup HelpPane. */}
          <button
            type="button"
            class="help-hint-btn"
            title={t("help.sshKey.tooltip")}
            onClick={(e) => {
              e.preventDefault();
              e.stopPropagation();
              p.onOpenSshHelp?.();
            }}
          >
            ?
          </button>
        </span>
        <div class="ws-auth-seg">
          <label class="ws-auth-opt">
            <input
              type="radio"
              name="ws-auth-mode"
              checked={s.authMode() === "key"}
              onChange={() => s.setAuthMode("key")}
            />
            <span>{t("ws.create.auth.key")}</span>
          </label>
          <label class="ws-auth-opt">
            <input
              type="radio"
              name="ws-auth-mode"
              checked={s.authMode() === "password"}
              onChange={() => {
                s.setAuthMode("password");
                s.setKeyPath("");
                s.setKeyPerms(null);
              }}
            />
            <span>{t("ws.create.auth.password")}</span>
          </label>
        </div>
      </label>

      <Show when={s.authMode() === "password"}>
        <p class="settings-hint ws-auth-hint">{t("ws.create.auth.password.hint")}</p>
      </Show>

      <Show when={s.authMode() === "key"}>
        <label>
          <span>{t("ws.create.field.key")}</span>
          <select
            value={s.keyMode()}
            onChange={(e) => {
              const v = e.currentTarget.value as "auto" | "detected" | "custom";
              s.setKeyMode(v);
              if (v === "auto") {
                s.setKeyPath("");
                s.setKeyPerms(null);
              } else if (v === "detected" && detectedKeys().length > 0) {
                void onPickDetectedKey(detectedKeys()[0].path);
              }
            }}
          >
            <option value="auto">{t("ws.create.key.mode.auto")}</option>
            <option value="detected" disabled={detectedKeys().length === 0}>
              {t("ws.create.key.mode.detected", { n: detectedKeys().length })}
            </option>
            <option value="custom">{t("ws.create.key.mode.custom")}</option>
          </select>
        </label>
        <Show when={s.keyMode() === "detected"}>
          <label>
            <span></span>
            <select
              value={s.keyPath()}
              onChange={(e) => void onPickDetectedKey(e.currentTarget.value)}
            >
              <For each={detectedKeys()}>
                {(k) => (
                  <option value={k.path}>
                    {k.filename}
                    {k.key_type ? ` (${k.key_type})` : ""}
                    {k.fingerprint ? ` · ${k.fingerprint.slice(0, 19)}…` : ""}
                  </option>
                )}
              </For>
            </select>
          </label>
        </Show>
        <Show when={s.keyMode() === "custom"}>
          <label>
            <span></span>
            <input
              value={s.keyPath()}
              onInput={(e) => {
                s.setKeyPath(e.currentTarget.value);
                if (e.currentTarget.value) void refreshPermsFor(e.currentTarget.value);
                else s.setKeyPerms(null);
              }}
              placeholder={t("ws.create.field.key.placeholder")}
            />
          </label>
        </Show>
        <Show when={s.keyPath() && s.keyPerms() && !s.keyPerms()!.ok}>
          <div class="wizard-row wizard-warn">
            <span>{t("wizard.perms_warn", { err: s.keyPerms()!.error ?? "" })}</span>
            <button
              type="button"
              class="wizard-fix"
              onClick={() => void onFixPerms()}
            >
              {t("wizard.perms_fix")}
            </button>
          </div>
        </Show>
        <Show when={s.keyPath() && s.keyPerms()?.ok}>
          <div class="wizard-row wizard-ok">{t("wizard.perms_ok")}</div>
        </Show>
      </Show>

      <div class="wizard-test">
        <div class="wizard-test-fields">
          <input
            type="password"
            placeholder={t("wizard.test_password.placeholder")}
            value={testPassword()}
            onInput={(e) => setTestPassword(e.currentTarget.value)}
          />
          <input
            type="password"
            placeholder={t("wizard.test_passphrase.placeholder")}
            value={testPassphrase()}
            onInput={(e) => setTestPassphrase(e.currentTarget.value)}
          />
        </div>
        <button
          type="button"
          class="wizard-test-btn"
          disabled={testing()}
          onClick={() => void onTestConnect()}
        >
          {testing() ? t("wizard.testing") : t("wizard.test_btn")}
        </button>
        <Show when={testResult()}>
          <div class={`wizard-test-result ${testResult()!.ok ? "ok" : "err"}`}>
            <div class="wizard-test-line">
              {testResult()!.ok ? <IconCheck size={14} /> : <IconClose size={14} />}{" "}
              {testResult()!.message ??
                (testResult()!.ok ? t("wizard.test_connected") : t("wizard.test_failed"))}
            </div>
            <Show when={testResult()!.method}>
              <div class="wizard-test-meta">
                {t("wizard.test_meta", {
                  method: testResult()!.method ?? "",
                  stage: testResult()!.stage,
                  ms: testResult()!.elapsed_ms,
                })}
              </div>
            </Show>
            <Show when={!testResult()!.ok && testResult()!.hint}>
              <div class="wizard-test-hint">{testResult()!.hint}</div>
            </Show>
          </div>
        </Show>
      </div>

      <Show when={showHostPicker()}>
        <div
          class="modal-backdrop"
          onClick={() => setShowHostPicker(false)}
          style={{ "z-index": 110 }}
        >
          <div
            class="modal wizard-host-picker"
            onClick={(e) => e.stopPropagation()}
          >
            <h3>{t("wizard.picker.title")}</h3>
            <Show
              when={sshHosts().length > 0}
              fallback={<p class="status-line">{t("wizard.picker.empty")}</p>}
            >
              <ul class="wizard-host-list">
                <For each={sshHosts()}>
                  {(h) => (
                    <li class="wizard-host-row" onClick={() => onImportHost(h)}>
                      <div class="wizard-host-alias">{h.alias}</div>
                      <div class="wizard-host-meta">
                        {(h.user ? `${h.user}@` : "") +
                          (h.hostname ?? h.alias) +
                          (h.port && h.port !== 22 ? `:${h.port}` : "")}
                        <Show when={h.identity_file}>
                          {" · " + h.identity_file}
                        </Show>
                      </div>
                    </li>
                  )}
                </For>
              </ul>
            </Show>
            <div class="modal-buttons">
              <button onClick={() => setShowHostPicker(false)}>{t("common.close")}</button>
            </div>
          </div>
        </div>
      </Show>
    </>
  );
}
