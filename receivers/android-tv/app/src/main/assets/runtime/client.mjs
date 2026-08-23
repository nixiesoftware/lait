import {
  authenticatePairingComplete,
  authenticatePairingStatus,
  authenticateRequest,
  BOUNDS,
  bytesToHex,
  confirmationPhrase,
  isLowerHex,
  isProfileId,
  PROTOCOL_MAJOR,
  ProtocolError,
  randomHex,
  requireProfile,
  sha256,
  verifyProgram,
} from "./protocol.mjs";
import { boundedBytes, boundedJson } from "./transport.mjs";
import { CredentialVault } from "./vault.mjs";

const encoder = new TextEncoder();
const JSON_LIMIT = 64 * 1024;
const PAIRING_LIMIT = 16 * 1024;
const LONG_POLL_WAIT_MS = 25_000;
const LIVE_PACKET_LIMIT = 34 * 1024 * 1024;
const LIVE_QUEUE_LIMIT = 48 * 1024 * 1024;
const LIVE_QUEUE_PACKETS = 16;

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
  if (parsed.protocol !== "https:" || parsed.username || parsed.password || parsed.search || parsed.hash
    || origin.slice("https://".length).includes("/")) {
    throw new ProtocolError("invalid_origin", "Coordinator origin must be credential-free HTTPS");
  }
  return parsed.origin;
}

function receiverBootstrap(bootstrap) {
  exactFields(bootstrap, ["protocol_major", "trust", "certificate_pem", "rendezvous"], "receiver bootstrap");
  if (bootstrap.protocol_major !== PROTOCOL_MAJOR
    || (bootstrap.rendezvous !== null && !isLowerHex(bootstrap.rendezvous, 32))) {
    throw new ProtocolError("invalid_bootstrap", "Receiver bootstrap does not speak protocol major 1");
  }
  if (bootstrap.trust?.kind === "web_pki_origin") {
    exactFields(bootstrap.trust, ["kind", "origin"], "coordinator trust");
    if (bootstrap.certificate_pem !== null) {
      throw new ProtocolError("invalid_bootstrap", "Web PKI bootstrap must not carry a pinned certificate");
    }
  } else if (bootstrap.trust?.kind === "pinned_certificate") {
    exactFields(bootstrap.trust, ["kind", "origin", "sha256"], "coordinator trust");
    if (!isLowerHex(bootstrap.trust.sha256, 64)
      || typeof bootstrap.certificate_pem !== "string"
      || bootstrap.certificate_pem.length < 1
      || bootstrap.certificate_pem.length > 16 * 1024) {
      throw new ProtocolError("invalid_bootstrap", "Pinned bootstrap trust material is invalid");
    }
  } else {
    throw new ProtocolError("unsupported_trust", "Receiver bootstrap trust kind is unsupported");
  }
  return {
    protocolMajor: bootstrap.protocol_major,
    trust: Object.freeze({ ...bootstrap.trust, origin: coordinatorOrigin(bootstrap.trust.origin) }),
    certificatePem: bootstrap.certificate_pem,
    rendezvous: bootstrap.rendezvous,
  };
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

export function parseLiveMediaPacket(arrayBuffer) {
  if (!(arrayBuffer instanceof ArrayBuffer)
    || arrayBuffer.byteLength < 36
    || arrayBuffer.byteLength > LIVE_PACKET_LIMIT) {
    throw new ProtocolError("live_packet", "Live media packet is outside its byte bound");
  }
  const view = new DataView(arrayBuffer);
  const kind = view.getUint8(0);
  const nameLength = view.getUint16(1, false);
  const payloadOffset = 36 + nameLength;
  if (![1, 2].includes(kind) || nameLength < 1 || nameLength > 64 || payloadOffset > arrayBuffer.byteLength) {
    throw new ProtocolError("live_packet", "Live media packet has an invalid envelope");
  }
  let rendition;
  try {
    rendition = new TextDecoder("utf-8", { fatal: true })
      .decode(new Uint8Array(arrayBuffer, 3, nameLength));
  } catch {
    throw new ProtocolError("live_packet", "Live rendition is not UTF-8");
  }
  if (!/^[a-zA-Z0-9_-]+$/.test(rendition)) {
    throw new ProtocolError("live_packet", "Live rendition is not canonical");
  }
  const metadata = 3 + nameLength;
  const sequence = view.getBigUint64(metadata, false);
  const publishedAtMicros = view.getBigInt64(metadata + 8, false);
  const startTimestamp = view.getBigUint64(metadata + 16, false);
  const duration = view.getBigUint64(metadata + 24, false);
  const discontinuity = view.getUint8(metadata + 32);
  if (discontinuity > 1 || (kind === 1 && (sequence !== 0n || duration !== 0n || discontinuity !== 0))) {
    throw new ProtocolError("live_packet", "Live media metadata is invalid");
  }
  return {
    kind: kind === 1 ? "init" : "fragment",
    rendition,
    sequence,
    publishedAtMicros,
    startTimestamp,
    duration,
    discontinuity: discontinuity === 1,
    payload: new Uint8Array(arrayBuffer.slice(payloadOffset)),
  };
}

function supportedLiveTracks(tracks) {
  if (typeof MediaSource !== "function" || typeof MediaSource.isTypeSupported !== "function") {
    throw new ProtocolError("unsupported", "Media Source Extensions are unavailable");
  }
  if (!Array.isArray(tracks) || tracks.length < 1 || tracks.length > 8) {
    throw new ProtocolError("live_catalog", "Live track count is outside its bound");
  }
  const selected = new Map();
  for (const track of tracks) {
    exactFields(
      track,
      ["rendition", "kind", "mime_type", "timescale", "target_latency_ms", "render_group"],
      "live track",
    );
    if (!/^[a-zA-Z0-9_-]{1,64}$/.test(track.rendition)
      || !["audio", "video"].includes(track.kind)
      || typeof track.mime_type !== "string"
      || !Number.isSafeInteger(track.timescale)
      || track.timescale < 1
      || !Number.isSafeInteger(track.target_latency_ms)
      || track.target_latency_ms < 1
      || (track.render_group !== null && !/^[a-zA-Z0-9_-]{1,64}$/.test(track.render_group))) {
      throw new ProtocolError("live_catalog", "Live track description is invalid");
    }
    if (!selected.has(track.kind) && MediaSource.isTypeSupported(track.mime_type)) {
      selected.set(track.kind, track);
    }
  }
  if (!selected.has("video")) {
    throw new ProtocolError("unsupported", "The live catalog has no supported video rendition");
  }
  return new Map(Array.from(selected.values(), (track) => [track.rendition, track]));
}

class MseLiveSession {
  constructor(origin, endpoint, onFailure) {
    this.origin = origin;
    this.endpoint = endpoint;
    this.onFailure = onFailure;
    this.video = null;
    this.socket = null;
    this.mediaSource = null;
    this.mediaUrl = null;
    this.tracks = new Map();
    this.initializations = new Map();
    this.buffers = new Map();
    this.queues = new Map();
    this.lastSequences = new Map();
    this.queuedBytes = 0;
    this.failed = false;
    this.released = false;
  }

  mount(container, summary) {
    if (!this.video) {
      this.video = document.createElement("video");
      this.video.autoplay = true;
      this.video.playsInline = true;
      this.video.setAttribute("aria-label", summary || "Assigned Astrolabe live media");
      this.video.addEventListener("error", () => this.fail(new ProtocolError("media_decode", "Live media decoder failed")));
    }
    if (this.video.parentElement !== container) container.replaceChildren(this.video);
    this.connect();
    this.seekLiveEdge();
    this.video.play().catch(() => {});
  }

  connect() {
    if (this.socket || this.released || this.failed) return;
    const url = new URL(this.endpoint, this.origin);
    url.protocol = "wss:";
    this.socket = new WebSocket(url.href);
    this.socket.binaryType = "arraybuffer";
    this.socket.addEventListener("message", (event) => {
      try {
        if (typeof event.data === "string") this.acceptHello(event.data);
        else this.acceptPacket(parseLiveMediaPacket(event.data));
      } catch (error) {
        this.fail(error);
      }
    });
    this.socket.addEventListener("error", () => this.fail(new ProtocolError("network", "Live media socket failed")));
    this.socket.addEventListener("close", () => {
      if (!this.released && !this.failed) this.fail(new ProtocolError("network", "Live media socket closed"));
    });
  }

  acceptHello(serialized) {
    if (this.tracks.size !== 0) throw new ProtocolError("live_catalog", "Live catalog was repeated");
    let hello;
    try { hello = JSON.parse(serialized); } catch { throw new ProtocolError("live_catalog", "Live catalog is not JSON"); }
    exactFields(hello, ["kind", "version", "tracks"], "live catalog");
    if (hello.kind !== "astrolabe_live" || hello.version !== 1) {
      throw new ProtocolError("live_catalog", "Live catalog version is unsupported");
    }
    this.tracks = supportedLiveTracks(hello.tracks);
    this.rebuildMediaSource();
  }

  acceptPacket(packet) {
    if (!this.tracks.has(packet.rendition)) return;
    if (packet.kind === "init") {
      if (packet.payload.byteLength < 1) throw new ProtocolError("live_packet", "Live init segment is empty");
      this.initializations.set(packet.rendition, packet.payload);
      this.enqueue(packet.rendition, packet.payload);
      return;
    }
    if (!this.initializations.has(packet.rendition) || packet.payload.byteLength < 1) {
      throw new ProtocolError("live_packet", "Live fragment arrived before its init segment");
    }
    const previous = this.lastSequences.get(packet.rendition);
    if (packet.discontinuity || (previous !== undefined && previous + 1n !== packet.sequence)) {
      this.lastSequences.set(packet.rendition, packet.sequence);
      this.rebuildMediaSource(packet);
      return;
    }
    this.lastSequences.set(packet.rendition, packet.sequence);
    this.enqueue(packet.rendition, packet.payload);
  }

  rebuildMediaSource(firstPacket = null) {
    if (!this.video || this.tracks.size === 0) return;
    this.buffers.clear();
    this.queues.clear();
    this.queuedBytes = 0;
    for (const track of this.tracks.values()) this.queues.set(track.rendition, []);
    const previousUrl = this.mediaUrl;
    this.mediaSource = new MediaSource();
    this.mediaUrl = URL.createObjectURL(this.mediaSource);
    this.video.src = this.mediaUrl;
    if (previousUrl) URL.revokeObjectURL(previousUrl);
    this.mediaSource.addEventListener("sourceopen", () => {
      try {
        for (const track of this.tracks.values()) {
          const buffer = this.mediaSource.addSourceBuffer(track.mime_type);
          this.buffers.set(track.rendition, buffer);
          buffer.addEventListener("updateend", () => {
            this.seekLiveEdge();
            this.pump(track.rendition);
          });
          this.pump(track.rendition);
        }
        this.mediaSource.duration = Number.POSITIVE_INFINITY;
      } catch (error) {
        this.fail(error);
      }
    }, { once: true });
    for (const [rendition, initialization] of this.initializations) {
      if (this.tracks.has(rendition)) this.enqueue(rendition, initialization);
    }
    if (firstPacket) this.enqueue(firstPacket.rendition, firstPacket.payload);
  }

  enqueue(rendition, payload) {
    const queue = this.queues.get(rendition);
    if (!queue) return;
    const queuedPackets = Array.from(this.queues.values(), (candidate) => candidate.length)
      .reduce((total, value) => total + value, 0);
    if (queuedPackets >= LIVE_QUEUE_PACKETS || this.queuedBytes + payload.byteLength > LIVE_QUEUE_LIMIT) {
      throw new ProtocolError("bound_exceeded", "Live decoder queue exceeded its bound");
    }
    const copy = payload.slice();
    queue.push(copy);
    this.queuedBytes += copy.byteLength;
    this.pump(rendition);
  }

  pump(rendition) {
    const buffer = this.buffers.get(rendition);
    const queue = this.queues.get(rendition);
    if (!buffer || !queue || buffer.updating || queue.length === 0) return;
    try {
      if (buffer.buffered.length > 0) {
        const start = buffer.buffered.start(0);
        const end = buffer.buffered.end(buffer.buffered.length - 1);
        if (end - start > 30) {
          buffer.remove(start, Math.max(start, end - 12));
          return;
        }
      }
      const payload = queue.shift();
      this.queuedBytes -= payload.byteLength;
      buffer.appendBuffer(payload);
      this.seekLiveEdge();
    } catch (error) {
      this.fail(error);
    }
  }

  seekLiveEdge() {
    if (!this.video || !this.video.buffered || this.video.buffered.length === 0) return;
    const edge = this.video.buffered.end(this.video.buffered.length - 1);
    if (!Number.isFinite(this.video.currentTime) || edge - this.video.currentTime > 8) {
      this.video.currentTime = Math.max(0, edge - 2);
    }
  }

  fail(error) {
    if (this.failed || this.released) return;
    this.failed = true;
    if (this.socket) this.socket.close();
    this.socket = null;
    this.onFailure(error instanceof ProtocolError ? error : new ProtocolError("media_decode", String(error)));
  }

  release() {
    this.released = true;
    if (this.socket) this.socket.close();
    if (this.video) {
      this.video.pause();
      this.video.removeAttribute("src");
      this.video.load();
    }
    if (this.mediaUrl) URL.revokeObjectURL(this.mediaUrl);
    this.socket = null;
    this.mediaUrl = null;
  }
}

export class DisplayReceiverClient {
  constructor({ bootstrap, capabilities, ui, vaultFactory = CredentialVault.open }) {
    const provisioned = receiverBootstrap(bootstrap);
    this.origin = provisioned.trust.origin;
    this.trust = provisioned.trust;
    this.certificatePem = provisioned.certificatePem;
    this.capabilities = capabilities;
    this.ui = ui;
    this.rendezvous = provisioned.rendezvous;
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
    this.lastSyncResidualMs = 0;
    this.correctionEvents = 0;
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
    const trustFields = this.trust.kind === "pinned_certificate"
      ? ["kind", "origin", "sha256"]
      : ["kind", "origin"];
    exactFields(instance.trust, trustFields, "coordinator trust");
    if (instance.trust.kind !== this.trust.kind
      || coordinatorOrigin(instance.trust.origin) !== this.origin
      || (this.trust.kind === "pinned_certificate"
        && instance.trust.sha256 !== this.trust.sha256)) {
      throw new ProtocolError("unsupported_trust", "Coordinator trust does not match the receiver bootstrap");
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
    if (this.trust.kind === "pinned_certificate"
      && response.coordinator_fingerprint !== this.trust.sha256) {
      throw new ProtocolError("pairing_integrity", "Pairing certificate does not match the receiver bootstrap");
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
        await this.recoverFailedLiveMedia();
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
        await this.recoverFailedLiveMedia();
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
        if (!item.scene.live || item.scene.protocol !== "mse" || item.scene.manifest.media_type !== "mse_manifest") {
          throw new ProtocolError("unsupported", "Receiver accepts only granted live MSE media");
        }
        const manifest = item.scene.manifest;
        if (assets.has(manifest.id)) continue;
        stagedBytes += manifest.encoded_len;
        if (stagedBytes > this.capabilities.max_staged_bytes || stagedBytes > BOUNDS.maxStagedBytes) {
          throw new ProtocolError("bound_exceeded", "Program exceeds negotiated staging bytes");
        }
        assets.set(manifest.id, await this.authorizedLiveMedia(item, program));
        continue;
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

  async authorizedLiveMedia(item, program) {
    const manifest = item.scene.manifest;
    const response = await this.authorizedJson({
      route: "live_ticket",
      method: "POST",
      path: "/head/v1/live/tickets",
      body: { transport: "mse" },
      overrides: {
        assignment: program.assignment,
        program: program.program,
        revision: program.revision,
        currentItem: item.id,
        elapsedMs: 0,
        asset: manifest.id,
      },
    });
    exactFields(response, ["protocol_major", "transport", "endpoint", "expires_at_unix_ms"], "live ticket");
    if (response.protocol_major !== PROTOCOL_MAJOR
      || response.transport !== "mse"
      || !/^\/head\/v1\/live\/[0-9a-f]{64}\/socket$/.test(response.endpoint)
      || !Number.isSafeInteger(response.expires_at_unix_ms)
      || response.expires_at_unix_ms <= Date.now()
      || response.expires_at_unix_ms > Date.now() + BOUNDS.maxStagingHorizonMs + 60_000) {
      throw new ProtocolError("live_ticket", "Coordinator returned an invalid live ticket");
    }
    const entry = { asset: manifest, item, session: null, lastError: null };
    entry.session = new MseLiveSession(this.origin, response.endpoint, (error) => {
      entry.lastError = error;
      if (this.program?.revision === program.revision
        && this.program.items[this.program.playback.current_index]?.id === item.id) {
        this.ui.showBlank("source_unavailable", sourceKind(this.program));
      }
    });
    return entry;
  }

  async recoverFailedLiveMedia() {
    if (!this.program) return;
    let changed = false;
    for (const item of this.program.items) {
      if (item.scene.kind !== "media") continue;
      const manifest = item.scene.manifest;
      const existing = this.staged.get(manifest.id);
      if (!existing?.session?.failed) continue;
      const replacement = await this.authorizedLiveMedia(item, this.program);
      existing.session.release();
      this.staged.set(manifest.id, replacement);
      changed = true;
    }
    if (changed) this.renderCurrent();
  }

  releaseStage(stage) {
    for (const entry of stage.values()) {
      if (entry.url) URL.revokeObjectURL(entry.url);
      if (entry.session) entry.session.release();
    }
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
    exactFields(playback, ["current_index", "elapsed_ms", "cycle", "sync"], "playback cursor");
    if (playback.sync !== null) {
      exactFields(playback.sync, ["group", "mode", "sampled_at_unix_ms"], "sync target");
      if (typeof playback.sync.group !== "string"
        || !/^[a-z0-9_-]+$/.test(playback.sync.group)
        || encoder.encode(playback.sync.group).length > BOUNDS.maxSyncGroupBytes
        || !["stay_in_sync", "positional"].includes(playback.sync.mode)
        || !Number.isSafeInteger(playback.sync.sampled_at_unix_ms)
        || playback.sync.sampled_at_unix_ms < 1) {
        throw new ProtocolError("invalid_cursor", "Coordinator sync target is invalid");
      }
    }
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
    const previous = this.currentPlayback();
    if (playback.sync !== null) {
      const residual = previous.currentIndex === playback.current_index
        ? playback.elapsed_ms - previous.elapsedMs
        : 0;
      this.lastSyncResidualMs = Math.max(-60_000, Math.min(60_000, residual));
      if (previous.currentIndex !== playback.current_index || residual !== 0) {
        this.correctionEvents = Math.min(0xffffffff, this.correctionEvents + 1);
      }
    } else {
      this.lastSyncResidualMs = 0;
    }
    if (programDelivery) this.lastProgramDeliveryAt = performance.now();
    this.program.playback.current_index = playback.current_index;
    this.program.playback.elapsed_ms = playback.elapsed_ms;
    this.program.playback.sync = playback.sync;
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
    } else if (item.scene.kind === "media") {
      const staged = this.staged.get(item.scene.manifest.id);
      if (!staged || staged.session.failed || typeof this.ui.showMedia !== "function") {
        this.ui.showBlank("source_unavailable", state);
        return;
      }
      this.ui.showMedia(staged.session, item.spoken_summary, state);
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
        playback: ["frame", "media"].includes(item.scene.kind) ? "displaying" : "blank",
        last_error: "none",
        staged_items: this.staged.size,
        staged_bytes: stagedBytes,
        decode_latency: "unobserved",
        swap_latency: "unobserved",
        drift_residual_ms: this.lastSyncResidualMs,
        correction_events: this.correctionEvents,
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
