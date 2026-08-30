// The Devices surface's rules. Two of them are the ones this ceremony exists
// for: a code is shown only by a device that is waiting to be added, and a
// device that could not be reached says so rather than disappearing from the
// list or being called off.

import { describe, expect, it } from "vitest";

import {
  keyFor, loadingClientView,
  type ClientView, type Marker, type MarkerStanding, type OwnDevice, type ProfileFacts,
} from "./client";
import {
  answeringOffer, canAddDevice, certification, codeToEnter, deviceStanding, devicesAbsence,
  expiryLabel, markerName, markerStanding, pairEnter, spacesHeld,
} from "./devices";

function device(overrides: Partial<OwnDevice> & { device: string }): OwnDevice {
  return { me: false, liveness: { kind: "notProbed" }, held: [], certifiedBy: [], ...overrides };
}

function marker(base: string, standing: MarkerStanding): Marker {
  return { marker: base, standing };
}

function profile(overrides: Partial<ProfileFacts> = {}): ProfileFacts {
  return {
    profile: "prf_1",
    me: "dev_laptop",
    origin: { kind: "founded" },
    devices: [device({ device: "dev_laptop", me: true })],
    deviceSetUnknown: false,
    pairing: null,
    offers: [],
    markers: [],
    ...overrides,
  };
}

function view(overrides: Partial<ClientView> = {}): ClientView {
  return { ...loadingClientView, loading: false, ...overrides };
}

describe("the rules for a person's own devices", () => {
  it("shows a code only while this device is waiting to be added, addresses and all", () => {
    // A device that is already one of somebody's holds no code, and drawing
    // one anyway would invite a person to add a device they already have.
    expect(codeToEnter(profile())).toBeNull();
    expect(codeToEnter(null)).toBeNull();
    expect(codeToEnter(profile({ pairing: { code: "ABCD-EFGH", direct: [], expiresAtMs: 1 } }))).toBe("ABCD-EFGH");
    // With no relay between the two devices the addresses are entered beside
    // the code — one thing to read out, not two.
    expect(codeToEnter(profile({
      pairing: { code: "ABCD-EFGH", direct: ["192.168.1.5:7717", "127.0.0.1:7717"], expiresAtMs: 1 },
    }))).toBe("ABCD-EFGH@192.168.1.5:7717,127.0.0.1:7717");
  });

  it("keeps a device that could not be reached apart from one nothing has asked about", () => {
    expect(deviceStanding(device({ device: "dev_pi", liveness: { kind: "couldNotAsk", why: "no route" } })))
      .toEqual({ label: "Could not be reached", tone: "warn" });
    // Nothing has asked yet, which is neither an answer nor a failure.
    expect(deviceStanding(device({ device: "dev_pi" }))).toEqual({ label: "Not checked yet", tone: "neutral" });
    expect(deviceStanding(device({ device: "dev_pi", liveness: { kind: "answered", version: "0.9.3", at: 5 } })))
      .toEqual({ label: "Reachable", tone: "good" });
    // The device answering is named as itself whether or not a probe reached
    // it — it is the one device nobody has to ask.
    expect(deviceStanding(device({ device: "dev_laptop", me: true }))).toEqual({ label: "This device", tone: "neutral" });
  });

  it("says which absence an empty list is, and never that a person has no devices", () => {
    expect(devicesAbsence(null)).toBe("Reading your devices…");
    expect(devicesAbsence(profile({ devices: [], deviceSetUnknown: true }))).toMatch(/has not read/);
    // A list that was read has rows, so there is nothing to explain.
    expect(devicesAbsence(profile())).toBeNull();
    // A device with no Spaces yet is an answer, not an unread list.
    expect(spacesHeld(device({ device: "dev_pi" }))).toBe("No Spaces yet");
    expect(spacesHeld(device({ device: "dev_pi", held: ["spc_a"] }))).toBe("1 Space");
    expect(spacesHeld(device({ device: "dev_pi", held: ["spc_a", "spc_b"] }))).toBe("2 Spaces");
  });

  it("dispatches the code that was typed, and refuses to dispatch nothing", () => {
    expect(pairEnter("   ")).toBeNull();
    const action = pairEnter("  ABCD-EFGH@127.0.0.1:7717 ");
    // Passed on as typed: which spellings are the same code is the daemon's
    // rule, and a second normalisation here could disagree with it.
    expect(action).toEqual({ type: "devicePairEnter", code: "ABCD-EFGH@127.0.0.1:7717" });
    expect(keyFor(action!)).toBe("device.pair.enter");
  });

  it("disables Add device on the frame it is pressed, and while there is nothing to send", () => {
    expect(canAddDevice(view(), "ABCD-EFGH")).toBe(true);
    expect(canAddDevice(view(), "  ")).toBe(false);
    expect(canAddDevice(view({ inFlight: ["device.pair.enter"] }), "ABCD-EFGH")).toBe(false);
  });

  it("answers one offer with one key, whichever way it is answered", () => {
    const offer = { pairing: "pai_7", device: "dev_pi", name: "raspberrypi", phrase: [], expiresAtMs: 0 };
    expect(keyFor({ type: "devicePairConfirm", pairing: offer.pairing, accept: true })).toBe("device.pair.confirm:pai_7");
    // Confirm and Reject are two answers to one question: one key, so
    // pressing either disables both rather than leaving the other live.
    expect(keyFor({ type: "devicePairConfirm", pairing: offer.pairing, accept: false })).toBe("device.pair.confirm:pai_7");
    expect(answeringOffer(view({ inFlight: ["device.pair.confirm:pai_7"] }), "pai_7")).toBe(true);
    expect(answeringOffer(view({ inFlight: ["device.pair.confirm:pai_7"] }), "pai_8")).toBe(false);
  });

  it("draws every marker standing as its own line, and none of them as a verdict", () => {
    const post = "https://post.example";
    const standings: MarkerStanding["kind"][] = [
      "answering", "neverAsked", "couldNotAsk", "answeredAsAnother",
      "answeredOlder", "unproven", "contradicted", "unreadable",
    ];
    const labels = standings.map((kind) => markerStanding(marker(post, { kind })).label);
    expect(new Set(labels).size).toBe(standings.length);
    // The two that are not answers are never warnings: a marker nobody has
    // asked, and one that could not be reached, say nothing about a person's
    // devices, and drawing either as a finding turns an outage into a verdict.
    expect(markerStanding(marker(post, { kind: "neverAsked" })).tone).toBe("neutral");
    expect(markerStanding(marker(post, { kind: "couldNotAsk" })).tone).toBe("neutral");
    expect(markerStanding(marker(post, { kind: "answering" })).tone).toBe("good");
    // A marker caught contradicting itself is a finding — about the marker.
    expect(markerStanding(marker(post, { kind: "contradicted" })).tone).toBe("warn");
    // The name is where it answers, with nothing invented on top of it.
    expect(markerName("https://post.foundation.pub/")).toBe("post.foundation.pub");
    expect(markerName("http://127.0.0.1:8080")).toBe("127.0.0.1:8080");
  });

  it("hangs certification on a device as a tier, keeping four answers apart", () => {
    const post = "https://post.example";
    const listed = device({ device: "dev_pi", certifiedBy: [post] });
    const unlisted = device({ device: "dev_pi" });

    // Listed is the only line that adds anything.
    expect(certification(listed, [marker(post, { kind: "answering" })]))
      .toEqual([{ marker: post, label: "Listed by post.example", tone: "good" }]);
    // A marker that answered and did not name this device is the only case in
    // which "not listed" is true — and even then it is not a warning.
    expect(certification(unlisted, [marker(post, { kind: "answering" })]))
      .toEqual([{ marker: post, label: "Not listed by post.example", tone: "neutral" }]);
    // The two absences are each their own line, and neither is "not listed":
    // a marker that could not be reached said nothing about anybody.
    expect(certification(unlisted, [marker(post, { kind: "couldNotAsk" })]))
      .toEqual([{ marker: post, label: "post.example could not be checked", tone: "neutral" }]);
    expect(certification(unlisted, [marker(post, { kind: "neverAsked" })]))
      .toEqual([{ marker: post, label: "post.example not checked yet", tone: "neutral" }]);
    // What a marker proved before an unusable answer still stands, so a
    // device it already named keeps the listing rather than losing it to the
    // marker's bad day.
    expect(certification(listed, [marker(post, { kind: "unproven" })])[0]?.label)
      .toBe("Listed by post.example");
    // And a device no marker has recorded is drawn as a device, not as a
    // problem: nothing about it is missing, hidden or disabled.
    expect(certification(unlisted, [])).toEqual([]);
    expect(devicesAbsence(profile({ devices: [unlisted] }))).toBeNull();
    expect(deviceStanding(unlisted).tone).not.toBe("warn");
  });

  it("counts a code's remaining life down, and says plainly when it is over", () => {
    const now = 1_755_000_000_000;
    expect(expiryLabel(now + 14 * 60_000, now)).toBe("in 14 min");
    expect(expiryLabel(now + 2 * 3_600_000, now)).toBe("in 2 h");
    expect(expiryLabel(now - 1_000, now)).toBe("expired");
  });
});
