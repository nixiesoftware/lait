import { describe, expect, it } from "vitest";

import type { Display, DisplayAssignment } from "./client";
import { assignmentDraftValid, assignmentFor, isSignageSurface, platformName, receiverBootstrap } from "./displays";

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

  it("holds the daemon's assignment bounds before a draft may cross", () => {
    const draft = { input: "{}", staleSeconds: "120", syncGroup: "", staticDelay: "0" };
    expect(assignmentDraftValid(draft)).toBe(true);
    expect(assignmentDraftValid({ ...draft, input: "  " })).toBe(false);
    expect(assignmentDraftValid({ ...draft, staleSeconds: "30" })).toBe(false);
    expect(assignmentDraftValid({ ...draft, staleSeconds: "31" })).toBe(true);
    expect(assignmentDraftValid({ ...draft, syncGroup: "wall-a" })).toBe(true);
    expect(assignmentDraftValid({ ...draft, syncGroup: "Wall A" })).toBe(false);
    expect(assignmentDraftValid({ ...draft, staticDelay: "-60000" })).toBe(true);
    expect(assignmentDraftValid({ ...draft, staticDelay: "60001" })).toBe(false);
    expect(assignmentDraftValid({ ...draft, staticDelay: "soon" })).toBe(false);
  });

  it("special-cases the signage program surface and nothing else", () => {
    expect(isSignageSurface({ world: "com.lait.signage", surface: "signage.program" })).toBe(true);
    expect(isSignageSurface({ world: "com.lait.signage", surface: "signage.other" })).toBe(false);
    expect(isSignageSurface({ world: "issues", surface: "signage.program" })).toBe(false);
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
