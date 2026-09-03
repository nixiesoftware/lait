// The probe's receiver: the shared protocol client, run headless in Node with
// a strict native-HLS player where a browser receiver keeps an MSE session.
//
// Everything about pairing, challenges, request tags, capabilities, the
// program long-poll and health is `DisplayReceiverClient`'s. What is overridden
// here is exactly what a native_hls receiver does differently from a browser
// one: staging accepts `hls` media and asks for an `hls` ticket, frames are
// verified but not decoded, and the "screen" is a play clock plus a ledger.

import fs from "node:fs";
import path from "node:path";

import { DisplayReceiverClient } from "../../../receivers/shared/web/client.mjs";
import { PROTOCOL_MAJOR, ProtocolError, authenticateRequest, isLowerHex, sha256 } from "../../../receivers/shared/web/protocol.mjs";
import { boundedBytes } from "../../../receivers/shared/web/transport.mjs";
import { checkTransportStream, parseMasterPlaylist, parseMediaPlaylist, percentile, PlayModel } from "./hls.mjs";

// `client.mjs` hands a stage back to the renderer on the next frame; Node has
// no frames.
globalThis.requestAnimationFrame ??= (callback) => setTimeout(callback, 16);

export const PROBE_BUILD = "display-probe/0.1.0";

export function probeCapabilities({ width = 1280, height = 720 } = {}) {
  // Mirrors `AstrolabeCapabilities` in receivers/roku/components/ReceiverTask.brs,
  // within the bounds `validate_capabilities` holds (crates/display-protocol).
  return {
    protocol_major: PROTOCOL_MAJOR,
    platform: "desktop",
    build: PROBE_BUILD,
    viewport: { width, height, scale_milli: 1000 },
    image_types: ["image_jpeg", "image_png", "image_webp"],
    max_asset_bytes: 16_777_216,
    max_staged_bytes: 50_331_648,
    max_program_items: 16,
    max_staging_horizon_ms: 86_400_000,
    locale: "en-US",
    accessibility: {
      native_screen_reader: false,
      spoken_summary: false,
      captions: false,
      audio_description: false,
    },
    playback: {
      tier: "native_hls",
      sync_class: "none",
      rate_control_probed: false,
      latency_class: "broadcast",
      health_granularity: "coarse",
    },
  };
}

/** The credential vault, as a JSON file: the probe's state directory is the secure store. */
export class FileVault {
  constructor(file) {
    this.file = file;
  }

  async load() {
    if (!fs.existsSync(this.file)) return null;
    const decoded = JSON.parse(fs.readFileSync(this.file, "utf8"));
    if (!decoded || decoded.version !== 1 || typeof decoded.mode !== "string") {
      throw new ProtocolError("credential_corrupt", `${this.file} is not a receiver credential`);
    }
    return decoded;
  }

  async save(state) {
    fs.mkdirSync(path.dirname(this.file), { recursive: true });
    fs.writeFileSync(this.file, JSON.stringify({ ...state, version: 1 }, null, 2), { mode: 0o600 });
  }

  async clear() {
    fs.rmSync(this.file, { force: true });
  }

  close() {}
}

/** Everything the run measures, and the report it becomes. */
export class Stats {
  constructor({ log }) {
    this.log = log;
    this.startedAt = performance.now();
    this.startedAtUnixMs = Date.now();
    this.pairedAt = null;
    this.pairingStartedAt = null;
    this.firstSegmentAt = null;
    this.firstPlaylist = null;
    this.playlistReloads = 0;
    this.playlistRefusals = 0;
    this.windowSegments = [];
    this.masterComments = [];
    this.renditions = [];
    this.rendition = null;
    this.targetDurationMs = null;
    this.segments = [];
    this.segmentFailures = 0;
    this.samples = [];
    this.stalls = [];
    this.violations = new Map();
    this.recoveries = [];
    this.openRecovery = null;
    this.health = { accepted: 0, refused: 0 };
    this.poll = { snapshot: 0, no_change: 0, reset: 0, unassigned: 0, revoked: 0, re_pair: 0, errors: 0 };
    this.apiRefusals = {};
    this.tickets = 0;
    this.sessions = 0;
    this.marks = {};
    this.fatal = null;
  }

  /** The first time something happened, in ms since the probe started. */
  mark(name) {
    if (!(name in this.marks)) this.marks[name] = this.since();
  }

  since(at = performance.now()) {
    return Math.round(at - this.startedAt);
  }

  violation({ kind, detail, ...extra }) {
    const entry = this.violations.get(kind) || { count: 0, first: null };
    entry.count += 1;
    if (!entry.first) entry.first = { at_ms: this.since(), detail, ...extra };
    this.violations.set(kind, entry);
    this.log(`VIOLATION ${kind}: ${detail}`);
  }

  beginRecovery(cause) {
    if (this.openRecovery) {
      this.openRecovery.causes.push(cause);
      return;
    }
    this.openRecovery = { cause, causes: [cause], started_at_ms: this.since(), refused_requests: 0 };
    this.log(`recovery began: ${cause}`);
  }

  refusedDuringRecovery() {
    if (this.openRecovery) this.openRecovery.refused_requests += 1;
  }

  endRecovery() {
    if (!this.openRecovery) return;
    const recovery = this.openRecovery;
    this.openRecovery = null;
    recovery.recovered_at_ms = this.since();
    recovery.recovery_ms = recovery.recovered_at_ms - recovery.started_at_ms;
    this.recoveries.push(recovery);
    this.log(`recovered in ${recovery.recovery_ms} ms after ${recovery.refused_requests} refused request(s)`);
  }

  segmentFetched({ sequence, latencyMs, bytes, status }) {
    if (this.firstSegmentAt === null) this.firstSegmentAt = performance.now();
    this.mark("first_segment");
    this.endRecovery();
    this.segments.push({ sequence, latency_ms: Math.round(latencyMs), bytes, status, at_ms: this.since() });
  }

  report({ options, coordinator, device, reusedCredential }) {
    const latencies = this.segments.map((segment) => segment.latency_ms);
    const runways = this.samples.map((sample) => sample.runway_ms);
    const stallTotal = this.stalls.reduce((sum, stall) => sum + stall.durationMs, 0);
    const violationsByKind = Object.fromEntries(this.violations);
    const violationsTotal = [...this.violations.values()].reduce((sum, entry) => sum + entry.count, 0);
    const exitCode = this.fatal && this.firstSegmentAt === null
      ? 2
      : violationsTotal === 0 && this.stalls.length === 0 && !this.fatal ? 0 : 1;
    return {
      probe: {
        build: PROBE_BUILD,
        started_at_unix_ms: this.startedAtUnixMs,
        ran_ms: this.since(),
        seconds_requested: options.seconds,
        prefetch_ms: options.prefetchMs,
        origin: options.origin,
        assignment_copied: options.assignment,
        device,
        reused_credential: reusedCredential,
      },
      coordinator: {
        ...coordinator,
        playlist_comments: this.masterComments,
      },
      startup: {
        pairing_ms: this.pairingStartedAt !== null && this.pairedAt !== null
          ? Math.round(this.pairedAt - this.pairingStartedAt)
          : null,
        pair_to_first_segment_ms: this.pairedAt !== null && this.firstSegmentAt !== null
          ? Math.round(this.firstSegmentAt - this.pairedAt)
          : null,
        start_to_first_segment_ms: this.firstSegmentAt !== null ? Math.round(this.firstSegmentAt - this.startedAt) : null,
        marks_ms: this.marks,
        first_playlist: this.firstPlaylist,
      },
      playlist: {
        rendition: this.rendition,
        renditions_offered: this.renditions,
        target_duration_ms: this.targetDurationMs,
        reloads: this.playlistReloads,
        refused: this.playlistRefusals,
        window_segments: {
          min: this.windowSegments.length ? Math.min(...this.windowSegments) : null,
          max: this.windowSegments.length ? Math.max(...this.windowSegments) : null,
        },
        tickets_minted: this.tickets,
        sessions: this.sessions,
      },
      runway_ms: {
        min: runways.length ? Math.min(...runways) : null,
        median: percentile(runways, 0.5),
        max: runways.length ? Math.max(...runways) : null,
      },
      stalls: {
        count: this.stalls.length,
        total_ms: Math.round(stallTotal),
        list: this.stalls.map((stall) => ({
          started_at_ms: Math.round(stall.startedAtMs - this.startedAt),
          duration_ms: Math.round(stall.durationMs),
          reason: stall.reason,
          sequence: stall.sequence,
        })),
      },
      segments: {
        fetched: this.segments.length,
        failed: this.segmentFailures,
        bytes: this.segments.reduce((sum, segment) => sum + segment.bytes, 0),
        latency_ms: {
          p50: percentile(latencies, 0.5),
          p95: percentile(latencies, 0.95),
          max: latencies.length ? Math.max(...latencies) : null,
        },
      },
      violations: { total: violationsTotal, by_kind: violationsByKind },
      recoveries: this.recoveries,
      recovery_in_progress: this.openRecovery,
      health: this.health,
      poll: this.poll,
      api_refusals: this.apiRefusals,
      samples: this.samples,
      fatal: this.fatal,
      exit_code: exitCode,
    };
  }
}

/**
 * A strict live HLS client against one ticket: reload the media playlist
 * every target duration, start three targets behind the edge, consume
 * segments on a play clock, fetch each when it is needed, and hold the
 * playlist to its invariants. Nothing is decoded; the segment bytes are
 * checked for transport-stream shape and dropped.
 */
export class HlsSession {
  constructor({ origin, endpoint, transport, stats, log, prefetchMs, onFailure }) {
    this.masterUrl = `${origin}${endpoint}`;
    this.transport = transport;
    this.stats = stats;
    this.log = log;
    this.prefetchMs = prefetchMs;
    this.onFailure = onFailure;
    this.model = new PlayModel();
    this.mediaUrl = null;
    this.timers = [];
    this.started = false;
    this.failed = false;
    this.released = false;
    this.lastError = null;
    this.stalls = 0;
  }

  start() {
    if (this.started || this.released || this.failed) return;
    this.started = true;
    this.stats.sessions += 1;
    this.run().catch((error) => this.fail("session", error));
  }

  async run() {
    const master = await this.fetchText(this.masterUrl, "master");
    if (!master) return;
    this.stats.mark("master_playlist");
    const parsed = parseMasterPlaylist(master.text);
    this.stats.masterComments = parsed.comments;
    this.stats.renditions = parsed.renditions.map((rendition) => ({
      uri: rendition.uri, bandwidth: rendition.bandwidth, resolution: rendition.resolution, codecs: rendition.codecs,
    }));
    if (parsed.renditions.length === 0) {
      this.stats.violation({ kind: "playlist_malformed", detail: "master playlist lists no renditions" });
      return this.fail("no_rendition", new Error("master playlist lists no renditions"));
    }
    const chosen = [...parsed.renditions].sort((a, b) => (b.bandwidth ?? 0) - (a.bandwidth ?? 0))[0];
    this.stats.rendition = chosen.uri;
    this.mediaUrl = new URL(chosen.uri, this.masterUrl).href;
    await this.reload();
    if (this.failed || this.released) return;
    this.timers.push(setInterval(() => this.tick(), 200));
    this.timers.push(setInterval(() => this.sample(), 1000));
  }

  async fetchText(url, what) {
    let response;
    try {
      response = await this.transport.request({ method: "GET", url, maximumBytes: 1024 * 1024, timeoutMs: 10_000 });
    } catch (error) {
      this.log(`${what} playlist fetch failed: ${error.message}`);
      this.stats.playlistRefusals += 1;
      return null;
    }
    if (response.status === 403) {
      this.stats.playlistRefusals += 1;
      this.stats.refusedDuringRecovery();
      this.fail("ticket_refused", new ProtocolError("live_ticket", `${what} playlist answered 403`));
      return null;
    }
    if (response.status !== 200) {
      this.stats.playlistRefusals += 1;
      this.stats.violation({ kind: "playlist_refused", detail: `${what} playlist answered HTTP ${response.status}` });
      return null;
    }
    return { text: response.body.toString("utf8"), latencyMs: response.latencyMs };
  }

  async reload() {
    if (this.failed || this.released) return;
    const fetched = await this.fetchText(this.mediaUrl, "media");
    if (this.failed || this.released) return;
    let delayMs = this.model.targetDurationMs ?? 1000;
    if (fetched) {
      let playlist;
      try {
        playlist = parseMediaPlaylist(fetched.text);
      } catch (error) {
        this.stats.violation({ kind: "playlist_malformed", detail: error.message });
        this.scheduleReload(delayMs);
        return;
      }
      this.stats.playlistReloads += 1;
      this.stats.mark("media_playlist");
      this.stats.windowSegments.push(playlist.segments.length);
      for (const violation of this.model.acceptPlaylist(playlist)) this.stats.violation(violation);
      this.stats.targetDurationMs = playlist.targetDurationMs;
      if (!this.model.started) {
        this.model.start(performance.now());
        this.stats.firstPlaylist = {
          media_sequence: playlist.mediaSequence,
          end_sequence: playlist.segments.at(-1)?.sequence ?? null,
          segments: playlist.segments.length,
          target_duration_ms: playlist.targetDurationMs,
          discontinuity_sequence: playlist.discontinuitySequence,
          endlist: playlist.endlist,
          start_sequence: this.model.playSequence,
          latency_ms: Math.round(fetched.latencyMs),
        };
        this.log(`playing ${this.mediaUrl.split("/").slice(-1)[0]} from ${this.model.playSequence} (window ${playlist.mediaSequence}..${playlist.segments.at(-1).sequence}, target ${playlist.targetDurationMs} ms)`);
        this.tick();
      }
      this.model.prune();
      delayMs = playlist.targetDurationMs;
      if (playlist.endlist) return;
    }
    this.scheduleReload(delayMs);
  }

  scheduleReload(delayMs) {
    if (this.failed || this.released) return;
    const timer = setTimeout(() => {
      this.timers = this.timers.filter((candidate) => candidate !== timer);
      this.reload().catch((error) => this.fail("reload", error));
    }, delayMs);
    this.timers.push(timer);
  }

  tick() {
    if (this.failed || this.released || !this.model.started) return;
    const wasStalled = this.model.stalled;
    this.model.tick(performance.now());
    if (this.model.stalled && !wasStalled) {
      this.log(`stall at sequence ${this.model.playSequence} (${this.model.stallReason})`);
    }
    if (this.model.stalls.length > this.stalls) {
      for (const stall of this.model.stalls.slice(this.stalls)) this.stats.stalls.push(stall);
      this.stalls = this.model.stalls.length;
    }
    const prefetchMs = this.prefetchMs ?? this.model.targetDurationMs ?? 2000;
    for (const sequence of this.model.needed(prefetchMs)) this.fetchSegment(sequence);
  }

  sample() {
    if (this.failed || this.released || !this.model.started) return;
    const sample = this.model.sample(performance.now());
    this.stats.samples.push({
      at_ms: this.stats.since(),
      play_sequence: sample.playSequence,
      end_sequence: sample.endSequence,
      runway_ms: Math.round(sample.runwayMs),
      stalled: sample.stalled,
    });
  }

  async fetchSegment(sequence) {
    const segment = this.model.listed.get(sequence);
    if (!segment) return;
    this.model.markInFlight(sequence);
    const url = new URL(segment.uri, this.mediaUrl).href;
    let response;
    try {
      response = await this.transport.request({ method: "GET", url, maximumBytes: 64 * 1024 * 1024, timeoutMs: 15_000 });
    } catch (error) {
      if (this.released) return;
      this.stats.segmentFailures += 1;
      this.model.markFailed(sequence, "network");
      this.log(`segment ${sequence} fetch failed: ${error.message}`);
      return;
    }
    if (this.released) return;
    if (response.status !== 200) {
      this.stats.segmentFailures += 1;
      this.model.markFailed(sequence, response.status);
      this.stats.refusedDuringRecovery();
      this.stats.violation({
        kind: "listed_segment_refused",
        detail: `segment ${sequence} answered HTTP ${response.status} while listed`,
        sequence,
        status: response.status,
      });
      if (response.status === 403) {
        this.fail("ticket_refused", new ProtocolError("live_ticket", `segment ${sequence} answered 403`));
      }
      return;
    }
    for (const violation of checkTransportStream(response.body)) {
      this.stats.violation({ ...violation, sequence });
    }
    this.model.markFetched(sequence);
    this.stats.segmentFetched({ sequence, latencyMs: response.latencyMs, bytes: response.body.length, status: response.status });
    this.tick();
  }

  fail(code, error) {
    if (this.failed || this.released) return;
    this.failed = true;
    this.lastError = error;
    this.clearTimers();
    this.log(`live session failed: ${code}: ${error.message}`);
    this.stats.beginRecovery(`session:${code}`);
    this.onFailure(error instanceof ProtocolError ? error : new ProtocolError(code, String(error)));
  }

  clearTimers() {
    for (const timer of this.timers) {
      clearTimeout(timer);
      clearInterval(timer);
    }
    this.timers = [];
  }

  release() {
    if (this.released) return;
    if (this.model.stalled) this.model.unstall(performance.now());
    for (const stall of this.model.stalls.slice(this.stalls)) this.stats.stalls.push(stall);
    this.stalls = this.model.stalls.length;
    this.released = true;
    this.clearTimers();
  }
}

/** The receiver's screen, as a log line and a ledger. */
export class ProbeUi {
  constructor({ log, stats, onFatal }) {
    this.log = log;
    this.stats = stats;
    this.onFatal = onFatal;
    this.transport = "offline";
  }

  showBooting() { this.log("booting"); }
  showConnecting() { this.log("connecting"); }
  showRecovering(code) { this.log(`recovering: ${code}`); this.stats.poll.errors += 1; }
  showPairing({ phrase, viaCode, confirmed }) {
    this.log(`pairing ${viaCode ? "by code" : `by words: ${phrase.join(" ")}`}${confirmed ? " (confirmed)" : ""}`);
  }
  showPairingWaiting() { this.log("pairing: waiting for the coordinator"); }
  showPairingNetworkError() { this.log("pairing: network error"); }
  showPairingRejected(kind, reason) { this.fatal("pairing_rejected", `${kind}${reason ? `: ${reason}` : ""}`); }
  showUnassigned(device) { this.log(`unassigned: ${device}`); }
  showRevoked() { this.fatal("revoked", "the coordinator revoked this receiver"); }
  showRePair(reason) { this.fatal("re_pair_required", reason); }
  showFailure(code, message) { this.fatal(code, message); }
  showBlank(reason, state) { this.log(`blank: ${reason} (${state})`); }
  showFrame(_url, summary, state) { this.log(`frame: ${summary || "(no summary)"} (${state})`); }
  showMedia(session, summary, state) {
    if (!session.started) this.log(`media: ${summary || "(no summary)"} (${state})`);
    session.start();
  }
  showPendingAtEnd() { this.log("program ended; polling"); }
  setTransportState(state) {
    if (state !== this.transport) this.log(`transport ${state}`);
    this.transport = state;
  }
  setStaleState(stale) { if (stale) this.log("delivery stale"); }
  setSourceState() {}

  fatal(code, message) {
    this.log(`FATAL ${code}: ${message}`);
    this.onFatal({ code, message });
  }
}

export class ProbeReceiver extends DisplayReceiverClient {
  constructor({ bootstrap, capabilities, ui, vault, transport, stats, log, prefetchMs }) {
    super({ bootstrap, capabilities, ui, vaultFactory: async () => vault });
    this.transport = transport;
    this.stats = stats;
    this.log = log;
    this.prefetchMs = prefetchMs;
    this.inFlightAuthorized = 0;
    this.recovering = null;
  }

  async start() {
    const stored = await this.vaultFactory().then((vault) => vault.load()).catch(() => null);
    if (stored && stored.mode === "paired") this.stats.pairedAt = performance.now();
    else this.stats.pairingStartedAt = performance.now();
    return super.start();
  }

  async finishEnrollment() {
    await super.finishEnrollment();
    if (this.stats.pairedAt === null) this.stats.pairedAt = performance.now();
    this.stats.mark("paired");
    this.log(`paired as device ${this.credential.device}`);
  }

  async authorizedJson(options) {
    this.inFlightAuthorized += 1;
    try {
      return await super.authorizedJson(options);
    } catch (error) {
      if (error instanceof ProtocolError && error.code !== "network" && error.code !== "timeout") {
        this.stats.apiRefusals[error.code] = (this.stats.apiRefusals[error.code] || 0) + 1;
        this.stats.refusedDuringRecovery();
      } else {
        this.stats.beginRecovery(`poll:${error.code || "network"}`);
      }
      throw error;
    } finally {
      this.inFlightAuthorized -= 1;
    }
  }

  async handleProgramResponse(response) {
    const kind = response && typeof response.kind === "string" ? response.kind : "invalid";
    if (kind in this.stats.poll) this.stats.poll[kind] += 1;
    if (kind === "reset") this.stats.beginRecovery(`reset:${response.reason}`);
    if (kind === "snapshot") this.stats.mark("snapshot");
    if (kind === "snapshot" && this.program === null) this.log("program snapshot adopted");
    return super.handleProgramResponse(response);
  }

  async negotiateCapabilities() {
    await super.negotiateCapabilities();
    this.stats.mark("capabilities_accepted");
  }

  async reportHealth() {
    try {
      await super.reportHealth();
      this.stats.health.accepted += 1;
    } catch (error) {
      this.stats.health.refused += 1;
      throw error;
    }
  }

  async rePair(reason) {
    // A headless probe has nobody to read six words; the run ends here.
    this.clearProgram();
    this.challenge = null;
    this.ui.showRePair(reason);
  }

  /** Native HLS: `hls` media is what this receiver plays; frames are verified and counted. */
  async stageProgram(program) {
    const assets = new Map();
    for (const item of program.items) {
      if (item.scene.kind === "media") {
        if (item.scene.protocol !== "hls" || item.scene.manifest.media_type !== "hls_manifest") {
          throw new ProtocolError("unsupported", `Probe plays native HLS; program offers ${item.scene.protocol}`);
        }
        const manifest = item.scene.manifest;
        if (assets.has(manifest.id)) continue;
        try {
          assets.set(manifest.id, await this.authorizedLiveMedia(item, program));
        } catch (error) {
          if (error instanceof ProtocolError && ["revoked", "re_pair_required", "unsupported"].includes(error.code)) throw error;
          this.log(`ticket refused for ${manifest.id.slice(0, 12)}: ${error.code || error.message}`);
          assets.set(manifest.id, { asset: manifest, item, session: { failed: true, started: true, release() {} }, lastError: error });
        }
        continue;
      }
      if (item.scene.kind !== "frame") continue;
      const asset = item.scene.asset;
      if (assets.has(asset.id)) continue;
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
      body: { transport: "hls" },
      overrides: {
        assignment: program.assignment,
        program: program.program,
        revision: program.revision,
        currentItem: item.id,
        elapsedMs: 0,
        asset: manifest.id,
      },
    });
    const fields = Object.keys(response).sort().join(",");
    if (fields !== "endpoint,expires_at_unix_ms,protocol_major,transport"
      || response.protocol_major !== PROTOCOL_MAJOR
      || response.transport !== "hls"
      || !/^\/head\/v1\/live\/[0-9a-f]{64}\/master\.m3u8$/.test(response.endpoint)
      || !Number.isSafeInteger(response.expires_at_unix_ms)
      || response.expires_at_unix_ms <= Date.now()) {
      throw new ProtocolError("live_ticket", "Coordinator returned an invalid HLS ticket");
    }
    this.stats.tickets += 1;
    this.stats.mark("ticket");
    this.log(`ticket minted, expires in ${Math.round((response.expires_at_unix_ms - Date.now()) / 1000)} s`);
    const entry = { asset: manifest, item, session: null, lastError: null };
    entry.session = new HlsSession({
      origin: this.origin,
      endpoint: response.endpoint,
      transport: this.transport,
      stats: this.stats,
      log: this.log,
      prefetchMs: this.prefetchMs,
      onFailure: (error) => {
        entry.lastError = error;
        this.recoverSoon();
      },
    });
    return entry;
  }

  /**
   * A refused ticket is re-minted without waiting for the long-poll to come
   * back, when no authenticated request is mid-flight (the challenge chain is
   * single-use); otherwise the loop's own recovery handles it after the poll.
   */
  recoverSoon() {
    if (!this.running || this.recovering || this.inFlightAuthorized > 0) return;
    this.recovering = this.recoverFailedLiveMedia()
      .catch((error) => this.log(`recovery attempt failed: ${error.code || error.message}`))
      .finally(() => { this.recovering = null; });
  }

  async recoverFailedLiveMedia() {
    if (this.recovering) return this.recovering;
    return super.recoverFailedLiveMedia();
  }

  /** A frame asset, fetched and digest-checked but never decoded: there is no screen. */
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
    const response = await boundedBytes({
      method: "GET",
      url: `${this.origin}/head/v1/assets/${encodeURIComponent(asset.id)}`,
      headers: this.requestHeaders(context, tag),
      maximumBytes: asset.encoded_len,
      timeoutMs: 30_000,
    });
    if (response.nextChallenge && isLowerHex(response.nextChallenge, 64)) this.challenge = response.nextChallenge;
    if (response.status < 200 || response.status >= 300 || !this.challenge) {
      throw new ProtocolError("asset_transfer", `Asset request returned HTTP ${response.status}`);
    }
    if (!response.body || response.body.byteLength !== asset.encoded_len) {
      throw new ProtocolError("asset_length", "Asset length does not match the snapshot");
    }
    if ((await sha256(new Uint8Array(response.body))) !== asset.sha256) {
      throw new ProtocolError("asset_digest", "Asset SHA-256 does not match the snapshot");
    }
    this.log(`frame ${asset.id.slice(0, 12)} verified (${asset.encoded_len} bytes)`);
    return { url: null, asset };
  }
}
