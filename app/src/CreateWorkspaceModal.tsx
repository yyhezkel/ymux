import { createEffect, createSignal, For, Show } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { isWindows } from "./platform";
import { createLogger } from "./logger";

const log = createLogger("WORKSPACE");
import { IconGitBranch } from "./icons";
import type { Connection, EnvVar, Workspace } from "./types";
import {
  createSshFormState,
  buildSshConnection,
  SshConnectionFields,
} from "./SshConnectionFields";
import { WorkspaceExtrasFields } from "./WorkspaceExtrasFields";

// Phase 80 (unified setup wizard): this modal is now EDIT-ONLY. Creation
// moved to SetupWizard (one "+ new workspace" button → mode tree); the
// sidebar "edit" action and the palette rename keep opening this modal
// directly. The SSH form and the extras block are shared with the wizard
// via SshConnectionFields / WorkspaceExtrasFields.

interface Props {
  open: boolean;
  // The workspace being edited. Nullable only for the <Show> guard — the
  // modal is only ever opened with a workspace since creation moved out.
  editing: Workspace | null;
  onClose: () => void;
  // Phase 34: open the SSH-key-setup HelpPane as a side-by-side split.
  // Optional — when omitted (e.g. tests, embedded usage), the `?` icon
  // next to the key field still renders but the click is a no-op.
  onOpenSshHelp?: () => void;
  onUpdate: (
    id: string,
    fields: {
      name?: string;
      color?: string;
      cwd?: string;
      setup_command?: string;
      teardown_command?: string;
      env?: EnvVar[];
      // Phase 37: editable connection (SSH workspaces). Absent = leave
      // the connection unchanged.
      connection?: Connection;
    }
  ) => void;
}

export function CreateWorkspaceModal(p: Props) {
  const [name, setName] = createSignal("");
  // 2026-08-19: WSL removed — native Windows panes get persistence from
  // zellij, which was the only thing WSL was here for.
  const [type, setType] = createSignal<"local" | "ssh">("local");
  const [shell, setShell] = createSignal("");
  const ssh = createSshFormState();
  // Phase 36 (#2.2): auto port-forward toggle (edit mode only).
  const [autoPortForward, setAutoPortForward] = createSignal(true);
  // Phase 49-B: worktree creator (edit mode + local + cwd is git repo).
  // wtBusy guards against double-clicks while git is running; wtErr
  // surfaces the backend's git stderr verbatim so the user can react.
  const [wtBranch, setWtBranch] = createSignal("");
  const [wtBase, setWtBase] = createSignal("main");
  const [wtBusy, setWtBusy] = createSignal(false);
  const [wtErr, setWtErr] = createSignal<string | null>(null);
  const [color, setColor] = createSignal("#7aa2f7");
  const [emoji, setEmoji] = createSignal("");
  // Phase 30: editable hex value for the custom-color text input. Kept
  // separate from `color` so a half-typed value (e.g. "#fff") doesn't
  // clobber the live preview until blur/validation succeeds.
  const [customHex, setCustomHex] = createSignal("");
  const [setupCmd, setSetupCmd] = createSignal("");
  const [teardownCmd, setTeardownCmd] = createSignal("");
  const [envRows, setEnvRows] = createSignal<EnvVar[]>([]);
  // Secret env keys that already have a stored value backend-side (names only).
  const [storedSecretKeys, setStoredSecretKeys] = createSignal<string[]>([]);

  // Phase 30: presets shown in the identity row. Eight named colors that
  // look distinct on both light and dark surfaces, and nine common
  // category-style glyphs. Both lists are deliberately short — too many
  // choices defeats the "instant recognition" goal.
  const COLOR_PRESETS = [
    "#1e40af", "#6d28d9", "#16a34a", "#ea580c",
    "#dc2626", "#ca8a04", "#0891b2", "#475569",
  ];
  const EMOJI_PRESETS = ["🟦", "🟣", "🟢", "🟠", "🔴", "🟡", "🔵", "⚪", "⬛"];
  const HEX_RE = /^#[0-9a-fA-F]{6}$/;

  // Phase 30: live-save helper. Used by the swatch / emoji buttons so a
  // click instantly persists + emits `workspaces:changed`; the Save
  // button is still required for non-identity fields (rename, env, …).
  const saveIdentity = async (nextColor: string | null, nextEmoji: string | null) => {
    if (!p.editing) return;
    try {
      const ws = await backend.call<Workspace>("workspace_set_identity", {
        workspaceId: p.editing.id,
        color: nextColor,
        emoji: nextEmoji,
      });
      // Reflect the server-canonical state in local signals. `workspaces:changed`
      // will reload the workspace list in App.tsx — but the modal keeps a stale
      // p.editing snapshot, so we update local fields explicitly here too.
      setColor(ws.color || "#7aa2f7");
      setEmoji(ws.emoji || "");
      setCustomHex(ws.color || "");
    } catch (e) {
      log.error("workspace_set_identity failed", e);
    }
  };

  const pickColor = (hex: string) => {
    setColor(hex);
    setCustomHex(hex);
    void saveIdentity(hex, emoji() || null);
  };

  const pickEmoji = (g: string) => {
    setEmoji(g);
    void saveIdentity(color() || null, g);
  };

  const onCustomHexBlur = () => {
    const v = customHex().trim();
    if (v === "") {
      // Empty = revert to whatever color() currently shows; do not save.
      setCustomHex(color());
      return;
    }
    if (HEX_RE.test(v)) {
      setColor(v);
      void saveIdentity(v, emoji() || null);
    } else {
      // Invalid: revert input to last accepted color.
      setCustomHex(color());
    }
  };

  const onCustomEmojiInput = (v: string) => {
    // Limit emoji input to 4 grapheme-ish chars (cheap byte cap will keep us
    // well under the backend's 16-byte ceiling for any plausible glyph).
    const trimmed = v.slice(0, 8);
    setEmoji(trimmed);
  };

  const onCustomEmojiBlur = () => {
    void saveIdentity(color() || null, emoji() || null);
  };

  const resetIdentity = () => {
    setColor("#7aa2f7");
    setEmoji("");
    setCustomHex("");
    void saveIdentity(null, null);
  };

  // Phase 36 (#2.2): live-toggle auto port forwarding (edit mode).
  const onToggleAutoPortForward = (enabled: boolean) => {
    setAutoPortForward(enabled);
    if (!p.editing) return;
    void backend.call("workspace_set_auto_port_forward", {
      workspaceId: p.editing.id,
      enabled,
    }).catch((e) => log.error("workspace_set_auto_port_forward failed", e));
  };

  // Phase 49-B: spawn `git worktree add` from the workspace's cwd. The
  // backend re-anchors the workspace's cwd to the new worktree path and
  // sets git_worktree, so new panes will spawn inside it.
  const onCreateWorktree = async () => {
    if (!p.editing) return;
    const branch = wtBranch().trim();
    const base = wtBase().trim();
    if (!branch || !base) return;
    setWtBusy(true);
    setWtErr(null);
    try {
      await backend.call("workspace_create_worktree", {
        workspaceId: p.editing.id,
        branchName: branch,
        baseBranch: base,
      });
      setWtBranch("");
    } catch (e) {
      setWtErr(String(e));
    } finally {
      setWtBusy(false);
    }
  };

  // Populate from `editing` whenever it (or open) changes.
  createEffect(() => {
    if (p.open && p.editing) {
      const w = p.editing;
      setName(w.name);
      setColor(w.color || "#7aa2f7");
      setEmoji(w.emoji || "");
      setCustomHex(w.color || "");
      setSetupCmd(w.setup_command || "");
      setTeardownCmd(w.teardown_command || "");
      setEnvRows(w.env ? [...w.env] : []);
      setStoredSecretKeys([]);
      backend.call<string[]>("workspace_secret_env_keys", { workspaceId: w.id })
        .then(setStoredSecretKeys)
        .catch((e) => log.error("workspace_secret_env_keys failed", e));
      setAutoPortForward(w.auto_port_forward ?? true);
      // Phase 37: connection fields are now editable (not read-only).
      const c = w.layout?.kind === "pane" ? w.layout.connection : w.connection;
      if (c?.type === "local") {
        setType("local");
        setShell(c.shell || "");
      } else if (c?.type === "wsl") {
        // A workspace opened before load_from_disk's migration rewrote it
        // (or one hand-edited back in). Show it as local — which is what it
        // becomes — rather than falling through to a blank editor.
        setType("local");
      } else if (c?.type === "ssh") {
        setType("ssh");
        ssh.setHost(c.host);
        ssh.setUser(c.user);
        ssh.setPort(c.port);
        ssh.setKeyPath(c.key_path || "");
        // Phase 37: derive auth mode from whether a key is set. A
        // workspace saved in password mode has key_path = null →
        // hydrate into "password" mode + custom key-mode so a later
        // switch to "key" shows the path field.
        if (c.key_path) {
          ssh.setAuthMode("key");
          ssh.setKeyMode("custom");
        } else {
          ssh.setAuthMode("password");
        }
      }
    }
  });

  const cleanedEnv = (): EnvVar[] =>
    envRows().filter((r) => r.key.trim() !== "");

  const submit = () => {
    if (!name().trim() || !p.editing) return;

    // Phase 37: connection fields are editable. For SSH workspaces send
    // the rebuilt connection so host/user/port/key/auth-mode changes
    // persist; local workspaces keep their existing connection (their
    // shell wizard state isn't reconstructed here).
    let editedConn: Connection | undefined;
    if (type() === "ssh") {
      const c = buildSshConnection(ssh);
      if (!c) return; // host/user required
      editedConn = c;
    }
    p.onUpdate(p.editing.id, {
      name: name().trim(),
      color: color(),
      setup_command: setupCmd(),
      teardown_command: teardownCmd(),
      env: cleanedEnv(),
      connection: editedConn,
    });
    p.onClose();
  };

  return (
    <Show when={p.open && p.editing}>
      <div class="modal-backdrop" onClick={p.onClose}>
        <div class="modal ws-create-modal" onClick={(e) => e.stopPropagation()}>
          <h3>{t("ws.create.title.edit")}</h3>
          {/* Phase 42: scrollable body — header (h3) above and footer
              (modal-buttons) below stay pinned via the flex column. */}
          <div class="ws-create-modal-body">

          <label>
            <span>{t("ws.create.field.name")}</span>
            <input
              autofocus
              value={name()}
              onInput={(e) => setName(e.currentTarget.value)}
              placeholder={t("ws.create.field.name.placeholder")}
            />
          </label>

          {/* Phase 30: rich identity picker. Each swatch / emoji is
              live-saved via workspace_set_identity. */}
          <div class="ws-identity-block">
            <div class="ws-identity-label">{t("ws.identity.color")}</div>
            <div class="ws-identity-row">
              <For each={COLOR_PRESETS}>
                {(c) => (
                  <button
                    type="button"
                    class={`ws-identity-swatch ${color() === c ? "selected" : ""}`}
                    style={{ background: c }}
                    title={c}
                    onClick={() => pickColor(c)}
                  />
                )}
              </For>
              <input
                type="text"
                class="ws-identity-hex"
                value={customHex()}
                placeholder={t("ws.identity.customColor")}
                spellcheck={false}
                onInput={(e) => setCustomHex(e.currentTarget.value)}
                onBlur={onCustomHexBlur}
              />
            </div>
            <div class="ws-identity-label" style="margin-top: 8px">{t("ws.identity.emoji")}</div>
            <div class="ws-identity-row">
              <For each={EMOJI_PRESETS}>
                {(g) => (
                  <button
                    type="button"
                    class={`ws-identity-emoji-btn ${emoji() === g ? "selected" : ""}`}
                    title={g}
                    onClick={() => pickEmoji(g)}
                  >
                    {g}
                  </button>
                )}
              </For>
              <input
                type="text"
                class="ws-identity-emoji-custom"
                value={emoji()}
                placeholder={t("ws.identity.customEmoji")}
                maxlength={8}
                onInput={(e) => onCustomEmojiInput(e.currentTarget.value)}
                onBlur={onCustomEmojiBlur}
              />
              <button
                type="button"
                class="ws-identity-reset"
                onClick={resetIdentity}
              >
                {t("ws.identity.reset")}
              </button>
            </div>
          </div>

          {/* Phase 36 (#2.2): auto port forwarding toggle. Edit-mode
              only — new workspaces default to ON (backend), toggle
              after creation here. */}
          <label class="ws-autoport">
            <input
              type="checkbox"
              checked={autoPortForward()}
              onChange={(e) => onToggleAutoPortForward(e.currentTarget.checked)}
            />
            <span>{t("ws.autoPortForward.label")}</span>
          </label>
          <p class="settings-hint ws-autoport-hint">{t("ws.autoPortForward.hint")}</p>

          {/* Phase 49-B: worktree creator. Shown only for local
              workspaces. If a worktree already exists for this
              workspace, the path is shown instead. */}
          <Show when={type() === "local"}>
            <div class="ws-worktree-block">
              <Show
                when={!p.editing?.git_worktree}
                fallback={
                  <p class="settings-hint">
                    <IconGitBranch size={13} /> {t("ws.worktree.alreadyOn")}{" "}
                    <code>{p.editing?.git_worktree}</code>
                  </p>
                }
              >
                <label>
                  <span>{t("ws.worktree.branchName")}</span>
                  <input
                    type="text"
                    value={wtBranch()}
                    placeholder="feature/my-thing"
                    onInput={(e) => setWtBranch(e.currentTarget.value)}
                    disabled={wtBusy()}
                  />
                </label>
                <label>
                  <span>{t("ws.worktree.baseBranch")}</span>
                  <input
                    type="text"
                    value={wtBase()}
                    placeholder="main"
                    onInput={(e) => setWtBase(e.currentTarget.value)}
                    disabled={wtBusy()}
                  />
                </label>
                <button
                  type="button"
                  onClick={() => void onCreateWorktree()}
                  disabled={wtBusy() || !wtBranch().trim() || !wtBase().trim()}
                >
                  {wtBusy() ? "…" : t("ws.worktree.create")}
                </button>
                <Show when={wtErr()}>
                  <p class="settings-hint" style="color:#e88">
                    {t("ws.worktree.failed")}: {wtErr()}
                  </p>
                </Show>
                <p class="settings-hint">{t("ws.worktree.hint")}</p>
              </Show>
            </div>
          </Show>

          <label>
            <span>{t("ws.create.field.type")}</span>
            <select value={type()} disabled>
              <option value="local">{t("ws.create.field.type.local")}</option>
              <option value="ssh">{t("ws.create.field.type.ssh")}</option>
            </select>
          </label>

          <Show when={type() === "local"}>
            <label>
              <span>{t("ws.create.custom_cmd.label")}</span>
              <input
                value={shell()}
                placeholder={t(
                  isWindows()
                    ? "ws.create.custom_cmd.placeholder"
                    : "ws.create.custom_cmd.placeholder.posix",
                )}
                disabled
              />
            </label>
          </Show>

          <Show when={type() === "ssh"}>
            <SshConnectionFields
              state={ssh}
              showImport={false}
              onOpenSshHelp={p.onOpenSshHelp}
            />
          </Show>

          <hr class="modal-sep" />

          <WorkspaceExtrasFields
            setupCmd={setupCmd}
            setSetupCmd={setSetupCmd}
            teardownCmd={teardownCmd}
            setTeardownCmd={setTeardownCmd}
            envRows={envRows}
            setEnvRows={setEnvRows}
            storedSecretKeys={storedSecretKeys}
          />

          </div>
          <div class="modal-buttons ws-create-modal-footer">
            <button onClick={p.onClose}>{t("common.cancel")}</button>
            <button class="primary" onClick={submit}>
              {t("common.save")}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
