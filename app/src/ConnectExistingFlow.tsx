import { createSignal, createMemo, For, Show } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { IconClose, IconCheck } from "./icons";
import type { ServerDiscovery } from "./bindings/ServerDiscovery";
import type { ConnectExistingInput } from "./bindings/ConnectExistingInput";
import type { ConnectExistingResult } from "./bindings/ConnectExistingResult";
import { createLogger } from "./logger";

const log = createLogger("CONNECT");

// Phase 65.R: the "Connect to existing server" multi-machine SSH
// onboarding flow, embedded inside the Provisioning wizard's "connect to
// existing" mode (was the standalone ConnectExistingWizard in 65.C). The
// Rust backend (connect_existing_discover / connect_existing_execute)
// does the heavy lifting; this component is the auth → discover → choose
// → execute → result flow over those two commands. It renders only the
// step body (no modal chrome) so it can live inside the host wizard's
// `.provisioning-body`. It always creates a NEW workspace — the old
// "add this machine to an existing workspace" mode was dropped in 65.Q.

interface Props {
  /** Called on success so the parent can activate the new workspace. */
  onCreated: (workspaceId: string) => void;
  /** Cancel the whole wizard (the host modal's close). */
  onClose: () => void;
}

export function ConnectExistingFlow(p: Props) {
  type FlowStep = "auth" | "choose" | "result";
  const [step, setStep] = createSignal<FlowStep>("auth");

  // Auth-step inputs. The password is sensitive (Rule #2): it lives only
  // in this signal, is never logged, and is reused for the execute call.
  const [host, setHost] = createSignal("");
  const [port, setPort] = createSignal(22);
  const [user, setUser] = createSignal("");
  const [password, setPassword] = createSignal("");

  const [discovering, setDiscovering] = createSignal(false);
  const [authError, setAuthError] = createSignal<string | null>(null);
  const [discovery, setDiscovery] = createSignal<ServerDiscovery | null>(null);

  // Choose-step state.
  type Mode = "existing" | "new";
  const [mode, setMode] = createSignal<Mode>("existing");
  const [existingUser, setExistingUser] = createSignal("");
  const [newUser, setNewUser] = createSignal("ymux-user");
  const [grantSudo, setGrantSudo] = createSignal(true);
  const [workspaceName, setWorkspaceName] = createSignal("");

  // Execute-step state.
  const [executing, setExecuting] = createSignal(false);
  const [execError, setExecError] = createSignal<string | null>(null);
  const [result, setResult] = createSignal<ConnectExistingResult | null>(null);

  // Friendly error mapping: the backend surfaces raw SSH errors; turn the
  // common "auth failed" case into actionable guidance.
  const friendlyError = (raw: string): string => {
    const low = raw.toLowerCase();
    if (
      low.includes("authentication") ||
      low.includes("auth failed") ||
      low.includes("permission denied") ||
      low.includes("password")
    ) {
      return t("connectExisting.error.authFailed");
    }
    return raw;
  };

  const runDiscover = async () => {
    setDiscovering(true);
    setAuthError(null);
    setDiscovery(null);
    try {
      const d = await backend.call<ServerDiscovery>("connect_existing_discover", {
        host: host().trim(),
        port: port(),
        user: user().trim(),
        password: password(),
      });
      setDiscovery(d);
      // Phase 65.R-fix: diagnostic — confirms discover ran and what the
      // picker will offer (captured by `ymux dev console-tail`).
      log.debug(
        `discovered users=${JSON.stringify(d.users)} ` +
          `is_root=${d.is_root} can_sudo=${d.can_sudo} → step=choose`
      );
      // Phase 65.R-fix: steer away from root. The backend now excludes
      // root/system accounts from `users`, so on a fresh VPS (root-only)
      // the list is empty → force "create a new dedicated user". Only when
      // a real non-root account already exists do we default to picking it.
      // We never seed `existingUser` with the auth user (which is usually
      // root) — that's the trap that re-created a root workspace.
      if (d.users.length > 0) {
        setExistingUser(d.users[0]);
        setMode("existing");
      } else {
        setExistingUser("");
        setMode("new");
      }
      setStep("choose");
    } catch (e) {
      setAuthError(friendlyError(String(e)));
    } finally {
      setDiscovering(false);
    }
  };

  // The user account the run targets, derived from the chosen mode.
  const targetUser = createMemo<string>(() =>
    mode() === "new" ? newUser().trim() : existingUser().trim()
  );

  const canExecute = createMemo<boolean>(() => {
    if (executing()) return false;
    if (mode() === "new") {
      return newUser().trim().length > 0 && (discovery()?.can_sudo ?? false);
    }
    return existingUser().trim().length > 0;
  });

  const runExecute = async () => {
    const d = discovery();
    if (!d) return;
    setExecuting(true);
    setExecError(null);
    try {
      const input: ConnectExistingInput = {
        host: host().trim(),
        port: port(),
        auth_user: user().trim(),
        password: password(),
        target_user: targetUser(),
        create_new_user: mode() === "new",
        grant_sudo: mode() === "new" ? grantSudo() : false,
        sudo_group: d.sudo_group,
        workspace_name: workspaceName().trim() || null,
        existing_workspace_id: null,
      };
      const r = await backend.call<ConnectExistingResult>("connect_existing_execute", {
        input,
      });
      setResult(r);
      setStep("result");
    } catch (e) {
      setExecError(friendlyError(String(e)));
    } finally {
      setExecuting(false);
    }
  };

  const finish = () => {
    const r = result();
    if (r) p.onCreated(r.workspace_id);
    p.onClose();
  };

  const stepLabel = createMemo<string>(() => {
    switch (step()) {
      case "auth":
        return t("connectExisting.step.auth");
      case "choose":
        return t("connectExisting.step.choose");
      case "result":
        return t("connectExisting.step.result");
    }
  });

  return (
    <>
      <p class="provisioning-substep">{stepLabel()}</p>

      {/* Step 1: auth + discover */}
      <Show when={step() === "auth"}>
        <p class="settings-hint">{t("connectExisting.auth.hint")}</p>
        <label>
          <span>{t("connectExisting.field.host")}</span>
          <input
            value={host()}
            onInput={(e) => setHost(e.currentTarget.value)}
            placeholder="1.2.3.4"
          />
        </label>
        <label>
          <span>{t("connectExisting.field.port")}</span>
          <input
            type="number"
            value={port()}
            onInput={(e) => setPort(parseInt(e.currentTarget.value) || 22)}
          />
        </label>
        <label>
          <span>{t("connectExisting.field.user")}</span>
          <input
            value={user()}
            onInput={(e) => setUser(e.currentTarget.value)}
            placeholder="root"
          />
        </label>
        <label>
          <span>{t("connectExisting.field.password")}</span>
          <input
            type="password"
            value={password()}
            onInput={(e) => setPassword(e.currentTarget.value)}
            placeholder={t("connectExisting.field.password.placeholder")}
          />
        </label>

        <Show when={authError()}>
          <div class="wizard-test-result err">
            <div class="wizard-test-line"><IconClose size={14} /> {authError()}</div>
          </div>
        </Show>

        <div class="modal-buttons">
          <button onClick={p.onClose}>{t("common.cancel")}</button>
          <button
            class="primary"
            disabled={discovering() || !host() || !user() || !password()}
            onClick={() => void runDiscover()}
          >
            {discovering()
              ? t("connectExisting.discovering")
              : t("connectExisting.btn.discover")}
          </button>
        </div>
      </Show>

      {/* Step 2: choose target user + execute */}
      <Show when={step() === "choose" && discovery()}>
        {(() => {
          const d = discovery()!;
          return (
            <>
              <div class="wizard-test-result ok">
                <div class="wizard-test-line">
                  <IconCheck size={14} />{" "}
                  {d.is_root
                    ? t("connectExisting.status.root")
                    : d.can_sudo
                      ? t("connectExisting.status.sudo")
                      : t("connectExisting.status.plain")}
                </div>
              </div>

              <label class="provisioning-mode-row">
                <input
                  type="radio"
                  name="connect-existing-mode"
                  value="existing"
                  checked={mode() === "existing"}
                  disabled={d.users.length === 0}
                  onChange={() => setMode("existing")}
                />
                <span>
                  <strong>{t("connectExisting.mode.existing")}</strong>
                  <span class="provisioning-mode-hint">
                    {t("connectExisting.mode.existing.hint")}
                  </span>
                </span>
              </label>
              {/* Phase 65.R-fix: fresh / root-only server — no non-root
                  account to pick, so the existing option is disabled and we
                  point the user at "create a new user" instead. */}
              <Show when={d.users.length === 0}>
                <p class="settings-hint">
                  {t("connectExisting.mode.existing.none")}
                </p>
              </Show>
              <Show when={mode() === "existing" && d.users.length > 0}>
                <label>
                  <span>{t("connectExisting.field.existingUser")}</span>
                  <select
                    value={existingUser()}
                    onChange={(e) => setExistingUser(e.currentTarget.value)}
                  >
                    <For each={d.users}>
                      {(u) => <option value={u}>{u}</option>}
                    </For>
                  </select>
                </label>
              </Show>

              <label class="provisioning-mode-row">
                <input
                  type="radio"
                  name="connect-existing-mode"
                  value="new"
                  checked={mode() === "new"}
                  disabled={!d.can_sudo}
                  onChange={() => setMode("new")}
                />
                <span>
                  <strong>{t("connectExisting.mode.new")}</strong>
                  <span class="provisioning-mode-hint">
                    {t("connectExisting.mode.new.hint")}
                  </span>
                </span>
              </label>
              <Show when={!d.can_sudo}>
                <p class="settings-hint">
                  {t("connectExisting.mode.new.needsSudo")}
                </p>
              </Show>
              <Show when={mode() === "new" && d.can_sudo}>
                <label>
                  <span>{t("connectExisting.field.newUser")}</span>
                  <input
                    value={newUser()}
                    onInput={(e) => setNewUser(e.currentTarget.value)}
                    placeholder="ymux-user"
                  />
                </label>
                <label class="provisioning-mode-row">
                  <input
                    type="checkbox"
                    checked={grantSudo()}
                    onChange={(e) => setGrantSudo(e.currentTarget.checked)}
                  />
                  <span>
                    {t("connectExisting.field.grantSudo", {
                      group: d.sudo_group,
                    })}
                  </span>
                </label>
              </Show>

              <label>
                <span>{t("connectExisting.field.workspaceName")}</span>
                <input
                  value={workspaceName()}
                  onInput={(e) => setWorkspaceName(e.currentTarget.value)}
                  placeholder={t(
                    "connectExisting.field.workspaceName.placeholder"
                  )}
                />
              </label>

              {/* Confirm summary of what the Connect button will do. */}
              <p class="settings-hint">
                {mode() === "new"
                  ? grantSudo()
                    ? t("connectExisting.confirm.newUserSudo", {
                        user: targetUser(),
                      })
                    : t("connectExisting.confirm.newUser", {
                        user: targetUser(),
                      })
                  : t("connectExisting.confirm.existingUser", {
                      user: targetUser(),
                    })}
              </p>

              <Show when={execError()}>
                <div class="wizard-test-result err">
                  <div class="wizard-test-line"><IconClose size={14} /> {execError()}</div>
                </div>
              </Show>

              <div class="modal-buttons">
                <button onClick={() => setStep("auth")}>
                  {t("connectExisting.btn.back")}
                </button>
                <button
                  class="primary"
                  disabled={!canExecute()}
                  onClick={() => void runExecute()}
                >
                  {executing()
                    ? t("connectExisting.connecting")
                    : t("connectExisting.btn.connect")}
                </button>
              </div>
            </>
          );
        })()}
      </Show>

      {/* Step 3: result */}
      <Show when={step() === "result" && result()}>
        <div class="wizard-test-result ok">
          <div class="wizard-test-line">
            <IconCheck size={14} />{" "}
            {t("connectExisting.result.ready", {
              name: result()!.workspace_name,
            })}
          </div>
          <div class="wizard-test-meta">
            {t("connectExisting.result.keyPath", {
              path: result()!.key_path,
            })}
          </div>
        </div>
        <div class="modal-buttons">
          <button onClick={p.onClose}>{t("common.close")}</button>
          <button class="primary" onClick={finish}>
            {t("connectExisting.btn.createPane")}
          </button>
        </div>
      </Show>
    </>
  );
}
