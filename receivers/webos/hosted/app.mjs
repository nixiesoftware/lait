import { DisplayReceiverClient } from "./runtime/client.mjs";
import { CredentialVault } from "./runtime/vault.mjs";
import {
  ProvisioningStore,
  deploymentRoot,
  normalizeSiteCode,
  siteOrigin,
  validSiteCode,
  webPkiBootstrap,
} from "./runtime/provisioning.mjs";

const panels = [
  "booting-panel",
  "provisioning-panel",
  "pairing-panel",
  "unassigned-panel",
  "frame-panel",
  "media-panel",
  "message-panel",
];

class WebOsReceiverUi {
  constructor() {
    this.client = null;
    this.detailsVisible = false;
    this.confirmButton = document.getElementById("confirm-pairing");
    this.retryButton = document.getElementById("retry-action");
    this.changeSiteButton = document.getElementById("change-site-action");
    this.canChangeSite = false;
    this.confirmButton.addEventListener("click", () => this.client && this.client.confirmPairing());
    this.retryButton.addEventListener("click", () => window.location.reload());
  }

  bind(client) { this.client = client; }

  show(name) {
    for (const panel of panels) document.getElementById(panel).hidden = panel !== name;
    if (name !== "media-panel") document.querySelector("#program-media video")?.pause();
  }

  showBooting() { this.show("booting-panel"); }

  /// Ask which location this display belongs to, and resolve once it is
  /// stored. The origin is never compiled in: one package serves every site,
  /// and the site is a fact about where the television is standing.
  askForSite(store, parent) {
    return new Promise((resolve) => {
      this.show("provisioning-panel");
      const form = document.getElementById("site-entry");
      const input = document.getElementById("site-code");
      const preview = document.getElementById("site-preview");
      const render = () => {
        const code = normalizeSiteCode(input.value);
        if (!code) {
          preview.textContent = "Enter the code printed on this location's setup card.";
        } else if (validSiteCode(code)) {
          preview.textContent = `This display will reach ${siteOrigin(code, parent).slice("https://".length)}`;
        } else {
          preview.textContent = "A site code is up to 32 letters, digits and hyphens.";
        }
      };
      input.addEventListener("input", render);
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        const code = normalizeSiteCode(input.value);
        if (!validSiteCode(code)) {
          render();
          return;
        }
        preview.textContent = "Storing this display's site…";
        store.save(code, parent).then(resolve, (error) => {
          preview.textContent = `This display could not store its site: ${error.message || error}`;
        });
      });
      input.value = "";
      render();
      input.focus();
    });
  }
  showConnecting() { this.message("Astrolabe Display", "Connecting…", "Authenticating this receiver and requesting its complete current program."); }

  showPairing({ phrase, fingerprint, confirmed }) {
    this.show("pairing-panel");
    const phraseElement = document.getElementById("pairing-phrase");
    phraseElement.replaceChildren(...phrase.map((word) => {
      const span = document.createElement("span");
      span.textContent = word;
      return span;
    }));
    document.getElementById("pairing-fingerprint").textContent = fingerprint.match(/.{1,8}/g).join(" ");
    this.confirmButton.hidden = confirmed;
    document.getElementById("pairing-state").textContent = confirmed
      ? "Confirmed on this display. Waiting for authenticated approval in Astrolabe…"
      : "Nothing is enrolled until you confirm the match here and approve it in Astrolabe.";
    if (!confirmed) this.confirmButton.focus();
  }

  showPairingWaiting() {
    document.getElementById("pairing-state").textContent = "This display is confirmed. Waiting for approval in Astrolabe…";
  }
  showPairingNetworkError() {
    document.getElementById("pairing-state").textContent = "Coordinator unavailable. Pairing will retry without changing the words.";
  }
  showPairingRejected(kind, reason) {
    this.message("Pairing stopped", kind === "expired" ? "Pairing expired" : "Pairing was not approved", reason || "Start a new ceremony from this display.", true);
  }

  showUnassigned(device) {
    this.show("unassigned-panel");
    document.getElementById("device-id").textContent = device;
    this.setSourceState("none");
  }

  showFrame(url, summary) {
    this.show("frame-panel");
    const image = document.getElementById("program-frame");
    image.src = url;
    image.alt = summary || "Assigned Astrolabe display frame";
  }

  showMedia(session, summary) {
    this.show("media-panel");
    session.mount(document.getElementById("program-media"), summary);
  }

  showBlank(reason) {
    const messages = {
      unassigned: ["Ready for an assignment", "Choose this display in Astrolabe Displays."],
      host_unavailable: ["Coordinator unavailable", "The assigned content is no longer eligible to remain on screen."],
      source_unavailable: ["Source unavailable", "Astrolabe could not produce a trustworthy current frame."],
      unsupported: ["Program unsupported", "This receiver refused an output it cannot interpret safely."],
      revoked: ["Display revoked", "This receiver no longer has an active assignment."],
      program_ended: ["Program complete", "Astrolabe is waiting for a newer assigned program."],
    };
    const selected = messages[reason] || messages.unsupported;
    this.message("Receiver-owned state", selected[0], selected[1]);
  }

  showRevoked() { this.message("Receiver access", "This display was revoked", "Staged content has been cleared. Re-enroll it from Astrolabe if access should return."); }
  showRePair(reason) { this.message("Trust changed", "Pairing is required again", reason); }
  showPendingAtEnd() { this.message("Program boundary", "Waiting for the next program", "The last verified frame has ended. Astrolabe is requesting a complete snapshot."); }
  showRecovering(code) { document.getElementById("pairing-state").textContent = `Delivery interrupted (${code}). Retrying with bounded backoff.`; }
  showFailure(code, detail) { this.message("Receiver refused", "Astrolabe cannot continue safely", `${code}: ${detail}`, true); }

  message(eyebrow, title, body, retry = false) {
    this.show("message-panel");
    document.getElementById("message-eyebrow").textContent = eyebrow;
    document.getElementById("message-title").textContent = title;
    document.getElementById("message-body").textContent = body;
    this.retryButton.hidden = !retry;
    this.changeSiteButton.hidden = !this.canChangeSite;
    if (retry) this.retryButton.focus();
  }

  /// A mistyped site code is a display that reaches a coordinator nobody
  /// deployed, and every refusal after that names the wrong thing. Offer the
  /// way back — but only while nothing is enrolled, because after enrollment
  /// the site is not a typo to correct, it is a credential to revoke, and that
  /// decision belongs to Astrolabe rather than to whoever holds the remote.
  allowChangeSite(unenrolled, forget) {
    this.canChangeSite = unenrolled;
    if (!unenrolled) return;
    this.changeSiteButton.addEventListener("click", () => {
      this.changeSiteButton.disabled = true;
      forget().then(() => window.location.reload(), (error) => {
        document.getElementById("message-body").textContent = `This display could not forget its site: ${error.message || error}`;
        this.changeSiteButton.disabled = false;
      });
    });
  }

  setTransportState(state) {
    const element = document.getElementById("transport-state");
    element.dataset.state = state;
    element.textContent = state === "online" ? "Online" : state === "offline" ? "Offline" : "Connecting";
    document.getElementById("detail-transport").textContent = element.textContent;
  }

  setSourceState(state) {
    const element = document.getElementById("source-state");
    element.dataset.state = state;
    element.textContent = state === "none" ? "No source" : state[0].toUpperCase() + state.slice(1);
    document.getElementById("detail-source").textContent = element.textContent;
  }

  setStaleState(stale) {
    document.getElementById("stale-state").hidden = !stale;
    document.getElementById("detail-delivery").textContent = stale ? "Stale" : "Current";
  }

  toggleDetails() {
    this.detailsVisible = !this.detailsVisible;
    document.getElementById("detail-panel").hidden = !this.detailsVisible;
  }
}

const ui = new WebOsReceiverUi();
const mseCapable = typeof MediaSource === "function"
  && typeof WebSocket === "function"
  && MediaSource.isTypeSupported('video/mp4; codecs="avc1.640028"')
  && MediaSource.isTypeSupported('audio/mp4; codecs="mp4a.40.2"');
const capabilities = {
  protocol_major: 1,
  platform: "webos",
  build: "astrolabe-webos/0.1.0",
  viewport: {
    width: Math.min(window.screen.width || 1920, 4096),
    height: Math.min(window.screen.height || 1080, 2160),
    scale_milli: 1000,
  },
  image_types: ["image_jpeg", "image_png", "image_webp"],
  max_asset_bytes: 16777216,
  max_staged_bytes: 50331648,
  max_program_items: 16,
  max_staging_horizon_ms: 86400000,
  locale: (navigator.language || "en-US").slice(0, 35),
  accessibility: {
    native_screen_reader: true,
    spoken_summary: true,
    captions: false,
    audio_description: false,
  },
  playback: {
    tier: mseCapable ? "mse_live" : "frame",
    sync_class: mseCapable ? "positional_b" : "boundary",
    rate_control_probed: false,
    latency_class: mseCapable ? "near_realtime" : "snapshot",
    health_granularity: "full",
  },
};

let client = null;

window.addEventListener("keydown", (event) => {
  const keyCode = event.keyCode;
  const pairing = client && !document.getElementById("pairing-panel").hidden;
  if (event.key === "Enter" || keyCode === 13) {
    if (pairing) {
      event.preventDefault();
      client.confirmPairing();
    }
  } else if (event.key === "Info" || keyCode === 457) {
    event.preventDefault();
    ui.toggleDetails();
  } else if ((event.key === "Escape" || keyCode === 461) && pairing) {
    event.preventDefault();
    client.cancelPairing();
  }
});

window.addEventListener("pagehide", () => client && client.stop());

// The origin is resolved before the client exists, because the client takes
// its coordinator as a constructor argument and has no notion of not having
// one yet. A stored site starts silently; an unprovisioned display asks.
async function boot() {
  ui.showBooting();
  const root = deploymentRoot(window.location.hostname);
  const store = await ProvisioningStore.open();
  const site = (await store.read(root)) || (await ui.askForSite(store, root));

  const vault = await CredentialVault.open();
  const enrolled = Boolean(await vault.load().catch(() => null));
  vault.close();
  ui.allowChangeSite(!enrolled, () => store.clear());

  client = new DisplayReceiverClient({
    bootstrap: webPkiBootstrap(site.origin),
    capabilities,
    ui,
  });
  ui.bind(client);
  client.start();
}

boot().catch((error) => ui.showFailure(error.code || "provisioning_failed", error.message || String(error)));
