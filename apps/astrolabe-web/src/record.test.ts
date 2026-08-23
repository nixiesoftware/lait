import { describe, expect, it } from "vitest";

import { loadingClientView, type ClientView, type WorldPerson } from "./client";
import { activityLine, glanceTiers, identityStatus, noticeSummary } from "./record";

const settled: ClientView = { ...loadingClientView, loading: false, stale: null, host: { version: "1.0.0", identityHome: "/h", spacesRoot: "/h/s", orbitCount: 1 } };

describe("the operational bar's rules", () => {
  it("ranks the identity statuses: failures, degradation, absence, health", () => {
    expect(identityStatus({ ...settled, loading: true }).label).toBe("Connecting to local identity");
    expect(identityStatus({ ...settled, failures: [{ what: "x", error: "y", retryable: true }] }).tone).toBe("error");
    expect(identityStatus({ ...settled, stale: { kind: "signalled", reason: "r" } }).label).toBe("Local identity degraded");
    // A degraded device degrades the identity even when nothing is stale.
    expect(identityStatus({
      ...settled,
      devices: [{ id: "d", label: "D", state: "running", owned: true, degraded: "sampling failed", home: "/h", pid: null, canForceStop: false, lastError: null, imageFingerprint: null }],
    }).label).toBe("Local identity degraded");
    expect(identityStatus({ ...settled, host: null }).label).toBe("Local identity unavailable");
    expect(identityStatus(settled)).toEqual({ label: "Local identity online", tone: "ok" });
  });

  it("chooses one sentence: in-flight, then refusal, then deduped notice", () => {
    expect(activityLine({ ...settled, inFlight: ["open:/"] })).toBe("Starting World…");
    expect(activityLine({ ...settled, inFlight: ["device.stop:d"] })).toBe("Updating device…");
    expect(activityLine({ ...settled, failures: [{ what: "Stop head", error: "refused", retryable: true }] }))
      .toBe("Stop head: refused");
    expect(activityLine({
      ...settled,
      notices: [{ said: "Same thing.", launched: null }, { said: "Same thing.", launched: null }],
    })).toBe("Same thing.");
    expect(activityLine(settled)).toBe("All local systems current");
  });

  it("never repeats a launch ticket: the summary keeps only the safe destination", () => {
    expect(noticeSummary({ said: "World is ready.", launched: "http://127.0.0.1:7717/?ticket=SECRET#f" }))
      .toBe("Opened http://127.0.0.1:7717/");
    expect(noticeSummary({ said: "World is ready.", launched: "not a url" })).toBe("Opened World in browser");
    expect(noticeSummary({ said: "Plain note.", launched: null })).toBe("Plain note.");
  });

  it("tiers the glance: here first, the rest by reach with measured absence last", () => {
    const person = (name: string, overrides: Partial<WorldPerson>): WorldPerson =>
      ({ name, picture: null, presence: null, agent: false, here: false, ...overrides });
    const { here, holding } = glanceTiers([
      person("offline", { presence: "offline" }),
      person("unmeasured", {}),
      person("present", { here: true, presence: "online" }),
      person("away", { presence: "away" }),
      person("online", { presence: "online" }),
    ]);
    expect(here.map((row) => row.name)).toEqual(["present"]);
    expect(holding.map((row) => row.name)).toEqual(["online", "away", "unmeasured", "offline"]);
  });
});
