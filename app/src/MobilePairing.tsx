import { createSignal, For, Show, onCleanup } from "solid-js";
import { backend } from "./backend";
import qrcode from "qrcode-generator";
import { t } from "./i18n";
import { IconClose, IconCircle, IconSmartphone } from "./icons";
import { createLogger } from "./logger";

const log = createLogger("MOBILE");

// Phase 70.C — Mobile pairing tab inside the Monitor. Drives the nginx-proxy
// install + the daemon's pairing endpoints via the mobile_pairing_* commands.

interface PairStatus {
  domain: string;
  nginx_active: boolean;
  configured: boolean;
}
// What the daemon-backed command hands us (unchanged, API-stable). The host /
// port are used ONLY to render the URL card — Phase 74 deliberately keeps them
// OUT of the QR so a photographed/intercepted QR carries no server address.
interface IssuedPairing {
  host: string;
  port: number;
  tls: boolean;
  device_id: string;
  token: string;
  expires_at: number; // unix seconds (used for the countdown)
}

// Human-facing URL: omit the default port (443 for https, 80 for http) since a
// reverse proxy almost always terminates on the default — `:443` is just noise.
function formatDisplayUrl(host: string, port: number, tls: boolean): string {
  const scheme = tls ? "https" : "http";
  const defaultPort = tls ? 443 : 80;
  return port === defaultPort ? `${scheme}://${host}` : `${scheme}://${host}:${port}`;
}

// The v2 QR payload — tokens only, no host/port/tls. `version: 2` tells the
// mobile parser this is the split-URL shape (v1 = legacy, carried host/port).
interface QrPayloadV2 {
  version: 2;
  device_id: string;
  token: string;
  expires_at: string; // ISO-8601, per the agreed v2 schema
  fingerprint?: string;
}
interface PairedDevice {
  device_id: string;
  device_name: string;
  status: string;
  last_seen: number;
  last_ip: string;
  /** Stored grants: "all", "" or a JSON array (daemon chat_pairing.go). */
  scopes?: string;
}

/** Phase 113: `shell:attach` is never implied by "all" — only an explicit grant. */
const hasShell = (d: PairedDevice): boolean => (d.scopes ?? "").includes('"shell:attach"');

function fmtWhen(unix: number): string {
  if (!unix) return "—";
  return new Date(unix * 1000).toLocaleString();
}

export function MobilePairing(p: { workspaceId?: string }) {
  const [domain, setDomain] = createSignal("");
  const [cfToken, setCfToken] = createSignal("");
  const [status, setStatus] = createSignal<PairStatus | null>(null);
  const [busy, setBusy] = createSignal(false);
  const [err, setErr] = createSignal<string | null>(null);
  const [note, setNote] = createSignal<string | null>(null);

  const [devices, setDevices] = createSignal<PairedDevice[]>([]);
  const [pairName, setPairName] = createSignal("");
  const [qrSvg, setQrSvg] = createSignal<string | null>(null);
  const [pairUrl, setPairUrl] = createSignal<string | null>(null);
  const [copied, setCopied] = createSignal(false);
  const [countdown, setCountdown] = createSignal(0);
  // device_id of the QR currently on screen — polled so we can auto-close the
  // QR the moment that device actually pairs (redeems), not only on expiry.
  const [pendingDeviceId, setPendingDeviceId] = createSignal<string | null>(null);
  let countdownTimer: ReturnType<typeof setInterval> | undefined;
  let pollTimer: ReturnType<typeof setInterval> | undefined;

  const ws = () => p.workspaceId ?? "";

  const refreshStatus = async () => {
    if (!ws()) return;
    try {
      const s = JSON.parse(await backend.call<string>("mobile_pairing_status", { workspaceId: ws() })) as PairStatus;
      setStatus(s);
      if (s.domain && !domain()) setDomain(s.domain);
    } catch (e) {
      setErr(String(e));
    }
  };

  const refreshDevices = async () => {
    if (!ws()) return;
    try {
      const r = JSON.parse(await backend.call<string>("mobile_pairing_list_devices", { workspaceId: ws() })) as {
        devices: PairedDevice[];
      };
      setDevices(r.devices ?? []);
    } catch {
      /* daemon may not be reachable yet — leave the list empty */
    }
  };

  // Initial load.
  void refreshStatus();
  void refreshDevices();

  const install = async () => {
    if (!ws() || !domain().trim() || !cfToken().trim()) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      const r = JSON.parse(
        await backend.call<string>("mobile_pairing_init", {
          workspaceId: ws(),
          domain: domain().trim(),
          cfToken: cfToken().trim(),
        }),
      ) as { ok: boolean; status: string };
      setCfToken(""); // clear the secret from the UI immediately
      setNote(r.status || t("mobile.installed"));
      await refreshStatus();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  // A domain is "configured" once nginx + cert are set up and the domain marker
  // is persisted remote-side. When true the setup form is replaced by a compact
  // connected view (domain + disconnect) — no need to re-enter it every visit.
  const configured = () => status()?.configured ?? false;

  // Forget the linked domain (removes the remote marker) → back to setup. nginx
  // + the cert stay installed; a re-install reconfigures them.
  const disconnect = async () => {
    if (!ws()) return;
    setBusy(true);
    setErr(null);
    setNote(null);
    try {
      await backend.call("mobile_pairing_disconnect", { workspaceId: ws() });
      setDomain(""); // clear the typed value so the setup form comes back empty
      await refreshStatus();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(false);
    }
  };

  const clearCountdown = () => {
    if (countdownTimer) clearInterval(countdownTimer);
    countdownTimer = undefined;
  };
  const clearPoll = () => {
    if (pollTimer) clearInterval(pollTimer);
    pollTimer = undefined;
  };
  // Tear down the QR + its timers (on expiry, on successful pair, or unmount).
  const closeQr = () => {
    clearCountdown();
    clearPoll();
    setQrSvg(null);
    setPairUrl(null);
    setPendingDeviceId(null);
    setCountdown(0);
  };
  onCleanup(() => {
    clearCountdown();
    clearPoll();
  });

  const generateQr = async () => {
    if (!ws()) return;
    setErr(null);
    setQrSvg(null);
    setPairUrl(null);
    setCopied(false);
    try {
      const issued = JSON.parse(
        await backend.call<string>("mobile_pairing_generate_qr", {
          workspaceId: ws(),
          deviceName: pairName().trim() || "device",
        }),
      ) as IssuedPairing;
      // Phase 74: the QR carries ONLY tokens — no host/port/tls. The user types
      // the server URL by hand (shown in the card below), so a leaked QR alone
      // is useless.
      const qrData: QrPayloadV2 = {
        version: 2,
        device_id: issued.device_id,
        token: issued.token,
        expires_at: new Date(issued.expires_at * 1000).toISOString(),
      };
      const qr = qrcode(0, "M");
      qr.addData(JSON.stringify(qrData));
      qr.make();
      setQrSvg(qr.createSvgTag({ cellSize: 5, margin: 4 }));
      setPendingDeviceId(issued.device_id);
      // The out-of-band URL — default port hidden (see formatDisplayUrl). The
      // Copy button uses this same clean string; redeem parses it either way.
      setPairUrl(formatDisplayUrl(issued.host, issued.port, issued.tls));
      // 5-min countdown from the token's expiry (numeric unix seconds).
      const secs = Math.max(0, issued.expires_at - Math.floor(Date.now() / 1000));
      setCountdown(secs);
      clearCountdown();
      countdownTimer = setInterval(() => {
        setCountdown((c) => {
          if (c <= 1) {
            closeQr();
            void refreshDevices();
            return 0;
          }
          return c - 1;
        });
      }, 1000);
      // Poll every 3s: the instant this device redeems (status → active),
      // close the QR and confirm — no need to wait out the full countdown.
      clearPoll();
      pollTimer = setInterval(() => void pollPaired(), 3000);
      void refreshDevices();
    } catch (e) {
      setErr(String(e));
    }
  };

  const pollPaired = async () => {
    const id = pendingDeviceId();
    if (!id) return;
    await refreshDevices();
    const d = devices().find((x) => x.device_id === id);
    if (d && d.status === "active") {
      closeQr();
      setNote(t("mobile.paired_ok"));
    }
  };

  const copyUrl = async () => {
    const u = pairUrl();
    if (!u) return;
    try {
      await navigator.clipboard.writeText(u);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (e) {
      log.warn("clipboard write failed", e);
    }
  };

  const revoke = async (id: string) => {
    try {
      await backend.call("mobile_pairing_revoke", { workspaceId: ws(), deviceId: id });
      await refreshDevices();
    } catch (e) {
      setErr(String(e));
    }
  };

  // Phase 113 (Web & devices): grant / withdraw a paired browser's terminal.
  const setShell = async (id: string, enabled: boolean) => {
    if (enabled && !window.confirm(t("mobile.shell.confirm"))) {
      await refreshDevices(); // put the checkbox back
      return;
    }
    try {
      await backend.call("mobile_pairing_set_shell", { workspaceId: ws(), deviceId: id, enabled });
    } catch (e) {
      setErr(String(e));
    }
    await refreshDevices();
  };

  const rename = async (id: string, current: string) => {
    const name = window.prompt(t("mobile.rename_prompt"), current);
    if (name == null) return;
    try {
      await backend.call("mobile_pairing_rename", { workspaceId: ws(), deviceId: id, name });
      await refreshDevices();
    } catch (e) {
      setErr(String(e));
    }
  };

  const mm = (s: number) => `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;

  return (
    <div class="mob-tab">
      <Show when={err()}>
        <div class="wizard-test-result err" style="margin:0 0 10px"><div class="wizard-test-line"><IconClose size={14} /> {err()}</div></div>
      </Show>

      {/* ── Setup (only until a domain is linked) / Connected view ── */}
      <h4 class="ins-h4">{configured() ? t("mobile.proxy") : t("mobile.setup")}</h4>

      <Show when={configured()}>
        {/* A domain is linked + persisted — show it + a disconnect, not the form. */}
        <div class="mob-connected">
          <div class="mob-conn-row">
            <span class={status()!.nginx_active ? "mob-ok" : "mob-warn"}>
              {status()!.nginx_active ? <><IconCircle size={14} /> nginx active</> : <><IconCircle size={14} /> nginx inactive</>}
            </span>
            <span class="mob-conn-domain" dir="ltr">{status()!.domain}</span>
          </div>
          <div class="settings-hint">{t("mobile.connected_note")}</div>
          <button class="ghost-danger" disabled={busy()} onClick={() => void disconnect()}>
            {t("mobile.disconnect")}
          </button>
        </div>
        <Show when={note()}><div class="settings-hint">{note()}</div></Show>
      </Show>

      <Show when={!configured()}>
        <div class="mob-setup">
          <label class="mob-field">
            <span>{t("mobile.domain")}</span>
            {/* dir="auto": a normal (latin) domain aligns LTR, but a full Hebrew/
                Arabic IDN aligns RTL by its own content. */}
            <input
              type="text"
              dir="auto"
              placeholder="ymux.example.com"
              value={domain()}
              onInput={(e) => setDomain(e.currentTarget.value)}
            />
          </label>
          <label class="mob-field">
            <span>{t("mobile.cf_token")}</span>
            {/* The token is always an opaque LTR string — force LTR even in an
                RTL app so it doesn't render reversed. */}
            <input
              type="password"
              dir="ltr"
              placeholder="cloudflare API token"
              value={cfToken()}
              onInput={(e) => setCfToken(e.currentTarget.value)}
            />
          </label>
          <button class="primary" disabled={busy() || !domain().trim() || !cfToken().trim()} onClick={() => void install()}>
            {busy() ? t("mobile.installing") : t("mobile.install")}
          </button>
        </div>

        {/* Cloudflare token requirements — what scopes to grant when creating it. */}
        <div class="mob-cf-help">
          <div class="mob-cf-title">{t("mobile.cf_help_title")}</div>
          <ul class="mob-cf-perms">
            <li><code>Zone</code> · <code>DNS</code> · <code>Edit</code></li>
            <li><code>Zone</code> · <code>Zone</code> · <code>Read</code></li>
            <li>{t("mobile.cf_perm_scope")}</li>
          </ul>
          <div class="settings-hint">{t("mobile.cf_domain_note")}</div>
          <a
            class="mob-cf-link"
            href="https://dash.cloudflare.com/profile/api-tokens"
            onClick={(e) => {
              e.preventDefault();
              void backend.host.openUrl("https://dash.cloudflare.com/profile/api-tokens").catch(() => {});
            }}
          >
            {t("mobile.cf_open_tokens")} →
          </a>
        </div>
        <div class="mob-status">
          <Show when={status()} fallback={<span class="settings-hint">{t("mobile.not_configured")}</span>}>
            <span class={status()!.nginx_active ? "mob-ok" : "mob-warn"}>
              {status()!.nginx_active ? <><IconCircle size={14} /> nginx active</> : <><IconCircle size={14} /> nginx inactive</>}
            </span>
            <Show when={status()!.domain}><span class="settings-hint"> · {status()!.domain}</span></Show>
          </Show>
          <Show when={note()}><div class="settings-hint">{note()}</div></Show>
        </div>
      </Show>

      {/* ── Pairing ── */}
      <h4 class="ins-h4">{t("mobile.pairing")}</h4>
      <div class="mob-pair">
        <input
          type="text"
          placeholder={t("mobile.device_name")}
          value={pairName()}
          onInput={(e) => setPairName(e.currentTarget.value)}
        />
        <button class="primary" disabled={!status()?.configured} onClick={() => void generateQr()}>
          {t("mobile.pair_new")}
        </button>
      </div>
      <Show when={qrSvg()}>
        <div class="mob-qr">
          {/* eslint-disable-next-line solid/no-innerhtml -- our own generated SVG */}
          <div class="mob-qr-img" innerHTML={qrSvg()!} />
          <div class="mob-qr-meta">
            <div class="settings-hint">{t("mobile.scan_hint")}</div>
            <div class="mob-countdown">{t("mobile.expires_in")} {mm(countdown())}</div>
          </div>
        </div>
        {/* Phase 74: the server URL travels out-of-band — user types it into the
            mobile app manually; it is intentionally absent from the QR above. */}
        <Show when={pairUrl()}>
          <div class="mob-url-card">
            <div class="mob-url-title"><IconSmartphone size={14} /> {t("mobile.url_enter")}</div>
            <div class="mob-url-row">
              <input class="mob-url-input" type="text" readOnly dir="ltr" value={pairUrl()!} />
              <button class="primary" onClick={() => void copyUrl()}>
                {copied() ? t("mobile.url_copied") : t("mobile.url_copy")}
              </button>
            </div>
            <div class="settings-hint">{t("mobile.url_then_scan")}</div>
          </div>
        </Show>
      </Show>

      {/* ── Devices / activity ── */}
      <h4 class="ins-h4">{t("mobile.devices")}</h4>
      <Show when={devices().length === 0}>
        <div class="settings-hint">{t("mobile.no_devices")}</div>
      </Show>
      <div class="mob-devices">
        <For each={devices()}>
          {(d) => (
            <div class="mob-dev">
              <span class="mob-dev-name">
                {d.status === "active" ? <IconCircle size={14} /> : d.status === "pending" ? <IconCircle size={14} /> : <IconClose size={14} />} {d.device_name || d.device_id}
              </span>
              <span class="mob-dev-meta settings-hint">
                {d.status} · {fmtWhen(d.last_seen)}{d.last_ip ? ` · ${d.last_ip}` : ""}
              </span>
              <label class="mob-dev-shell settings-checkbox" title={t("mobile.shell.hint")}>
                <input
                  type="checkbox"
                  checked={hasShell(d)}
                  disabled={d.status !== "active"}
                  onChange={(e) => void setShell(d.device_id, e.currentTarget.checked)}
                />
                <span>{t("mobile.shell.label")}</span>
              </label>
              <span class="mob-dev-actions">
                <button onClick={() => void rename(d.device_id, d.device_name)}>{t("common.rename")}</button>
                <button onClick={() => void revoke(d.device_id)}>{t("mobile.revoke")}</button>
              </span>
            </div>
          )}
        </For>
      </div>
    </div>
  );
}
