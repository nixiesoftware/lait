import assert from "node:assert/strict";
import { describe, it } from "node:test";

import {
  checkPlaylistInvariants,
  checkTransportStream,
  parseMasterPlaylist,
  parseMediaPlaylist,
  percentile,
  PlayModel,
  startSequence,
} from "./lib/hls.mjs";

const MASTER = `#EXTM3U
#EXT-X-VERSION:3
# lait 0.9.9
#EXT-X-STREAM-INF:BANDWIDTH=4000000,CODECS="avc1.640028",RESOLUTION=1280x720
./renditions/prog-abc.m3u8
`;

/** A live window: `first..first+count-1`, every segment `durationMs`, discontinuities at the sequences named. */
function live({ first, count, durationMs = 2000, target = 2, discontinuitySequence = 0, discontinuities = [], endlist = false }) {
  const lines = ["#EXTM3U", "#EXT-X-VERSION:3", `#EXT-X-TARGETDURATION:${target}`, `#EXT-X-MEDIA-SEQUENCE:${first}`];
  if (!endlist) lines.push(`#EXT-X-DISCONTINUITY-SEQUENCE:${discontinuitySequence}`);
  for (let sequence = first; sequence < first + count; sequence += 1) {
    if (discontinuities.includes(sequence)) lines.push("#EXT-X-DISCONTINUITY");
    lines.push(`#EXTINF:${(durationMs / 1000).toFixed(3)},`, `../segments/${sequence}.ts`);
  }
  if (endlist) lines.push("#EXT-X-ENDLIST");
  return parseMediaPlaylist(`${lines.join("\n")}\n`);
}

describe("playlist parsing", () => {
  it("reads the master's renditions and its comments", () => {
    const master = parseMasterPlaylist(MASTER);
    assert.equal(master.renditions.length, 1);
    assert.deepEqual(master.renditions[0], {
      bandwidth: 4_000_000, codecs: "avc1.640028", resolution: "1280x720", uri: "./renditions/prog-abc.m3u8",
    });
    assert.deepEqual(master.comments, ["lait 0.9.9"]);
  });

  it("reads a live media playlist into millisecond segments", () => {
    const playlist = live({ first: 41, count: 3, discontinuitySequence: 7, discontinuities: [42] });
    assert.equal(playlist.targetDurationMs, 2000);
    assert.equal(playlist.mediaSequence, 41);
    assert.equal(playlist.discontinuitySequence, 7);
    assert.equal(playlist.endlist, false);
    assert.deepEqual(playlist.segments.map((segment) => [segment.sequence, segment.durationMs, segment.discontinuity]), [
      [41, 2000, false], [42, 2000, true], [43, 2000, false],
    ]);
    assert.equal(playlist.segments[0].uri, "../segments/41.ts");
  });

  it("reads a complete playlist as VOD with ENDLIST", () => {
    const playlist = live({ first: 0, count: 2, endlist: true });
    assert.equal(playlist.endlist, true);
    assert.equal(playlist.discontinuitySequence, null);
  });

  it("refuses a segment without EXTINF", () => {
    assert.throws(() => parseMediaPlaylist("#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\nseg.ts\n"), /no EXTINF/);
  });
});

describe("start position", () => {
  it("starts three target durations behind the live edge", () => {
    // 6 × 2 s = 12 s; 3 targets back is 6 s in, which is the start of segment 13.
    const playlist = live({ first: 10, count: 6 });
    assert.equal(startSequence(playlist), 13);
  });

  it("starts at the first segment when the window is shorter than three targets", () => {
    assert.equal(startSequence(live({ first: 5, count: 2 })), 5);
  });

  it("starts a complete playlist from its beginning", () => {
    assert.equal(startSequence(live({ first: 0, count: 50, endlist: true })), 0);
  });
});

describe("playlist invariants across reloads", () => {
  it("passes a window that slides forward cleanly", () => {
    const a = live({ first: 10, count: 6, discontinuitySequence: 2, discontinuities: [11] });
    const b = live({ first: 12, count: 6, discontinuitySequence: 3 });
    assert.deepEqual(checkPlaylistInvariants(a, b), []);
  });

  it("catches a media sequence that goes backwards", () => {
    const kinds = checkPlaylistInvariants(live({ first: 12, count: 6 }), live({ first: 11, count: 6 })).map((v) => v.kind);
    assert.deepEqual(kinds, ["media_sequence_decreased"]);
  });

  it("catches a listed segment whose duration changed", () => {
    const a = live({ first: 10, count: 3 });
    const b = live({ first: 10, count: 3, durationMs: 2500, target: 3 });
    const kinds = checkPlaylistInvariants(a, b).map((v) => v.kind);
    assert.ok(kinds.includes("segment_duration_changed"));
    assert.ok(kinds.includes("target_duration_changed"));
  });

  it("catches a discontinuity sequence that does not account for what left the window", () => {
    const a = live({ first: 10, count: 6, discontinuitySequence: 2, discontinuities: [11] });
    const stale = live({ first: 12, count: 6, discontinuitySequence: 2 });
    const violations = checkPlaylistInvariants(a, stale);
    assert.equal(violations.length, 1);
    assert.equal(violations[0].kind, "discontinuity_sequence_mismatch");
    assert.match(violations[0].detail, /should become 3/);
  });

  it("catches a window that jumped past the previous one", () => {
    const kinds = checkPlaylistInvariants(live({ first: 10, count: 3 }), live({ first: 20, count: 3 })).map((v) => v.kind);
    assert.deepEqual(kinds, ["window_gap"]);
  });

  it("catches a segment longer than the target and a missing discontinuity sequence", () => {
    const text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:0\n#EXTINF:2.500,\n../segments/0.ts\n";
    const kinds = checkPlaylistInvariants(null, parseMediaPlaylist(text)).map((v) => v.kind);
    assert.deepEqual(kinds.sort(), ["playlist_malformed", "segment_exceeds_target"]);
  });
});

describe("transport stream shape", () => {
  it("accepts whole synced packets", () => {
    const bytes = new Uint8Array(188 * 3);
    for (const offset of [0, 188, 376]) bytes[offset] = 0x47;
    assert.deepEqual(checkTransportStream(bytes), []);
  });

  it("names a wrong first byte, a ragged length, and unsynced packets", () => {
    const ragged = new Uint8Array(190);
    ragged[0] = 0x47;
    assert.deepEqual(checkTransportStream(ragged).map((v) => v.kind), ["segment_not_ts"]);
    const unsynced = new Uint8Array(188 * 2);
    unsynced[0] = 0x47;
    assert.deepEqual(checkTransportStream(unsynced).map((v) => v.kind), ["ts_packet_unsynced"]);
    const wrong = new Uint8Array(188);
    assert.deepEqual(checkTransportStream(wrong).map((v) => v.kind), ["segment_not_ts", "ts_packet_unsynced"]);
  });
});

describe("play clock", () => {
  function primed(playlistOptions = { first: 10, count: 6 }) {
    const model = new PlayModel();
    model.acceptPlaylist(live(playlistOptions));
    for (const segment of model.listed.values()) model.markFetched(segment.sequence);
    model.start(0);
    return model;
  }

  it("consumes segments by their durations in real time", () => {
    const model = primed();
    assert.equal(model.playSequence, 13);
    assert.equal(model.runwayMs(), 6000);
    model.tick(1500);
    assert.equal(model.playSequence, 13);
    assert.equal(model.runwayMs(), 4500);
    model.tick(2500);
    assert.equal(model.playSequence, 14);
    assert.equal(model.offsetMs, 500);
    assert.equal(model.stalled, false);
  });

  it("stalls when the next segment is not listed, and resumes when it arrives", () => {
    const model = primed();
    model.tick(6000);
    assert.equal(model.playSequence, 16);
    assert.equal(model.stalled, true);
    assert.equal(model.stallReason, "not_listed");
    assert.equal(model.runwayMs(), 0);
    model.tick(7000);
    assert.equal(model.stalled, true);
    model.acceptPlaylist(live({ first: 11, count: 6 }));
    model.markFetched(16);
    model.tick(7250);
    assert.equal(model.stalled, false);
    assert.equal(model.stalls.length, 1);
    assert.equal(model.stalls[0].durationMs, 1250);
    assert.equal(model.stalls[0].reason, "not_listed");
    model.tick(8250);
    assert.equal(model.playSequence, 16);
    assert.equal(model.offsetMs, 1000);
  });

  it("buffers before the first segment without counting a stall", () => {
    const model = new PlayModel();
    model.acceptPlaylist(live({ first: 0, count: 6 }));
    model.start(0);
    assert.equal(model.buffering, true);
    assert.equal(model.stalled, false);
    model.tick(900);
    assert.equal(model.offsetMs, 0);
    model.markFetched(3);
    model.tick(1000);
    assert.equal(model.buffering, false);
    model.tick(1500);
    assert.equal(model.offsetMs, 500);
    assert.equal(model.stalls.length, 0);
  });

  it("stalls on a listed segment whose bytes have not arrived", () => {
    const model = new PlayModel();
    model.acceptPlaylist(live({ first: 0, count: 6 }));
    model.markFetched(3);
    model.start(0);
    assert.equal(model.playSequence, 3);
    assert.equal(model.stalled, false);
    model.tick(2000);
    assert.equal(model.playSequence, 4);
    assert.equal(model.stalled, true);
    assert.equal(model.stallReason, "not_fetched");
    model.markFailed(4, 404);
    model.markFetched(4);
    model.tick(2300);
    assert.equal(model.stalled, false);
    assert.equal(model.stalls[0].durationMs, 300);
  });

  it("asks for the current segment and those starting within the prefetch lead", () => {
    const model = new PlayModel();
    model.acceptPlaylist(live({ first: 0, count: 6 }));
    model.start(0);
    assert.equal(model.playSequence, 3);
    assert.deepEqual(model.needed(0), [3]);
    assert.deepEqual(model.needed(2000), [3, 4]);
    model.markInFlight(3);
    assert.deepEqual(model.needed(2000), [4]);
    model.markFetched(3);
    assert.equal(model.buffering, true);
    model.tick(1500);
    // The clock starts when the first segment lands: 4 starts in 2000 ms,
    // 5 in 4000 ms, so only 4 is inside a 2000 ms lead.
    assert.equal(model.buffering, false);
    assert.deepEqual(model.needed(2000), [4]);
    model.tick(3500);
    assert.equal(model.playSequence, 4);
    assert.equal(model.stallReason, "not_fetched");
    assert.deepEqual(model.needed(2000), [4, 5]);
  });

  it("re-syncs to the edge when the window slides past a stalled sequence", () => {
    const model = primed();
    model.tick(6000);
    assert.equal(model.stalled, true);
    model.acceptPlaylist(live({ first: 20, count: 6 }));
    model.tick(6500);
    assert.equal(model.playSequence, 23);
    assert.equal(model.stalled, true);
    assert.equal(model.stallReason, "behind_window");
    model.markFetched(23);
    model.tick(6600);
    assert.equal(model.stalled, false);
    assert.equal(model.stalls[0].reason, "behind_window");
  });

  it("ends a complete playlist instead of stalling after its last segment", () => {
    const model = primed({ first: 0, count: 3, endlist: true });
    assert.equal(model.playSequence, 0);
    model.tick(6500);
    assert.equal(model.ended, true);
    assert.equal(model.stalled, false);
  });

  it("prunes bytes the window no longer lists", () => {
    const model = primed();
    model.acceptPlaylist(live({ first: 14, count: 6 }));
    model.prune();
    assert.deepEqual([...model.fetched].sort((a, b) => a - b), [14, 15]);
  });
});

describe("percentile", () => {
  it("takes the nearest-rank percentile", () => {
    assert.equal(percentile([5, 1, 3, 2, 4], 0.5), 3);
    assert.equal(percentile([5, 1, 3, 2, 4], 0.95), 5);
    assert.equal(percentile([], 0.5), null);
  });
});
