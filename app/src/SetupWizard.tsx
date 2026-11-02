import { createSignal, Show } from "solid-js";
import { backend } from "./backend";
import { t } from "./i18n";
import { isWindows } from "./platform";
import { createLogger } from "./logger";

const log = createLogger("SETUP");
import {
  IconClose,
  IconCloud,
  IconTerminal,
  IconBadgePlus,
  IconLink,
  IconBadge,
  IconSparkles,
} from "./icons";
import type { CreateWorkspaceInput } from "./types";
import { ProvisionNewServerFlow } from "./ProvisionNewServerFlow";
import { ConnectExistingFlow } from "./ConnectExistingFlow";
import { QuickSshCreateFlow } from "./QuickSshCreateFlow";
import { QuickLocalCreateFlow } from "./QuickLocalCreateFlow";
import { LocalSetupFlow } from "./LocalSetupFlow";

// Phase 80 (unified setup wizard): ONE "+ new workspace" entry point.
// This host renders the modal chrome + a two-level mode tree and mounts
// exactly one body-only leaf flow:
//
//   server  → new       → ProvisionNewServerFlow  (full provisioning)
//   server  → existing  → key      → QuickSshCreateFlow   (record + test)
//                       → password → ConnectExistingFlow  (smart key install)
//   local   → existing  → QuickLocalCreateFlow    (plain cmd/pwsh shell; zsh/bash on macOS)
//   local   → new       → LocalSetupFlow          (smart install + WSL tmux)
//
// Every level defaults to null — this preserves the 65.R-fix intent: no
// flow (and especially not root provisioning) is reachable without an
// explicit click. Do NOT add a "remember last choice" convenience here.
// Back = deselect = the leaf flow UNMOUNTS, so re-entering any flow
// always starts from a blank form (no manual reset code needed). The
// whole host is remounted per open via <Show keyed> in App.tsx.

type Target = "server" | "local";
type Flavor = "new" | "existing";
type SshMethod = "key" | "password";

interface Props {
  onClose: () => void;
  /** = App.handleCreate → workspace_create. Used by the quick flows. */
  onCreateWorkspace: (input: CreateWorkspaceInput) => void;
  /** Activate + connect a workspace created by a provisioning-style flow. */
  onOpenWorkspace?: (workspaceId: string, mode: "default" | "claude") => void;
  /** Phase 34: open the SSH-key-setup HelpPane as a side-by-side split. */
  onOpenSshHelp?: () => void;
  /** Optional deep-link: palette "SSH: Provision a server" pre-selects server. */
  initialTarget?: Target;
}

// macOS port: the "existing shell" tile names the shells it will offer,
// and the "smart setup" tile drops the WSL mention.
const localExistingKey = () =>
  isWindows() ? "setup.local.existing" : "setup.local.existing.posix";
const localNewKey = () => (isWindows() ? "setup.local.new" : "setup.local.new.posix");
const localNewHintKey = () =>
  isWindows() ? "setup.local.new.hint" : "setup.local.new.hint.posix";

export function SetupWizard(p: Props) {
  const [target, setTarget] = createSignal<Target | null>(p.initialTarget ?? null);
  const [flavor, setFlavor] = createSignal<Flavor | null>(null);
  const [sshMethod, setSshMethod] = createSignal<SshMethod | null>(null);

  const pickTarget = (v: Target) => {
    log.debug(`[setup] target → ${v}`);
    setTarget(v);
    setFlavor(null);
    setSshMethod(null);
  };
  const pickFlavor = (v: Flavor) => {
    log.debug(`[setup] flavor → ${v}`);
    setFlavor(v);
    setSshMethod(null);
  };
  const pickSshMethod = (v: SshMethod) => {
    log.debug(`[setup] sshMethod → ${v}`);
    setSshMethod(v);
  };

  // The leaf flow that should render, or null while still picking.
  const leaf = ():
    | "provision"
    | "quick-ssh"
    | "connect-existing"
    | "quick-local"
    | "local-setup"
    | null => {
    if (target() === "server") {
      if (flavor() === "new") return "provision";
      if (flavor() === "existing") {
        if (sshMethod() === "key") return "quick-ssh";
        if (sshMethod() === "password") return "connect-existing";
      }
      return null;
    }
    if (target() === "local") {
      if (flavor() === "existing") return "quick-local";
      if (flavor() === "new") return "local-setup";
      return null;
    }
    return null;
  };

  // Back pops the deepest non-null level.
  const canGoBack = () => target() !== null;
  const goBack = () => {
    if (sshMethod() !== null) setSshMethod(null);
    else if (flavor() !== null) setFlavor(null);
    else setTarget(null);
  };

  const crumbs = () => {
    const parts: string[] = [];
    const tg = target();
    if (tg) parts.push(t(`setup.target.${tg}`));
    const fl = flavor();
    if (tg && fl) {
      parts.push(
        tg === "server"
          ? t(fl === "new" ? "provisioning.mode.new" : "provisioning.mode.existing")
          : t(fl === "new" ? localNewKey() : localExistingKey())
      );
    }
    const m = sshMethod();
    if (m) parts.push(t(`setup.ssh.method.${m}`));
    return parts.join(" › ");
  };

  const modeCard = (opts: {
    active: boolean;
    onPick: () => void;
    icon: () => ReturnType<typeof IconCloud>;
    label: string;
    hint: string;
    radioGroup: string;
  }) => (
    <label
      class={`provisioning-mode-row ${opts.active ? "active" : ""}`}
      onClick={opts.onPick}
    >
      <input
        type="radio"
        name={opts.radioGroup}
        checked={opts.active}
        onChange={opts.onPick}
      />
      <span>
        <strong>{opts.icon()} {opts.label}</strong>
        <span class="provisioning-mode-hint">{opts.hint}</span>
      </span>
    </label>
  );

  return (
    <div class="modal-backdrop" onClick={p.onClose}>
      <div
        class="modal provisioning-modal"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div class="provisioning-head">
          <h3>{t("ws.create.title.new")}</h3>
          <Show when={crumbs()}>
            <span class="provisioning-step-indicator setup-breadcrumb">{crumbs()}</span>
          </Show>
          <Show when={canGoBack()}>
            <button class="setup-back-btn" onClick={goBack}>
              {t("provisioning.btn.back")}
            </button>
          </Show>
          <button class="feed-x" title={t("common.close")} onClick={p.onClose}><IconClose /></button>
        </div>

        <div class="provisioning-body">
          {/* Picker: progressive disclosure. Hidden once a leaf is
              active — the breadcrumb + Back carry the context, and the
              provisioning flow needs the vertical space. */}
          <Show when={!leaf()}>
            {/* Level 1: where should this workspace run? */}
            <div class="provisioning-mode">
              <p class="settings-hint">{t("setup.target.label")}</p>
              {backend.can("ssh") && modeCard({
                active: target() === "server",
                onPick: () => pickTarget("server"),
                icon: () => <IconCloud size={14} />,
                label: t("setup.target.server"),
                hint: t("setup.target.server.hint"),
                radioGroup: "setup-target",
              })}
              {backend.can("localPanes") && modeCard({
                active: target() === "local",
                onPick: () => pickTarget("local"),
                icon: () => <IconTerminal size={14} />,
                label: t("setup.target.local"),
                hint: t("setup.target.local.hint"),
                radioGroup: "setup-target",
              })}
            </div>

            {/* Level 2 (server): new vs existing — reuses the exact
                labels the old ProvisioningWizard mode picker had. */}
            <Show when={target() === "server"}>
              <div class="provisioning-mode">
                <p class="settings-hint">{t("provisioning.mode.label")}</p>
                {modeCard({
                  active: flavor() === "new",
                  onPick: () => pickFlavor("new"),
                  icon: () => <IconBadgePlus size={14} />,
                  label: t("provisioning.mode.new"),
                  hint: t("provisioning.mode.new.hint"),
                  radioGroup: "setup-flavor",
                })}
                {modeCard({
                  active: flavor() === "existing",
                  onPick: () => pickFlavor("existing"),
                  icon: () => <IconLink size={14} />,
                  label: t("provisioning.mode.existing"),
                  hint: t("provisioning.mode.existing.hint"),
                  radioGroup: "setup-flavor",
                })}
              </div>

              {/* Level 3 (server → existing): access method. */}
              <Show when={flavor() === "existing"}>
                <div class="provisioning-mode">
                  <p class="settings-hint">{t("setup.ssh.method.label")}</p>
                  {modeCard({
                    active: sshMethod() === "key",
                    onPick: () => pickSshMethod("key"),
                    icon: () => <IconBadge size={14} />,
                    label: t("setup.ssh.method.key"),
                    hint: t("setup.ssh.method.key.hint"),
                    radioGroup: "setup-ssh-method",
                  })}
                  {modeCard({
                    active: sshMethod() === "password",
                    onPick: () => pickSshMethod("password"),
                    icon: () => <IconSparkles size={14} />,
                    label: t("setup.ssh.method.password"),
                    hint: t("setup.ssh.method.password.hint"),
                    radioGroup: "setup-ssh-method",
                  })}
                </div>
              </Show>
            </Show>

            {/* Level 2 (local): existing shell vs smart setup. */}
            <Show when={target() === "local"}>
              <div class="provisioning-mode">
                <p class="settings-hint">{t("setup.local.label")}</p>
                {modeCard({
                  active: flavor() === "existing",
                  onPick: () => pickFlavor("existing"),
                  icon: () => <IconTerminal size={14} />,
                  label: t(localExistingKey()),
                  hint: t("setup.local.existing.hint"),
                  radioGroup: "setup-flavor",
                })}
                {modeCard({
                  active: flavor() === "new",
                  onPick: () => pickFlavor("new"),
                  icon: () => <IconSparkles size={14} />,
                  label: t(localNewKey()),
                  hint: t(localNewHintKey()),
                  radioGroup: "setup-flavor",
                })}
              </div>
            </Show>
          </Show>

          {/* Exactly one leaf. Deselecting (Back) unmounts it — fresh
              state guaranteed on re-entry. */}
          <Show when={leaf() === "provision"}>
            <ProvisionNewServerFlow
              onClose={p.onClose}
              onOpenWorkspace={p.onOpenWorkspace}
            />
          </Show>
          <Show when={leaf() === "quick-ssh"}>
            <QuickSshCreateFlow
              onCreate={p.onCreateWorkspace}
              onClose={p.onClose}
              onOpenSshHelp={p.onOpenSshHelp}
            />
          </Show>
          <Show when={leaf() === "connect-existing"}>
            <ConnectExistingFlow
              onCreated={(wsId) => {
                p.onOpenWorkspace?.(wsId, "default");
                p.onClose();
              }}
              onClose={p.onClose}
            />
          </Show>
          <Show when={leaf() === "quick-local"}>
            <QuickLocalCreateFlow
              onCreate={p.onCreateWorkspace}
              onClose={p.onClose}
            />
          </Show>
          <Show when={leaf() === "local-setup"}>
            <LocalSetupFlow
              onCreated={(wsId) => {
                p.onOpenWorkspace?.(wsId, "default");
              }}
              onClose={p.onClose}
            />
          </Show>
        </div>
      </div>
    </div>
  );
}
