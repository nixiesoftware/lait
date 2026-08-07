import { describe, expect, it } from "vitest";

import { buildSequence } from "./sequence";
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
    expect(model.criticalPath).toEqual([]);
  });

  it("has no waves at all for no rows", () => {
    const model = buildSequence([], [], STATES);
    expect(model.waves).toEqual([]);
    expect(model.criticalPath).toEqual([]);
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
});

describe("critical path", () => {
  it("is the longest chain, in order", () => {
    const model = buildSequence(
      [row("a"), row("b"), row("c"), row("solo")],
      [blocks("a", "b"), blocks("b", "c")],
      STATES,
    );
    expect(model.criticalPath).toEqual(["a", "b", "c"]);
    expect(model.byDoc.get("solo")!.critical).toBe(false);
  });

  it("breaks a length tie on total estimate", () => {
    // Two chains of two. `heavy` carries more work, so it is the one that sets
    // the floor on finishing.
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
    expect(model.criticalPath).toEqual(["h1", "h2"]);
  });

  it("reports nothing when no issue blocks another", () => {
    // A chain of one is an issue, not a critical path — badging it would claim
    // a constraint that does not exist.
    const model = buildSequence([row("a"), row("b")], [], STATES);
    expect(model.criticalPath).toEqual([]);
  });

  it("marks only adjacent path members as critical edges", () => {
    // `a` blocks `c` directly as well as through `b`. That chord is a real
    // constraint but it is not a step along the path, and drawing it with the
    // same emphasis would claim the path runs through it.
    const model = buildSequence(
      [row("a"), row("b"), row("c")],
      [blocks("a", "b"), blocks("b", "c"), blocks("a", "c")],
      STATES,
    );
    expect(model.criticalPath).toEqual(["a", "b", "c"]);
    const chord = model.edges.find((e) => e.from === "a" && e.to === "c");
    expect(chord?.critical).toBe(false);
    expect(model.edges.find((e) => e.from === "a" && e.to === "b")?.critical).toBe(true);
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
  });
});
