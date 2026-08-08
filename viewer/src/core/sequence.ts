import type { GraphEdgeDto, Row, StatusCategory, WorkflowState } from "../types";

/**
 * Laying a project out by what has to happen first.
 *
 * Every other tracker's timeline is a Gantt chart: a time axis, and bars placed
 * on it from a start date and an end date somebody typed. lait has no start
 * date to type — an issue carries a due date and an estimate, nothing more — so
 * that chart is not available, and inventing starts to draw one would have the
 * view assert dates no human set.
 *
 * What lait does have, and Gantt tools mostly bolt on afterwards, is a real
 * dependency graph: `blocks` edges in the catalog, kept whole. So the axis here
 * is **sequence, not time**. Column N holds the issues that can only begin once
 * column N-1 is finished. That is a fact about the work, derived entirely from
 * edges somebody actually drew, and it stays true whether or not a single date
 * has been filled in.
 *
 * The vocabulary, since the rest of the view speaks it:
 *
 * - **wave** — an issue's dependency depth. Wave 0 is everything nothing
 *   blocks: the work that could start this morning. Wave N is what waits on
 *   wave N-1.
 * - **slack** — how many rounds an issue could sit later in the order without
 *   pushing the end of the project out. Zero slack means it is on *a* longest
 *   chain, and that if it slips the whole project slips.
 *
 *   This replaced a single "critical path". The path was the wrong shape for
 *   two reasons. Its *length* was already on screen twice — the longest chain's
 *   length is by construction the number of waves, so the legend printed one
 *   number under two labels. And when several chains tied, which of them got
 *   drawn came down to total estimate and then to **doc id**: the chart painted
 *   one arbitrarily chosen chain as the constraint while an equally
 *   constraining one sat next to it in grey. Slack has no winner to pick. It
 *   names every issue that constrains the finish, which is both the honest
 *   answer and the one you can act on.
 * - **cycle** — `blocks` edges have no CRDT stopping them forming a loop (the
 *   sub-issue tree does; this is not that tree). A loop has no depth, so it is
 *   detected and set aside rather than being allowed to hang the layout.
 * - **stalled** — not in a loop, but downstream of one, so it has no depth
 *   either. Kahn's cannot tell these two apart on its own and the first cut did
 *   not try: everything the queue failed to drain was reported as "these issues
 *   block each other", which is a false statement about a node that merely
 *   waits on a loop it is not part of. They are separated here because the view
 *   says different sentences about them and one of those sentences has to be
 *   true.
 */

/** An issue placed in the sequence. */
export interface SequenceNode {
  row: Row;
  /** Dependency depth. `0` = nothing blocks it. See `cyclic` for the exception. */
  wave: number;
  /** Doc ids of the issues directly blocking this one. */
  blockedBy: string[];
  /** Doc ids of the issues this one directly blocks. */
  blocks: string[];
  /**
   * Rounds this issue could slip before the project's finish moves.
   *
   * `0` means it is on a longest chain. A project of one column has no chain to
   * be on, so nothing there has zero slack — reporting otherwise would badge
   * every issue in a dependency-free project as constraining.
   */
  slack: number;
  /** `slack === 0`: nothing between this and the end of the project has room. */
  critical: boolean;
  /** On a dependency loop, so it has no honest depth. Parked in its own bucket. */
  cyclic: boolean;
  /**
   * Downstream of a loop, so it has no honest depth either — but it is not in
   * one, and must not be described as if it were.
   */
  stalled: boolean;
  /** Every blocker is finished, so the only thing between this and starting is
   *  someone picking it up. False for anything already done. */
  ready: boolean;
  /**
   * This issue is due no later than something that must finish before it.
   *
   * Not a derived date — both ends are dates a person actually set, and the
   * graph says one has to precede the other. That makes it a contradiction in
   * the plan rather than a projection, which is the only kind of scheduling
   * claim this view is entitled to make.
   */
  impossible: boolean;
  /**
   * The blockers behind `impossible`, by doc id.
   *
   * The flag alone was a warning triangle a person could not act on: it said a
   * contradiction existed and refused to say between which two dates. Naming
   * the other end is what turns it from an alarm into a fix.
   */
  conflicts: string[];
}

export interface SequenceModel {
  /** Waves in order; `waves[i]` holds the nodes at depth `i`. Never sparse. */
  waves: SequenceNode[][];
  /** Every node by doc id, for connector lookup. */
  byDoc: Map<string, SequenceNode>;
  /** Nodes on a dependency loop, which have no depth and are drawn apart. */
  cyclic: SequenceNode[];
  /** Nodes downstream of a loop: no depth either, but not part of the loop. */
  stalled: SequenceNode[];
  /**
   * Each loop, as its members in the order the edges run, so the view can name
   * the cycle ("A → B → A") instead of listing a set and hoping.
   */
  loops: string[][];
  /** How many nodes have zero slack. `0` when nothing blocks anything. */
  criticalCount: number;
  /** Direct `blocks` edges between nodes that both survived into the model. */
  edges: SequenceEdge[];
}

export interface SequenceEdge {
  from: string;
  to: string;
  /** A step along a longest chain: both ends have zero slack and the second
   *  sits exactly one round after the first. Drawn with emphasis. */
  critical: boolean;
  /** The blocker is done, so this constraint is already satisfied. */
  cleared: boolean;
}

const isDone = (category: StatusCategory | undefined) => category === "done";

/**
 * Build the sequence model for one project.
 *
 * `edges` may name docs that are not in `rows` (a filter is on, say). Those are
 * dropped rather than synthesised: a connector to a row the user cannot see is
 * a line to nowhere, and inventing a placeholder would put an issue on the
 * chart that the filter deliberately removed.
 */
export function buildSequence(
  rows: Row[],
  edges: GraphEdgeDto[],
  states: WorkflowState[],
): SequenceModel {
  const categoryOf = new Map(states.map((s) => [s.id, s.category]));
  const done = (row: Row) => isDone(categoryOf.get(row.status));

  const rowByDoc = new Map(rows.map((r) => [r.doc_id, r]));
  const blockedBy = new Map<string, string[]>();
  const blocks = new Map<string, string[]>();
  for (const doc of rowByDoc.keys()) {
    blockedBy.set(doc, []);
    blocks.set(doc, []);
  }

  // `blocks` only. `relates` is a reading aid with no direction worth laying
  // out, and `duplicates` is an assertion that two rows are one thing — neither
  // says anything about order, and treating them as sequence would invent
  // constraints nobody agreed to.
  for (const edge of edges) {
    if (edge.kind !== "blocks") continue;
    if (!rowByDoc.has(edge.from) || !rowByDoc.has(edge.to)) continue;
    if (edge.from === edge.to) continue;
    blockedBy.get(edge.to)!.push(edge.from);
    blocks.get(edge.from)!.push(edge.to);
  }

  const { wave, cyclic, stalled, loops } = depths(rowByDoc, blockedBy, blocks);

  // Structural depth, computed over every edge regardless of status — a done
  // blocker still holds its position in the sequence.
  //
  // The alternative, dropping cleared edges, makes the chart rearrange itself
  // every time someone closes an issue: rows jump left, connectors reroute, and
  // the shape you learned yesterday is gone. The sequence is a property of the
  // work, not of how far along it is. Progress shows up in how the nodes are
  // drawn — `ready`, `cleared` — not in where they sit.
  const nodes = new Map<string, SequenceNode>();
  for (const [doc, row] of rowByDoc) {
    const blockers = blockedBy.get(doc)!;
    const due = row.due_date ?? null;
    const conflicts =
      due === null
        ? []
        : blockers.filter((b) => {
            const blockerDue = rowByDoc.get(b)?.due_date ?? null;
            return blockerDue !== null && blockerDue >= due;
          });
    nodes.set(doc, {
      row,
      wave: wave.get(doc) ?? 0,
      blockedBy: blockers,
      blocks: blocks.get(doc)!,
      slack: 0,
      critical: false,
      cyclic: cyclic.has(doc),
      stalled: stalled.has(doc),
      ready: !done(row) && blockers.every((b) => {
        const blocker = rowByDoc.get(b);
        return blocker !== undefined && done(blocker);
      }),
      impossible: conflicts.length > 0,
      conflicts,
    });
  }

  // A node with no honest depth has no honest slack either, whether it is in
  // the loop or merely behind one.
  const unplaced = new Set([...cyclic, ...stalled]);
  const placed = [...nodes.values()].filter((n) => !unplaced.has(n.row.doc_id));
  const depth = Math.max(0, ...placed.map((n) => n.wave));
  assignSlack(nodes, placed, depth);
  const criticalCount = placed.filter((n) => n.critical).length;

  const modelEdges: SequenceEdge[] = [];
  for (const [doc, node] of nodes) {
    for (const target of node.blocks) {
      const to = nodes.get(target);
      if (!to) continue;
      modelEdges.push({
        from: doc,
        to: target,
        // A step, not merely a pair of constrained ends. A chord skipping a
        // round joins two zero-slack issues without being a link in any longest
        // chain, and drawing it with the same weight would claim the chain runs
        // through it.
        critical: node.critical && to.critical && to.wave === node.wave + 1,
        cleared: done(node.row),
      });
    }
  }

  const waves: SequenceNode[][] = Array.from({ length: placed.length ? depth + 1 : 0 }, () => []);
  for (const node of placed) waves[node.wave]!.push(node);

  return {
    waves,
    byDoc: nodes,
    cyclic: [...nodes.values()].filter((n) => n.cyclic),
    stalled: [...nodes.values()].filter((n) => n.stalled),
    loops,
    criticalCount,
    edges: modelEdges,
  };
}

/**
 * How far each issue could slip before the project's end moves.
 *
 * `wave` is already the earliest round an issue can sit in — the longest path
 * down to it. The other half is the longest path *onward* from it to something
 * nothing waits on; subtract that from the last round and you have the latest
 * round it could occupy. The gap between the two is its slack, and zero slack
 * is the precise statement of "this one constrains the finish".
 *
 * The onward walk goes in reverse wave order, which is a valid topological
 * order for the placed subgraph by construction — every edge runs from a lower
 * wave to a higher one, so a node's dependents are all settled before it is
 * reached. No second sort, and no risk of recursing down a chain of user data.
 *
 * A project of one column is exempt. Every issue in it would otherwise measure
 * zero slack and get badged as constraining the finish, which is technically
 * consistent and useless: with nothing blocking anything, nothing is holding
 * anything up.
 */
function assignSlack(
  nodes: ReadonlyMap<string, SequenceNode>,
  placed: readonly SequenceNode[],
  depth: number,
): void {
  if (depth === 0) return;
  /** Longest path in hops from this node to something nothing waits on. */
  const onward = new Map<string, number>();
  for (const node of [...placed].sort((a, b) => b.wave - a.wave)) {
    let longest = 0;
    for (const next of node.blocks) {
      const dependent = nodes.get(next);
      if (!dependent || !onward.has(next)) continue;
      longest = Math.max(longest, onward.get(next)! + 1);
    }
    onward.set(node.row.doc_id, longest);
  }
  for (const node of placed) {
    node.slack = depth - (onward.get(node.row.doc_id) ?? 0) - node.wave;
    node.critical = node.slack === 0;
  }
}

/**
 * Longest-path depth, by Kahn's algorithm.
 *
 * Longest and not shortest: an issue must sit after *every* blocker, so its
 * depth is one past the deepest of them. Shortest-path would place a node one
 * column after its nearest blocker and draw the rest of its constraints as
 * lines running backwards.
 *
 * Kahn's also answers *whether* there is a cycle for free: a node in a loop
 * never reaches in-degree zero, so whatever the queue does not drain is exactly
 * the set with no honest depth.
 *
 * What it cannot answer is *which* of those nodes are in the loop. Everything
 * downstream of a loop also fails to drain, and calling those "issues that
 * block each other" is simply false — a fixture with a two-issue loop reported
 * three, and the third was an ordinary issue waiting behind it. So the
 * undrained set is split by strong connectivity: a component of more than one
 * node is a loop, and everything else that failed to drain is stalled behind
 * one.
 */
function depths(
  rowByDoc: ReadonlyMap<string, Row>,
  blockedBy: ReadonlyMap<string, string[]>,
  blocks: ReadonlyMap<string, string[]>,
): {
  wave: Map<string, number>;
  cyclic: Set<string>;
  stalled: Set<string>;
  loops: string[][];
} {
  const indegree = new Map<string, number>();
  const dependents = new Map<string, string[]>();
  for (const doc of rowByDoc.keys()) {
    indegree.set(doc, blockedBy.get(doc)?.length ?? 0);
    dependents.set(doc, []);
  }
  for (const [doc, blockers] of blockedBy) {
    for (const blocker of blockers) dependents.get(blocker)?.push(doc);
  }

  const wave = new Map<string, number>();
  const queue: string[] = [];
  for (const [doc, n] of indegree) {
    if (n === 0) {
      wave.set(doc, 0);
      queue.push(doc);
    }
  }
  let head = 0;
  while (head < queue.length) {
    const doc = queue[head]!;
    head += 1;
    for (const dependent of dependents.get(doc)!) {
      wave.set(dependent, Math.max(wave.get(dependent) ?? 0, (wave.get(doc) ?? 0) + 1));
      const left = indegree.get(dependent)! - 1;
      indegree.set(dependent, left);
      if (left === 0) queue.push(dependent);
    }
  }

  const undrained: string[] = [];
  for (const doc of rowByDoc.keys()) if (!wave.has(doc)) undrained.push(doc);

  // Strong connectivity over the undrained subgraph only. The drained part is
  // acyclic by construction, so there is nothing there to find and no reason to
  // walk it.
  const within = new Set(undrained);
  const onward = new Map<string, string[]>();
  for (const doc of undrained) {
    onward.set(doc, (blocks.get(doc) ?? []).filter((next) => within.has(next)));
  }

  const cyclic = new Set<string>();
  const loops: string[][] = [];
  for (const component of stronglyConnected(undrained, onward)) {
    // A component of one is a node that cannot reach itself — a self-edge would
    // make it a loop of one, and those are rejected before they ever get here.
    if (component.length < 2) continue;
    for (const doc of component) cyclic.add(doc);
    loops.push(traverseLoop(component, onward));
  }

  const stalled = new Set(undrained.filter((doc) => !cyclic.has(doc)));
  return { wave, cyclic, stalled, loops };
}

/**
 * Tarjan's strongly connected components, iteratively.
 *
 * Iterative rather than the textbook recursion because the recursion depth is
 * the size of the component, and the input here is user data: a project that
 * has managed to chain several thousand issues into one loop should get a
 * warning, not a blown stack in the middle of a render.
 */
function stronglyConnected(
  nodes: readonly string[],
  onward: ReadonlyMap<string, string[]>,
): string[][] {
  const index = new Map<string, number>();
  const low = new Map<string, number>();
  const onStack = new Set<string>();
  const stack: string[] = [];
  const components: string[][] = [];
  let counter = 0;

  const open = (doc: string) => {
    index.set(doc, counter);
    low.set(doc, counter);
    counter += 1;
    stack.push(doc);
    onStack.add(doc);
  };

  for (const root of nodes) {
    if (index.has(root)) continue;
    open(root);
    /** The explicit call stack: which node, and how far through its edges. */
    const frames: Array<{ doc: string; cursor: number }> = [{ doc: root, cursor: 0 }];

    while (frames.length > 0) {
      const frame = frames[frames.length - 1]!;
      const edges = onward.get(frame.doc) ?? [];
      if (frame.cursor < edges.length) {
        const next = edges[frame.cursor]!;
        frame.cursor += 1;
        if (!index.has(next)) {
          open(next);
          frames.push({ doc: next, cursor: 0 });
        } else if (onStack.has(next)) {
          low.set(frame.doc, Math.min(low.get(frame.doc)!, index.get(next)!));
        }
        continue;
      }

      frames.pop();
      const parent = frames[frames.length - 1];
      if (parent) low.set(parent.doc, Math.min(low.get(parent.doc)!, low.get(frame.doc)!));
      if (low.get(frame.doc) === index.get(frame.doc)) {
        const component: string[] = [];
        for (;;) {
          const popped = stack.pop()!;
          onStack.delete(popped);
          component.push(popped);
          if (popped === frame.doc) break;
        }
        components.push(component);
      }
    }
  }
  return components;
}

/**
 * A loop's members in the order its edges actually run.
 *
 * Tarjan hands back a set in finish order, which is not the order anybody wants
 * to read: "these four block each other" says nothing a person can act on,
 * while "A blocks B blocks C blocks A" names the edge to cut. Starts at the
 * lexicographically first member so the same loop reads the same way twice.
 */
function traverseLoop(component: readonly string[], onward: ReadonlyMap<string, string[]>): string[] {
  const members = new Set(component);
  const start = [...component].sort()[0]!;
  const order: string[] = [];
  const seen = new Set<string>();
  let cursor: string | undefined = start;
  while (cursor !== undefined && !seen.has(cursor)) {
    seen.add(cursor);
    order.push(cursor);
    cursor = (onward.get(cursor) ?? [])
      .filter((next) => members.has(next))
      .sort()
      .find((next) => !seen.has(next));
  }
  // Anything the walk could not reach in one pass (a component woven from more
  // than one loop) still belongs in the answer; appending beats dropping it.
  for (const doc of [...component].sort()) if (!seen.has(doc)) order.push(doc);
  return order;
}

/**
 * One issue's chain: everything it waits on, everything that waits on it, and
 * how many hops away each of those is.
 *
 * Two separate walks, one up the `blockedBy` edges and one down the `blocks`
 * edges — and never a mixture. That is the whole correctness of this function.
 * A single walk that follows both directions at each step does not trace a
 * chain at all: from `a` it goes up to a blocker and straight back down to
 * every *sibling* that shares it, which is not something `a` depends on nor
 * something that depends on `a`. In a graph of any density that reaches almost
 * everything, which is why the first cut lit thirty-one of thirty-four rows and
 * the animation played across the whole page for any issue you touched.
 *
 * Transitive within each direction, though, which the one-hop version this
 * replaced deliberately was not. The argument then was that breadth says
 * nothing — true, when breadth is all that is on screen. The view animates the
 * reach outward a hop at a time now, so distance is carried by *when* a mark
 * lights, and the ancestors-and-descendants set is exactly the set a person
 * means by "this issue's chain".
 */
export function reachFrom(model: SequenceModel, doc: string): Map<string, number> {
  const hops = new Map<string, number>();
  if (!model.byDoc.has(doc)) return hops;
  hops.set(doc, 0);
  for (const direction of ["blockedBy", "blocks"] as const) {
    let frontier = [doc];
    let depth = 0;
    while (frontier.length > 0) {
      depth += 1;
      const next: string[] = [];
      for (const at of frontier) {
        for (const other of model.byDoc.get(at)?.[direction] ?? []) {
          if (hops.has(other) || !model.byDoc.has(other)) continue;
          hops.set(other, depth);
          next.push(other);
        }
      }
      frontier = next;
    }
  }
  return hops;
}
