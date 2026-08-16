import { DisplayReceiverClient } from "./runtime/client.mjs";
import { TizenCredentialVault } from "./tizen-vault.mjs";

const panels = ["booting-panel", "pairing-panel", "unassigned-panel", "frame-panel", "media-panel", "message-panel"];

class TizenReceiverUi {
  constructor() {
    this.client = null;
    this.detailsVisible = false;
    this.confirmButton = document.getElementById("confirm-pairing");
    this.retryButton = document.getElementById("retry-action");
    this.confirmButton.addEventListener("click", () => this.client && this.client.confirmPairing());
    this.retryButton.addEventListener("click", () => window.location.reload());
  }

  bind(client) { this.client = client; }

  show(name) {
    for (const panel of panels) document.getElementById(panel).hidden = panel !== name;
    if (name !== "media-panel") document.querySelector("#program-media video")?.pause();
  }

  showBooting() { this.show("booting-panel"); }
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
    if (retry) this.retryButton.focus();
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

const ui = new TizenReceiverUi();
const mseCapable = typeof MediaSource === "function"
  && typeof WebSocket === "function"
  && MediaSource.isTypeSupported('video/mp4; codecs="avc1.640028"')
  && MediaSource.isTypeSupported('audio/mp4; codecs="mp4a.40.2"');
const capabilities = {
  protocol_major: 1,
  platform: "tizen",
  build: "astrolabe-tizen/0.1.0",
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

const client = new DisplayReceiverClient({
  bootstrap: {
    protocol_major: 1,
    trust: { kind: "web_pki_origin", origin: "https://nixiesoftware.com" },
    certificate_pem: null,
    rendezvous: null,
  },
  capabilities,
  ui,
  vaultFactory: TizenCredentialVault.open,
});
ui.bind(client);

window.addEventListener("keydown", (event) => {
  const keyCode = event.keyCode;
  if (event.key === "Enter" || keyCode === 13) {
    if (!document.getElementById("pairing-panel").hidden) {
      event.preventDefault();
      client.confirmPairing();
    }
  } else if (event.key === "Info" || keyCode === 457) {
    event.preventDefault();
    ui.toggleDetails();
  } else if (event.key === "Escape" || keyCode === 10009) {
    if (!document.getElementById("pairing-panel").hidden) {
      event.preventDefault();
      client.cancelPairing();
    } else if (globalThis.tizen && tizen.application) {
      client.stop();
      tizen.application.getCurrentApplication().exit();
    }
  }
});

window.addEventListener("pagehide", () => client.stop());
client.start();
