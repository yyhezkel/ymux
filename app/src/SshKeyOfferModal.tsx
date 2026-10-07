import { createSignal, onCleanup, onMount, Show } from "solid-js";
import { backend, type UnlistenFn } from "./backend";
import { t } from "./i18n";
import { createLogger } from "./logger";

const log = createLogger("SSH");

// Phase 32.B: SSH key auto-setup offer.
// When the user authenticates via password, the backend emits
// `ssh-key-offer` with the workspace + connection metadata. We pop a
// modal asking whether to generate an ed25519 keypair and install the
// pubkey to the remote's ~/.ssh/authorized_keys. The password is
// re-asked (it lives only in the running pane's session and isn't
// persisted; asking once more is the safest path) and used for the
// short-lived install session.

interface OfferPayload {
  workspace_id: string;
  pane_id: string;
  ssh_user: string;
  ssh_host: string;
  ssh_port: number;
}

export function SshKeyOfferModal() {
  const [offer, setOffer] = createSignal<OfferPayload | null>(null);
  const [password, setPassword] = createSignal("");
  const [dontShow, setDontShow] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(null);
  const [success, setSuccess] = createSignal<string | null>(null);

  let unlisten: UnlistenFn | null = null;
  onMount(async () => {
    unlisten = await backend.on<OfferPayload>("ssh-key-offer", (e) => {
      setError(null);
      setSuccess(null);
      setPassword("");
      setDontShow(false);
      setOffer(e.payload);
    });
  });
  onCleanup(() => unlisten?.());

  const close = () => {
    setOffer(null);
    setPassword("");
  };

  const confirm = async () => {
    const o = offer();
    if (!o) return;
    setBusy(true);
    setError(null);
    try {
      await backend.call<string>("ssh_key_generate_and_install", {
        workspaceId: o.workspace_id,
        paneId: o.pane_id,
        sshUser: o.ssh_user,
        sshHost: o.ssh_host,
        sshPort: o.ssh_port,
        password: password(),
        dontShowAgain: dontShow(),
      });
      setSuccess(t("sshKey.offer.success"));
      // Linger briefly so the user sees the success message, then
      // dismiss.
      setTimeout(close, 1800);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const dismiss = async () => {
    try {
      await backend.call("ssh_key_offer_dismiss", { dontShowAgain: dontShow() });
    } catch (e) {
      log.warn("ssh_key_offer_dismiss failed", e);
    }
    close();
  };

  // Phase 33: open the manual setup guide as a horizontal split
  // alongside the originating pane. Closes this modal (the user is
  // committing to the manual flow; the offer banner will pop again
  // if they reconnect via password later, unless they tick "Don't
  // show again" first).
  const openManualGuide = async () => {
    const o = offer();
    if (!o) return;
    try {
      await backend.call("workspace_split", {
        workspaceId: o.workspace_id,
        paneId: o.pane_id,
        direction: "horizontal",
        paneKind: "help",
        browserUrl: null,
        helpTopic: "ssh-key-setup",
      });
    } catch (e) {
      log.warn("workspace_split (help) failed", e);
    }
    close();
  };

  return (
    <Show when={offer()}>
      <div class="modal-backdrop" onClick={dismiss}>
        <div class="modal sshkey-offer" onClick={(e) => e.stopPropagation()}>
          <div class="settings-head">
            <h3>{t("sshKey.offer.title")}</h3>
            <button class="feed-x" title={t("common.close")} onClick={dismiss}>
              ×
            </button>
          </div>
          <div class="sshkey-offer-body">
            <p>{t("sshKey.offer.body")}</p>
            <p class="sshkey-offer-target">
              {offer()!.ssh_user}@{offer()!.ssh_host}:{offer()!.ssh_port}
            </p>
            <p class="sshkey-offer-warning">{t("sshKey.offer.warning")}</p>
            <label class="sshkey-offer-pw">
              <span>{t("sshKey.offer.password")}</span>
              <input
                type="password"
                value={password()}
                onInput={(e) => setPassword(e.currentTarget.value)}
                autocomplete="current-password"
                disabled={busy() || !!success()}
              />
            </label>
            <label class="sshkey-offer-checkbox">
              <input
                type="checkbox"
                checked={dontShow()}
                onChange={(e) => setDontShow(e.currentTarget.checked)}
                disabled={busy() || !!success()}
              />
              <span>{t("sshKey.offer.dontShow")}</span>
            </label>
            <Show when={error()}>
              <div class="sshkey-offer-error">{error()}</div>
            </Show>
            <Show when={success()}>
              <div class="sshkey-offer-success">{success()}</div>
            </Show>
          </div>
          <div class="modal-buttons sshkey-offer-buttons">
            <button onClick={dismiss} disabled={busy()}>
              {t("sshKey.offer.skip")}
            </button>
            <button onClick={() => void openManualGuide()} disabled={busy()}>
              {t("help.manualGuide.button")}
            </button>
            <button
              class="primary"
              onClick={() => void confirm()}
              disabled={busy() || password().length === 0 || !!success()}
            >
              {busy()
                ? t("sshKey.offer.installing")
                : t("sshKey.offer.confirm")}
            </button>
          </div>
        </div>
      </div>
    </Show>
  );
}
