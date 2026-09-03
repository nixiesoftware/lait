// The pure half of the probe: playlist parsing, the play-clock model a strict
// live HLS client keeps, and the invariants a coordinator's playlist must hold
// across reloads. No I/O and no wall clock — every time is a parameter — so
// `probe.test.mjs` can drive it with fixture playlists.

function tagValue(line, tag) {
  return line.startsWith(`${tag}:`) ? line.slice(tag.length + 1) : null;
}

function attributes(text) {
  const out = {};
  const pattern = /([A-Z0-9-]+)=("([^"]*)"|[^,]*)/g;
  let match;
  while ((match = pattern.exec(text)) !== null) {
    out[match[1]] = match[3] !== undefined ? match[3] : match[2];
  }
  return out;
}

/** `#EXT-X-STREAM-INF` entries with the URI that follows each. */
export function parseMasterPlaylist(text) {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter((line) => line.length > 0);
  if (lines[0] !== "#EXTM3U") throw new Error("master playlist does not begin with #EXTM3U");
  const renditions = [];
  const comments = [];
  let pending = null;
  for (const line of lines.slice(1)) {
    const inf = tagValue(line, "#EXT-X-STREAM-INF");
    if (inf !== null) {
      const attrs = attributes(inf);
      pending = {
        bandwidth: attrs.BANDWIDTH ? Number(attrs.BANDWIDTH) : null,
        codecs: attrs.CODECS ?? null,
        resolution: attrs.RESOLUTION ?? null,
        uri: null,
      };
      continue;
    }
    if (line.startsWith("#")) {
      if (!line.startsWith("#EXT")) comments.push(line.replace(/^#\s*/, ""));
      continue;
    }
    if (pending) {
      pending.uri = line;
      renditions.push(pending);
      pending = null;
    }
  }
  return { renditions, comments };
}

/** A media playlist as the model consumes it: durations in whole milliseconds. */
export function parseMediaPlaylist(text) {
  const lines = text.split(/\r?\n/).map((line) => line.trim()).filter((line) => line.length > 0);
  if (lines[0] !== "#EXTM3U") throw new Error("media playlist does not begin with #EXTM3U");
  const playlist = {
    targetDurationMs: null,
    mediaSequence: null,
    discontinuitySequence: null,
    playlistType: null,
    endlist: false,
    segments: [],
    comments: [],
  };
  let durationMs = null;
  let discontinuity = false;
  let index = 0;
  for (const line of lines.slice(1)) {
    let value;
    if ((value = tagValue(line, "#EXT-X-TARGETDURATION")) !== null) {
      playlist.targetDurationMs = Math.round(Number(value) * 1000);
    } else if ((value = tagValue(line, "#EXT-X-MEDIA-SEQUENCE")) !== null) {
      playlist.mediaSequence = Number(value);
    } else if ((value = tagValue(line, "#EXT-X-DISCONTINUITY-SEQUENCE")) !== null) {
      playlist.discontinuitySequence = Number(value);
    } else if ((value = tagValue(line, "#EXT-X-PLAYLIST-TYPE")) !== null) {
      playlist.playlistType = value;
    } else if (line === "#EXT-X-ENDLIST") {
      playlist.endlist = true;
    } else if (line === "#EXT-X-DISCONTINUITY") {
      discontinuity = true;
    } else if ((value = tagValue(line, "#EXTINF")) !== null) {
      durationMs = Math.round(Number(value.split(",")[0]) * 1000);
    } else if (line.startsWith("#")) {
      if (!line.startsWith("#EXT")) playlist.comments.push(line.replace(/^#\s*/, ""));
    } else {
      if (durationMs === null) throw new Error(`segment ${line} has no EXTINF`);
      playlist.segments.push({
        sequence: playlist.mediaSequence === null ? index : playlist.mediaSequence + index,
        durationMs,
        discontinuity,
        uri: line,
      });
      index += 1;
      durationMs = null;
      discontinuity = false;
    }
  }
  return playlist;
}

/**
 * What a strict player holds a live playlist to, reload after reload. Returns
 * the violations found comparing `next` against `previous` (`null` for the
 * first playlist); each carries a `kind` and a human-readable `detail`.
 */
export function checkPlaylistInvariants(previous, next) {
  const violations = [];
  const violate = (kind, detail, extra = {}) => violations.push({ kind, detail, ...extra });

  if (next.targetDurationMs === null) violate("playlist_malformed", "EXT-X-TARGETDURATION is missing");
  if (next.mediaSequence === null) violate("playlist_malformed", "EXT-X-MEDIA-SEQUENCE is missing");
  if (!next.endlist && next.discontinuitySequence === null) {
    violate("playlist_malformed", "live playlist carries no EXT-X-DISCONTINUITY-SEQUENCE");
  }
  if (next.segments.length === 0) violate("playlist_malformed", "playlist lists no segments");
  for (const segment of next.segments) {
    if (next.targetDurationMs !== null && segment.durationMs > next.targetDurationMs) {
      violate(
        "segment_exceeds_target",
        `segment ${segment.sequence} is ${segment.durationMs} ms against a ${next.targetDurationMs} ms target`,
        { sequence: segment.sequence },
      );
    }
  }
  if (!previous) return violations;

  if (next.targetDurationMs !== previous.targetDurationMs) {
    violate("target_duration_changed", `${previous.targetDurationMs} ms became ${next.targetDurationMs} ms`);
  }
  if (next.mediaSequence < previous.mediaSequence) {
    violate("media_sequence_decreased", `${previous.mediaSequence} became ${next.mediaSequence}`);
  }
  const before = new Map(previous.segments.map((segment) => [segment.sequence, segment]));
  for (const segment of next.segments) {
    const was = before.get(segment.sequence);
    if (!was) continue;
    if (was.durationMs !== segment.durationMs) {
      violate(
        "segment_duration_changed",
        `segment ${segment.sequence} was ${was.durationMs} ms, now ${segment.durationMs} ms`,
        { sequence: segment.sequence },
      );
    }
    if (was.discontinuity !== segment.discontinuity) {
      violate(
        "discontinuity_flag_changed",
        `segment ${segment.sequence} discontinuity was ${was.discontinuity}, now ${segment.discontinuity}`,
        { sequence: segment.sequence },
      );
    }
  }
  const previousLast = previous.segments.length ? previous.segments.at(-1).sequence : previous.mediaSequence - 1;
  if (next.mediaSequence > previousLast + 1) {
    violate(
      "window_gap",
      `window jumped from ending at ${previousLast} to starting at ${next.mediaSequence}; a player loses its place`,
    );
  } else if (previous.discontinuitySequence !== null && next.discontinuitySequence !== null) {
    const dropped = previous.segments
      .filter((segment) => segment.discontinuity && segment.sequence < next.mediaSequence)
      .length;
    const expected = previous.discontinuitySequence + dropped;
    if (next.discontinuitySequence !== expected) {
      violate(
        "discontinuity_sequence_mismatch",
        `${dropped} discontinuous segment(s) left the window so ${previous.discontinuitySequence} should become ${expected}, but the playlist says ${next.discontinuitySequence}`,
      );
    }
  }
  return violations;
}

/** MPEG-TS sanity on a fetched segment: sync byte first, whole packets, every packet synced. */
export function checkTransportStream(bytes) {
  const violations = [];
  if (bytes.length === 0) {
    violations.push({ kind: "segment_not_ts", detail: "segment is empty" });
    return violations;
  }
  if (bytes[0] !== 0x47) {
    violations.push({ kind: "segment_not_ts", detail: `first byte is 0x${bytes[0].toString(16)} not 0x47` });
  }
  if (bytes.length % 188 !== 0) {
    violations.push({ kind: "segment_not_ts", detail: `${bytes.length} bytes is not a multiple of 188` });
  } else {
    let unsynced = 0;
    for (let offset = 0; offset < bytes.length; offset += 188) if (bytes[offset] !== 0x47) unsynced += 1;
    if (unsynced > 0) {
      violations.push({ kind: "ts_packet_unsynced", detail: `${unsynced} of ${bytes.length / 188} packets lack the sync byte` });
    }
  }
  return violations;
}

/**
 * The Apple/Roku starting point: the segment that begins at least
 * `behind` target durations before the end of the playlist, or the first
 * one when the playlist is shorter than that.
 */
export function startSequence(playlist, behind = 3) {
  const { segments, targetDurationMs } = playlist;
  if (segments.length === 0) throw new Error("cannot start on an empty playlist");
  if (playlist.endlist) return segments[0].sequence;
  const total = segments.reduce((sum, segment) => sum + segment.durationMs, 0);
  const latestStart = total - behind * targetDurationMs;
  let offset = 0;
  let chosen = segments[0].sequence;
  for (const segment of segments) {
    if (offset > latestStart) break;
    chosen = segment.sequence;
    offset += segment.durationMs;
  }
  return chosen;
}

/**
 * The play clock. Media time is consumed at wall-clock rate through the
 * segments the playlist lists; a segment is playable once its bytes have
 * arrived. Reaching a segment that is not listed, or whose fetch has not
 * succeeded, is a stall: the clock stops until it can move.
 */
export class PlayModel {
  constructor({ startBehindTargets = 3 } = {}) {
    this.startBehindTargets = startBehindTargets;
    this.playlist = null;
    this.listed = new Map();
    this.fetched = new Set();
    this.inFlight = new Set();
    this.failed = new Map();
    this.playSequence = null;
    this.offsetMs = 0;
    this.lastTickMs = null;
    this.stalledSince = null;
    this.stallReason = null;
    this.stalls = [];
    this.ended = false;
    // Between choosing a start and holding its bytes: the initial buffer a
    // player fills before its clock runs, reported as startup, not a stall.
    this.buffering = false;
  }

  /** Adopt a (re)loaded playlist; returns invariant violations against the last one. */
  acceptPlaylist(playlist) {
    const violations = checkPlaylistInvariants(this.playlist, playlist);
    this.playlist = playlist;
    this.listed = new Map(playlist.segments.map((segment) => [segment.sequence, segment]));
    return violations;
  }

  get targetDurationMs() {
    return this.playlist ? this.playlist.targetDurationMs : null;
  }

  get endSequence() {
    return this.playlist && this.playlist.segments.length ? this.playlist.segments.at(-1).sequence : null;
  }

  get started() {
    return this.playSequence !== null;
  }

  get stalled() {
    return this.stalledSince !== null;
  }

  start(nowMs) {
    if (!this.playlist) throw new Error("start needs a playlist");
    this.playSequence = startSequence(this.playlist, this.startBehindTargets);
    this.offsetMs = 0;
    this.lastTickMs = nowMs;
    this.stalledSince = null;
    this.buffering = !this.playable(this.playSequence);
  }

  playable(sequence) {
    return this.listed.has(sequence) && this.fetched.has(sequence);
  }

  markInFlight(sequence) {
    this.inFlight.add(sequence);
  }

  markFetched(sequence) {
    this.inFlight.delete(sequence);
    this.failed.delete(sequence);
    this.fetched.add(sequence);
  }

  markFailed(sequence, status) {
    this.inFlight.delete(sequence);
    this.failed.set(sequence, status);
  }

  stall(nowMs, reason) {
    if (this.stalledSince !== null) return;
    this.stalledSince = nowMs;
    this.stallReason = reason;
  }

  unstall(nowMs) {
    if (this.stalledSince === null) return;
    this.stalls.push({
      startedAtMs: this.stalledSince,
      endedAtMs: nowMs,
      durationMs: nowMs - this.stalledSince,
      reason: this.stallReason,
      sequence: this.playSequence,
    });
    this.stalledSince = null;
    this.stallReason = null;
  }

  /** Advance the clock to `nowMs`. */
  tick(nowMs) {
    if (!this.started || this.ended) return;
    if (this.buffering) {
      if (this.playlist && this.playSequence < this.playlist.mediaSequence) {
        this.playSequence = startSequence(this.playlist, this.startBehindTargets);
      }
      if (!this.playable(this.playSequence)) return;
      this.buffering = false;
      this.lastTickMs = nowMs;
      return;
    }
    const elapsed = Math.max(0, nowMs - this.lastTickMs);
    this.lastTickMs = nowMs;
    if (this.stalled) {
      if (!this.playable(this.playSequence)) {
        // A window that slid past the stalled sequence is a player that lost
        // its place; re-sync to the live edge the way a player re-seeks.
        if (this.playlist && this.playSequence < this.playlist.mediaSequence) {
          this.playSequence = startSequence(this.playlist, this.startBehindTargets);
          this.offsetMs = 0;
          this.stallReason = "behind_window";
          if (this.playable(this.playSequence)) this.unstall(nowMs);
        }
        return;
      }
      this.unstall(nowMs);
      return;
    }
    this.offsetMs += elapsed;
    while (true) {
      const current = this.listed.get(this.playSequence);
      if (!current) {
        this.stall(nowMs, this.playSequence < (this.playlist?.mediaSequence ?? 0) ? "behind_window" : "not_listed");
        return;
      }
      if (this.offsetMs < current.durationMs) return;
      this.offsetMs -= current.durationMs;
      const next = this.playSequence + 1;
      if (!this.listed.has(next)) {
        if (this.playlist && this.playlist.endlist) {
          this.ended = true;
          this.offsetMs = current.durationMs;
          return;
        }
        this.playSequence = next;
        this.offsetMs = 0;
        this.stall(nowMs, "not_listed");
        return;
      }
      this.playSequence = next;
      if (!this.fetched.has(next)) {
        this.offsetMs = 0;
        this.stall(nowMs, this.failed.has(next) ? "fetch_failed" : "not_fetched");
        return;
      }
    }
  }

  /** Media milliseconds listed ahead of the play clock. */
  runwayMs() {
    if (!this.started) return 0;
    const current = this.listed.get(this.playSequence);
    if (!current) return 0;
    let runway = current.durationMs - this.offsetMs;
    for (let sequence = this.playSequence + 1; this.listed.has(sequence); sequence += 1) {
      runway += this.listed.get(sequence).durationMs;
    }
    return Math.max(0, runway);
  }

  /** Milliseconds until playback reaches `sequence` (0 when it is current or past). */
  startsInMs(sequence) {
    if (!this.started || sequence <= this.playSequence) return 0;
    const current = this.listed.get(this.playSequence);
    if (!current) return 0;
    let ahead = current.durationMs - this.offsetMs;
    for (let s = this.playSequence + 1; s < sequence; s += 1) {
      const segment = this.listed.get(s);
      if (!segment) return Number.POSITIVE_INFINITY;
      ahead += segment.durationMs;
    }
    return Math.max(0, ahead);
  }

  /**
   * Listed segments the player must have within `prefetchMs`: the current
   * one, and those starting no later than that far ahead. A failed fetch is
   * asked for again while it stays listed.
   */
  needed(prefetchMs) {
    if (!this.started || !this.playlist) return [];
    const wanted = [];
    for (let sequence = Math.max(this.playSequence, this.playlist.mediaSequence); this.listed.has(sequence); sequence += 1) {
      if (this.fetched.has(sequence) || this.inFlight.has(sequence)) continue;
      if (this.startsInMs(sequence) > prefetchMs) break;
      wanted.push(sequence);
    }
    return wanted;
  }

  /** Forget bytes the window no longer lists, so the sets stay bounded. */
  prune() {
    if (!this.playlist) return;
    const floor = this.playlist.mediaSequence;
    for (const sequence of this.fetched) if (sequence < floor) this.fetched.delete(sequence);
    for (const sequence of this.failed.keys()) if (sequence < floor) this.failed.delete(sequence);
  }

  sample(nowMs) {
    return {
      playSequence: this.playSequence,
      endSequence: this.endSequence,
      runwayMs: this.runwayMs(),
      stalled: this.stalled,
      buffering: this.buffering,
      stallReason: this.stallReason,
      stalledForMs: this.stalled ? nowMs - this.stalledSince : 0,
    };
  }
}

export function percentile(values, fraction) {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(fraction * sorted.length) - 1));
  return sorted[index];
}
