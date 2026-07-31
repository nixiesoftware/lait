import { describe, expect, it } from "vitest";

import { downloadUrl, isComplete, readResidency, statusMessage } from "./content";

describe("content residency", () => {
  it("reads what a HEAD answers", () => {
    const headers = new Headers({
      "content-length": "700000",
      "x-lait-chunk-count": "3",
      "x-lait-resident-chunks": "3",
      "x-lait-pinned": "1",
    });
    expect(readResidency(headers)).toEqual({
      size: 700000,
      chunkCount: 3,
      residentChunks: 3,
      pinned: true,
    });
  });

  it("treats a missing or nonsense header as zero rather than NaN", () => {
    // A NaN here would propagate into a progress bar and render as an empty
    // element with no clue why.
    const headers = new Headers({ "x-lait-chunk-count": "not a number" });
    const residency = readResidency(headers);
    expect(residency.size).toBe(0);
    expect(residency.chunkCount).toBe(0);
    expect(residency.pinned).toBe(false);
  });

  it("knows a partially arrived file from a complete one", () => {
    expect(isComplete({ size: 10, chunkCount: 3, residentChunks: 3, pinned: false })).toBe(true);
    expect(isComplete({ size: 10, chunkCount: 3, residentChunks: 2, pinned: false })).toBe(false);
    // A content with no chunks is not "complete" — it is a content nobody has
    // said anything about yet.
    expect(isComplete({ size: 0, chunkCount: 0, residentChunks: 0, pinned: false })).toBe(false);
  });
});

describe("download urls", () => {
  it("escapes both the space and the content id", () => {
    const url = downloadUrl("ws /x", "ab+cd");
    expect(url).toContain("ws%20%2Fx");
    expect(url).toContain("ab%2Bcd");
  });

  it("never carries a credential", () => {
    // A download URL is pasted, put in a src, and left in history. The engine
    // refuses a query token on this route; this is the client half of the same
    // rule, so a regression here shows up as a test failure rather than as a
    // 401 nobody can explain.
    const url = downloadUrl("ws_x", "deadbeef", "notes.txt", 4096);
    expect(url).not.toContain("token");
  });

  it("puts a name and an offset in the query only when there is one", () => {
    expect(downloadUrl("ws_x", "c")).toBe("/api/spaces/ws_x/content/c");
    expect(downloadUrl("ws_x", "c", "a b.txt")).toContain("name=a+b.txt");
    expect(downloadUrl("ws_x", "c", undefined, 100)).toContain("offset=100");
  });

  it("escapes a hostile name into the query rather than out of it", () => {
    const url = downloadUrl("ws_x", "c", "../../evil.txt&token=stolen");
    expect(url).not.toContain("&token=stolen");
    expect(url).toContain("name=");
  });
});

describe("status messages", () => {
  it("says something different, and useful, for each refusal", () => {
    // The engine's refusals are typed because each is a different next move.
    // The messages have to keep that distinction or the typing was pointless.
    const seen = new Set<string>();
    for (const status of [403, 404, 409, 413, 416, 422, 503]) {
      const message = statusMessage(status);
      expect(message.length).toBeGreaterThan(0);
      expect(seen.has(message)).toBe(false);
      seen.add(message);
    }
  });

  it("distinguishes not-here-yet from not-a-thing", () => {
    // The one worth reading twice: 409 fixes itself once a transfer runs, and
    // 404 never will.
    expect(statusMessage(409)).toMatch(/arriv/i);
    expect(statusMessage(404)).toMatch(/not in this space/i);
  });

  it("still says something for a status it has never seen", () => {
    expect(statusMessage(418)).toContain("418");
  });
});
