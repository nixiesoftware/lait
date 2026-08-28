import { DisplayReceiverClient } from "./runtime/client.mjs";
import { groupRendezvousCode, rendezvousFromCode } from "./runtime/protocol.mjs";
import { CredentialVault } from "./runtime/vault.mjs";
import {
  ProvisioningStore,
  deploymentRoot,
  parseEntry,
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

  /// Ask what Astrolabe showed: a site, and the code beside it if a display
  /// was added there. The origin is never compiled in — one package serves
  /// every site, and the site is a fact about where the television stands.
  /// A site with no code is the long way, the words ceremony, and is still
  /// a way in.
  ///
  /// Resolves `{ site, rendezvous }` once the site is stored: `site` is the
  /// stored record, `rendezvous` the wire id the code names, or `null`.
  askForEntry(store, parent, storedSite) {
    return new Promise((resolve) => {
      this.show("provisioning-panel");
      const form = document.getElementById("site-entry");
      const input = document.getElementById("site-code");
      const preview = document.getElementById("site-preview");
      const render = () => {
        const entry = parseEntry(input.value);
        if (!entry.site) {
          preview.textContent = "Enter the code shown in Astrolabe, or just this location's site name.";
        } else if (!validSiteCode(entry.site)) {
          preview.textContent = "A site name is up to 32 letters, digits and hyphens.";
        } else if (entry.code) {
          preview.textContent = `This display will connect to ${siteOrigin(entry.site, parent).slice("https://".length)} with code ${groupRendezvousCode(entry.code)}.`;
        } else {
          preview.textContent = `This display will reach ${siteOrigin(entry.site, parent).slice("https://".length)} and show words to compare in Astrolabe.`;
        }
      };
      input.addEventListener("input", render);
      form.addEventListener("submit", (event) => {
        event.preventDefault();
        const entry = parseEntry(input.value);
        if (!validSiteCode(entry.site)) {
          render();
          return;
        }
        preview.textContent = "Storing this display's site…";
        Promise.all([
          store.save(entry.site, parent),
          entry.code ? rendezvousFromCode(entry.code) : Promise.resolve(null),
        ]).then(([site, rendezvous]) => resolve({ site, rendezvous }), (error) => {
          preview.textContent = `This display could not store its site: ${error.message || error}`;
        });
      });
      // A display that already knows its site is most likely here holding a
      // code: start it after the site, ready for the rest.
      input.value = storedSite ? `${storedSite}-` : "";
      render();
      input.focus();
      if (storedSite) input.setSelectionRange(input.value.length, input.value.length);
    });
  }
  showConnecting() { this.message("Astrolabe Display", "Connecting…", "Authenticating this receiver and requesting its complete current program."); }

  showPairing({ phrase, fingerprint, confirmed, viaCode = false }) {
    if (viaCode) {
      // The code was the confirmation, made at the controller. Nothing here
      // is compared, and nothing here is pressed.
      this.message("Connecting this display", "Code accepted", "Enrolling with Astrolabe…");
      return;
    }
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
  showFailure(code, detail) {
    if (code === "rendezvous_refused") {
      // The one refusal a person at the television can act on. The site is
      // kept; the retry lands back on the entry screen with it filled in.
      this.message(
        "Code not accepted",
        "That code isn't one this site holds",
        "A code works once and lasts fifteen minutes. Get a fresh one in Astrolabe: Displays, then Add a display.",
        true,
      );
      return;
    }
    this.message("Receiver refused", "Astrolabe cannot continue safely", `${code}: ${detail}`, true);
  }

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
// one yet. An enrolled display with a stored site starts silently. Anything
// else asks — with the site filled in when one is stored — because what a
// person is most likely holding at that screen is a code from Astrolabe.
async function boot() {
  ui.showBooting();
  const root = deploymentRoot(window.location.hostname);
  const store = await ProvisioningStore.open();
  const stored = await store.read(root);

  const vault = await CredentialVault.open();
  const enrolled = Boolean(await vault.load().catch(() => null));
  vault.close();

  const entry = stored && enrolled
    ? { site: stored, rendezvous: null }
    : await ui.askForEntry(store, root, stored ? stored.code : null);
  ui.allowChangeSite(!enrolled, () => store.clear());

  client = new DisplayReceiverClient({
    bootstrap: webPkiBootstrap(entry.site.origin, entry.rendezvous),
    capabilities,
    ui,
  });
  ui.bind(client);
  client.start();
}

boot().catch((error) => ui.showFailure(error.code || "provisioning_failed", error.message || String(error)));
