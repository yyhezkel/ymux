import { For } from "solid-js";
import { t } from "./i18n";
import type { EnvVar } from "./types";

// Phase 80 (unified setup wizard): the setup/teardown/env block extracted
// from CreateWorkspaceModal so the edit modal and the wizard's quick
// create flows render the same fields without triplicating the markup.
// The parent owns the signals (hydration + submit both live there).

interface Props {
  setupCmd: () => string;
  setSetupCmd: (v: string) => void;
  teardownCmd: () => string;
  setTeardownCmd: (v: string) => void;
  envRows: () => EnvVar[];
  setEnvRows: (v: EnvVar[]) => void;
  // Keys whose secret value is already stored backend-side (edit mode).
  storedSecretKeys?: () => string[];
}

export function WorkspaceExtrasFields(p: Props) {
  return (
    <>
      <label class="modal-textarea-label">
        <span>{t("ws.create.field.setup_cmd")}</span>
        <textarea
          rows="2"
          value={p.setupCmd()}
          onInput={(e) => p.setSetupCmd(e.currentTarget.value)}
          placeholder={t("ws.create.field.setup_cmd.placeholder")}
        />
      </label>

      <label class="modal-textarea-label">
        <span>{t("ws.create.field.teardown_cmd")}</span>
        <textarea
          rows="2"
          value={p.teardownCmd()}
          onInput={(e) => p.setTeardownCmd(e.currentTarget.value)}
          placeholder={t("ws.create.field.teardown_cmd.placeholder")}
        />
      </label>

      <div class="env-editor">
        <div class="env-editor-head">
          <span>{t("ws.create.field.env")}</span>
          <button
            class="env-add"
            onClick={() => p.setEnvRows([...p.envRows(), { key: "", value: "", secret: false }])}
          >
            {t("ws.create.btn.add_env")}
          </button>
        </div>
        <For each={p.envRows()}>
          {(row, i) => (
            <div class="env-row">
              <input
                placeholder={t("ws.create.env.key.placeholder")}
                value={row.key}
                onInput={(e) => {
                  const next = [...p.envRows()];
                  next[i()] = { ...next[i()], key: e.currentTarget.value };
                  p.setEnvRows(next);
                }}
              />
              <span class="env-eq">=</span>
              <input
                type={row.secret ? "password" : "text"}
                autocomplete="off"
                placeholder={
                  row.secret
                    ? row.key && p.storedSecretKeys?.().includes(row.key)
                      ? t("ws.create.env.secret.stored")
                      : t("ws.create.env.secret.placeholder")
                    : t("ws.create.env.value.placeholder")
                }
                value={row.value}
                onInput={(e) => {
                  const next = [...p.envRows()];
                  next[i()] = { ...next[i()], value: e.currentTarget.value };
                  p.setEnvRows(next);
                }}
              />
              <label class="env-secret">
                <input
                  type="checkbox"
                  checked={row.secret}
                  onChange={(e) => {
                    const next = [...p.envRows()];
                    // ticking secret keeps the typed value; unchecking drops it so a secret never lingers as plain text
                    next[i()] = {
                      ...next[i()],
                      secret: e.currentTarget.checked,
                      value: e.currentTarget.checked ? next[i()].value : "",
                    };
                    p.setEnvRows(next);
                  }}
                />
                <span>{t("ws.create.env.secret")}</span>
              </label>
              <button
                class="env-remove"
                title={t("ws.create.env.remove")}
                onClick={() => {
                  const next = [...p.envRows()];
                  next.splice(i(), 1);
                  p.setEnvRows(next);
                }}
              >
                ×
              </button>
            </div>
          )}
        </For>
      </div>
    </>
  );
}
