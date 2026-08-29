import { describe, expect, it } from "vitest";

import type { Display, DisplayAssignment, DisplayReceiver, DisplaySurface } from "./client";
import {
  agoLabel, assignmentFor, failureOf, heldBy, inputPrompt, isIssuesBoard, isSignageSurface, platformName, receiverBootstrap,
  signageInputKey, surfaceInput, tvStatus,
} from "./displays";

const surfaces: DisplaySurface[] = [
  { world: "com.lait.issues", surface: "issues.board.wall", title: "Issues board", contractVersion: 1, outputs: [] },
  { world: "com.lait.signage", surface: "signage.program", title: "Signage program", contractVersion: 4, outputs: [] },
];
/** The Signage World as released: its surface still calls a screen a program. */
const releasedSignage: DisplaySurface = { ...surfaces[1], contractVersion: 3 };

describe("the displays coordination rules", () => {
  it("spells the pinned receiver bootstrap exactly", () => {
    const bootstrap = JSON.parse(receiverBootstrap({
      origin: "https://192.168.1.20:7443",
      certificateSha256: "abc123",
      certificatePem: "-----BEGIN CERTIFICATE-----",
    }));
    expect(bootstrap).toEqual({
      protocol_major: 1,
      trust: { kind: "pinned_certificate", origin: "https://192.168.1.20:7443", sha256: "abc123" },
      certificate_pem: "-----BEGIN CERTIFICATE-----",
      rendezvous: null,
    });
  });

  it("presents known receiver platforms by their product names", () => {
    expect(platformName("android_tv")).toBe("Android TV");
    expect(platformName("webos")).toBe("webOS");
    expect(platformName("some_new_thing")).toBe("Some New Thing");
  });

  it("wraps what the person typed the way each package declares its input", () => {
    // Which key Signage takes is the surface's declared contract, not this
    // build's: the installed release (contract 3) refused `screen` with
    // "missing field `program`", and this tree's (contract 4) refuses the
    // reverse.
    expect(signageInputKey(surfaces[1])).toBe("screen");
    expect(signageInputKey(releasedSignage)).toBe("program");
    expect(surfaceInput(surfaces[1], "bod_lobby")).toBe(JSON.stringify({ screen: "bod_lobby" }));
    expect(surfaceInput(releasedSignage, "bod_lobby")).toBe(JSON.stringify({ program: "bod_lobby" }));
    expect(surfaceInput(surfaces[0], "ENG")).toBe(JSON.stringify({ project: "ENG" }));
    expect(surfaceInput({ world: "other", surface: "x", contractVersion: 1 }, "{\"a\":1}")).toBe("{\"a\":1}");
  });

  it("special-cases the signage and issues surfaces and nothing else, whichever World serves them", () => {
    expect(isSignageSurface({ world: "com.lait.signage", surface: "signage.program" })).toBe(true);
    // A local copy of Signage serves the same surface under its own id, and
    // it is the same form — keyed on the World it fell to "package JSON".
    expect(isSignageSurface({ world: "local.signage", surface: "signage.program" })).toBe(true);
    expect(isSignageSurface({ world: "com.lait.signage", surface: "signage.other" })).toBe(false);
    expect(isIssuesBoard({ world: "com.lait.issues", surface: "issues.board.wall" })).toBe(true);
    expect(isIssuesBoard({ world: "issues", surface: "board" })).toBe(false);
  });

  it("says which World holds a TV, and nothing more about what it shows", () => {
    const library = [{ world: "com.lait.signage", displayName: "Signage" }, { world: "local.signage", displayName: "Signage" }];
    const assignment = { world: "com.lait.signage", surface: "signage.program" } as DisplayAssignment;
    expect(heldBy(assignment, surfaces, library)).toBe("Held by Signage");
    // Two copies of a World are told apart, as the Library tells them apart.
    expect(heldBy({ world: "local.signage", surface: "signage.program" } as DisplayAssignment, surfaces, library)).toBe("Held by Signage (local copy)");
    // A World the library does not name is called by its surface's title.
    expect(heldBy({ world: "com.lait.issues", surface: "issues.board.wall" } as DisplayAssignment, surfaces, library)).toBe("Held by Issues board");
    expect(heldBy(undefined, surfaces, library)).toMatch(/Not held by any World/);
  });

  it("tells a TV's whole state in one chip, by the clock as much as by the report", () => {
    const now = 1_755_000_000_000;
    const receiver = (overrides: Partial<DisplayReceiver>): DisplayReceiver => ({
      device: "dev", label: "TV", platform: "webos", build: "1", issuedAtUnixMs: now - 10_000, revokedAtUnixMs: null, health: null,
      ...overrides,
    });
    const health = {
      revision: "r", currentItem: "i", elapsedMs: 0, connection: "online", playback: "displaying", lastError: "none",
      stagedItems: 0, stagedBytes: 0, driftResidualMs: 0, correctionEvents: 0, pipelineUnobservable: true, reportedAtUnixMs: now - 20_000,
    };
    // Enrolled a moment ago and not yet heard: connecting. Enrolled long ago
    // and not heard since this daemon started listening: say so.
    expect(tvStatus(receiver({}), now)).toEqual({ label: "Connecting…", tone: "neutral" });
    expect(tvStatus(receiver({ issuedAtUnixMs: now - 3 * 60_000 }), now)).toEqual({ label: "Not heard from", tone: "warn" });
    expect(tvStatus(receiver({ health }), now)).toEqual({ label: "Connected", tone: "good" });
    expect(tvStatus(receiver({ health: { ...health, connection: "offline" } }), now)).toEqual({ label: "Offline", tone: "crit" });
    expect(tvStatus(receiver({ health: { ...health, connection: "retrying" } }), now)).toEqual({ label: "Reconnecting…", tone: "warn" });
    // A TV whose last word was "online" ten minutes ago is gone, not connected.
    expect(tvStatus(receiver({ health: { ...health, reportedAtUnixMs: now - 10 * 60_000 } }), now))
      .toEqual({ label: "Last seen 10 min ago", tone: "warn" });
    // A daemon that does not say when: the report is all there is.
    expect(tvStatus(receiver({ health: { ...health, reportedAtUnixMs: null } }), now)).toEqual({ label: "Connected", tone: "good" });
    expect(tvStatus(receiver({ revokedAtUnixMs: 5 }), now)).toEqual({ label: "Removed", tone: "neutral" });
    expect(agoLabel(90_000)).toBe("2 min ago");
    expect(agoLabel(5 * 3_600_000)).toBe("5 h ago");
    expect(agoLabel(3 * 86_400_000)).toBe("3 days ago");
  });

  it("finds the failure of one action and no other", () => {
    const failures = [
      { what: "a", error: "refused", retryable: true, key: "display.rendezvous.mint" },
      { what: "b", error: "other", retryable: true, key: null },
    ];
    expect(failureOf(failures, "display.rendezvous.mint")?.error).toBe("refused");
    expect(failureOf(failures, "display.device.revoke:x")).toBeUndefined();
  });

  it("resolves a receiver to its latest unrevoked assignment", () => {
    const assignment = (overrides: Partial<DisplayAssignment>): DisplayAssignment => ({
      assignment: "asg", device: "dev", orbit: "orb", space: "spc", program: "prg",
      world: "issues", surface: "board", controller: "ctl", theme: "dark",
      syncGroup: null, syncMode: null, staticDelayMs: 0, expiresAtUnixMs: null,
      revokedAtUnixMs: null, ...overrides,
    });
    const display = {
      assignments: [
        assignment({ assignment: "old", device: "tv" }),
        assignment({ assignment: "revoked", device: "tv", revokedAtUnixMs: 5 }),
        assignment({ assignment: "current", device: "tv" }),
        assignment({ assignment: "other", device: "other-tv" }),
      ],
    } as Display;
    expect(assignmentFor(display, "tv")?.assignment).toBe("current");
    expect(assignmentFor(display, "unknown")).toBeUndefined();
  });
});
