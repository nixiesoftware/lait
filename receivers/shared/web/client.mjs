import {
  authenticatePairingComplete,
  authenticatePairingStatus,
  authenticateRequest,
  BOUNDS,
  bytesToHex,
  confirmationPhrase,
  isLowerHex,
  PROTOCOL_MAJOR,
  ProtocolError,
  randomHex,
  sha256,
  verifyProgram,
} from "./protocol.mjs";
import { boundedBytes, boundedJson } from "./transport.mjs";
import { CredentialVault } from "./vault.mjs";

const encoder = new TextEncoder();
const JSON_LIMIT = 64 * 1024;
const PAIRING_LIMIT = 16 * 1024;
const LONG_POLL_WAIT_MS = 25_000;

const MEDIA_TYPES = Object.freeze({
  image_png: "image/png",
  image_jpeg: "image/jpeg",
  image_webp: "image/webp",
});

function exactFields(value, fields, name) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ProtocolError("invalid_shape", `${name} must be an object`);
  }
  const actual = Object.keys(value).sort();
  const expected = [...fields].sort();
  if (actual.length !== expected.length || actual.some((field, index) => field !== expected[index])) {
    throw new ProtocolError("unknown_field", `${name} fields do not match protocol major 1`);
  }
}

function delay(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function coordinatorOrigin(origin) {
  const parsed = new URL(origin);
  if (parsed.protocol !== "https:" || parsed.username || parsed.password || parsed.search || parsed.hash) {
    throw new ProtocolError("invalid_origin", "Coordinator origin must be credential-free HTTPS");
  }
  return parsed.origin;
}

function bodyDigestInput(body) {
  return body == null ? new Uint8Array() : encoder.encode(JSON.stringify(body));
}

function sourceKind(program) {
  const item = program.items[program.playback.current_index];
  if (program.program_state.kind === "unavailable" || item.source_state.kind === "unavailable") {
    return "unavailable";
  }
  if (program.program_state.kind === "partial" || item.source_state.kind === "partial") {
    return "partial";
  }
  return "current";
}

async function decodeFrame(arrayBuffer, asset) {
  const mediaType = MEDIA_TYPES[asset.media_type];
  if (!mediaType) throw new ProtocolError("unsupported", "Receiver supports frame assets only");
  const blob = new Blob([arrayBuffer], { type: mediaType });
  const url = URL.createObjectURL(blob);
  try {
    const dimensions = await new Promise((resolve, reject) => {
      const image = new Image();
      image.onload = () => resolve({ width: image.naturalWidth, height: image.naturalHeight });
      image.onerror = () => reject(new ProtocolError("asset_decode", "Frame image could not be decoded"));
      image.src = url;
    });
    if (dimensions.width !== asset.width || dimensions.height !== asset.height) {
      throw new ProtocolError("asset_dimensions", "Decoded frame dimensions do not match the snapshot");
    }
    return { url, asset };
  } catch (error) {
    URL.revokeObjectURL(url);
    throw error;
  }
}

export class DisplayReceiverClient {
  constructor({ origin, capabilities, ui, rendezvous = null, vaultFactory = CredentialVault.open }) {
    this.origin = coordinatorOrigin(origin);
    this.capabilities = capabilities;
    this.ui = ui;
    this.rendezvous = rendezvous;
    this.vaultFactory = vaultFactory;
    this.vault = null;
    this.credential = null;
    this.challenge = null;
    this.program = null;
    this.staged = new Map();
    this.itemStartedAt = 0;
    this.elapsedBase = 0;
    this.playbackTimer = null;
    this.staleTimer = null;
    this.lastProgramDeliveryAt = 0;
    this.lastHealthAt = 0;
    this.deliveryStale = false;
    this.running = false;
    this.pairingPoll = null;
    this.pairingGeneration = 0;
  }

  async start() {
    this.running = true;
    this.ui.showBooting();
    try {
      this.vault = await this.vaultFactory();
      this.credential = await this.vault.load();
      if (this.credential && this.credential.origin !== this.origin) {
        throw new ProtocolError("coordinator_changed", "Stored credential belongs to another coordinator");
      }
      if (!this.credential) {
        await this.startPairing();
        return;
      }
      if (this.credential.mode === "pairing") {
        this.presentPairing();
        if (this.credential.userConfirmed) this.pollPairing().catch((error) => this.fail(error));
        return;
      }
      if (this.credential.mode === "enrolling") {
        await this.finishEnrollment();
      }
      if (this.credential.mode !== "paired") {
        throw new ProtocolError("credential_corrupt", "Unknown receiver credential state");
      }
      this.runProgramLoop();
    } catch (error) {
      this.fail(error);
    }
  }

  stop() {
    this.running = false;
    this.pairingGeneration += 1;
    clearTimeout(this.playbackTimer);
    clearInterval(this.staleTimer);
    this.releaseStage(this.staged);
    this.staged = new Map();
    if (this.vault) this.vault.close();
  }

  fail(error) {
    const protocolError = error instanceof ProtocolError
      ? error
      : new ProtocolError("internal", String(error));
    this.ui.showFailure(protocolError.code, protocolError.message);
  }

  async publicJson(path, method = "GET", body = null) {
    const response = await boundedJson({
      method,
      url: `${this.origin}${path}`,
      body,
      maximumBytes: PAIRING_LIMIT,
      timeoutMs: 30_000,
    });
    if (response.status < 200 || response.status >= 300) {
      throw new ProtocolError("coordinator_refused", `Coordinator returned HTTP ${response.status}`);
    }
    return response.body;
  }

  async fetchInstance() {
    const instance = await this.publicJson("/head/v1/instance");
    exactFields(instance, ["protocol_major", "instance", "label", "trust"], "coordinator instance");
    if (instance.protocol_major !== PROTOCOL_MAJOR
      || !isLowerHex(instance.instance, 32)
      || typeof instance.label !== "string"
      || new TextEncoder().encode(instance.label).byteLength < 1
      || new TextEncoder().encode(instance.label).byteLength > 96
      || /[\u0000-\u001f\u007f-\u009f]/u.test(instance.label)) {
      throw new ProtocolError("unsupported", "Coordinator does not speak protocol major 1");
    }
    exactFields(instance.trust, ["kind", "origin"], "coordinator trust");
    if (instance.trust.kind !== "web_pki_origin"
      || coordinatorOrigin(instance.trust.origin) !== this.origin) {
      throw new ProtocolError(
        "unsupported_trust",
        "This web receiver requires a matching Web PKI coordinator origin",
      );
    }
    return instance;
  }

  async startPairing() {
    const instance = await this.fetchInstance();
    const receiverNonce = randomHex(32);
    const pollKey = randomHex(32);
    const request = {
      protocol_major: PROTOCOL_MAJOR,
      receiver_nonce: receiverNonce,
      poll_key: pollKey,
      rendezvous: this.rendezvous,
      capabilities: this.capabilities,
    };
    const response = await this.publicJson("/head/v1/pairings", "POST", request);
    exactFields(
      response,
      ["protocol_major", "pairing", "expires_in_ms", "confirmation_phrase", "coordinator_fingerprint"],
      "pairing start response",
    );
    if (response.protocol_major !== PROTOCOL_MAJOR
      || !isLowerHex(response.pairing, 32)
      || !isLowerHex(response.coordinator_fingerprint, 64)
      || !Number.isSafeInteger(response.expires_in_ms)
      || response.expires_in_ms <= 0
      || response.expires_in_ms > 600_000) {
      throw new ProtocolError("invalid_pairing", "Coordinator returned an invalid pairing offer");
    }
    const expectedPhrase = await confirmationPhrase(
      response.coordinator_fingerprint,
      response.pairing,
      receiverNonce,
    );
    if (JSON.stringify(expectedPhrase) !== JSON.stringify(response.confirmation_phrase)) {
      throw new ProtocolError("pairing_integrity", "Pairing confirmation phrase did not verify");
    }
    this.credential = {
      mode: "pairing",
      origin: this.origin,
      pairing: response.pairing,
      receiverNonce,
      pollKey,
      fingerprint: response.coordinator_fingerprint,
      phrase: expectedPhrase,
      userConfirmed: false,
    };
    await this.vault.save(this.credential);
    this.presentPairing();
  }

  presentPairing() {
    this.ui.showPairing({
      phrase: this.credential.phrase,
      fingerprint: this.credential.fingerprint,
      confirmed: this.credential.userConfirmed,
    });
  }

  async confirmPairing() {
    if (!this.credential || this.credential.mode !== "pairing" || this.credential.userConfirmed) return;
    this.credential = { ...this.credential, userConfirmed: true };
    await this.vault.save(this.credential);
    this.presentPairing();
    this.pollPairing().catch((error) => this.fail(error));
  }

  async cancelPairing() {
    if (!this.credential || this.credential.mode !== "pairing") return;
    this.pairingGeneration += 1;
    this.pairingPoll = null;
    await this.vault.clear();
    this.credential = null;
    await this.startPairing();
  }

  pollPairing() {
    if (this.pairingPoll) return this.pairingPoll;
    const generation = this.pairingGeneration;
    let poll;
    poll = this.pollPairingLoop(generation).finally(() => {
      if (this.pairingPoll === poll) this.pairingPoll = null;
    });
    this.pairingPoll = poll;
    return poll;
  }

  async pollPairingLoop(generation) {
    while (this.running
      && generation === this.pairingGeneration
      && this.credential
      && this.credential.mode === "pairing") {
      try {
        const current = this.credential;
        const proof = await authenticatePairingStatus(
          current.pollKey,
          current.pairing,
        );
        const response = await this.publicJson("/head/v1/pairings/status", "POST", {
          protocol_major: PROTOCOL_MAJOR,
          pairing: current.pairing,
          proof,
        });
        if (!this.running
          || generation !== this.pairingGeneration
          || !this.credential
          || this.credential.mode !== "pairing"
          || this.credential.pairing !== current.pairing
          || this.credential.pollKey !== current.pollKey) return;
        if (response.kind === "pending") {
          exactFields(response, ["kind", "retry_after_ms"], "pending pairing status");
          if (!Number.isSafeInteger(response.retry_after_ms)
            || response.retry_after_ms < 1
            || response.retry_after_ms > 60_000) {
            throw new ProtocolError("invalid_pairing", "Pairing retry interval is outside protocol bounds");
          }
          this.ui.showPairingWaiting();
          await delay(Math.max(response.retry_after_ms, 1000));
          continue;
        }
        if (response.kind === "rejected") {
          exactFields(response, ["kind", "reason"], "rejected pairing status");
          if (!["user_rejected", "controller_unavailable", "policy_refused", "fingerprint_mismatch"].includes(response.reason)) {
            throw new ProtocolError("invalid_pairing", "Unknown pairing rejection reason");
          }
          this.ui.showPairingRejected(response.kind, response.reason || null);
          return;
        }
        if (response.kind === "expired") {
          exactFields(response, ["kind"], "expired pairing status");
          this.ui.showPairingRejected(response.kind, null);
          return;
        }
        if (response.kind !== "approved") {
          throw new ProtocolError("invalid_pairing", "Unknown pairing status");
        }
        exactFields(
          response,
          ["kind", "device", "proof_key", "enrollment_challenge"],
          "approved pairing status",
        );
        if (!isLowerHex(response.device, 32)
          || !isLowerHex(response.proof_key, 64)
          || !isLowerHex(response.enrollment_challenge, 64)) {
          throw new ProtocolError("invalid_pairing", "Approved credential has an invalid encoding");
        }
        this.credential = {
          mode: "enrolling",
          origin: this.origin,
          pairing: current.pairing,
          device: response.device,
          proofKey: response.proof_key,
          enrollmentChallenge: response.enrollment_challenge,
        };
        await this.vault.save(this.credential);
        await this.finishEnrollment();
        this.runProgramLoop();
        return;
      } catch (error) {
        if (!this.running || generation !== this.pairingGeneration) return;
        this.ui.showPairingNetworkError();
        await delay(3000);
        if (!(error instanceof ProtocolError) || !["network", "timeout"].includes(error.code)) {
          throw error;
        }
      }
    }
  }

  async finishEnrollment() {
    const proof = await authenticatePairingComplete(
      this.credential.proofKey,
      this.credential.pairing,
      this.credential.device,
      this.credential.enrollmentChallenge,
    );
    const response = await this.publicJson("/head/v1/pairings/complete", "POST", {
      protocol_major: PROTOCOL_MAJOR,
      pairing: this.credential.pairing,
      device: this.credential.device,
      enrollment_challenge: this.credential.enrollmentChallenge,
      proof,
    });
    if (response.kind !== "enrolled" && response.kind !== "already_enrolled") {
      throw new ProtocolError("invalid_pairing", "Pairing completion did not enroll this receiver");
    }
    exactFields(response, ["kind", "device", "next_challenge"], "pairing completion");
    if (response.device !== this.credential.device || !isLowerHex(response.next_challenge, 64)) {
      throw new ProtocolError("pairing_integrity", "Pairing completion changed receiver identity");
    }
    this.challenge = response.next_challenge;
    this.credential = {
      mode: "paired",
      origin: this.origin,
      device: this.credential.device,
      proofKey: this.credential.proofKey,
    };
    await this.vault.save(this.credential);
  }

  async ensureChallenge() {
    if (this.challenge) return;
    const response = await this.publicJson("/head/v1/challenges", "POST", {
      protocol_major: PROTOCOL_MAJOR,
      device: this.credential.device,
    });
    exactFields(response, ["protocol_major", "challenge", "expires_in_ms"], "challenge response");
    if (response.protocol_major !== PROTOCOL_MAJOR
      || !isLowerHex(response.challenge, 64)
      || !Number.isSafeInteger(response.expires_in_ms)
      || response.expires_in_ms <= 0
      || response.expires_in_ms > 120_000) {
      throw new ProtocolError("invalid_challenge", "Coordinator returned an invalid challenge");
    }
    this.challenge = response.challenge;
  }

  requestHeaders(context, authenticationTag) {
    const headers = {
      Authorization: `Astrolabe-HMAC ${authenticationTag}`,
      "X-Astrolabe-Protocol-Major": String(PROTOCOL_MAJOR),
      "X-Astrolabe-Route": context.route,
      "X-Astrolabe-Device": context.device,
      "X-Astrolabe-Challenge": context.challenge,
      "X-Astrolabe-Body-SHA256": context.bodySha256,
    };
    const optional = [
      ["X-Astrolabe-Assignment", context.assignment],
      ["X-Astrolabe-Program", context.program],
      ["X-Astrolabe-Revision", context.revision],
      ["X-Astrolabe-Current-Item", context.currentItem],
      ["X-Astrolabe-Elapsed-Ms", context.elapsedMs],
      ["X-Astrolabe-Wait-Ms", context.waitMs],
      ["X-Astrolabe-Asset", context.asset],
      ["X-Astrolabe-Range-Start", context.range ? context.range.start : null],
      ["X-Astrolabe-Range-Length", context.range ? context.range.length : null],
    ];
    for (const [name, value] of optional) if (value != null) headers[name] = String(value);
    return headers;
  }

  currentContext(route, method, body, overrides = {}) {
    const playback = this.currentPlayback();
    return {
      protocolMajor: PROTOCOL_MAJOR,
      method,
      route,
      device: this.credential.device,
      assignment: this.program ? this.program.assignment : null,
      program: this.program ? this.program.program : null,
      revision: this.program ? this.program.revision : null,
      currentItem: this.program ? this.program.items[playback.currentIndex].id : null,
      elapsedMs: this.program ? playback.elapsedMs : null,
      waitMs: null,
      asset: null,
      range: null,
      challenge: this.challenge,
      bodySha256: null,
      ...overrides,
    };
  }

  async authorizedJson({ route, method, path, body = null, overrides = {}, timeoutMs = 30_000 }) {
    await this.ensureChallenge();
    const bodySha256 = await sha256(bodyDigestInput(body));
    const context = this.currentContext(route, method, body, { ...overrides, bodySha256 });
    const tag = await authenticateRequest(this.credential.proofKey, context);
    const consumedChallenge = this.challenge;
    this.challenge = null;
    try {
      const response = await boundedJson({
        method,
        url: `${this.origin}${path}`,
        body,
        headers: this.requestHeaders(context, tag),
        maximumBytes: JSON_LIMIT,
        timeoutMs,
      });
      if (response.nextChallenge && isLowerHex(response.nextChallenge, 64)) {
        this.challenge = response.nextChallenge;
      }
      if (response.status < 200 || response.status >= 300) {
        this.handleApiError(response.body, response.status);
      }
      if (!this.challenge) {
        throw new ProtocolError("invalid_challenge", "Authenticated response omitted its next challenge");
      }
      this.ui.setTransportState("online");
      return response.body;
    } catch (error) {
      this.challenge = null;
      this.ui.setTransportState("offline");
      if (error instanceof ProtocolError) throw error;
      throw new ProtocolError("network", `Request using challenge ${consumedChallenge} failed`);
    }
  }

  handleApiError(body, status) {
    exactFields(body, ["protocol_major", "code", "retry_after_ms", "next_challenge"], "API error");
    const codes = [
      "invalid_request", "authentication_failed", "challenge_expired", "challenge_consumed",
      "not_enrolled", "unassigned", "revoked", "re_pair_required", "unsupported_protocol",
      "bound_exceeded", "temporarily_unavailable",
    ];
    if (body.protocol_major !== PROTOCOL_MAJOR
      || !codes.includes(body.code)
      || (body.retry_after_ms !== null
        && (!Number.isSafeInteger(body.retry_after_ms) || body.retry_after_ms < 1 || body.retry_after_ms > 60_000))
      || (body.next_challenge !== null && !isLowerHex(body.next_challenge, 64))) {
      throw new ProtocolError("invalid_api_error", `Malformed coordinator error at HTTP ${status}`);
    }
    if (body && body.code === "re_pair_required") {
      this.rePair("Coordinator requires a new trust ceremony");
      throw new ProtocolError("re_pair_required", "Coordinator requires re-pairing");
    }
    if (body && body.code === "revoked") {
      this.handleRevoked();
      throw new ProtocolError("revoked", "Receiver was revoked");
    }
    throw new ProtocolError(body && body.code ? body.code : "coordinator_refused", `HTTP ${status}`);
  }

  async authorizedAsset(asset, program) {
    await this.ensureChallenge();
    const emptyDigest = await sha256(new Uint8Array());
    const context = this.currentContext("asset", "GET", null, {
      assignment: program.assignment,
      program: program.program,
      revision: program.revision,
      currentItem: null,
      elapsedMs: null,
      waitMs: null,
      asset: asset.id,
      bodySha256: emptyDigest,
    });
    const tag = await authenticateRequest(this.credential.proofKey, context);
    this.challenge = null;
    try {
      const response = await boundedBytes({
        method: "GET",
        url: `${this.origin}/head/v1/assets/${encodeURIComponent(asset.id)}`,
        headers: this.requestHeaders(context, tag),
        maximumBytes: asset.encoded_len,
        timeoutMs: 30_000,
      });
      if (response.nextChallenge && isLowerHex(response.nextChallenge, 64)) {
        this.challenge = response.nextChallenge;
      }
      if (response.status < 200 || response.status >= 300 || !this.challenge) {
        throw new ProtocolError("asset_transfer", `Asset request returned HTTP ${response.status}`);
      }
      if (!response.body || response.body.byteLength !== asset.encoded_len) {
        throw new ProtocolError("asset_length", "Asset length does not match the snapshot");
      }
      if ((response.contentType.split(";", 1)[0] || "").toLowerCase() !== MEDIA_TYPES[asset.media_type]) {
        throw new ProtocolError("asset_media_type", "Asset media type does not match the snapshot");
      }
      const digest = await sha256(new Uint8Array(response.body));
      if (digest !== asset.sha256) {
        throw new ProtocolError("asset_digest", "Asset SHA-256 does not match the snapshot");
      }
      this.ui.setTransportState("online");
      return decodeFrame(response.body, asset);
    } catch (error) {
      this.challenge = null;
      this.ui.setTransportState("offline");
      throw error;
    }
  }

  async negotiateCapabilities() {
    const response = await this.authorizedJson({
      route: "capabilities",
      method: "POST",
      path: "/head/v1/capabilities",
      body: this.capabilities,
    });
    exactFields(response, ["kind"], "capability response");
    if (response.kind !== "accepted") {
      throw new ProtocolError("capability_refused", "Coordinator refused receiver capabilities");
    }
  }

  async runProgramLoop() {
    if (!this.running || this.credential.mode !== "paired") return;
    this.ui.showConnecting();
    this.startStaleMonitor();
    let backoff = 1000;
    let capabilitiesAccepted = false;

    while (this.running && this.credential.mode === "paired") {
      try {
        if (!capabilitiesAccepted) {
          await this.negotiateCapabilities();
          capabilitiesAccepted = true;
        }
        const response = this.program
          ? await this.authorizedJson({
            route: "program_changes",
            method: "GET",
            path: "/head/v1/program/changes",
            overrides: { waitMs: LONG_POLL_WAIT_MS },
            timeoutMs: LONG_POLL_WAIT_MS + 10_000,
          })
          : await this.authorizedJson({
            route: "program_snapshot",
            method: "GET",
            path: "/head/v1/program",
          });
        await this.handleProgramResponse(response);
        if (this.program && performance.now() - this.lastHealthAt >= 30_000) {
          await this.reportHealth();
        }
        backoff = 1000;
        if (!this.program) await delay(5000);
      } catch (error) {
        if (!this.running || ["revoked", "re_pair_required"].includes(error.code)) return;
        this.ui.showRecovering(error.code || "network");
        await delay(backoff);
        backoff = Math.min(backoff * 2, 30_000);
      }
    }
  }

  async handleProgramResponse(response) {
    if (!response || typeof response.kind !== "string") {
      throw new ProtocolError("invalid_program_response", "Program route returned no closed outcome");
    }
    switch (response.kind) {
      case "snapshot":
        exactFields(response, ["kind", "program"], "program snapshot response");
        await this.adoptProgram(response.program);
        return;
      case "no_change":
        exactFields(response, ["kind", "revision", "playback"], "program no-change response");
        if (!this.program || response.revision !== this.program.revision) {
          throw new ProtocolError("invalid_revision", "No-change cursor does not name the current program");
        }
        this.adoptCursor(response.playback, true);
        return;
      case "reset":
        exactFields(response, ["kind", "reason"], "program reset response");
        this.clearProgram();
        return;
      case "unassigned":
        exactFields(response, ["kind"], "unassigned response");
        this.clearProgram();
        this.ui.showUnassigned(this.credential.device);
        this.lastProgramDeliveryAt = performance.now();
        return;
      case "revoked":
        exactFields(response, ["kind"], "revoked response");
        await this.handleRevoked();
        return;
      case "re_pair":
        exactFields(response, ["kind"], "re-pair response");
        await this.rePair("Coordinator trust or receiver credential changed");
        return;
      default:
        throw new ProtocolError("unsupported", "Unknown program outcome");
    }
  }

  async adoptProgram(program) {
    await verifyProgram(program);
    if (this.program && this.program.revision === program.revision) {
      this.program = program;
      this.adoptCursor(program.playback, true);
      return;
    }
    const staged = await this.stageProgram(program);
    const previous = this.staged;
    this.staged = staged;
    this.program = program;
    this.adoptCursor(program.playback, true);
    requestAnimationFrame(() => this.releaseStage(previous));
  }

  async stageProgram(program) {
    const assets = new Map();
    let stagedBytes = 0;
    for (const item of program.items) {
      if (item.scene.kind === "media") {
        throw new ProtocolError("unsupported", "Production media is disabled until byte grants ship");
      }
      if (item.scene.kind !== "frame") continue;
      const asset = item.scene.asset;
      if (assets.has(asset.id)) continue;
      stagedBytes += asset.encoded_len;
      if (stagedBytes > this.capabilities.max_staged_bytes || stagedBytes > BOUNDS.maxStagedBytes) {
        throw new ProtocolError("bound_exceeded", "Program exceeds negotiated staging bytes");
      }
      assets.set(asset.id, await this.authorizedAsset(asset, program));
    }
    return assets;
  }

  releaseStage(stage) {
    for (const entry of stage.values()) URL.revokeObjectURL(entry.url);
  }

  clearProgram() {
    clearTimeout(this.playbackTimer);
    this.releaseStage(this.staged);
    this.staged = new Map();
    this.program = null;
    this.deliveryStale = false;
    this.ui.setStaleState(false);
  }

  currentPlayback() {
    if (!this.program) return { currentIndex: 0, elapsedMs: 0 };
    const elapsed = this.elapsedBase + Math.max(0, Math.floor(performance.now() - this.itemStartedAt));
    return { currentIndex: this.program.playback.current_index, elapsedMs: Math.min(elapsed, 0xffffffff) };
  }

  adoptCursor(playback, programDelivery = false) {
    exactFields(playback, ["current_index", "elapsed_ms", "cycle"], "playback cursor");
    if (!this.program
      || !Number.isSafeInteger(playback.current_index)
      || playback.current_index < 0
      || playback.current_index >= this.program.items.length
      || !Number.isSafeInteger(playback.elapsed_ms)
      || playback.elapsed_ms < 0
      || playback.cycle !== this.program.playback.cycle
      || (this.program.items[playback.current_index].duration_ms != null
        && playback.elapsed_ms >= this.program.items[playback.current_index].duration_ms)) {
      throw new ProtocolError("invalid_cursor", "Coordinator cursor is outside the current program");
    }
    if (programDelivery) this.lastProgramDeliveryAt = performance.now();
    this.program.playback.current_index = playback.current_index;
    this.program.playback.elapsed_ms = playback.elapsed_ms;
    this.elapsedBase = playback.elapsed_ms;
    this.itemStartedAt = performance.now();
    this.renderCurrent();
  }

  renderCurrent() {
    clearTimeout(this.playbackTimer);
    if (!this.program) return;
    const index = this.program.playback.current_index;
    const item = this.program.items[index];
    const state = sourceKind(this.program);
    const stale = Boolean(this.lastProgramDeliveryAt)
      && performance.now() - this.lastProgramDeliveryAt >= this.program.freshness.stale_after_ms;
    this.deliveryStale = stale;
    this.ui.setStaleState(stale);
    if (stale && this.program.freshness.on_stale === "blank") {
      this.ui.showBlank("host_unavailable", state);
      this.ui.setSourceState(state);
      return;
    }
    if (item.scene.kind === "frame") {
      const staged = this.staged.get(item.scene.asset.id);
      if (!staged) {
        this.ui.showBlank("host_unavailable", state);
        return;
      }
      this.ui.showFrame(staged.url, item.spoken_summary, state);
    } else if (item.scene.kind === "blank") {
      this.ui.showBlank(item.scene.reason, state);
    } else {
      this.ui.showBlank("unsupported", state);
    }
    this.ui.setSourceState(state);

    if (item.duration_ms == null) return;
    const remaining = Math.max(0, item.duration_ms - this.currentPlayback().elapsedMs);
    this.playbackTimer = setTimeout(() => this.advancePlayback(), remaining);
  }

  advancePlayback() {
    if (!this.program) return;
    const next = this.program.playback.current_index + 1;
    if (next < this.program.items.length) {
      this.program.playback.current_index = next;
    } else {
      switch (this.program.playback.cycle) {
        case "loop":
          this.program.playback.current_index = 0;
          break;
        case "blank_at_end":
          this.ui.showBlank("program_ended", sourceKind(this.program));
          return;
        case "poll_at_end":
          this.ui.showPendingAtEnd();
          this.clearProgram();
          return;
        case "hold_last":
        default:
          return;
      }
    }
    this.program.playback.elapsed_ms = 0;
    this.elapsedBase = 0;
    this.itemStartedAt = performance.now();
    this.renderCurrent();
  }

  startStaleMonitor() {
    clearInterval(this.staleTimer);
    this.staleTimer = setInterval(() => {
      if (!this.program || !this.lastProgramDeliveryAt) return;
      const stale = performance.now() - this.lastProgramDeliveryAt >= this.program.freshness.stale_after_ms;
      const wasStale = this.deliveryStale;
      this.deliveryStale = stale;
      this.ui.setStaleState(stale);
      if (stale && this.program.freshness.on_stale === "blank") {
        this.ui.showBlank("host_unavailable", sourceKind(this.program));
      } else if (wasStale && !stale) {
        this.renderCurrent();
      }
    }, 1000);
  }

  async reportHealth() {
    if (!this.program) return;
    const playback = this.currentPlayback();
    const item = this.program.items[playback.currentIndex];
    const displayed = item.scene.kind === "frame" ? item.scene.asset : null;
    let stagedBytes = 0;
    for (const entry of this.staged.values()) stagedBytes += entry.asset.encoded_len;
    const response = await this.authorizedJson({
      route: "health",
      method: "POST",
      path: "/head/v1/health",
      body: {
        protocol_major: PROTOCOL_MAJOR,
        platform: this.capabilities.platform,
        build: this.capabilities.build,
        revision: this.program.revision,
        current_item: item.id,
        elapsed_ms: playback.elapsedMs,
        last_displayed_asset: displayed ? { id: displayed.id, sha256: displayed.sha256 } : null,
        connection: "online",
        playback: displayed ? "displaying" : "blank",
        last_error: "none",
        staged_items: this.staged.size,
        staged_bytes: stagedBytes,
        decode_latency: "unobserved",
        swap_latency: "unobserved",
        drift_residual_ms: 0,
        correction_events: 0,
        pipeline_unobservable: true,
      },
    });
    exactFields(response, ["kind"], "health response");
    if (response.kind !== "accepted") {
      throw new ProtocolError("health_refused", "Coordinator refused bounded receiver health");
    }
    this.lastHealthAt = performance.now();
  }

  async handleRevoked() {
    this.clearProgram();
    this.challenge = null;
    if (this.credential) this.credential = { ...this.credential, mode: "revoked" };
    this.ui.showRevoked();
  }

  async rePair(reason) {
    this.clearProgram();
    this.challenge = null;
    await this.vault.clear();
    this.credential = null;
    this.ui.showRePair(reason);
    if (this.running) await this.startPairing();
  }
}
