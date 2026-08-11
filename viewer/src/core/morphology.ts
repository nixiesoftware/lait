import type { GeometryNode, GeometryRole, GeometryView } from "../types";

/**
 * Placing a project's issues in two dimensions.
 *
 * The timeline used to be a list whose horizontal position meant something: one
 * row per issue, x for dependency depth, y for nothing but reading order. That
 * is one and a half dimensions, and it spends the whole vertical axis on a
 * sequence the eye already gets from the order of the lines.
 *
 * Here both axes carry the graph. **x is the dependency layer**, unchanged and
 * still left to right: column 0 is what could start this morning. **y is a
 * slot**, and a node takes the slot of the blocker it continues — so a chain
 * comes out as a straight horizontal run, a fork visibly forks, and a
 * convergence visibly converges. Top to bottom is still reading order in the
 * sense that matters: the first track is the biggest, and inside a track the
 * trunk sits above the branches it sheds.
 *
 * **Nothing here is invented.** Every number this module places a node by comes
 * from `products/issues/src/geometry.rs` — `layer` from its Kahn pass,
 * `component` from its connectivity walk, `slack` from its longest-path
 * arithmetic, `ordinal` for the tie-break. The engine compiles the morphology
 * and this file bends it into a picture, which is the division `geometry.rs`
 * states in its own header: a layout engine "may bend this phenotype into an
 * organic view, but may not invent a node, edge, position, or gap."
 *
 * That is also why ~250 lines of Kahn and Tarjan left `core/sequence.ts` to get
 * here: they were a second implementation of the engine's own algorithms,
 * written months apart, agreeing by luck rather than by construction.
 *
 * Determinism is a hard requirement and not merely a nicety — two replicas
 * reading the same generation must draw the same chart. Every sort in this file
 * ends in a doc-id comparison, nothing consults the clock, and no measurement is
 * taken from the DOM. The same `GeometryView` in produces the same pixels out.
 */

/**
 * The layout metrics. Everything on the chart is arithmetic on these, which is
 * what lets a connector be routed without measuring the node it lands on.
 *
 * `COL` is wider than the old chart's 92 because a node now carries its key
 * rather than being an anonymous bar beside a rail that named it. `PILL_MAX`
 * holds back `COL - PILL_MAX` of clear space in every column so a wire always
 * has somewhere to run.
 */
export const COL = 136;
export const SLOT = 28;
export const PILL_H = 20;
export const PAD_LEFT = 16;
export const PAD_TOP = 14;
/** Air between one track and the next — enough that the rule between them is
 *  reading as a boundary rather than as a crowded row. */
export const BAND_GAP = 30;
export const GUTTER = 24;

/** Monospace advance at the `2xs` rung. 10px compact, 11px comfortable; this is
 *  the comfortable case, so a key never clips at either density. (Measured on a
 *  live head: 6.0px at 10px, so the compact case carries ~10% of headroom.) */
const CHAR = 6.6;
const PILL_PAD = 8;
/**
 * The status dot and the gap after it.
 *
 * Named constants rather than a fudge factor because their absence was a real
 * defect and a silent one. The first cut sized a pill from its key and its
 * padding, and forgot that the dot sits inside the same box — so every node on
 * a live project came out as `EXEC…`, ellipsised by exactly these 12px. A node
 * that cannot print its key is an anonymous mark in a graph of eighty, which is
 * the one thing this layout may not produce: the key is the only reason the
 * node is labelled, and the rail that used to name it is gone.
 */
const DOT = 6;
const DOT_GAP = 6;
/** A floor, so a project with two-character keys does not draw a row of chips
 *  too small to aim at. */
const PILL_MIN = 60;
/** The ceiling holds `COL - PILL_MAX` clear in every column for a wire to run
 *  through. A key past ~10 characters is ellipsised rather than closing that
 *  gap — the wires are the other half of the picture. */
const PILL_MAX = COL - 40;
/** How much of a pill's width the estimate may claim. The mark still carries
 *  "how big" as well as "when", which was the one thing the old bar did that a
 *  labelled node would otherwise lose. */
const EST_BONUS = 24;

/** A node, placed. */
export interface PlacedNode {
  node: GeometryNode;
  /** The dependency layer — the column, left to right. */
  layer: number;
  /** The slot within its track — the row, top to bottom. */
  slot: number;
  /** Left edge of the pill. */
  x: number;
  /** Vertical centre of the pill, which is where wires meet it. */
  y: number;
  width: number;
  /** The component this node belongs to. */
  band: string;
  /** Longest chain onward from here, in hops. Decides which of two nodes keeps
   *  its blocker's slot when they both want it: the trunk wins over the twig. */
  onward: number;
  /** Zero slack — if this slips, the project slips. False in a project with no
   *  depth at all, where it would otherwise be true of everything. */
  critical: boolean;
  /** Due no later than something that must precede it, by doc id. From the
   *  engine's `due_order_conflict` residual, not recomputed here. */
  conflicts: string[];
}

/** One connected patch of the project, and the slots it occupies. */
export interface Band {
  id: string;
  /** 1-based, biggest first — "track 1" is the main body of the project. */
  ordinal: number;
  top: number;
  height: number;
  slots: number;
  /** Its nodes, in reading order (layer, then slot). */
  nodes: PlacedNode[];
}

export interface RoutedEdge {
  key: string;
  from: string;
  to: string;
  role: GeometryRole;
  /** The blocker is closed, so the constraint is already satisfied. */
  cleared: boolean;
  d: string;
}

export interface Morphology {
  bands: Band[];
  byDoc: Map<string, PlacedNode>;
  edges: RoutedEdge[];
  /** Drawn, but with no honest layer: in a loop, or waiting behind one. */
  unplaced: GeometryNode[];
  /** Each loop as its edges actually run, so it can be read as a sentence. */
  loops: string[][];
  /** How many columns the chart has. */
  layers: number;
  /** Drawn nodes per layer, for the scale along the top. */
  counts: number[];
  criticalCount: number;
  width: number;
  height: number;
}

/**
 * A finite stand-in for "no preference".
 *
 * `Infinity` reads better and cannot be used: the sort below subtracts two
 * preferences, and `Infinity - Infinity` is `NaN`, which makes the comparator
 * inconsistent and the resulting order implementation-defined. That is exactly
 * the class of bug this module's determinism requirement exists to forbid.
 */
const NO_PREFERENCE = Number.MAX_SAFE_INTEGER;

/**
 * Compile a `GeometryView` into a drawing.
 *
 * `visible` is what survived the filter. The geometry is always the whole
 * selection — the shape of the project is a fact about the project, and a
 * filter scopes what is drawn without rewriting how many rounds of work there
 * are — so anything outside `visible` is dropped from the picture and its edges
 * with it. A wire to a node the reader cannot see is a line to nowhere.
 */
export function layoutMorphology(
  geometry: GeometryView,
  visible: ReadonlySet<string>,
): Morphology {
  const drawn = geometry.nodes.filter((node) => visible.has(node.row.doc_id));
  const placedNodes = drawn.filter((node) => node.layer != null);
  const unplaced = drawn.filter((node) => node.layer == null);
  const inPlay = new Set(placedNodes.map((node) => node.row.doc_id));

  // `blocks` only. Containment and association say nothing about order, and the
  // engine has already labelled which is which, so this is a filter rather than
  // a second opinion about what the relations mean.
  const constraints = geometry.edges.filter(
    (edge) => edge.role === "constraint" && inPlay.has(edge.from) && inPlay.has(edge.to),
  );
  const blocks = new Map<string, string[]>();
  const blockedBy = new Map<string, string[]>();
  for (const node of placedNodes) {
    blocks.set(node.row.doc_id, []);
    blockedBy.set(node.row.doc_id, []);
  }
  for (const edge of constraints) {
    blocks.get(edge.from)!.push(edge.to);
    blockedBy.get(edge.to)!.push(edge.from);
  }

  // Longest chain onward from each node. Descending layer order is a valid
  // topological order for the placed subgraph by construction — every
  // constraint runs from a lower layer to a higher one — so one pass settles it
  // with no sort of its own and no recursion into user data.
  const onward = new Map<string, number>();
  for (const node of [...placedNodes].sort((a, b) => b.layer! - a.layer!)) {
    let longest = 0;
    for (const next of blocks.get(node.row.doc_id)!) {
      const depth = onward.get(next);
      if (depth !== undefined) longest = Math.max(longest, depth + 1);
    }
    onward.set(node.row.doc_id, longest);
  }

  const maxLayer = placedNodes.length === 0
    ? 0
    : Math.max(...placedNodes.map((node) => node.layer!));
  const layers = placedNodes.length === 0 ? 0 : maxLayer + 1;

  const conflictsByDoc = new Map<string, string[]>();
  for (const residual of geometry.residuals) {
    if (residual.kind !== "due_order_conflict") continue;
    const at = residual.at[0];
    if (at !== undefined) conflictsByDoc.set(at, residual.requires);
  }

  // Biggest track first. A project's main body should be the thing at the top
  // of the window, and a two-issue offshoot should not push it down.
  const members = new Map<string, GeometryNode[]>();
  for (const node of placedNodes) {
    members.set(node.component, [...(members.get(node.component) ?? []), node]);
  }
  const tracks = [...members.entries()].sort(
    ([leftId, left], [rightId, right]) =>
      right.length - left.length || leftId.localeCompare(rightId),
  );

  const slotOf = new Map<string, number>();
  const bands: Band[] = [];
  let top = PAD_TOP;
  for (const [index, [id, nodes]] of tracks.entries()) {
    const maxSlot = assignSlots(nodes, blockedBy, onward, slotOf);
    const slots = maxSlot + 1;
    const height = slots * SLOT;
    bands.push({ id, ordinal: index + 1, top, height, slots, nodes: [] });
    top += height + BAND_GAP;
  }

  const largest = Math.max(1, ...drawn.map((node) => node.row.estimate ?? 0));
  const byDoc = new Map<string, PlacedNode>();
  for (const band of bands) {
    for (const node of members.get(band.id)!) {
      const doc = node.row.doc_id;
      const slot = slotOf.get(doc)!;
      const placed: PlacedNode = {
        node,
        layer: node.layer!,
        slot,
        x: PAD_LEFT + node.layer! * COL,
        y: band.top + slot * SLOT + SLOT / 2,
        width: pillWidth(node, largest),
        band: band.id,
        onward: onward.get(doc) ?? 0,
        // A project with no depth is exempt. Every issue in it measures zero
        // slack, and badging all of them as constraining the finish is
        // technically consistent and useless: with nothing blocking anything,
        // nothing is holding anything up.
        critical: maxLayer > 0 && (node.slack ?? 0) === 0,
        conflicts: conflictsByDoc.get(doc) ?? [],
      };
      byDoc.set(doc, placed);
      band.nodes.push(placed);
    }
    band.nodes.sort(
      (a, b) => a.layer - b.layer || a.slot - b.slot ||
        a.node.row.doc_id.localeCompare(b.node.row.doc_id),
    );
  }

  const edges: RoutedEdge[] = [];
  for (const edge of constraints) {
    const from = byDoc.get(edge.from);
    const to = byDoc.get(edge.to);
    if (!from || !to) continue;
    const x1 = from.x + from.width;
    const x2 = to.x;
    // A single cubic with horizontal handles: it leaves the node going right
    // and arrives going right, so the eye reads it as flow rather than as a
    // wire that happens to join two points. Every constraint runs to a strictly
    // higher layer, so there are no backward edges to route around.
    const bend = Math.max(14, (x2 - x1) / 2);
    edges.push({
      key: `${edge.from}->${edge.to}`,
      from: edge.from,
      to: edge.to,
      role: edge.role,
      cleared: from.node.closure === "closed",
      d: `M ${x1} ${from.y} C ${x1 + bend} ${from.y}, ${x2 - bend} ${to.y}, ${x2} ${to.y}`,
    });
  }

  const counts = Array.from({ length: layers }, () => 0);
  for (const placed of byDoc.values()) counts[placed.layer] = (counts[placed.layer] ?? 0) + 1;

  return {
    bands,
    byDoc,
    edges,
    unplaced,
    loops: readableLoops(geometry, visible),
    layers,
    counts,
    criticalCount: [...byDoc.values()].filter((placed) => placed.critical).length,
    width: PAD_LEFT + Math.max(layers, 1) * COL + GUTTER,
    height: Math.max(PAD_TOP + SLOT, bands.length === 0 ? 0 : top - BAND_GAP + PAD_TOP),
  };
}

/**
 * Give every node in one track a slot, and answer the deepest one used.
 *
 * The rule is *continue your blocker*. A node prefers the slot of the blocker
 * it continues, so an unbranched chain comes out as one horizontal line; when
 * two nodes want the same slot the one with more work behind it keeps it and
 * the other drops to the next free one, which is what makes a fork look like a
 * fork.
 *
 * Where a node has several blockers the preference is their **median** slot,
 * not the first or the topmost. Median is the standard barycentre heuristic for
 * exactly this problem: it puts a convergence between the things converging on
 * it instead of pinning it to whichever blocker happened to sort first, and it
 * is what stops six wires crossing each other to reach one node.
 *
 * This is one forward sweep, not full Sugiyama — no backward pass, no iterated
 * crossing count. It is O(V + E) and it is right for the shape most projects
 * are: mostly trees, converging occasionally. A pathological graph will draw
 * more crossings than an iterated solver would, and it will draw them in the
 * same place every time, which is the property that actually matters here.
 */
function assignSlots(
  nodes: readonly GeometryNode[],
  blockedBy: ReadonlyMap<string, string[]>,
  onward: ReadonlyMap<string, number>,
  slotOf: Map<string, number>,
): number {
  const byLayer = new Map<number, GeometryNode[]>();
  for (const node of nodes) {
    byLayer.set(node.layer!, [...(byLayer.get(node.layer!) ?? []), node]);
  }

  let deepest = 0;
  for (const layer of [...byLayer.keys()].sort((a, b) => a - b)) {
    const group = byLayer.get(layer)!;
    const preference = new Map<string, number>();
    for (const node of group) {
      const slots = (blockedBy.get(node.row.doc_id) ?? [])
        .map((blocker) => slotOf.get(blocker))
        .filter((slot): slot is number => slot !== undefined)
        .sort((a, b) => a - b);
      // The lower median, so a node with two blockers sits level with the upper
      // one rather than in the gap between them — a slot is a discrete row and
      // there is nothing to occupy half of one.
      preference.set(
        node.row.doc_id,
        slots.length === 0 ? NO_PREFERENCE : slots[(slots.length - 1) >> 1]!,
      );
    }

    const ordered = [...group].sort((a, b) => {
      const left = preference.get(a.row.doc_id)!;
      const right = preference.get(b.row.doc_id)!;
      return (
        left - right ||
        // The trunk before the twig: whichever of them has more work behind it
        // is the one whose chain a reader is following.
        (onward.get(b.row.doc_id) ?? 0) - (onward.get(a.row.doc_id) ?? 0) ||
        a.ordinal - b.ordinal ||
        a.row.doc_id.localeCompare(b.row.doc_id)
      );
    });

    const taken = new Set<number>();
    for (const node of ordered) {
      const wanted = preference.get(node.row.doc_id)!;
      // Downward only. Searching both ways would let a fork jump *above* the
      // chain it came off, which reads as two unrelated lines rather than as
      // one line shedding a branch.
      let slot = wanted === NO_PREFERENCE ? 0 : wanted;
      while (taken.has(slot)) slot += 1;
      taken.add(slot);
      slotOf.set(node.row.doc_id, slot);
      deepest = Math.max(deepest, slot);
    }
  }
  return deepest;
}

function pillWidth(node: GeometryNode, largest: number): number {
  const key = node.row.key_alias ?? node.row.reff;
  const label = key.length * CHAR + PILL_PAD * 2 + DOT + DOT_GAP;
  const estimate = node.row.estimate == null
    ? 0
    : (EST_BONUS * Math.min(node.row.estimate, largest)) / largest;
  return Math.min(PILL_MAX, Math.max(PILL_MIN, label + estimate));
}

/**
 * Each loop as its edges actually run.
 *
 * The engine reports a loop as a strongly-connected component, which is a
 * *set* in Tarjan's finish order — "these four block each other" says nothing
 * anybody can act on. Walking it produces "A blocks B blocks C blocks A", which
 * names the edge to cut. Starting at the lexicographically first member makes
 * the same loop read the same way twice.
 *
 * Kept in the viewer rather than pushed into `geometry.rs` because the ordering
 * exists to make an English sentence, and the engine has no sentence to make.
 */
function readableLoops(geometry: GeometryView, visible: ReadonlySet<string>): string[][] {
  const blocksByDoc = new Map(geometry.nodes.map((node) => [node.row.doc_id, node.blocks]));
  const walked: string[][] = [];
  for (const component of geometry.components) {
    for (const loop of component.loops) {
      // A loop that is entirely filtered out is somebody else's problem. One
      // with a single member on screen is named in full, because a partial
      // explanation of why that member has no column explains nothing.
      if (!loop.some((doc) => visible.has(doc))) continue;
      walked.push(traverseLoop(loop, blocksByDoc));
    }
  }
  return walked;
}

function traverseLoop(
  loop: readonly string[],
  blocksByDoc: ReadonlyMap<string, string[]>,
): string[] {
  const inLoop = new Set(loop);
  const order: string[] = [];
  const seen = new Set<string>();
  let cursor: string | undefined = [...loop].sort()[0];
  while (cursor !== undefined && !seen.has(cursor)) {
    seen.add(cursor);
    order.push(cursor);
    cursor = (blocksByDoc.get(cursor) ?? [])
      .filter((next) => inLoop.has(next))
      .sort()
      .find((next) => !seen.has(next));
  }
  // A component woven from more than one loop will not be exhausted in a single
  // walk. Appending the remainder beats dropping members of the answer.
  for (const doc of [...loop].sort()) if (!seen.has(doc)) order.push(doc);
  return order;
}

/**
 * One node's chain: everything it waits on, everything waiting on it, and how
 * many hops away each of those is.
 *
 * Two separate walks, one up the blockers and one down the dependents, and
 * never a mixture. That is the whole correctness of this function. A single
 * walk that follows both directions at each step does not trace a chain: from
 * `a` it goes up to a blocker and straight back down to every *sibling* sharing
 * that blocker, which is neither something `a` depends on nor something that
 * depends on `a`. In a graph of any density that reaches almost everything.
 */
export function reachFrom(morphology: Morphology, doc: string): Map<string, number> {
  const hops = new Map<string, number>();
  if (!morphology.byDoc.has(doc)) return hops;
  hops.set(doc, 0);
  for (const direction of ["blocked_by", "blocks"] as const) {
    let frontier = [doc];
    let depth = 0;
    while (frontier.length > 0) {
      depth += 1;
      const next: string[] = [];
      for (const at of frontier) {
        for (const other of morphology.byDoc.get(at)?.node[direction] ?? []) {
          if (hops.has(other) || !morphology.byDoc.has(other)) continue;
          hops.set(other, depth);
          next.push(other);
        }
      }
      frontier = next;
    }
  }
  return hops;
}
