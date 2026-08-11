import { describe, expect, it } from "vitest";

import type { GeometryEdge, GeometryNode, GeometryView, Row } from "../types";
import { layoutMorphology, reachFrom, COL, PAD_LEFT, SLOT } from "./morphology";

function row(key: string): Row {
  return {
    reff: key,
    doc_id: key,
    project_id: "prj",
    key_alias: key.toUpperCase(),
    title: `Issue ${key}`,
    status: "active",
    priority: "none",
    assignee_summary: "",
    assignees: [],
    tombstone: false,
    provisional: false,
  };
}

function node(
  key: string,
  layer: number | null,
  extra: Partial<GeometryNode> = {},
): GeometryNode {
  return {
    row: row(key),
    component: "component-1",
    layer,
    ordinal: 0,
    hierarchy_depth: 0,
    children: [],
    blocked_by: [],
    blocks: [],
    closure: layer === null ? "cycle" : "ready",
    slack: 1,
    facets: [],
    ...extra,
  };
}

function blocks(from: string, to: string): GeometryEdge {
  return { from, relation: "blocks", role: "constraint", to };
}

function view(nodes: GeometryNode[], edges: GeometryEdge[] = [], over: Partial<GeometryView> = {}): GeometryView {
  const components = [...new Set(nodes.map((n) => n.component))].map((id) => ({
    id,
    nodes: nodes.filter((n) => n.component === id).map((n) => n.row.doc_id),
    roots: [],
    terminals: [],
    loops: [],
  }));
  return {
    schema_version: 1,
    generation: "ab".repeat(32),
    project: "prj",
    roots: [],
    nodes,
    edges,
    components,
    residuals: [],
    closure: { total: nodes.length, closed: 0, ready: nodes.length, blocked: 0, cyclic: 0, stalled: 0 },
    ...over,
  };
}

const all = (v: GeometryView) => new Set(v.nodes.map((n) => n.row.doc_id));

describe("morphology layout", () => {
  it("puts the layer on x, left to right", () => {
    const v = view(
      [node("a", 0), node("b", 1), node("c", 2)],
      [blocks("a", "b"), blocks("b", "c")],
    );
    const m = layoutMorphology(v, all(v));
    expect(m.byDoc.get("a")!.x).toBe(PAD_LEFT);
    expect(m.byDoc.get("b")!.x).toBe(PAD_LEFT + COL);
    expect(m.byDoc.get("c")!.x).toBe(PAD_LEFT + 2 * COL);
    expect(m.layers).toBe(3);
  });

  // The whole point of the slot rule: an unbranched chain is a straight line,
  // so a reader follows it without their eye having to step down every column.
  it("keeps an unbranched chain on one row", () => {
    const v = view(
      [node("a", 0), node("b", 1), node("c", 2), node("d", 3)],
      [blocks("a", "b"), blocks("b", "c"), blocks("c", "d")],
    );
    const m = layoutMorphology(v, all(v));
    const ys = ["a", "b", "c", "d"].map((doc) => m.byDoc.get(doc)!.y);
    expect(new Set(ys).size).toBe(1);
  });

  it("drops a fork one slot below the chain it comes off", () => {
    // `b` continues the trunk (it has work behind it); `c` is the twig.
    const v = view(
      [
        node("a", 0),
        node("b", 1, { blocks: ["d"] }),
        node("c", 1),
        node("d", 2),
      ],
      [blocks("a", "b"), blocks("a", "c"), blocks("b", "d")],
    );
    const m = layoutMorphology(v, all(v));
    expect(m.byDoc.get("a")!.slot).toBe(0);
    expect(m.byDoc.get("b")!.slot).toBe(0);
    expect(m.byDoc.get("c")!.slot).toBe(1);
    // And the trunk stays level across the fork.
    expect(m.byDoc.get("d")!.slot).toBe(0);
  });

  // Median rather than first-blocker: a convergence sits between the things
  // converging on it, which is what stops the wires crossing to reach it.
  it("places a convergence at the median of its blockers", () => {
    const v = view(
      [node("a", 0), node("b", 0), node("c", 0), node("z", 1)],
      [blocks("a", "z"), blocks("b", "z"), blocks("c", "z")],
    );
    const m = layoutMorphology(v, all(v));
    const slots = ["a", "b", "c"].map((doc) => m.byDoc.get(doc)!.slot).sort();
    expect(slots).toEqual([0, 1, 2]);
    expect(m.byDoc.get("z")!.slot).toBe(1);
  });

  it("gives each disconnected patch its own track, biggest first", () => {
    const v = view([
      node("solo", 0, { component: "component-2" }),
      node("a", 0),
      node("b", 1),
      node("c", 2),
    ], [blocks("a", "b"), blocks("b", "c")]);
    const m = layoutMorphology(v, all(v));
    expect(m.bands.map((band) => band.id)).toEqual(["component-1", "component-2"]);
    expect(m.bands[0]!.ordinal).toBe(1);
    // The second track starts below the first, never interleaved with it.
    expect(m.byDoc.get("solo")!.y).toBeGreaterThan(m.byDoc.get("a")!.y + SLOT);
  });

  it("sets aside anything with no honest layer", () => {
    const v = view(
      [node("a", 0), node("x", null), node("y", null, { closure: "stalled" })],
      [],
    );
    const m = layoutMorphology(v, all(v));
    expect(m.byDoc.has("x")).toBe(false);
    expect(m.unplaced.map((n) => n.row.doc_id)).toEqual(["x", "y"]);
  });

  it("reads a loop as a walk, not as a set", () => {
    const nodes = [
      node("a", null, { blocks: ["b"] }),
      node("b", null, { blocks: ["c"] }),
      node("c", null, { blocks: ["a"] }),
    ];
    const v = view(nodes, []);
    v.components[0]!.loops = [["c", "a", "b"]];
    const m = layoutMorphology(v, all(v));
    expect(m.loops).toEqual([["a", "b", "c"]]);
  });

  // A filter scopes what is drawn; it must not resurrect a wire to a node the
  // reader can no longer see.
  it("drops edges whose other end was filtered out", () => {
    const v = view(
      [node("a", 0), node("b", 1), node("c", 2)],
      [blocks("a", "b"), blocks("b", "c")],
    );
    const m = layoutMorphology(v, new Set(["a", "c"]));
    expect(m.edges).toHaveLength(0);
    expect(m.byDoc.size).toBe(2);
  });

  // Every issue in a flat project measures zero slack, and badging all of them
  // as constraining the finish is consistent and useless.
  it("exempts a project with no depth from the zero-slack mark", () => {
    const flat = view([node("a", 0, { slack: 0 }), node("b", 0, { slack: 0 })]);
    expect(layoutMorphology(flat, all(flat)).criticalCount).toBe(0);

    const deep = view(
      [node("a", 0, { slack: 0 }), node("b", 1, { slack: 0 }), node("c", 1, { slack: 3 })],
      [blocks("a", "b"), blocks("a", "c")],
    );
    expect(layoutMorphology(deep, all(deep)).criticalCount).toBe(2);
  });

  it("carries the engine's due-order conflict onto the node", () => {
    const v = view([node("a", 0), node("b", 1)], [blocks("a", "b")], {
      residuals: [
        { kind: "due_order_conflict", component: "component-1", layer: 1, at: ["b"], requires: ["a"] },
      ],
    });
    const m = layoutMorphology(v, all(v));
    expect(m.byDoc.get("b")!.conflicts).toEqual(["a"]);
    expect(m.byDoc.get("a")!.conflicts).toEqual([]);
  });

  // Caught on a live head, not here: a pill sized from its key and its padding
  // alone forgot that the status dot sits inside the same box, and every node
  // on a 68-issue project came out ellipsised as "EXEC…". The label is the only
  // reason a node is labelled, so the floor is stated rather than eyeballed.
  it("sizes a node for its key, its dot and its padding together", () => {
    const v = view([node("exec-100", 0)]);
    const width = layoutMorphology(v, all(v)).byDoc.get("exec-100")!.width;
    // "EXEC-100" is 8 characters at a 6.0px advance, plus 16px of padding, a
    // 6px dot and the 6px gap after it.
    expect(width).toBeGreaterThanOrEqual(8 * 6 + 16 + 6 + 6);
  });

  it("never lets a node close the gap a wire runs through", () => {
    const v = view([node("a-very-long-issue-key", 0)]);
    expect(layoutMorphology(v, all(v)).byDoc.get("a-very-long-issue-key")!.width)
      .toBeLessThanOrEqual(COL - 40);
  });

  it("lays the same geometry out identically twice", () => {
    const v = view(
      [node("a", 0), node("b", 1), node("c", 1), node("d", 2)],
      [blocks("a", "b"), blocks("a", "c"), blocks("b", "d"), blocks("c", "d")],
    );
    const shape = (m: ReturnType<typeof layoutMorphology>) =>
      [...m.byDoc].map(([doc, p]) => [doc, p.x, p.y, p.width]);
    expect(shape(layoutMorphology(v, all(v)))).toEqual(shape(layoutMorphology(v, all(v))));
  });
});

describe("reachFrom", () => {
  // A single walk following both directions goes up to a blocker and straight
  // back down to every sibling sharing it, which is neither an ancestor nor a
  // descendant. Two separate walks is the whole correctness of the function.
  it("walks ancestors and descendants but never siblings", () => {
    const v = view(
      [
        node("a", 0, { blocks: ["b", "c"] }),
        node("b", 1, { blocked_by: ["a"], blocks: ["d"] }),
        node("c", 1, { blocked_by: ["a"] }),
        node("d", 2, { blocked_by: ["b"] }),
      ],
      [blocks("a", "b"), blocks("a", "c"), blocks("b", "d")],
    );
    const m = layoutMorphology(v, all(v));
    const hops = reachFrom(m, "b");
    expect(hops.get("b")).toBe(0);
    expect(hops.get("a")).toBe(1);
    expect(hops.get("d")).toBe(1);
    expect(hops.has("c")).toBe(false);
  });
});
