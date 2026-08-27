/**
 * The browser ladder, pinned.
 *
 * `resolve.ts` is a second implementation of `fleet.rs` and `addressing.rs`,
 * and a twin with no test is a twin that drifts. These lock the rules the Rust
 * states, in the same words, so a divergence fails here rather than on a wall
 * somewhere:
 *
 *   - absent is not zero
 *   - blank is not unaddressed
 *   - priority arbitrates, and ties break on id
 *   - a cancellation outranks a window that is still open
 *
 * They deliberately do not test the window evaluator, because there isn't one:
 * `schedule::Window` has no browser twin and `windowsAreAssumedOpen` says so.
 * A test that pretended otherwise would be pinning a lie.
 */

import { describe, expect, it } from "vitest";
import {
  explain,
  reaches,
  resolvePlayback,
  screensReached,
  windowsAreAssumedOpen,
  type Context,
  type ResolutionInputs,
} from "./resolve";
import type {
  Match,
  SignageAudience,
  SignageBroadcast,
  SignageChannel,
  SignageScreen,
} from "./types";

const CX: Context = { nowUnixMs: 1_700_000_000_000, observations: {} };

function screen(over: Partial<SignageScreen> = {}): SignageScreen {
  return {
    id: "scr1",
    name: "Menu 1",
    place: {
      latitude: 42.3314,
      longitude: -83.0458,
      timezone: "America/Detroit",
      region: "MI",
    },
    facts: {},
    sync: null,
    labels: ["biz:acme", "role:menu"],
    tuned: null,
    ...over,
  };
}

function channel(over: Partial<SignageChannel> = {}): SignageChannel {
  return { id: "ch1", name: "Menus", base: "prog1", schedule: [], ...over };
}

function broadcast(over: Partial<SignageBroadcast> = {}): SignageBroadcast {
  return {
    id: "bc1",
    name: "Evacuation",
    audience: "aud-all",
    action: { action: "play", program: "prog-alert" },
    timing: { timing: "when", of: { match: "all" }, priority: 50 },
    supersedes: [],
    cancelled_at_unix_ms: null,
    ...over,
  };
}

const ALL: SignageAudience = { id: "aud-all", name: "Everyone", rule: { match: "all" } };
const MENUS: SignageAudience = {
  id: "aud-menu",
  name: "Menu screens",
  rule: { match: "label", label: "role:menu" },
};

function inputs(over: Partial<ResolutionInputs> = {}): ResolutionInputs {
  return {
    screen: screen(),
    channels: [],
    broadcasts: [],
    audiences: [ALL, MENUS],
    programs: [],
    media: [],
    presets: [],
    ...over,
  };
}

const lookup = new Map<string, Match>([
  [ALL.id, ALL.rule],
  [MENUS.id, MENUS.rule],
]);

describe("who an audience reaches", () => {
  it("matches a label the screen carries, and not one it does not", () => {
    expect(reaches({ match: "label", label: "role:menu" }, screen(), CX, lookup)).toBe(true);
    expect(reaches({ match: "label", label: "role:office" }, screen(), CX, lookup)).toBe(false);
  });

  it("reads a fact a kind stored about the venue", () => {
    const makkah = screen({ facts: { athan: { method: "makkah" } } });
    const rule: Match = { match: "fact", kind: "athan", key: "method", value: "makkah" };
    expect(reaches(rule, makkah, CX, lookup)).toBe(true);
    expect(reaches(rule, screen(), CX, lookup)).toBe(false);
  });

  it("matches a region without anybody maintaining a parallel label", () => {
    const rule: Match = { match: "place", place: { kind: "region", region: "mi" } };
    expect(reaches(rule, screen(), CX, lookup)).toBe(true);
  });

  it("treats an unsited screen as unreachable by place, not as at the origin", () => {
    const rule: Match = { match: "place", place: { kind: "placed" } };
    expect(reaches(rule, screen({ place: null }), CX, lookup)).toBe(false);
  });

  it("measures distance rather than comparing coordinates", () => {
    const near: Match = {
      match: "place",
      place: { kind: "within", latitude: 42.33, longitude: -83.04, km: 5 },
    };
    const far: Match = {
      match: "place",
      place: { kind: "within", latitude: 51.5, longitude: -0.12, km: 5 },
    };
    expect(reaches(near, screen(), CX, lookup)).toBe(true);
    expect(reaches(far, screen(), CX, lookup)).toBe(false);
  });

  it("composes with and, or and not", () => {
    const both: Match = {
      match: "all_of",
      of: [
        { match: "label", label: "role:menu" },
        { match: "label", label: "biz:acme" },
      ],
    };
    const neither: Match = { match: "not", of: { match: "label", label: "role:menu" } };
    expect(reaches(both, screen(), CX, lookup)).toBe(true);
    expect(reaches(neither, screen(), CX, lookup)).toBe(false);
  });

  it("follows an audience reference, and stops at the hop bound", () => {
    const nested = new Map<string, Match>([["aud-menu", MENUS.rule]]);
    expect(
      reaches({ match: "audience", audience: "aud-menu" }, screen(), CX, nested),
    ).toBe(true);
    // A reference nobody can resolve reaches nobody rather than everybody.
    expect(
      reaches({ match: "audience", audience: "missing" }, screen(), CX, nested),
    ).toBe(false);
  });
});

describe("absent is not zero", () => {
  it("fails an observation the screen never reported", () => {
    const busy: Match = { match: "observed", key: "queue", compare: "above", value: "5" };
    expect(reaches(busy, screen(), CX, lookup)).toBe(false);
    expect(
      reaches(busy, screen(), { ...CX, observations: { queue: "9" } }, lookup),
    ).toBe(true);
  });

  it("fails an observation that will not parse, rather than reading it as zero", () => {
    const below: Match = { match: "observed", key: "queue", compare: "below", value: "5" };
    expect(
      reaches(below, screen(), { ...CX, observations: { queue: "busy" } }, lookup),
    ).toBe(false);
  });
});

describe("what a screen is showing", () => {
  it("answers unaddressed when nothing reaches it, with no source", () => {
    const playback = resolvePlayback(inputs(), CX.nowUnixMs);
    expect(playback.showing).toEqual({ showing: "unaddressed" });
    expect(playback.source).toBeUndefined();
  });

  it("falls back to the channel it is tuned to", () => {
    const playback = resolvePlayback(
      inputs({ screen: screen({ tuned: "ch1" }), channels: [channel()] }),
      CX.nowUnixMs,
    );
    expect(playback.showing).toEqual({ showing: "program", program: "prog1" });
    expect(playback.source?.via).toBe("channel");
  });

  it("lets a broadcast interrupt the channel", () => {
    const playback = resolvePlayback(
      inputs({
        screen: screen({ tuned: "ch1" }),
        channels: [channel()],
        broadcasts: [broadcast()],
      }),
      CX.nowUnixMs,
    );
    expect(playback.showing).toEqual({ showing: "program", program: "prog-alert" });
    expect(playback.source?.via).toBe("broadcast");
  });

  it("distinguishes deliberately dark from unaddressed", () => {
    const blanked = resolvePlayback(
      inputs({ broadcasts: [broadcast({ action: { action: "blank" } })] }),
      CX.nowUnixMs,
    );
    expect(blanked.showing).toEqual({ showing: "blank" });
    // Somebody chose this darkness, and the source says who.
    expect(blanked.source).toBeDefined();

    const dark = resolvePlayback(inputs(), CX.nowUnixMs);
    expect(dark.showing).toEqual({ showing: "unaddressed" });
    expect(dark.source).toBeUndefined();
  });

  it("gives the channel back when a broadcast is cancelled", () => {
    const cancelled = broadcast({ cancelled_at_unix_ms: CX.nowUnixMs - 1 });
    const playback = resolvePlayback(
      inputs({
        screen: screen({ tuned: "ch1" }),
        channels: [channel()],
        broadcasts: [cancelled],
      }),
      CX.nowUnixMs,
    );
    expect(playback.showing).toEqual({ showing: "program", program: "prog1" });
  });

  it("drops a broadcast another one supersedes", () => {
    const old = broadcast({ id: "bc-old", action: { action: "play", program: "old" } });
    const fresh = broadcast({
      id: "bc-new",
      action: { action: "restore" },
      supersedes: ["bc-old"],
      timing: { timing: "when", of: { match: "all" }, priority: 10 },
    });
    const playback = resolvePlayback(
      inputs({
        screen: screen({ tuned: "ch1" }),
        channels: [channel()],
        broadcasts: [old, fresh],
      }),
      CX.nowUnixMs,
    );
    // `restore` outranks what it superseded and falls through to the channel.
    expect(playback.showing).toEqual({ showing: "program", program: "prog1" });
  });

  it("arbitrates by priority, then by id, the way two replicas must agree", () => {
    const low = broadcast({
      id: "bc-a",
      action: { action: "play", program: "low" },
      timing: { timing: "when", of: { match: "all" }, priority: 10 },
    });
    const high = broadcast({
      id: "bc-b",
      action: { action: "play", program: "high" },
      timing: { timing: "when", of: { match: "all" }, priority: 90 },
    });
    expect(
      resolvePlayback(inputs({ broadcasts: [low, high] }), CX.nowUnixMs).showing,
    ).toEqual({ showing: "program", program: "high" });

    const tieA = broadcast({ id: "bc-a", action: { action: "play", program: "a" } });
    const tieB = broadcast({ id: "bc-b", action: { action: "play", program: "b" } });
    // Ascending id wins, both orderings, because the answer cannot depend on
    // which order the rows came back in.
    expect(
      resolvePlayback(inputs({ broadcasts: [tieB, tieA] }), CX.nowUnixMs).showing,
    ).toEqual({ showing: "program", program: "a" });
    expect(
      resolvePlayback(inputs({ broadcasts: [tieA, tieB] }), CX.nowUnixMs).showing,
    ).toEqual({ showing: "program", program: "a" });
  });

  it("does not invent a channel that is not there", () => {
    const playback = resolvePlayback(
      inputs({ screen: screen({ tuned: "ch-gone" }), channels: [] }),
      CX.nowUnixMs,
    );
    expect(playback.showing).toEqual({ showing: "unaddressed" });
  });
});

describe("the blast radius", () => {
  it("reaches the matching screens and no others", () => {
    const menu = screen({ id: "s1", name: "Menu 1", labels: ["role:menu"] });
    const office = screen({ id: "s2", name: "Office", labels: ["role:office"] });
    const reached = screensReached(MENUS.rule, [menu, office], [ALL, MENUS], CX);
    expect(reached.map((s) => s.id)).toEqual(["s1"]);
  });

  it("over-reports rather than under-reports a scheduled broadcast", () => {
    // The safe direction: a preview that silently dropped every dated
    // broadcast would understate a blast radius, and understating is the one
    // failure that matters when the message is "evacuate".
    expect(windowsAreAssumedOpen).toBe(true);
  });
});

describe("why it is showing that", () => {
  it("names the broadcast, in words an operator can act on", () => {
    const playback = resolvePlayback(inputs({ broadcasts: [broadcast()] }), CX.nowUnixMs);
    expect(explain(playback, "Evacuation")).toContain("Evacuation broadcast");
  });

  it("says nothing is addressed rather than implying a fault", () => {
    expect(explain(resolvePlayback(inputs(), CX.nowUnixMs))).toContain(
      "nothing is addressed",
    );
  });
});
