import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ArrowUpRight, GitBranch, TriangleAlert } from "lucide-react";

import type {
  BoardView,
  GeometryNode,
  GeometryView,
  MilestoneDto,
  ProjectDto,
  WorkflowState,
} from "../types";
import {
  layoutMorphology,
  reachFrom,
  BAND_GAP,
  COL,
  PAD_LEFT,
  PILL_H,
  type Morphology,
  type PlacedNode,
} from "../core/morphology";
import { EmptyState } from "./AppState";
import { catalogColor } from "./colors";
import { cn } from "./primitives";
import { dueLabel } from "./time";
import { indexBy } from "../core/performance";

/**
 * The project's morphology — the dependency graph, drawn in two dimensions.
 *
 * **x is the dependency layer** and reads left to right: column 0 is what could
 * start this morning, column N waits on column N-1. **y is a slot**, and a node
 * takes the slot of the blocker it continues — so an unbranched chain is a
 * straight horizontal run, a fork visibly forks, and six things converging on
 * one issue visibly converge. Disconnected patches of the project get their own
 * horizontal track, biggest first, so unrelated work never interleaves.
 *
 * This replaced a list. The old view was one row per issue with x for depth,
 * which is one and a half dimensions: the vertical axis was spent on reading
 * order, and reading order is the one thing the eye reconstructs for free. The
 * rail of titles down the left went with it — forty titles beside a diagram is
 * a list with a chart attached, and the words win. A node carries its key; the
 * title is on hover, and on the chip in the legend once you have picked one.
 *
 * **Nothing here decides where a node goes.** `layer`, `component`, `slack` and
 * `ordinal` all come from `products/issues/src/geometry.rs`, which compiles them
 * at one World generation; `core/morphology.ts` turns them into coordinates.
 * That division is the engine's own: a layout may bend the phenotype into an
 * organic view but may not invent a node, edge, position, or gap. It is also
 * why this file no longer reaches for `project_graph` — geometry arrives with
 * the graph already solved, which deleted the viewer's second copy of Kahn and
 * Tarjan along with it.
 *
 * What a walk through the running list view established, and this inherits:
 *
 * - *Faint has to still be a mark.* The resting weave and the lit chain both
 *   clear 3:1, and the pair does not invert between themes.
 * - *Every dependency is drawn, all the time.* Structure is what the view
 *   exists to show, and a chart that reveals it only under the cursor has made
 *   it a lookup rather than a picture.
 * - *Selection is ink, hue is structure.* The chain you picked is `fg`; nothing
 *   competes with it in accent.
 * - *A pin outranks hover.* The cursor sits over the chart while the wheel
 *   turns, so a hover-only highlight is lost the moment you scroll to follow
 *   it. Click pins, Escape releases, and the pin is what lets the highlight be
 *   quiet enough that nothing else has to be dimmed.
 *
 * The stack, since four things overlap and the order is load-bearing: track
 * rules at the bottom, then the wires at `z-10`, then the pills at `z-20`, then
 * the scale over all of it at `z-40`. Every layer is numbered rather than
 * relying on DOM order — that was got wrong twice in the old view, in both
 * directions.
 *
 * Nothing is measured except the scroll port, and that only to know whether the
 * track has run past the window. Layers, slots and pill widths are arithmetic,
 * so a connector is never drawn against a layout that has since moved.
 */

/** How long the cursor must rest on a node before it means it. Without this the
 *  chart answers a pointer that is only crossing it on the way to the filter. */
const HOVER_INTENT_MS = 90;
/**
 * The chain reveal's two numbers, and the relationship between them is the
 * whole feel of it.
 *
 * `HOP_MS` is the stagger between one hop and the next; `DRAW_MS` is how long a
 * single connector takes to draw. At 130/200 the stagger was most of the draw,
 * so the reveal arrived as a series of discrete steps you could count. At 70/460
 * each hop starts while the one before it is barely a sixth of the way through,
 * and eleven overlapping draws read as one wave crossing the chart.
 */
const HOP_MS = 70;
const DRAW_MS = 460;
/** How long a line takes to change layers. Longer than the draw, because
 *  leaving should be the quieter half of the gesture. */
const FADE_MS = 560;

/**
 * How far a connector is from the focused node.
 *
 * `chain` — both ends on it. `adjacent` — one end, so it is one step from what
 * was asked about. `far` — touching neither. With no focus every wire is
 * `adjacent`, which is the resting chart.
 */
type Depth = "chain" | "adjacent" | "far";

export function ProjectTimeline({
  board,
  geometry,
  milestones,
  project,
  filtered,
  selection,
  onSelect,
}: {
  /** The rows that survived the filter. Only their doc ids are read: the chart
   *  is drawn from geometry, and this says which of it to draw. */
  board: BoardView;
  /**
   * The project's compiled morphology, at one World generation.
   *
   * Always the whole project even when a filter is on — how many rounds of work
   * a project takes is a fact about the project, and a control that is supposed
   * to change what is on screen must not quietly rewrite it. `null` while the
   * first request is in flight.
   */
  geometry: GeometryView | null;
  /** For naming a milestone on the focused chip. The chart itself does not draw
   *  milestones: a lane per milestone was considered and is a different view. */
  milestones: MilestoneDto[];
  project: ProjectDto;
  /** A filter is on, so the chart is a window onto the morphology, not all of it. */
  filtered: boolean;
  /** The open issue, by ref. */
  selection: string | null;
  onSelect: (reff: string) => void;
}) {
  const states: WorkflowState[] = useMemo(
    () => board.columns.map((column) => column.state),
    [board.columns],
  );
  const visible = useMemo(
    () =>
      new Set(
        board.columns.flatMap((c) => c.rows).filter((r) => !r.tombstone).map((r) => r.doc_id),
      ),
    [board.columns],
  );

  if (geometry === null) {
    return (
      <p className="text-mute py-12 text-center text-sm">Reading the project&rsquo;s structure…</p>
    );
  }
  if (geometry.nodes.length === 0) {
    return (
      <EmptyState
        icon={<GitBranch className="size-icon-lg" />}
        title={`No issues in ${project.name}`}
        body="This view orders work by what blocks what. Add an issue to start one."
      />
    );
  }
  return (
    <MorphologyChart
      geometry={geometry}
      visible={visible}
      states={states}
      milestones={milestones}
      filtered={filtered}
      selection={selection}
      onSelect={onSelect}
    />
  );
}

function MorphologyChart({
  geometry,
  visible,
  states,
  milestones,
  filtered,
  selection,
  onSelect,
}: {
  geometry: GeometryView;
  visible: ReadonlySet<string>;
  states: WorkflowState[];
  milestones: MilestoneDto[];
  filtered: boolean;
  selection: string | null;
  onSelect: (reff: string) => void;
}) {
  const stateById = useMemo(() => indexBy(states, (s) => s.id), [states]);
  const milestoneById = useMemo(() => indexBy(milestones, (m) => m.id), [milestones]);
  const morphology = useMemo(
    () => layoutMorphology(geometry, visible),
    [geometry, visible],
  );

  /**
   * Two ways to aim the highlight, and `pinned` is the one that matters.
   *
   * Hover alone cannot survive a scroll, so following a chain across a
   * twelve-column project meant losing it the moment you moved — which is also
   * what forced the old dimming: if the highlight is a glimpse, it has to
   * shout. A pinned chain persists, so the highlight can be quiet and nothing
   * needs to be faded to make it legible.
   *
   * A pin outranks hover, and the order matters more than it looks. The other
   * way round reads as "hover previews, releasing returns you to the pin", and
   * it is unusable for the thing pinning is *for*: the cursor sits over the
   * chart while the wheel turns, so every node sliding under it would steal the
   * highlight and you would arrive with something else selected.
   */
  const [pinned, setPinned] = useState<string | null>(null);
  const [hover, setHover] = useState<string | null>(null);
  const focus = pinned ?? hover;
  const chain = useMemo(
    () => (focus === null ? null : reachFrom(morphology, focus)),
    [focus, morphology],
  );
  const focused = focus === null ? null : (morphology.byDoc.get(focus) ?? null);
  const togglePin = useCallback(
    (doc: string) => setPinned((current) => (current === doc ? null : doc)),
    [],
  );

  /** Hover, but only once the cursor has stayed. See `HOVER_INTENT_MS`. */
  const intent = useRef<ReturnType<typeof setTimeout> | null>(null);
  const clearIntent = () => {
    if (intent.current !== null) clearTimeout(intent.current);
    intent.current = null;
  };
  /**
   * Where the current highlight came from.
   *
   * A *pointer* hover has to be dropped on scroll: the cursor is no longer
   * meaningfully on anything. A *keyboard* one must not be — the focus ring
   * travels with the element, arrow-key navigation scrolls its target into view
   * on purpose, and clearing on scroll would cancel the preview of the very
   * node it just moved to.
   */
  const fromKeyboard = useRef(false);
  const armHover = useCallback((doc: string) => {
    clearIntent();
    intent.current = setTimeout(() => {
      fromKeyboard.current = false;
      setHover(doc);
    }, HOVER_INTENT_MS);
  }, []);
  const dropHover = useCallback(() => {
    clearIntent();
    if (!fromKeyboard.current) setHover(null);
  }, []);
  /** Keyboard focus is a statement of intent already; it does not wait. */
  const setHoverNow = useCallback((doc: string | null) => {
    clearIntent();
    fromKeyboard.current = doc !== null;
    setHover(doc);
  }, []);
  useEffect(() => clearIntent, []);

  useEffect(() => {
    if (pinned === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPinned(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [pinned]);

  /** The scroll port. Read for its scroll offset and nothing else. */
  const port = useRef<HTMLDivElement | null>(null);
  const [edges, setEdges] = useState({ left: false, right: false });
  const onScroll = useCallback(() => {
    const node = port.current;
    if (!node) return;
    const left = node.scrollLeft > 0;
    const right = node.scrollLeft + node.clientWidth < node.scrollWidth - 1;
    setEdges((current) =>
      current.left === left && current.right === right ? current : { left, right },
    );
  }, []);
  /**
   * Measure on attach, via a callback ref rather than a layout effect.
   *
   * The effect version has a hole in it: this component returns an empty state
   * on some first renders, so a `useLayoutEffect(…, [])` fires once against a
   * null ref and is never invited back. A callback ref runs when the node
   * attaches, whenever that turns out to be.
   */
  const observer = useRef<ResizeObserver | null>(null);
  const attachPort = useCallback(
    (node: HTMLDivElement | null) => {
      observer.current?.disconnect();
      observer.current = null;
      port.current = node;
      if (!node) return;
      onScroll();
      observer.current = new ResizeObserver(onScroll);
      observer.current.observe(node);
    },
    [onScroll],
  );
  useLayoutEffect(onScroll, [onScroll, morphology]);

  /**
   * Walk the graph with the arrow keys.
   *
   * Left and right follow the dependency — to a blocker, to something this
   * blocks — which is the move the whole view is about. Up and down move within
   * the column, which only became a meaningful direction when the layout
   * stopped being one row per issue.
   */
  const step = useCallback(
    (from: PlacedNode, key: string): boolean => {
      let target: string | undefined;
      if (key === "ArrowLeft" || key === "ArrowRight") {
        const candidates =
          key === "ArrowLeft" ? from.node.blocked_by : from.node.blocks;
        target = candidates.find((doc) => morphology.byDoc.has(doc));
      } else {
        const delta = key === "ArrowUp" ? -1 : 1;
        target = [...morphology.byDoc.values()]
          .filter((p) => p.band === from.band && p.layer === from.layer)
          .sort((a, b) => a.slot - b.slot)
          .find((p) => (delta < 0 ? p.slot < from.slot : p.slot > from.slot))
          ?.node.row.doc_id;
        if (delta < 0) {
          target = [...morphology.byDoc.values()]
            .filter((p) => p.band === from.band && p.layer === from.layer && p.slot < from.slot)
            .sort((a, b) => b.slot - a.slot)[0]
            ?.node.row.doc_id;
        }
      }
      if (target === undefined) return false;
      const node = port.current?.querySelector<HTMLElement>(`[data-node="${target}"]`);
      if (!node) return false;
      node.focus();
      node.scrollIntoView({ block: "nearest", inline: "nearest" });
      return true;
    },
    [morphology],
  );

  const drawn = morphology.byDoc.size;
  const tangledOnScreen = morphology.unplaced.length > 0;
  if (drawn === 0 && !tangledOnScreen) {
    return (
      <EmptyState
        kind="filtered-empty"
        title="No issues match the filter"
        body="The morphology is still there — nothing that survived the filter is on it."
      />
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Legend
        geometry={geometry}
        morphology={morphology}
        focused={focused}
        milestone={
          focused?.node.row.milestone
            ? milestoneById.get(focused.node.row.milestone)?.name
            : undefined
        }
        shown={drawn}
        filtered={filtered}
        onOpen={onSelect}
      />
      <div className="relative min-h-0 flex-1">
        <div ref={attachPort} onScroll={onScroll} className="h-full overflow-auto">
          <div className="relative" style={{ width: morphology.width, minWidth: "100%" }}>
            <LayerScale counts={morphology.counts} />
            <div className="relative" style={{ height: morphology.height }}>
              <TrackRules morphology={morphology} />
              <Wires morphology={morphology} chain={chain} />
              {[...morphology.byDoc.values()].map((placed) => (
                <NodePill
                  key={placed.node.row.doc_id}
                  placed={placed}
                  state={stateById.get(placed.node.row.status)}
                  morphology={morphology}
                  hop={chain?.get(placed.node.row.doc_id)}
                  pinned={pinned === placed.node.row.doc_id}
                  open={selection !== null && selection === placed.node.row.reff}
                  onPin={togglePin}
                  onHover={armHover}
                  onLeave={dropHover}
                  onFocusHover={setHoverNow}
                  onStep={step}
                />
              ))}
            </div>
            {morphology.layers <= 1 && <NoDependenciesNote />}
            <TangleNote morphology={morphology} stateById={stateById} onSelect={onSelect} />
          </div>
        </div>
        {/* The chart runs past the window long before a project is unusual, and
            nothing said so. A fade is the cheapest true statement that there is
            more of it than there is window. */}
        {edges.right && (
          <div className="from-bg pointer-events-none absolute inset-y-0 right-0 w-8 bg-gradient-to-l to-transparent" />
        )}
      </div>
    </div>
  );
}

/**
 * What the chart is claiming, what its marks mean, and which node you picked.
 *
 * The right-hand chip is where the titles went. Printing forty of them down the
 * side of a diagram made a list with a chart attached; printing *one* — the one
 * you asked about — costs no vertical space and answers the only question the
 * keys cannot. It is also the affordance that opens the issue, since clicking a
 * node pins its chain rather than navigating away from the chart.
 */
function Legend({
  geometry,
  morphology,
  focused,
  milestone,
  shown,
  filtered,
  onOpen,
}: {
  geometry: GeometryView;
  morphology: Morphology;
  focused: PlacedNode | null;
  milestone: string | undefined;
  shown: number;
  filtered: boolean;
  onOpen: (reff: string) => void;
}) {
  const tangled = geometry.closure.cyclic + geometry.closure.stalled;
  const conflicts = geometry.residuals.filter((r) => r.kind === "due_order_conflict").length;
  const tracks = morphology.bands.length;
  return (
    <div className="border-line/70 text-mute flex h-bar-md shrink-0 items-center gap-3 border-b px-4 text-xs">
      <span>
        <span className="text-fg font-medium">{geometry.closure.ready}</span> ready to start
      </span>
      <span>
        <span className="text-fg font-medium">{morphology.layers}</span> round
        {morphology.layers === 1 ? "" : "s"} of work
      </span>
      {/* New, and only sayable now that the layout reads `component`: a project
          in four disconnected pieces is a different thing from a project with
          one plan, and the old chart drew both as one staircase. */}
      {tracks > 1 && (
        <span>
          <span className="text-fg font-medium">{tracks}</span> tracks
        </span>
      )}
      {morphology.criticalCount > 0 && (
        <span>
          <span className="text-fg font-medium">{morphology.criticalCount}</span> can&rsquo;t slip
        </span>
      )}
      {conflicts > 0 && (
        <span className="text-warn flex items-center gap-1">
          <TriangleAlert className="size-icon-xs" />
          {conflicts} due before a blocker
        </span>
      )}
      {tangled > 0 && (
        <span className="text-warn flex items-center gap-1">
          <TriangleAlert className="size-icon-xs" />
          {tangled} out of the order
        </span>
      )}
      {filtered && (
        <span className="text-dim">
          showing <span className="font-medium">{shown}</span> of{" "}
          <span className="font-medium">{geometry.closure.total}</span>
        </span>
      )}
      {focused && (
        <button
          type="button"
          onClick={() => onOpen(focused.node.row.reff)}
          className="hover:bg-hover ml-auto flex min-w-0 items-center gap-2 rounded-control px-2 py-0.5"
          title={`Open ${focused.node.row.title}`}
        >
          <span className="text-mute shrink-0 font-mono text-2xs">
            {focused.node.row.key_alias ?? focused.node.row.reff}
          </span>
          <span className="text-fg min-w-0 truncate">{focused.node.row.title}</span>
          {milestone && <span className="text-mute shrink-0 text-2xs">{milestone}</span>}
          <span className="text-mute shrink-0 text-2xs tabular-nums">
            {focused.critical ? "no slack" : `+${focused.node.slack ?? 0}`}
          </span>
          <ArrowUpRight className="text-mute size-icon-xs shrink-0" aria-hidden />
        </button>
      )}
    </div>
  );
}

/**
 * The scale along the top — the only chrome the columns get.
 *
 * Column 0 is named rather than numbered. "Ready" is what it means, and it is
 * the column a person is looking for.
 *
 * The count sits *under* the label, on the same line as the bar measuring it.
 * Side by side they read as one number — "1 7", "2 7", "3 5" across the top of
 * the chart — which is the kind of defect that survives review because everyone
 * reading the code already knows which is which.
 */
function LayerScale({ counts }: { counts: readonly number[] }) {
  const busiest = Math.max(1, ...counts);
  return (
    <div
      className="bg-bg border-line/70 sticky top-0 z-40 flex h-bar-md items-end border-b"
      style={{ paddingLeft: PAD_LEFT }}
    >
      {counts.map((count, layer) => (
        <div key={layer} className="shrink-0 pb-1.5" style={{ width: COL }}>
          <div className={cn("text-2xs", layer === 0 ? "text-dim font-medium" : "text-mute")}>
            {layer === 0 ? "Ready" : layer}
          </div>
          <div className="mt-0.5 flex items-center gap-1.5">
            <div
              className={cn("h-0.5 rounded-full", layer === 0 ? "bg-dim" : "bg-line-strong")}
              style={{ width: `${Math.max(4, (count / busiest) * (COL - 40))}px` }}
            />
            <span className="text-mute text-2xs tabular-nums opacity-70">{count}</span>
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * The line between one track and the next.
 *
 * Only drawn when there is more than one, because a single rule above a single
 * track is a box around the whole chart claiming to be a distinction. The label
 * is deliberately weak: a track is not a thing anybody named, it is what the
 * dependency graph happens to have fallen into, and labelling it "Track 2" in
 * the same weight as a milestone would imply somebody decided it.
 */
function TrackRules({ morphology }: { morphology: Morphology }) {
  if (morphology.bands.length < 2) return null;
  return (
    <>
      {morphology.bands.slice(1).map((band) => (
        <div
          key={band.id}
          className="border-line/50 absolute inset-x-0 flex items-center gap-2 border-t"
          style={{ top: band.top - BAND_GAP / 2 }}
        >
          <span className="bg-bg text-mute -mt-px pr-2 text-2xs" style={{ marginLeft: PAD_LEFT }}>
            track {band.ordinal}
          </span>
        </div>
      ))}
    </>
  );
}

/**
 * Preview a chain on focus, but only when the focus came from the keyboard.
 *
 * A plain `onFocus` leaves a highlight standing after any mouse click: the
 * button keeps focus, the chain stays lit, and nothing on screen explains why.
 * `:focus-visible` is the browser's own answer to "did they mean to focus
 * this", so a tabbing reader gets the preview and a clicking one does not get a
 * highlight they did not ask for.
 */
function focusPreview(doc: string, onHover: (doc: string | null) => void) {
  return (event: React.FocusEvent<HTMLElement>) => {
    if (event.currentTarget.matches(":focus-visible")) onHover(doc);
  };
}

/**
 * One issue, as a node.
 *
 * It carries its key, because in a 2D field there is no rail to say which row
 * is which and an anonymous mark in a graph of eighty is unfindable. Width
 * still carries the estimate — that was the old bar's one honest second channel
 * and it survives the move.
 *
 * The zero-slack underline is a reversal, and the argument it reverses was
 * right at the time. In the list, painting "can't slip" onto marks produced ten
 * scattered highlights, no one of which told you to do anything, so it was
 * pulled back to a sentence in the legend. In two dimensions the same set is
 * not scattered: it is a contiguous run from the left edge to the right, and
 * seeing *where* the project's constraint physically runs is the thing this
 * layout can show that the list could not. It is a neutral hairline rather than
 * a hue, so it does not compete with the chain.
 */
function NodePill({
  placed,
  state,
  morphology,
  hop,
  pinned,
  open,
  onPin,
  onHover,
  onLeave,
  onFocusHover,
  onStep,
}: {
  placed: PlacedNode;
  state: WorkflowState | undefined;
  morphology: Morphology;
  /** Hops from the focused node, or `undefined` if it is not on the chain. */
  hop: number | undefined;
  pinned: boolean;
  /** The issue open in the detail view. */
  open: boolean;
  onPin: (doc: string) => void;
  onHover: (doc: string) => void;
  onLeave: () => void;
  onFocusHover: (doc: string | null) => void;
  onStep: (from: PlacedNode, key: string) => boolean;
}) {
  const { row } = placed.node;
  const stage = state?.category ?? "backlog";
  const done = stage === "done";
  const conflict = placed.conflicts.length > 0;
  const sentence = conflict ? conflictSentence(placed, morphology) : row.title;
  return (
    <button
      type="button"
      data-node={row.doc_id}
      aria-pressed={pinned}
      aria-label={`${row.key_alias ?? row.reff}: ${row.title}. Show what it blocks and waits on`}
      title={sentence}
      onClick={() => onPin(row.doc_id)}
      onMouseEnter={() => onHover(row.doc_id)}
      onMouseLeave={onLeave}
      onFocus={focusPreview(row.doc_id, onFocusHover)}
      onBlur={() => onFocusHover(null)}
      onKeyDown={(event) => {
        if (!event.key.startsWith("Arrow")) return;
        if (onStep(placed, event.key)) event.preventDefault();
      }}
      className={cn(
        "absolute z-20 flex items-center gap-1.5 overflow-hidden rounded-control border px-2",
        "transition-[background-color,border-color,color]",
        // The resting node. A chip, not a bar: it holds a label now, and a
        // solid fill with text knocked out of it would need a second colour
        // ramp for the text at every state this thing can be in.
        "bg-raised border-line text-dim",
        stage === "active" && "text-fg",
        done && "bg-sunken text-mute",
        // The chain lights the node, and distance is carried by *when* it
        // lights — a deep chain reads as a wave travelling outward rather than
        // as a blob that appeared at once.
        //
        // The node you picked takes a lighter edge. It was `line-strong`,
        // which is the same value the zero-slack underline uses — so selection
        // and "can't slip" were drawn identically, and on a node that was both
        // there was nothing to tell you which you were looking at.
        //
        // `dim` rather than `fg`. Ink was the first answer and it was a step
        // too far: at 93.5% against a 31% resting edge the selected node stops
        // reading as a node with an emphasised border and starts reading as a
        // different kind of object, which is loud in a picture whose entire
        // vocabulary is hairlines. `dim` still clears the resting edge by most
        // of the neutral ramp and clears `line-strong` by enough that the two
        // marks are never confusable.
        //
        // A rung either way, never a hand-picked value: every one of these is
        // a `light-dark()` pair, so a literal that looked right in dark would
        // invert its relationship to `line` in light.
        hop === 0 && "bg-active border-dim text-bright",
        hop !== undefined && hop > 0 && "bg-hover text-fg",
        // The one exception that gets a hue. A contradiction between two dates
        // a person set is a fact about this work, so the work says so.
        conflict && "border-warn text-warn",
        // Ink for the issue that is actually open, so it is distinguishable
        // from the chain it is at the head of.
        open && "border-fg",
        // Zero slack, as the bottom edge only. Every border on this node is
        // 1px — the chart is flat and hairline throughout, and a 2px edge on
        // 68 nodes was the one thing on it drawn at a different weight.
        //
        // Width was tempting because it survives a colour-blind reader and a
        // monochrome print, and it is still wrong here: it would be the only
        // heavy line in a picture whose whole vocabulary is hairlines and
        // whose subject — the wires — has to stay the boldest thing on screen.
        // Holding one weight also removes the resize question the 2px version
        // had to be careful about, since nothing about the box moves any more.
        //
        // Suppressed on the selected node, and this is not a detail: Tailwind
        // emits the one-sided rule after the all-sided one, so `border-b-*`
        // beats `border-fg` whatever order they are written in here. A picked
        // node would come out with ink on three edges and a dark hairline
        // along the bottom, which reads as a rendering fault rather than as
        // two facts. Selection owns the whole border while it holds; the slack
        // mark is a resting-state signal and comes back when you let go.
        placed.critical && hop !== 0 && "border-b-line-strong",
      )}
      style={{
        left: placed.x,
        top: placed.y - PILL_H / 2,
        width: placed.width,
        height: PILL_H,
        transitionDuration: `${FADE_MS}ms`,
        transitionDelay: hop ? `${hop * HOP_MS}ms` : undefined,
      }}
    >
      {state && (
        <span
          aria-hidden
          className="size-mark-xs shrink-0 rounded-full"
          style={{ background: catalogColor(state.color) }}
        />
      )}
      <span className={cn("truncate font-mono text-2xs", done && "line-through")}>
        {row.key_alias ?? row.reff}
      </span>
    </button>
  );
}

/**
 * The contradiction, with both dates in it.
 *
 * "1 due before a blocker" told you a contradiction existed and then refused to
 * say between what, which makes it an alarm rather than a finding. Naming the
 * blocker and printing the two dates is the whole difference between the two.
 */
function conflictSentence(placed: PlacedNode, morphology: Morphology): string {
  const due = placed.node.row.due_date;
  const first = placed.conflicts[0];
  const blocker = first === undefined ? undefined : morphology.byDoc.get(first);
  if (due == null || !blocker || blocker.node.row.due_date == null) {
    return "Due no later than something that must precede it";
  }
  const key = blocker.node.row.key_alias ?? blocker.node.row.reff;
  const more = placed.conflicts.length - 1;
  return (
    `Due ${dueLabel(due)}, but ${key} must finish first and is due ${dueLabel(blocker.node.row.due_date)}` +
    (more > 0 ? ` (and ${more} more)` : "")
  );
}

/**
 * The state most projects are actually in.
 *
 * A project with no `blocks` edges draws one column of nodes and an empty
 * window — technically the honest picture, and useless: it teaches nothing and
 * does not say what would make it into a chart. It draws the column still,
 * because those are real issues, and puts the missing sentence in the space the
 * chart is not using.
 */
function NoDependenciesNote() {
  return (
    <div
      className="border-line bg-raised text-dim absolute top-20 max-w-sm rounded-surface border p-3 text-xs leading-5"
      style={{ left: PAD_LEFT + COL + 32 }}
    >
      <div className="text-fg mb-1 font-medium">Nothing blocks anything yet</div>
      This view arranges a project by what has to happen first, so with no
      dependencies it is one column: everything could start today. Open an issue
      and add a <span className="text-fg">Blocked by</span> link to give it a
      shape.
    </div>
  );
}

/**
 * Issues with no place in the order.
 *
 * Two different problems, and reporting them as one is a false statement. A
 * loop has no depth, so nothing in it can be placed; but everything
 * *downstream* of a loop also fails to resolve, and those are ordinary issues
 * waiting behind somebody else's mistake. Calling them "issues that block each
 * other" is wrong about the one row whose owner would go looking.
 *
 * `blocks` edges have no CRDT preventing a cycle — the sub-issue tree has one,
 * and this is not that tree — so this is reachable in normal use rather than a
 * corruption state. A note, not a panel.
 */
function TangleNote({
  morphology,
  stateById,
  onSelect,
}: {
  morphology: Morphology;
  stateById: ReadonlyMap<string, WorkflowState>;
  onSelect: (reff: string) => void;
}) {
  const nameOf = (doc: string) => {
    const placed = morphology.byDoc.get(doc)?.node;
    const loose = morphology.unplaced.find((node) => node.row.doc_id === doc);
    const row = (placed ?? loose)?.row;
    return row ? (row.key_alias ?? row.reff) : doc;
  };
  const cyclic = morphology.unplaced.filter((node) => node.closure === "cycle");
  const stalled = morphology.unplaced.filter((node) => node.closure === "stalled");
  if (morphology.loops.length === 0 && cyclic.length === 0 && stalled.length === 0) return null;
  return (
    <div className="border-line/70 mt-4 border-t px-4 py-2">
      {morphology.loops.map((loop, i) => (
        <div key={i} className="text-warn mb-1 flex items-center gap-1.5 text-2xs">
          <TriangleAlert className="size-icon-xs shrink-0" />
          {/* The edge to cut, named. "N issues block each other" is a count; a
              walk is an instruction. */}
          <span className="min-w-0 truncate">
            {[...loop, loop[0] ?? ""].map(nameOf).join(" blocks ")} — remove one of these links
            to place them
          </span>
        </div>
      ))}
      {cyclic.map((node) => (
        <TangleRow key={node.row.doc_id} node={node} stateById={stateById} onSelect={onSelect} />
      ))}
      {stalled.length > 0 && (
        <div className="text-mute mt-1 mb-1 text-2xs">
          Waiting behind {morphology.loops.length === 1 ? "that loop" : "a loop"}, so
          {stalled.length === 1 ? " it has" : " they have"} no place in the order either:
        </div>
      )}
      {stalled.map((node) => (
        <TangleRow key={node.row.doc_id} node={node} stateById={stateById} onSelect={onSelect} />
      ))}
    </div>
  );
}

function TangleRow({
  node,
  stateById,
  onSelect,
}: {
  node: GeometryNode;
  stateById: ReadonlyMap<string, WorkflowState>;
  onSelect: (reff: string) => void;
}) {
  const state = stateById.get(node.row.status);
  return (
    <button
      type="button"
      onClick={() => onSelect(node.row.reff)}
      className="hover:bg-hover flex h-ctl-md w-full items-center gap-2 rounded-control px-1 text-left"
    >
      {state && (
        <span
          aria-hidden
          className="size-mark-xs shrink-0 rounded-full"
          style={{ background: catalogColor(state.color) }}
        />
      )}
      <span className="text-mute shrink-0 font-mono text-2xs tabular-nums">
        {node.row.key_alias ?? node.row.reff}
      </span>
      <span className="text-fg truncate text-xs">{node.row.title}</span>
    </button>
  );
}

/**
 * The lines between nodes.
 *
 * Every constraint runs to a strictly higher layer, so every wire runs left to
 * right and a plain cubic is enough — no routing, no arrowheads, no measuring.
 *
 * Two layers, and the same wires drawn in both. SVG has no z-index — a path's
 * depth *is* its position in the document — so the obvious way to put a lit
 * chain in front of the grey is to sort the paths before rendering. That works
 * and it is why the dimming used to snap: re-sorting moves DOM nodes, and
 * moving a node cancels every transition on it. So neither list is ever
 * reordered. The base layer holds every wire at its resting weight; the chain
 * layer sits on top holding the same wires again, invisible except where they
 * are lit. Depth is which *layer* a wire is showing in, which costs one extra
 * path each and buys a DOM that never moves.
 */
function Wires({
  morphology,
  chain,
}: {
  morphology: Morphology;
  chain: ReadonlyMap<string, number> | null;
}) {
  const layered = useMemo(
    () =>
      morphology.edges.map((wire) => {
        const from = chain?.get(wire.from);
        const to = chain?.get(wire.to);
        const depth: Depth =
          chain === null
            ? "adjacent"
            : from !== undefined && to !== undefined
              ? "chain"
              : from !== undefined || to !== undefined
                ? "adjacent"
                : "far";
        return {
          wire,
          depth,
          hop: depth === "chain" ? Math.min(from!, to!) : 0,
          // Which end the reveal arrives at first. Downstream of the focus that
          // is the edge's own source, so the line draws the way it points;
          // upstream it is the target, and the line draws backwards. Both
          // halves then travel away from the node you picked.
          outward: depth === "chain" && to! < from!,
        };
      }),
    [chain, morphology],
  );

  return (
    <>
      <svg
        className="pointer-events-none absolute top-0 left-0 z-10"
        width={morphology.width}
        height={morphology.height}
        aria-hidden
      >
        {layered.map(({ wire, depth }) => (
          <path
            key={wire.key}
            d={wire.d}
            fill="none"
            stroke="currentColor"
            strokeWidth={1}
            strokeDasharray={wire.cleared ? "3 3" : undefined}
            // An edge with one end on the chain is one step from what you asked
            // about and a reader wants it; an edge touching neither end is
            // genuinely elsewhere. Grading those apart is what gives a
            // convergence some air — where six curves land on one node, the two
            // that matter stop adding to the same knot.
            className={cn(
              "transition-[opacity,color] ease-out",
              depth === "far" ? "text-line" : "text-mute",
            )}
            style={{
              opacity: depth === "chain" ? 0 : depth === "adjacent" ? 0.5 : 0.22,
              transitionDuration: `${FADE_MS}ms`,
            }}
          />
        ))}
      </svg>
      <svg
        className="pointer-events-none absolute top-0 left-0 z-10"
        width={morphology.width}
        height={morphology.height}
        aria-hidden
      >
        {layered.map(({ wire, depth, hop, outward }) => (
          <path
            key={wire.key}
            d={wire.d}
            fill="none"
            stroke="currentColor"
            strokeWidth={2}
            // Normalised, so one keyframe draws a curve of any real length and
            // nothing has to be measured back out of the DOM.
            pathLength={1}
            strokeDasharray={1}
            className="text-fg transition-opacity ease-out"
            style={
              depth === "chain"
                ? {
                    transitionDuration: `${FADE_MS}ms`,
                    animation: `${outward ? "lait-chain-draw-back" : "lait-chain-draw"} ${DRAW_MS}ms cubic-bezier(0.22, 1, 0.36, 1) ${hop * HOP_MS}ms both`,
                    // Trailing off on the same curve as the nodes it lights, so
                    // a twelve-hop chain does not end as loudly as it began.
                    opacity: Math.max(0.16, 0.95 - hop * 0.16),
                  }
                : { opacity: 0, transitionDuration: `${FADE_MS}ms` }
            }
          />
        ))}
      </svg>
    </>
  );
}
