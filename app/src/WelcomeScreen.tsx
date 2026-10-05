import { Show } from "solid-js";
import { t } from "./i18n";
import { backend } from "./backend";

// Design Pass 01 (#1): the empty-workspace welcome. Shown in the main area
// when there are zero workspaces — replaces the old bare `<p>` + button.
// Every action reuses an existing flow: onCreate → CreateWorkspaceModal,
// onConnectSsh → same modal preset to SSH, onProvision → ProvisioningWizard.
// Pure presentational; all state lives in App.tsx.

interface Props {
  onCreate: () => void;
  onConnectSsh: () => void;
  onProvision: () => void;
}

export function WelcomeScreen(p: Props) {
  return (
    <div class="welcome">
      <div class="welcome-inner">
        <div class="welcome-glyph" aria-hidden="true">▚</div>
        <h1 class="welcome-title">{t("ws.welcome.title")}</h1>
        <p class="welcome-subtitle">{t("ws.welcome.subtitle")}</p>

        <div class="welcome-cards">
          <Show when={backend.can("localPanes")}>
            <button class="welcome-card" onClick={p.onCreate}>
              <span class="welcome-card-icon">▮</span>
              <span class="welcome-card-title">{t("ws.welcome.local.title")}</span>
              <span class="welcome-card-desc">{t("ws.welcome.local.desc")}</span>
            </button>
          </Show>

          <Show when={backend.can("ssh")}>
            <button class="welcome-card featured" onClick={p.onConnectSsh}>
              <span class="welcome-card-icon">🌐</span>
              <span class="welcome-card-title">{t("ws.welcome.ssh.title")}</span>
              <span class="welcome-card-desc">{t("ws.welcome.ssh.desc")}</span>
              <span class="welcome-card-badge">{t("ws.welcome.ssh.badge")}</span>
            </button>

            <button class="welcome-card" onClick={p.onProvision}>
              <span class="welcome-card-icon">🚀</span>
              <span class="welcome-card-title">{t("ws.welcome.provision.title")}</span>
              <span class="welcome-card-desc">{t("ws.welcome.provision.desc")}</span>
            </button>
          </Show>
        </div>

        <div class="welcome-hint">
          <span>{t("ws.welcome.palette_press")}</span>
          <kbd class="welcome-kbd">Ctrl</kbd>
          <kbd class="welcome-kbd">Shift</kbd>
          <kbd class="welcome-kbd">P</kbd>
          <span>{t("ws.welcome.palette_hint")}</span>
        </div>
      </div>
    </div>
  );
}
