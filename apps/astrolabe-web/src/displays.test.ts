import { describe, expect, it } from "vitest";

import type { Display, DisplayAssignment, DisplayChoices, DisplayReceiver, DisplaySurface } from "./client";
import {
  agoLabel, assignmentDraftValid, assignmentFor, assignmentPayload, chooser, choicesFor, codeEntry, failureOf, inputProblem, inputPrompt,
  isIssuesBoard, isSignageSurface, minutesLeft, newAssignmentDraft, platformName, receiverBootstrap, showingLine, signageInputKey,
  surfaceChoice, surfaceInput, tvStatus,
} from "./displays";

const surfaces: DisplaySurface[] = [
  { world: "com.lait.issues", surface: "issues.board.wall", title: "Issues board", contractVersion: 1, outputs: [] },
  { world: "com.lait.signage", surface: "signage.program", title: "Signage program", contractVersion: 4, outputs: [] },
];
/** The Signage World as released: its surface still calls a screen a program. */
const releasedSignage: DisplaySurface = { ...surfaces[1], contractVersion: 3 };
const orbits = [{ space: "orb_1", name: "Home", path: "/x", lastOpened: null }];

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

  it("offers each surface as a choice and asks for the one thing it needs", () => {
    expect(surfaceChoice(surfaces[1])).toBe("A Signage screen");
    expect(surfaceChoice(surfaces[0])).toBe("An Issues board");
    expect(inputPrompt(surfaces[1])).toMatchObject({ label: "Screen", json: false });
    expect(inputPrompt(surfaces[0])).toMatchObject({ label: "Project", json: false });
    expect(inputPrompt({ world: "other", surface: "x" })).toMatchObject({ json: true });
    // Signage first when it is there: the surface a TV most often shows.
    expect(newAssignmentDraft(surfaces, orbits).chosenKey).toBe("com.lait.signage signage.program");
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

  it("offers a picker when the World listed its choices, and a typed field that says why not", () => {
    const prompt = { label: "Screen", hint: "The screen's id." };
    const lobby = { id: "bod_lobby", title: "Lobby" };
    const listed = (over: Partial<DisplayChoices>): DisplayChoices => ({
      orbit: "orb_1", world: "com.lait.signage", surface: "signage.program", choices: [lobby], unavailable: null, ...over,
    });
    expect(choicesFor([listed({})], "orb_1", surfaces[1])?.choices).toEqual([lobby]);
    // Another Space's listing is not this one's.
    expect(choicesFor([listed({})], "orb_2", surfaces[1])).toBeUndefined();
    expect(chooser(listed({}), false, prompt)).toEqual({ kind: "pick", choices: [lobby] });
    // Nothing to show and could-not-list are different facts, each said.
    expect(chooser(listed({ choices: [] }), false, prompt)).toMatchObject({ kind: "type", hint: expect.stringMatching(/Nothing to show/) });
    expect(chooser(listed({ choices: null, unavailable: "runner predates listing" }), false, prompt))
      .toMatchObject({ kind: "type", hint: expect.stringMatching(/Couldn't list them \(runner predates listing\)/) });
    // Not yet asked, or still asking: the typed field, honestly labelled.
    expect(chooser(undefined, true, prompt)).toMatchObject({ kind: "type", hint: expect.stringMatching(/Looking up/) });
    expect(chooser(undefined, false, prompt)).toEqual({ kind: "type", hint: prompt.hint });
  });

  it("turns a draft into the assignment the daemon takes", () => {
    const draft = newAssignmentDraft(surfaces, orbits);
    expect(assignmentPayload(draft, surfaces)).toBeNull();
    expect(assignmentPayload({ ...draft, input: "bod_lobby" }, surfaces)).toEqual({
      orbit: "orb_1", world: "com.lait.signage", surface: "signage.program",
      inputJson: JSON.stringify({ screen: "bod_lobby" }), theme: "dark", staleAfterMs: 120_000,
      onStale: "keepWithNativeBanner", syncGroup: null, syncMode: "stayInSync", staticDelayMs: 0, expiresAtUnixMs: null,
    });
    expect(assignmentPayload({ ...draft, chosenKey: "com.lait.issues issues.board.wall", input: "ENG" }, surfaces)?.inputJson)
      .toBe(JSON.stringify({ project: "ENG" }));
    // A draft with nothing to show is not an assignment.
    expect(assignmentPayload({ ...draft, chosenKey: "nowhere", input: "x" }, surfaces)).toBeNull();
    // A JSON surface refuses non-JSON here, with a reason, not from the
    // other window after the fact.
    const generic: DisplaySurface[] = [{ world: "other", surface: "x", title: "Other", contractVersion: 1, outputs: [] }];
    expect(inputProblem({ chosenKey: "other x", input: "bod_lobby" }, generic)).toMatch(/must be JSON/);
    expect(inputProblem({ chosenKey: "other x", input: "{\"project\":\"ENG\"}" }, generic)).toBeNull();
    expect(inputProblem({ chosenKey: "com.lait.signage signage.program", input: "bod_lobby" }, surfaces)).toBeNull();
    expect(inputProblem({ chosenKey: "com.lait.signage signage.program", input: " " }, surfaces)).toMatch(/screen/);
  });

  it("special-cases the signage and issues surfaces and nothing else, whichever World serves them", () => {
    expect(isSignageSurface({ world: "com.lait.signage", surface: "signage.program" })).toBe(true);
    // A local copy of Signage serves the same surface under its own id, and
    // it is the same form — keyed on the World it fell to "package JSON".
    expect(isSignageSurface({ world: "local.signage", surface: "signage.program" })).toBe(true);
    expect(isSignageSurface({ world: "com.lait.signage", surface: "signage.other" })).toBe(false);
    expect(isIssuesBoard({ world: "com.lait.issues", surface: "issues.board.wall" })).toBe(true);
    expect(isIssuesBoard({ world: "issues", surface: "board" })).toBe(false);
    // Two copies are told apart in the choice, not left as twins.
    expect(surfaceChoice({ ...surfaces[1], world: "local.signage" })).toBe("A Signage screen (local copy)");
    expect(surfaceChoice(surfaces[1])).toBe("A Signage screen");
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

  it("says what a TV shows by the World's own name for it", () => {
    const assignment = { world: "com.lait.signage", surface: "signage.program" } as DisplayAssignment;
    expect(showingLine(assignment, surfaces)).toBe("Showing Signage program");
    expect(showingLine({ world: "x", surface: "y" } as DisplayAssignment, surfaces)).toBe("Showing x · y");
    expect(showingLine(undefined, surfaces)).toBe("Nothing showing yet");
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
