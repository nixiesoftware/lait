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
 * - **critical path** — the longest chain of blockers. Its length *is* the
 *   number of waves, so nothing finishes sooner than this chain does, however
 *   many people you add.
 * - **cycle** — `blocks` edges have no CRDT stopping them forming a loop (the
 *   sub-issue tree does; this is not that tree). A loop has no depth, so it is
 *   detected and set aside rather than being allowed to hang the layout.
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
  /** On the longest chain — the run of work that sets the floor on finishing. */
  critical: boolean;
  /** In a dependency loop, so it has no honest depth. Parked in its own bucket. */
  cyclic: boolean;
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
}

export interface SequenceModel {
  /** Waves in order; `waves[i]` holds the nodes at depth `i`. Never sparse. */
  waves: SequenceNode[][];
  /** Every node by doc id, for connector lookup. */
  byDoc: Map<string, SequenceNode>;
  /** Nodes in a dependency loop, which have no depth and are drawn apart. */
  cyclic: SequenceNode[];
  /** Doc ids on the critical path, in order. Empty when nothing blocks anything. */
  criticalPath: string[];
  /** Direct `blocks` edges between nodes that both survived into the model. */
  edges: SequenceEdge[];
}

export interface SequenceEdge {
  from: string;
  to: string;
  /** Both ends on the critical path, adjacently — drawn with emphasis. */
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

  const { wave, cyclic } = depths(rowByDoc, blockedBy);

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
    nodes.set(doc, {
      row,
      wave: wave.get(doc) ?? 0,
      blockedBy: blockers,
      blocks: blocks.get(doc)!,
      critical: false,
      cyclic: cyclic.has(doc),
      ready: !done(row) && blockers.every((b) => {
        const blocker = rowByDoc.get(b);
        return blocker !== undefined && done(blocker);
      }),
      impossible:
        due !== null &&
        blockers.some((b) => {
          const blocker = rowByDoc.get(b);
          const blockerDue = blocker?.due_date ?? null;
          return blockerDue !== null && blockerDue >= due;
        }),
    });
  }

  const criticalPath = longestChain(nodes, cyclic);
  const onPath = new Set(criticalPath);
  for (const doc of criticalPath) nodes.get(doc)!.critical = true;

  const modelEdges: SequenceEdge[] = [];
  for (const [doc, node] of nodes) {
    for (const target of node.blocks) {
      const to = nodes.get(target);
      if (!to) continue;
      modelEdges.push({
        from: doc,
        to: target,
        // Adjacent on the path, not merely both on it: a chord between two
        // path members is a different constraint and drawing it with the same
        // emphasis would claim the path runs through it.
        critical:
          onPath.has(doc) &&
          onPath.has(target) &&
          criticalPath.indexOf(target) === criticalPath.indexOf(doc) + 1,
        cleared: done(node.row),
      });
    }
  }

  const depth = Math.max(0, ...[...nodes.values()].filter((n) => !n.cyclic).map((n) => n.wave));
  const waves: SequenceNode[][] = Array.from({ length: nodes.size ? depth + 1 : 0 }, () => []);
  for (const node of nodes.values()) {
    if (node.cyclic) continue;
    waves[node.wave]!.push(node);
  }

  return {
    waves,
    byDoc: nodes,
    cyclic: [...nodes.values()].filter((n) => n.cyclic),
    criticalPath,
    edges: modelEdges,
  };
}

/**
 * Longest-path depth, by Kahn's algorithm.
 *
 * Longest and not shortest: an issue must sit after *every* blocker, so its
 * depth is one past the deepest of them. Shortest-path would place a node one
 * column after its nearest blocker and draw the rest of its constraints as
 * lines running backwards.
 *
 * Kahn's also answers the cycle question for free. A node in a loop never
 * reaches in-degree zero, so whatever the queue does not drain is exactly the
 * set with no honest depth — no separate cycle hunt needed.
 */
function depths(
  rowByDoc: ReadonlyMap<string, Row>,
  blockedBy: ReadonlyMap<string, string[]>,
): { wave: Map<string, number>; cyclic: Set<string> } {
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

  const cyclic = new Set<string>();
  for (const doc of rowByDoc.keys()) if (!wave.has(doc)) cyclic.add(doc);
  return { wave, cyclic };
}

/**
 * The longest chain of blockers, as doc ids from first to last.
 *
 * Ties break on total estimate, then on doc id. The estimate tie-break is the
 * useful one — two chains of equal length are not equally alarming if one holds
 * three times the work — and the doc-id fallback is what makes the answer
 * stable: without it, two equal chains would swap places between renders
 * depending on map order, and the highlight would flicker.
 */
function longestChain(
  nodes: ReadonlyMap<string, SequenceNode>,
  cyclic: ReadonlySet<string>,
): string[] {
  const weight = (doc: string) => nodes.get(doc)?.row.estimate ?? 1;
  const best = new Map<string, { length: number; cost: number; prev: string | null }>();

  const ordered = [...nodes.values()]
    .filter((n) => !cyclic.has(n.row.doc_id))
    .sort((a, b) => a.wave - b.wave || a.row.doc_id.localeCompare(b.row.doc_id));

  for (const node of ordered) {
    const doc = node.row.doc_id;
    let pick: { length: number; cost: number; prev: string | null } = {
      length: 1,
      cost: weight(doc),
      prev: null,
    };
    for (const blocker of node.blockedBy) {
      const from = best.get(blocker);
      if (!from) continue;
      const candidate = {
        length: from.length + 1,
        cost: from.cost + weight(doc),
        prev: blocker,
      };
      if (
        candidate.length > pick.length ||
        (candidate.length === pick.length && candidate.cost > pick.cost) ||
        (candidate.length === pick.length &&
          candidate.cost === pick.cost &&
          pick.prev !== null &&
          blocker.localeCompare(pick.prev) < 0)
      ) {
        pick = candidate;
      }
    }
    best.set(doc, pick);
  }

  let tail: string | null = null;
  for (const [doc, entry] of best) {
    const champion = tail ? best.get(tail)! : null;
    if (
      !champion ||
      entry.length > champion.length ||
      (entry.length === champion.length && entry.cost > champion.cost) ||
      (entry.length === champion.length &&
        entry.cost === champion.cost &&
        doc.localeCompare(tail!) < 0)
    ) {
      tail = doc;
    }
  }

  // A chain of one is not a critical path — it is an issue. Reporting it would
  // put a "critical path" badge on a project where nothing blocks anything.
  if (tail === null || best.get(tail)!.length < 2) return [];

  const chain: string[] = [];
  for (let cursor: string | null = tail; cursor !== null; cursor = best.get(cursor)!.prev) {
    chain.push(cursor);
  }
  return chain.reverse();
}

/**
 * Group a wave's nodes by milestone, in the order the milestones are given.
 *
 * The lanes are the project's milestones — its intended sequence, spec to
 * completion — and the columns are the sequence the graph actually forces.
 * Reading them together is the point of the view: an issue sitting in an early
 * milestone but a late wave is a plan the dependencies do not support.
 *
 * `null` collects issues with no milestone, and always sorts last: it is a
 * holding pen, not a stage of the work.
 */
export function laneOf(node: SequenceNode): string | null {
  return node.row.milestone ?? null;
}
