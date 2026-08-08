import { describe, expect, it } from "vitest";

import { buildSequence, reachFrom } from "./sequence";
import type { GraphEdgeDto, Row, WorkflowState } from "../types";

const STATES: WorkflowState[] = [
  { id: "backlog", name: "Backlog", category: "backlog", color: "gray" },
  { id: "doing", name: "In Progress", category: "active", color: "blue" },
  { id: "done", name: "Done", category: "done", color: "green" },
];

function row(doc: string, over: Partial<Row> = {}): Row {
  return {
    reff: `iss_${doc}`,
    doc_id: doc,
    project_id: "prj_1",
    key_alias: `ENG-${doc}`,
    title: `Issue ${doc}`,
    status: "backlog",
    priority: "none",
    assignee_summary: "",
    assignees: [],
    tombstone: false,
    provisional: false,
    ...over,
  };
}

/** `a blocks b` — the direction the engine stores and the view reads. */
const blocks = (from: string, to: string): GraphEdgeDto => ({ from, kind: "blocks", to });

const waveOf = (model: ReturnType<typeof buildSequence>, doc: string) =>
  model.byDoc.get(doc)?.wave;

describe("dependency waves", () => {
  it("places a chain one wave per hop", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c")],
      [blocks("a", "b"), blocks("b", "c")],
      STATES,
    );
    expect([waveOf(model, "a"), waveOf(model, "b"), waveOf(model, "c")]).toEqual([0, 1, 2]);
    expect(model.waves.map((w) => w.length)).toEqual([1, 1, 1]);
  });

  /**
   * The reason depth is a longest path and not a shortest one.
   *
   * `d` is blocked by `a` directly *and* by `c` three hops away. Shortest-path
   * would put it in wave 1, next to its nearest blocker, and then draw the
   * constraint from `c` as a line running backwards out of wave 2. Depth has to
   * clear every blocker, so it is one past the deepest.
   */
  it("puts a node past its deepest blocker, not its nearest", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c"), row("d")],
      [blocks("a", "b"), blocks("b", "c"), blocks("c", "d"), blocks("a", "d")],
      STATES,
    );
    expect(waveOf(model, "d")).toBe(3);
  });

  it("merges a diamond at the deeper arm", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c"), row("d")],
      [blocks("a", "b"), blocks("a", "c"), blocks("b", "d"), blocks("c", "d")],
      STATES,
    );
    expect(waveOf(model, "d")).toBe(2);
    expect(model.waves[1]).toHaveLength(2);
  });

  it("leaves everything at wave 0 when nothing blocks anything", () => {
    const model = buildSequence([row("a"), row("b")], [], STATES);
    expect(model.waves).toHaveLength(1);
    expect(model.waves[0]).toHaveLength(2);
    expect(model.criticalCount).toBe(0);
  });

  it("has no waves at all for no rows", () => {
    const model = buildSequence([], [], STATES);
    expect(model.waves).toEqual([]);
    expect(model.criticalCount).toBe(0);
  });
});

describe("edges the layout must not trust", () => {
  it("ignores relates and duplicates — neither states an order", () => {
    const model = buildSequence(
      [row("a"), row("b")],
      [
        { from: "a", kind: "relates", to: "b" },
        { from: "a", kind: "duplicates", to: "b" },
      ],
      STATES,
    );
    expect(waveOf(model, "b")).toBe(0);
    expect(model.edges).toEqual([]);
  });

  it("drops an edge whose other end is not on screen", () => {
    // `z` is filtered out of `rows`. Drawing a connector to it would be a line
    // to nowhere; synthesising a placeholder would put back the row the filter
    // removed.
    const model = buildSequence([row("a")], [blocks("z", "a")], STATES);
    expect(waveOf(model, "a")).toBe(0);
    expect(model.edges).toEqual([]);
  });

  it("ignores an issue that blocks itself", () => {
    const model = buildSequence([row("a")], [blocks("a", "a")], STATES);
    expect(waveOf(model, "a")).toBe(0);
    expect(model.cyclic).toEqual([]);
  });
});

describe("cycles", () => {
  /**
   * `blocks` edges have no CRDT preventing a loop — the sub-issue tree has one,
   * and this is not that tree. A loop has no depth, so the layout must set it
   * aside rather than spin looking for one.
   */
  it("parks a loop instead of hanging", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c")],
      [blocks("a", "b"), blocks("b", "a"), blocks("c", "c")],
      STATES,
    );
    expect(model.cyclic.map((n) => n.row.doc_id).sort()).toEqual(["a", "b"]);
    expect(model.waves.flat().map((n) => n.row.doc_id)).toEqual(["c"]);
  });

  it("keeps the clean part of a graph that contains a loop", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("x"), row("y")],
      [blocks("a", "b"), blocks("b", "a"), blocks("x", "y")],
      STATES,
    );
    expect(waveOf(model, "y")).toBe(1);
    expect(model.cyclic).toHaveLength(2);
  });

  /**
   * The distinction the first cut did not draw. Kahn's leaves `tail` undrained
   * for the same reason it leaves `a` and `b` undrained — it waits on something
   * that never clears — but `tail` is an ordinary issue that is not part of any
   * loop, and telling its owner it "blocks other issues" is simply untrue.
   */
  it("separates an issue stalled behind a loop from the loop itself", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("tail")],
      [blocks("a", "b"), blocks("b", "a"), blocks("b", "tail")],
      STATES,
    );
    expect(model.cyclic.map((n) => n.row.doc_id).sort()).toEqual(["a", "b"]);
    expect(model.stalled.map((n) => n.row.doc_id)).toEqual(["tail"]);
    expect(model.byDoc.get("tail")!.cyclic).toBe(false);
    expect(model.byDoc.get("tail")!.stalled).toBe(true);
  });

  /**
   * A node with in-degree and out-degree inside the undrained set is still not
   * necessarily on a cycle. `x` sits on a path *between* two loops, so a
   * degree-pruning shortcut would call it cyclic; strong connectivity does not.
   */
  it("does not call a node between two loops a member of either", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("x"), row("c"), row("d")],
      [
        blocks("a", "b"),
        blocks("b", "a"),
        blocks("b", "x"),
        blocks("x", "c"),
        blocks("c", "d"),
        blocks("d", "c"),
      ],
      STATES,
    );
    expect(model.cyclic.map((n) => n.row.doc_id).sort()).toEqual(["a", "b", "c", "d"]);
    expect(model.stalled.map((n) => n.row.doc_id)).toEqual(["x"]);
    expect(model.loops).toHaveLength(2);
  });

  it("reports each loop as a walk, so the view can name the edge to cut", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c")],
      [blocks("a", "b"), blocks("b", "c"), blocks("c", "a")],
      STATES,
    );
    expect(model.loops).toEqual([["a", "b", "c"]]);
  });

  it("keeps a stalled issue out of the waves and off the critical path", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("tail"), row("x"), row("y")],
      [blocks("a", "b"), blocks("b", "a"), blocks("b", "tail"), blocks("x", "y")],
      STATES,
    );
    expect(model.waves.flat().map((n) => n.row.doc_id).sort()).toEqual(["x", "y"]);
    expect(model.criticalCount).toBe(2);
  });
});

describe("slack", () => {
  const slackOf = (model: ReturnType<typeof buildSequence>, doc: string) =>
    model.byDoc.get(doc)?.slack;

  it("is zero along a longest chain and positive beside it", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c"), row("solo")],
      [blocks("a", "b"), blocks("b", "c")],
      STATES,
    );
    expect([slackOf(model, "a"), slackOf(model, "b"), slackOf(model, "c")]).toEqual([0, 0, 0]);
    // `solo` could sit in any of the three rounds without moving the end.
    expect(slackOf(model, "solo")).toBe(2);
    expect(model.byDoc.get("solo")!.critical).toBe(false);
    expect(model.criticalCount).toBe(3);
  });

  /**
   * The reason this replaced a single "critical path".
   *
   * Two chains of two constrain the finish exactly equally. The old code broke
   * the tie on total estimate and then on **doc id**, so one of them was drawn
   * as *the* critical path and the other in plain grey — a claim about the work
   * that came down to which id sorted first. Slack has no tie to break.
   */
  it("marks every equally-constraining chain, not an arbitrary winner", () => {
    const model = buildSequence(
      [
        row("l1", { estimate: 1 }),
        row("l2", { estimate: 1 }),
        row("h1", { estimate: 8 }),
        row("h2", { estimate: 8 }),
      ],
      [blocks("l1", "l2"), blocks("h1", "h2")],
      STATES,
    );
    expect(["l1", "l2", "h1", "h2"].map((d) => slackOf(model, d))).toEqual([0, 0, 0, 0]);
    expect(model.criticalCount).toBe(4);
  });

  it("gives a shorter parallel arm the slack it actually has", () => {
    // `short` runs one round between `a` and `end`; the other arm takes two. So
    // `short` could slip a round and change nothing.
    const model = buildSequence(
      [row("a"), row("m1"), row("m2"), row("short"), row("end")],
      [
        blocks("a", "m1"),
        blocks("m1", "m2"),
        blocks("m2", "end"),
        blocks("a", "short"),
        blocks("short", "end"),
      ],
      STATES,
    );
    expect(slackOf(model, "m1")).toBe(0);
    expect(slackOf(model, "short")).toBe(1);
  });

  it("claims nothing when no issue blocks another", () => {
    // With one column every issue would measure zero slack, which is consistent
    // and useless: nothing is holding anything up.
    const model = buildSequence([row("a"), row("b")], [], STATES);
    expect(model.criticalCount).toBe(0);
    expect(model.byDoc.get("a")!.critical).toBe(false);
  });

  it("marks only a step along a chain as a critical edge", () => {
    // `a` blocks `c` directly as well as through `b`. That chord is a real
    // constraint between two zero-slack issues, but it is not a link in any
    // longest chain, and drawing it with the same emphasis would say it was.
    const model = buildSequence(
      [row("a"), row("b"), row("c")],
      [blocks("a", "b"), blocks("b", "c"), blocks("a", "c")],
      STATES,
    );
    expect(model.criticalCount).toBe(3);
    const chord = model.edges.find((e) => e.from === "a" && e.to === "c");
    expect(chord?.critical).toBe(false);
    expect(model.edges.find((e) => e.from === "a" && e.to === "b")?.critical).toBe(true);
  });

  it("gives a loop and everything behind it no slack claim at all", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("tail"), row("x"), row("y")],
      [blocks("a", "b"), blocks("b", "a"), blocks("b", "tail"), blocks("x", "y")],
      STATES,
    );
    expect(model.byDoc.get("a")!.critical).toBe(false);
    expect(model.byDoc.get("tail")!.critical).toBe(false);
    expect(model.criticalCount).toBe(2);
  });
});

describe("readiness", () => {
  it("is true only once every blocker is done", () => {
    const model = buildSequence(
      [row("a", { status: "done" }), row("b", { status: "backlog" }), row("c")],
      [blocks("a", "c"), blocks("b", "c")],
      STATES,
    );
    expect(model.byDoc.get("c")!.ready).toBe(false);

    const cleared = buildSequence(
      [row("a", { status: "done" }), row("b", { status: "done" }), row("c")],
      [blocks("a", "c"), blocks("b", "c")],
      STATES,
    );
    expect(cleared.byDoc.get("c")!.ready).toBe(true);
  });

  it("is false for work already finished", () => {
    const model = buildSequence([row("a", { status: "done" })], [], STATES);
    expect(model.byDoc.get("a")!.ready).toBe(false);
  });

  it("keeps a done blocker in its wave so the chart does not reshuffle", () => {
    // The sequence is a property of the work, not of how far along it is. If a
    // cleared edge collapsed, every close would jump rows left and reroute
    // connectors, and the shape you learned yesterday would be gone.
    const model = buildSequence(
      [row("a", { status: "done" }), row("b")],
      [blocks("a", "b")],
      STATES,
    );
    expect(waveOf(model, "b")).toBe(1);
    expect(model.edges[0]!.cleared).toBe(true);
  });
});

describe("dates that contradict the graph", () => {
  /**
   * The only scheduling claim this view is entitled to make. Both dates were
   * set by a person and the graph says one must precede the other — so this is
   * a contradiction in the plan, not a projection from an invented start date.
   */
  it("flags an issue due no later than something that must precede it", () => {
    const model = buildSequence(
      [row("a", { due_date: 2_000 }), row("b", { due_date: 1_000 })],
      [blocks("a", "b")],
      STATES,
    );
    expect(model.byDoc.get("b")!.impossible).toBe(true);
    expect(model.byDoc.get("a")!.impossible).toBe(false);
  });

  it("flags an exact tie — a blocker cannot finish the same instant", () => {
    const model = buildSequence(
      [row("a", { due_date: 1_000 }), row("b", { due_date: 1_000 })],
      [blocks("a", "b")],
      STATES,
    );
    expect(model.byDoc.get("b")!.impossible).toBe(true);
  });

  it("says nothing when either end has no date", () => {
    const model = buildSequence(
      [row("a"), row("b", { due_date: 1_000 })],
      [blocks("a", "b")],
      STATES,
    );
    expect(model.byDoc.get("b")!.impossible).toBe(false);
    expect(model.byDoc.get("b")!.conflicts).toEqual([]);
  });

  /**
   * The flag on its own is an alarm with no fix attached. Naming the blockers
   * is what lets the view say which two dates disagree.
   */
  it("names the blockers the date disagrees with, and only those", () => {
    const model = buildSequence(
      [
        row("late", { due_date: 3_000 }),
        row("early", { due_date: 1_000 }),
        row("undated"),
        row("target", { due_date: 2_000 }),
      ],
      [blocks("late", "target"), blocks("early", "target"), blocks("undated", "target")],
      STATES,
    );
    expect(model.byDoc.get("target")!.conflicts).toEqual(["late"]);
    expect(model.byDoc.get("target")!.impossible).toBe(true);
  });
});

describe("reaching outward from one issue", () => {
  it("counts hops up the blockers and down the dependents", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c"), row("side"), row("far")],
      [blocks("a", "b"), blocks("b", "c"), blocks("side", "b")],
      STATES,
    );
    const hops = reachFrom(model, "b");
    expect(hops.get("b")).toBe(0);
    // One step each way: `a` and `side` block it, `c` waits on it.
    expect([hops.get("a"), hops.get("side"), hops.get("c")]).toEqual([1, 1, 1]);
    // Nothing joins `far` to the rest, so it is not on the chain at all.
    expect(hops.has("far")).toBe(false);
  });

  /**
   * The defect that made the reveal play across the whole page.
   *
   * `sibling` shares a blocker with `a` and is otherwise unrelated to it — it
   * neither waits on `a` nor is waited on by it. A walk that changes direction
   * mid-chain goes up to `root` and straight back down to `sibling`, and in a
   * graph of any density that is very nearly everything.
   */
  it("does not reach a sibling by going up and back down", () => {
    const model = buildSequence(
      [row("root"), row("a"), row("sibling"), row("child")],
      [blocks("root", "a"), blocks("root", "sibling"), blocks("a", "child")],
      STATES,
    );
    const hops = reachFrom(model, "a");
    expect(hops.get("root")).toBe(1);
    expect(hops.get("child")).toBe(1);
    expect(hops.has("sibling")).toBe(false);
  });

  it("keeps the shortest hop when a node is reachable two ways", () => {
    // `d` is two hops along the chain and one hop by the chord. The reveal is a
    // wavefront, so a node lights once, at its nearest arrival.
    const model = buildSequence(
      [row("a"), row("b"), row("c"), row("d")],
      [blocks("a", "b"), blocks("b", "c"), blocks("c", "d"), blocks("a", "d")],
      STATES,
    );
    expect(reachFrom(model, "a").get("d")).toBe(1);
  });

  it("is empty for an issue the model does not hold", () => {
    expect(reachFrom(buildSequence([row("a")], [], STATES), "ghost").size).toBe(0);
  });
});
