import { describe, expect, it } from "vitest";

import type { Display, DisplayAssignment, DisplaySurface } from "./client";
import {
  assignmentDraftValid, assignmentFor, assignmentPayload, codeEntry, inputProblem, isSignageSurface, minutesLeft,
  newAssignmentDraft, platformName, receiverBootstrap,
} from "./displays";

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

  it("spells a code the way the television takes it: site, then code", () => {
    expect(codeEntry({ site: "acme", code: "7K3Q-0111" })).toBe("acme-7K3Q-0111");
    // No published site: the television already reaches this coordinator,
    // and the code stands alone.
    expect(codeEntry({ site: null, code: "7K3Q-0111" })).toBe("7K3Q-0111");
  });

  it("counts a code's remaining minutes up, and never below zero", () => {
    expect(minutesLeft(10 * 60_000 + 1, 0)).toBe(11);
    expect(minutesLeft(10 * 60_000, 0)).toBe(10);
    expect(minutesLeft(5, 10)).toBe(0);
  });

  it("turns a draft into the assignment the daemon takes, wrapping only the signage program id", () => {
    const surfaces: DisplaySurface[] = [
      { world: "com.lait.signage", surface: "signage.program", title: "Signage program", contractVersion: 1, outputs: [] },
      { world: "issues", surface: "board", title: "Issue board", contractVersion: 1, outputs: [] },
    ];
    const orbits = [{ space: "orb_1", name: "Home", path: "/x", lastOpened: null }];
    const draft = newAssignmentDraft(surfaces, orbits);
    expect(assignmentPayload(draft, surfaces)).toBeNull();
    expect(assignmentPayload({ ...draft, input: "bod_lobby" }, surfaces)).toEqual({
      orbit: "orb_1", world: "com.lait.signage", surface: "signage.program",
      inputJson: JSON.stringify({ program: "bod_lobby" }), theme: "dark", staleAfterMs: 120_000,
      onStale: "keepWithNativeBanner", syncGroup: null, syncMode: "stayInSync", staticDelayMs: 0, expiresAtUnixMs: null,
    });
    // Any other surface's input crosses verbatim.
    expect(assignmentPayload({ ...draft, chosenKey: "issues board", input: "{\"project\":\"ENG\"}" }, surfaces)?.inputJson)
      .toBe("{\"project\":\"ENG\"}");
    // A draft with nothing to show is not an assignment.
    expect(assignmentPayload({ ...draft, chosenKey: "nowhere", input: "x" }, surfaces)).toBeNull();
    // Non-JSON input for a JSON surface is refused here, with a reason, not
    // by the daemon from the other window.
    expect(assignmentPayload({ ...draft, chosenKey: "issues board", input: "bod_lobby" }, surfaces)).toBeNull();
    expect(inputProblem({ chosenKey: "issues board", input: "bod_lobby" }, surfaces)).toMatch(/must be JSON/);
    expect(inputProblem({ chosenKey: "issues board", input: "{\"project\":\"ENG\"}" }, surfaces)).toBeNull();
    expect(inputProblem({ chosenKey: "com.lait.signage signage.program", input: "bod_lobby" }, surfaces)).toBeNull();
    expect(inputProblem({ chosenKey: "com.lait.signage signage.program", input: " " }, surfaces)).toMatch(/body id/);
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
