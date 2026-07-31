import { describe, expect, it } from "vitest";

import type { Bridge, BridgeEvent, Question } from "./bridge";
import { WorldViewStore } from "./core/worldViewStore";
import {
  applyLive,
  applySignals,
  caretPhrase,
  carets,
  emptyLive,
  liveKey,
  LivePlane,
  liveSilenceMs,
  LiveSlots,
  type LiveState,
  liveUnavailable,
  maxHeldSignals,
  maxLiveSlots,
  noSignals,
  type SignalDrain,
  signalsKey,
  typists,
  watchers,
  watching,
} from "./live";
import type { LiveEntry, Response, SignalEntry } from "./types";

function ping(nonce: string): SignalEntry {
  return {
    actor: "act_a",
    session_id: "0".repeat(32),
    session_epoch: "1".repeat(32),
    signal: { signal: "ping", nonce },
  };
}

function presence(actor: string, ageMs: number, body = "aaaa"): LiveEntry {
  return {
    actor,
    scope: { scope: "issue_view", world: "com.lait.issues", body },
    kind: "presence",
    age_ms: ageMs,
    uncertain: false,
    caret: null,
    focus: null,
  };
}

describe("applyLive", () => {
  it("takes the whole table when the daemon sends one", () => {
    const reply: Response = {
      kind: "live",
      generation: 7,
      partial: true,
      entries: [presence("act_a", 10)],
    };
    const state = applyLive(emptyLive, reply);
    expect(state.generation).toBe(7);
    expect(state.partial).toBe(true);
    expect(state.entries).toHaveLength(1);
    expect(state.unavailable).toBe(false);
  });

  it("keeps the same entries array on unchanged, so a consumer memoising on it holds", () => {
    // The whole reason `live_unchanged` is its own reply rather than an absent
    // field: the daemon has asserted nothing moved, and a copy here would make
    // every memoised render redraw anyway.
    const first = applyLive(emptyLive, {
      kind: "live",
      generation: 3,
      partial: false,
      entries: [presence("act_a", 10)],
    });
    const second = applyLive(first, { kind: "live_unchanged", generation: 3 });
    expect(second.entries).toBe(first.entries);
    expect(second.generation).toBe(3);
  });

  it("starts with no generation, so a first read is never claimed to be held", () => {
    // Generation starts at zero in the daemon. Defaulting to zero here would
    // make a first read indistinguishable from holding an empty table, and the
    // daemon would answer "unchanged" about a view nobody has seen.
    expect(emptyLive.generation).toBeNull();
  });

  it("ignores a reply that is neither, rather than clearing what it holds", () => {
    const held = applyLive(emptyLive, {
      kind: "live",
      generation: 1,
      partial: false,
      entries: [presence("act_a", 5)],
    });
    expect(applyLive(held, { kind: "ok", message: null })).toBe(held);
  });
});

describe("liveUnavailable", () => {
  it("clears the table rather than freezing it", () => {
    // A facepile stuck on whoever happened to be there when the daemon went
    // away is worse than an empty one: it is wrong and it looks current.
    const state = liveUnavailable();
    expect(state.entries).toHaveLength(0);
    expect(state.unavailable).toBe(true);
    expect(state.generation).toBeNull();
  });
});

describe("watchers", () => {
  it("is one row per person, not one per device", () => {
    // The daemon's table is keyed by station. A laptop and a phone open on the
    // same issue are two entries and one human, and both resolve to one actor.
    const rows = watchers([presence("act_a", 900), presence("act_a", 20), presence("act_b", 50)]);
    expect(rows).toEqual(["act_a", "act_b"]);
  });

  it("orders freshest first", () => {
    expect(watchers([presence("act_slow", 5_000), presence("act_fast", 5)])).toEqual([
      "act_fast",
      "act_slow",
    ]);
  });

  it("ignores kinds that are not presence", () => {
    const caret: LiveEntry = {
      actor: "act_c",
      scope: { scope: "text_caret", world: "com.lait.issues", body: "aaaa", field: "description" },
      kind: "caret",
      age_ms: 1,
      uncertain: false,
      caret: { caret: "at", position: 12 },
      focus: null,
    };
    expect(watchers([caret])).toEqual([]);
  });
});

describe("watching", () => {
  it("shows an uncertain person, marked, rather than dropping them", () => {
    // Hiding the uncertain row is how a quiet collaborator disappears: they are
    // still there, the daemon has simply not heard from them inside the grace
    // window, and a facepile that omits them says the room is emptier than it is.
    const stale = { ...presence("act_a", 9_000), uncertain: true };
    expect(watching([stale])).toEqual([{ actor: "act_a", uncertain: true }]);
  });

  it("lets the freshest device decide, so one quiet tab does not ghost a person", () => {
    const stale = { ...presence("act_a", 9_000), uncertain: true };
    expect(watching([stale, presence("act_a", 30)])).toEqual([{ actor: "act_a", uncertain: false }]);
  });
});

describe("carets", () => {
  const at = (actor: string, position: number, ageMs = 10): LiveEntry => ({
    actor,
    scope: { scope: "text_caret", world: "com.lait.issues", body: "aaaa", field: "description" },
    kind: "caret",
    age_ms: ageMs,
    uncertain: false,
    caret: { caret: "at", position },
    focus: null,
  });

  it("keeps a drifted caret out of the numbers", () => {
    // The whole reason `CaretPosition` is a union rather than a nullable number.
    // Drifted means the material the offset was attached to is gone; rendering
    // the last number anyone saw would point confidently at the wrong character.
    const drifted: LiveEntry = { ...at("act_a", 4), caret: { caret: "drifted" } };
    const rows = carets([drifted]);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.position).toEqual({ caret: "drifted" });
    expect(caretPhrase({ caret: "drifted" })).not.toMatch(/\d/);
  });

  it("says something different about drifted and about unresolved", () => {
    // Two different facts: one says the position was lost, the other says it was
    // never worked out. One phrase for both would report a live caret as lost.
    expect(caretPhrase({ caret: "drifted" })).not.toBe(caretPhrase({ caret: "unresolved" }));
    expect(caretPhrase({ caret: "unresolved" })).not.toMatch(/\d/);
    expect(caretPhrase({ caret: "at", position: 12 })).toContain("12");
  });

  it("is one row per person per field, freshest first", () => {
    const other: LiveEntry = {
      ...at("act_a", 99, 3),
      scope: { scope: "typing", world: "com.lait.issues", body: "aaaa", field: "title" },
    };
    const rows = carets([at("act_a", 4, 500), at("act_a", 7, 40), other]);
    expect(rows.map((row) => [row.field, row.position])).toEqual([
      ["title", { caret: "at", position: 99 }],
      ["description", { caret: "at", position: 7 }],
    ]);
  });

  it("ignores an entry whose scope names no field", () => {
    // A residency or an issue-level presence is about a document, not a place
    // inside one, and there is nowhere honest to draw it.
    const scoped: LiveEntry = { ...presence("act_a", 5), caret: { caret: "at", position: 1 } };
    expect(carets([scoped])).toEqual([]);
  });
});

describe("LiveSlots", () => {
  const table = (generation: number, actors: string[]): Response => ({
    kind: "live",
    generation,
    partial: false,
    entries: actors.map((actor) => presence(actor, 10)),
  });

  function held(store: WorldViewStore, space: string, issue: string | null): LiveState | undefined {
    return store.read<LiveState>(liveKey(space, issue)).data;
  }

  /** Ask, then take the answer — the order the socket produces them in. */
  function answer(slots: LiveSlots, issue: string, reply: Response): void {
    slots.ask({ space: "orb_a", issue });
    slots.admit("orb_a", issue, reply);
  }

  it("admits an answer under the question it answers", () => {
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    answer(slots, "iss_1", table(1, ["act_a"]));
    expect(held(store, "orb_a", "iss_1")?.entries).toHaveLength(1);
    // The whole-table question is a different slot, not the same one narrowed.
    expect(held(store, "orb_a", null)).toBeUndefined();
  });

  it("drops a frame answering somebody else's question", () => {
    // The transient lane is one broadcast for the whole server, so this tab
    // sees the answer to every other tab's question. Taking them would put a
    // slot in the store for every issue anybody anywhere opens.
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    slots.ask({ space: "orb_a", issue: "iss_1" });
    slots.admit("orb_a", "iss_2", table(1, ["act_a"]));
    expect(held(store, "orb_a", "iss_2")).toBeUndefined();
  });

  it("supersedes rather than accumulating", () => {
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    answer(slots, "iss_1", table(1, ["act_a", "act_b"]));
    answer(slots, "iss_1", table(2, ["act_c"]));
    expect(held(store, "orb_a", "iss_1")?.entries.map((e) => e.actor)).toEqual(["act_c"]);
    expect(held(store, "orb_a", "iss_1")?.generation).toBe(2);
  });

  it("writes nothing for a reply that is neither a table nor an unchanged one", () => {
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    answer(slots, "iss_1", { kind: "ok", message: null });
    expect(held(store, "orb_a", "iss_1")).toBeUndefined();
  });

  it("blanks a table nobody is asking about any more", () => {
    let now = 0;
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => now);
    answer(slots, "iss_1", table(1, ["act_a"]));
    slots.ask(null);
    now = liveSilenceMs + 1;
    slots.sweep();
    expect(held(store, "orb_a", "iss_1")?.entries).toEqual([]);
  });

  it("leaves the asked table alone, because silence there means nothing moved", () => {
    // The engine sends a frame only when the generation moves. Expiring the
    // watched slot on a timer would blank a stable facepile every sweep and
    // spend the generation on a flicker.
    let now = 0;
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => now);
    answer(slots, "iss_1", table(1, ["act_a"]));
    now = liveSilenceMs * 10;
    slots.sweep();
    expect(held(store, "orb_a", "iss_1")?.entries).toHaveLength(1);
  });

  it("caps how many tables survive a question nobody holds", () => {
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    for (let n = 0; n < maxLiveSlots + 4; n += 1) {
      answer(slots, `iss_${n}`, table(1, ["act_a"]));
    }
    slots.ask(null);
    slots.sweep();
    const surviving = Array.from({ length: maxLiveSlots + 4 }, (_, n) =>
      held(store, "orb_a", `iss_${n}`),
    ).filter((slot) => slot !== undefined);
    expect(surviving).toHaveLength(maxLiveSlots);
    // Oldest read first: the four that went is the four nobody has touched.
    expect(held(store, "orb_a", "iss_0")).toBeUndefined();
  });

  it("keeps the asked table through an eviction", () => {
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    answer(slots, "iss_0", table(1, ["act_a"]));
    for (let n = 1; n < maxLiveSlots + 4; n += 1) {
      answer(slots, `iss_${n}`, table(1, ["act_a"]));
    }
    // Back on the oldest of them, and answered nothing yet: the slot a surface
    // is drawing from is the one eviction may not take.
    slots.ask({ space: "orb_a", issue: "iss_0" });
    slots.sweep();
    expect(held(store, "orb_a", "iss_0")?.entries).toHaveLength(1);
  });

  it("marks every table unavailable when the socket stops answering", () => {
    // Not the same as an empty room, and the two must not render the same: one
    // says nobody is here, the other says this node has no idea who is.
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    answer(slots, "iss_1", table(1, ["act_a"]));
    slots.silence();
    expect(held(store, "orb_a", "iss_1")?.unavailable).toBe(true);
    expect(held(store, "orb_a", "iss_1")?.entries).toEqual([]);
  });

  it("does not let a later sweep turn unavailable back into merely empty", () => {
    let now = 0;
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => now);
    answer(slots, "iss_1", table(1, ["act_a"]));
    slots.silence();
    now = liveSilenceMs * 10;
    slots.sweep();
    expect(held(store, "orb_a", "iss_1")?.unavailable).toBe(true);
  });

  it("holds a drain without being asked, because the daemon's copy is gone", () => {
    // A live view is refused unless this tab asked for it; a signal is not.
    // The server drained it out of the daemon's queue on this tab's behalf, so
    // there is nowhere for it to arrive later and nobody else to take it.
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    slots.deliver("orb_a", { kind: "signals", signals: [ping("aa")], dropped: 0 });
    expect(store.read<SignalDrain>(signalsKey("orb_a")).data?.signals).toHaveLength(1);
  });

  it("never lets a sweep touch a delivered signal", () => {
    // The bound that applies to a facepile must not apply here: a view is
    // superseded by the next view, and an invitation evicted to make room for
    // one is the loss the Control lane exists to rule out.
    let now = 0;
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => now);
    slots.deliver("orb_a", { kind: "signals", signals: [ping("aa")], dropped: 0 });
    for (let n = 0; n < maxLiveSlots + 4; n += 1) {
      answer(slots, `iss_${n}`, table(1, ["act_a"]));
    }
    slots.ask(null);
    now = liveSilenceMs * 10;
    slots.sweep();
    slots.silence();
    expect(store.read<SignalDrain>(signalsKey("orb_a")).data?.signals).toHaveLength(1);
  });

  it("forgets a drain only when somebody says it has been dealt with", () => {
    const store = new WorldViewStore();
    const slots = new LiveSlots(store, () => 0);
    slots.deliver("orb_a", { kind: "signals", signals: [ping("aa")], dropped: 2 });
    slots.forget("orb_a");
    expect(store.read<SignalDrain>(signalsKey("orb_a")).data).toEqual(noSignals);
  });
});

describe("applySignals", () => {
  it("appends rather than replacing, because nothing supersedes a signal", () => {
    // The difference from the live table, and the whole reason the two ride
    // different lanes: a facepile is replaced by the next facepile, while an
    // invitation that was overwritten before anybody acted on it is gone.
    const first = applySignals(noSignals, {
      kind: "signals",
      signals: [ping("aa")],
      dropped: 0,
    });
    const second = applySignals(first, {
      kind: "signals",
      signals: [ping("bb")],
      dropped: 0,
    });
    expect(second.signals.map((s) => (s.signal.signal === "ping" ? s.signal.nonce : ""))).toEqual([
      "aa",
      "bb",
    ]);
  });

  it("accumulates the losses, which do not stop being true", () => {
    const held = applySignals({ signals: [], dropped: 2 }, {
      kind: "signals",
      signals: [],
      dropped: 3,
    });
    expect(held.dropped).toBe(5);
  });

  it("ignores a reply that is not a drain", () => {
    const held = applySignals(noSignals, { kind: "ok", message: null });
    expect(held).toBe(noSignals);
  });

  it("bounds what it holds, and counts what the bound cost", () => {
    // A list that only grew would be a leak on the one lane that may not drop.
    // The oldest go, which is the daemon's own rule for the same queue, and
    // they are counted — a loss nobody can name is worse than one they can.
    const flood = Array.from({ length: maxHeldSignals + 5 }, (_, n) => ping(`${n}`));
    const held = applySignals(noSignals, { kind: "signals", signals: flood, dropped: 0 });
    expect(held.signals).toHaveLength(maxHeldSignals);
    expect(held.dropped).toBe(5);
    const oldest = held.signals[0];
    expect(oldest?.signal.signal === "ping" ? oldest.signal.nonce : null).toBe("5");
  });
});

describe("LivePlane", () => {
  interface Wired {
    plane: LivePlane;
    store: WorldViewStore;
    push: (event: BridgeEvent) => void;
    declared: Question[];
  }

  /** A socket that records what it was told and hands back a way to push. */
  function socket(): Wired {
    const store = new WorldViewStore();
    const declared: Question[] = [];
    let push: (event: BridgeEvent) => void = () => undefined;
    const open = (onEvent: (event: BridgeEvent) => void): Bridge => {
      push = onEvent;
      return {
        watch: (question: Question) => declared.push(question),
        close: () => undefined,
      };
    };
    const plane = new LivePlane(store, open, () => 0);
    return { plane, store, push: (event) => push(event), declared };
  }

  it("holds what the control lane delivers", () => {
    // The lane's whole guarantee ends here. The engine drained these out of the
    // daemon's queue on this tab's behalf and kept no copy, so a plane that let
    // the frame fall through would destroy them for every reader there is.
    const { plane, store, push } = socket();
    plane.ask({ space: "orb_a", issue: "iss_1" });
    push({
      kind: "signals",
      space: "orb_a",
      drained: { kind: "signals", signals: [ping("aa")], dropped: 1 },
    });
    const held = store.read<SignalDrain>(signalsKey("orb_a")).data;
    expect(held?.signals).toHaveLength(1);
    expect(held?.dropped).toBe(1);
  });

  it("stays in the room when the question goes", () => {
    // The declaration is what the engine drains signals for. A detail pane
    // closing is not a person leaving the space, and dropping the declaration
    // with it would take the Control lane down with a facepile.
    const { plane, declared } = socket();
    plane.attach("orb_a");
    plane.ask({ space: "orb_a", issue: "iss_1" });
    plane.ask(null);
    expect(declared).toEqual([
      { space: "orb_a" },
      { space: "orb_a", issue: "iss_1" },
      { space: "orb_a" },
    ]);
  });

  it("blanks its tables when the socket stops answering, and keeps the signals", () => {
    const { plane, store, push } = socket();
    plane.ask({ space: "orb_a", issue: "iss_1" });
    push({
      kind: "signals",
      space: "orb_a",
      drained: { kind: "signals", signals: [ping("aa")], dropped: 0 },
    });
    push({
      kind: "live",
      space: "orb_a",
      issue: "iss_1",
      view: { kind: "live", generation: 1, partial: false, entries: [presence("act_a", 10)] },
    });
    push({ kind: "liveness", liveness: "retrying" });
    expect(store.read<LiveState>(liveKey("orb_a", "iss_1")).data?.unavailable).toBe(true);
    expect(store.read<SignalDrain>(signalsKey("orb_a")).data?.signals).toHaveLength(1);
  });
});

describe("typists", () => {
  it("collapses to actors", () => {
    const typing = (actor: string): LiveEntry => ({
      actor,
      scope: { scope: "typing", world: "com.lait.issues", body: "aaaa", field: "description" },
      kind: "typing",
      age_ms: 30,
      uncertain: false,
      caret: null,
      focus: null,
    });
    expect(typists([typing("act_b"), typing("act_a"), typing("act_a")])).toEqual([
      "act_a",
      "act_b",
    ]);
  });
});
