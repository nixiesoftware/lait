import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { GitBranch, TriangleAlert } from "lucide-react";

import type {
  BoardView,
  MilestoneDto,
  ProjectDto,
  ProjectGraphView,
  Row,
  WorkflowState,
} from "../types";
import {
  buildSequence,
  reachFrom,
  type SequenceModel,
  type SequenceNode,
} from "../core/sequence";
import { EmptyState } from "./AppState";
import { catalogColor } from "./colors";
import { StatusIcon } from "./icons";
import { cn, interactiveRow } from "./primitives";
import { dueLabel } from "./time";
import { indexBy } from "../core/performance";

/**
 * The project timeline — one row per issue, read left to right.
 *
 * A timeline is a list whose horizontal position means something. That is the
 * whole shape: the rail on the left is the issue, the track on the right is
 * where it sits in the order of work. Anything that puts cards in columns is a
 * board with extra steps, however the columns are labelled — the first attempt
 * at this view learned that the hard way.
 *
 * What the axis means is the one departure from a Gantt. lait has no start date
 * to place a bar on, and Calendar already owns time. It has something a Gantt
 * tool usually bolts on afterwards: a real dependency graph. So **x is
 * sequence** — an issue's column is its dependency depth, the number of things
 * that must finish before it can begin. Column 0 is what could start this
 * morning.
 *
 * Rows are ordered by that same depth, so the bars fall away to the right as
 * you read down: the staircase *is* the plan, and its width is how many rounds
 * of work the project takes. Bar width is the estimate, so the chart carries
 * both when a thing happens and how big it is, in one mark.
 *
 * Every dependency is drawn, all the time, but faintly. The structure of the
 * work is what this view exists to show, and a chart that reveals it only under
 * the cursor has made it a lookup rather than a picture.
 *
 * **What a walk through the running view changed.** Four of these are worth
 * stating because the code that produced them looked correct:
 *
 * - *The weave was not there.* "Faint" was `line-strong` at `opacity .22`,
 *   which against the light ground is about 1.06:1 — below the threshold at
 *   which a mark is a mark. So was the waiting bar, at 1.4:1, and it was the
 *   majority of the chart. Worse, the pair inverted between themes: in light,
 *   ready read heavier than waiting; in dark, waiting read heavier. Both now
 *   come from tokens whose *order* holds in both (`dim` is always nearer the
 *   foreground than `mute`), and both clear 3:1.
 * - *The rail scrolled away.* Ten waves already overflow a laptop window, and
 *   74px of horizontal scroll was enough to cut the status glyph and the key
 *   off every row. The rail is sticky now; it is the one column that must never
 *   leave.
 * - *Nothing decoded the marks.* The legend stated three derived numbers and
 *   not one of the four visual channels carrying them. It carries a key now.
 *   (A hover card answering per-bar in words came and went: at 280px it covered
 *   the chart it was describing, which is a bad trade in a view whose whole
 *   value is the picture. The rail opens the issue, and the issue's Links
 *   section is where the relations belong.)
 * - *Two blues meant two things.* The constrained chain and the chain you
 *   picked were both `accent`, separated only by a ring. Selection is ink
 *   (`fg`) now; hue means "structural" and nothing else.
 * - *The headline said one thing twice.* "Longest chain N" sat beside "N rounds
 *   of work" and the two were the same number by construction. Worse, when
 *   chains tied, which one got drawn came down to doc id. The accent marks
 *   every issue with zero slack now — see `core/sequence.ts`.
 *
 * The stack, since four things overlap and the order is load-bearing: row
 * backgrounds at the bottom, then the wires, then the pills, then the sticky
 * rail and scale over all of it.
 *
 * That order has now been got wrong twice, in both directions. First the wires
 * sat under the backgrounds, which cut every curve into segments wherever it
 * crossed a row. Then the fix left the wires at `z-0` and the rows at `auto` —
 * the same painting level, resolved by DOM order, so a *hovered* row's
 * background came out on top and erased the lines under the one row you were
 * asking about. Every layer is numbered now rather than relying on order: rows
 * are unnumbered ground, wires `z-10`, pills `z-20`, rail `z-30`, scale `z-40`.
 *
 * Only one thing is measured, and it is not row geometry: the width of the
 * scroll port, which decides how much of it the rail may take. Rows are a fixed
 * height and columns a fixed width, so every position is still arithmetic on an
 * index — which is not only faster than reading it back from the DOM, it
 * removes the class of bug where a connector is drawn against a layout that has
 * since moved.
 */

/** Row height and column width. Everything on the chart is placed from these
 *  two numbers, so they are the only things to change to retune its density.
 *
 *  `ROW` is a control rung (`h-ctl-lg`), and the bar inside it is 12px. The two
 *  used to be 30 and 8, a ratio of 0.27 — which is the proportion of an inline
 *  meter in a table row, not of a duration bar on a chart. Every timeline
 *  product measured for this sits between 0.40 and 0.85; at 12/32 this one
 *  finally reads as a bar rather than as a rule. */
const ROW = 32;
const COL = 92;
/** Bar height. Also its minimum width: below its own height a bar stops being a
 *  bar, so the shortest one clamps to a square and reads as a deliberate tile
 *  rather than as a clipped sliver. */
const BAR_H = 16;
/**
 * The rail's range, rather than the single number it used to be.
 *
 * 300 was right for a deep project and wrong for every other one: a project
 * with no dependencies draws a single 92px column and then held titles to 300px
 * with 570px of empty track beside them. The rail takes whatever the track does
 * not need, between a floor that fits a key and a readable title and a ceiling
 * past which a line of text stops being scannable.
 */
const RAIL_MIN = 360;
const RAIL_MAX = 560;
/** A bar with no estimate still has to be visible and still has to read as
 *  "unsized" rather than as "small". */
const BAR_MIN = 26;
const BAR_MAX = COL - 14;
/**
 * How long the cursor must rest on a row before it means it.
 *
 * Without this the chart answered a cursor that was only passing through: the
 * highlight changed while the pointer crossed the chart on its way to the
 * filter menu, and a chain lit up behind an open popover. Short enough to feel
 * immediate when you are actually pointing at something.
 */
const HOVER_INTENT_MS = 90;
/**
 * The chain reveal's two numbers, and the relationship between them is the
 * whole feel of it.
 *
 * `HOP_MS` is the stagger between one hop and the next; `DRAW_MS` is how long a
 * single connector takes to draw. At 130/200 the stagger was most of the draw,
 * so the reveal arrived as a series of discrete steps — you counted them. At
 * 70/460 each hop starts while the one before it is barely a sixth of the way
 * through, and eleven overlapping draws read as one wave crossing the chart
 * rather than as eleven events. Slower and subtler at once, which is not a
 * contradiction: the individual motion lengthened and the thing separating the
 * motions shrank.
 */
const HOP_MS = 70;
const DRAW_MS = 460;
/** How long a line takes to change layers — to dim away, or come back. Longer
 *  than the draw, because leaving should be the quieter half of the gesture. */
const FADE_MS = 560;

/**
 * How far a connector is from the focused issue.
 *
 * `chain` — both ends on it. `adjacent` — one end, so it is one step from what
 * was asked about. `far` — touching neither, and genuinely elsewhere. With no
 * focus every wire is `adjacent`, which is the resting chart.
 */
type Depth = "chain" | "adjacent" | "far";

/** One drawable row, at the y it sits at. */
interface Line {
  node: SequenceNode;
  y: number;
  /** First row at this depth — takes the hairline between rounds. */
  startsWave: boolean;
  /** First row of a milestone run — the only row that prints its name. */
  showMilestone: boolean;
}

/**
 * The sequence chart for one project — the project's Issues → Timeline layout.
 */
export function ProjectTimeline({
  board,
  whole,
  graph,
  milestones,
  project,
  filtered,
  selection,
  onSelect,
}: {
  /** The rows to draw — what survived the filter. */
  board: BoardView;
  /**
   * The whole project, filter or no filter.
   *
   * The sequence is computed from this and only the drawing is scoped, because
   * "how many rounds of work is this project" is a fact about the project. It
   * used to be computed from the filtered rows, so scoping to one milestone
   * quietly rewrote the headline from "10 rounds" to "6" — the number the view
   * exists to state, changed by a control that is supposed to change only what
   * is on screen. Now the columns and the slack hold still and the legend says
   * how much of the project you are looking at.
   */
  whole: BoardView;
  /** The project's whole edge set, from `project_graph`. Absent while it loads —
   *  the chart still draws, as a single column, which is the honest picture of
   *  "no dependencies known yet" and beats a spinner over a usable view. */
  graph: ProjectGraphView | null;
  milestones: MilestoneDto[];
  project: ProjectDto;
  /** A filter is on, so the chart is a window onto the sequence, not all of it. */
  filtered: boolean;
  /** The open issue, by ref — the timeline is a select surface like the list. */
  selection: string | null;
  onSelect: (reff: string) => void;
}) {
  const states: WorkflowState[] = useMemo(
    () => whole.columns.map((c) => c.state),
    [whole.columns],
  );
  const allRows: Row[] = useMemo(
    () => whole.columns.flatMap((c) => c.rows).filter((r) => !r.tombstone),
    [whole.columns],
  );
  const visible = useMemo(
    () =>
      new Set(
        board.columns.flatMap((c) => c.rows).filter((r) => !r.tombstone).map((r) => r.doc_id),
      ),
    [board.columns],
  );
  const model = useMemo(
    () => buildSequence(allRows, graph?.edges ?? [], states),
    [allRows, graph, states],
  );
  return (
    <SequenceChart
      model={model}
      visible={visible}
      milestones={milestones}
      states={states}
      rows={allRows}
      filtered={filtered}
      emptyTitle={`No issues in ${project.name}`}
      selection={selection}
      onSelect={onSelect}
    />
  );
}

function SequenceChart({
  model,
  visible,
  milestones,
  states,
  rows: allRows,
  filtered,
  emptyTitle,
  selection,
  onSelect,
}: {
  model: SequenceModel;
  /** Docs that survived the filter — what to actually draw. */
  visible: ReadonlySet<string>;
  milestones: MilestoneDto[];
  states: WorkflowState[];
  /** Every row in scope, for the estimate scale. */
  rows: Row[];
  filtered: boolean;
  emptyTitle: string;
  selection: string | null;
  onSelect: (reff: string) => void;
}) {
  const stateById = useMemo(() => indexBy(states, (s) => s.id), [states]);
  const milestoneById = useMemo(() => indexBy(milestones, (m) => m.id), [milestones]);
  /**
   * A milestone's dot colour, by its place in its project's own order.
   *
   * Drawn from the cool half of the catalog only. The full ring starts at red
   * and runs through orange, yellow and green, and every one of those already
   * means something on this chart — `warn` is a date contradiction, `ok` is
   * readiness. A first milestone in red and a fourth in green read as "bad" and
   * "good" rather than as "first" and "fourth", which is a status claim about a
   * grouping that has no status. Grey is skipped for the opposite reason: a
   * grey dot is not a group, it is a bullet.
   */
  const milestoneTone = useMemo(() => {
    const hues = ["blue", "purple", "teal", "pink"] as const;
    return new Map(milestones.map((m, i) => [m.id, catalogColor(hues[i % hues.length]!)]));
  }, [milestones]);

  /**
   * Every drawable row, in reading order, with the y each one sits at.
   *
   * Ordered by wave first — that is what makes the bars descend to the right
   * instead of scattering — then by the project's own milestone order, so work
   * that belongs together stays together inside a wave, and finally by ref so
   * the order cannot shuffle between renders.
   */
  const lines = useMemo(() => {
    const rank = new Map(milestones.map((m, i) => [m.id, i]));
    const ordered = [...model.byDoc.values()]
      .filter((n) => !n.cyclic && !n.stalled && visible.has(n.row.doc_id))
      .sort(
        (a, b) =>
          a.wave - b.wave ||
          (rank.get(a.row.milestone ?? "") ?? milestones.length) -
            (rank.get(b.row.milestone ?? "") ?? milestones.length) ||
          a.row.reff.localeCompare(b.row.reff),
      );
    // Two boundaries, at two weights. A wave change is the staircase's own
    // step and takes the heavier rule; a milestone change inside a wave takes a
    // fainter one, which is what recovers the sense of a *run* now that the
    // coloured spine has gone. `showMilestone` is vestigial — the chip prints
    // on every row — but the flag still marks where a run begins.
    return ordered.map((node, i): Line => {
      const before = ordered[i - 1];
      return {
        node,
        y: i * ROW,
        startsWave: before === undefined || before.wave !== node.wave,
        showMilestone:
          node.row.milestone != null &&
          (before === undefined || before.row.milestone !== node.row.milestone),
      };
    });
  }, [milestones, model, visible]);

  const yByDoc = useMemo(
    () => new Map(lines.map((l) => [l.node.row.doc_id, l.y + ROW / 2])),
    [lines],
  );

  /** Bar width is the estimate, scaled against the largest one present. An
   *  unsized issue draws the minimum, which reads as a tick rather than a
   *  claim that it is small. */
  const widthOf = useMemo(() => {
    const largest = Math.max(1, ...allRows.map((r) => r.estimate ?? 0));
    return (estimate: number | null | undefined) =>
      estimate == null ? BAR_MIN : BAR_MIN + ((BAR_MAX - BAR_MIN) * Math.min(estimate, largest)) / largest;
  }, [allRows]);

  /**
   * Two ways to aim the highlight, and `pinned` is the one that matters.
   *
   * Hover alone cannot survive a scroll, so following a chain down a
   * twelve-round project meant losing it the moment you moved — which is also
   * what forced the old dimming: if the highlight is a glimpse, it has to shout.
   * A pinned chain persists, so the highlight can be quiet, and nothing needs to
   * be faded to make it legible.
   *
   * A pin outranks hover, and the order matters more than it looks. The other
   * way round reads as "hover previews, releasing returns you to the pin", and
   * it is unusable for the thing pinning is *for*: scrolling. The cursor sits
   * over the chart while the wheel turns, so every row that slides under it
   * steals the highlight and you arrive with something else selected. Once you
   * have said which chain you care about, nothing takes it back but another
   * click or Escape.
   */
  /** The scroll port. Read for its width and its scroll offset, never for the
   *  position of anything drawn inside it. */
  const port = useRef<HTMLDivElement | null>(null);

  const [pinned, setPinned] = useState<string | null>(null);
  /**
   * What the cursor is on — a doc id, and nothing else.
   *
   * There was a card here: hovering a bar opened a panel naming its blockers,
   * its estimate and its round. It answered real questions and it was the wrong
   * shape for this view — a panel that size sits on top of the chart it is
   * describing, so pointing at anything hid the thing you were pointing at, and
   * the diagram stopped being a diagram. The chart keeps the highlight, which is
   * the part that reads *as* the chart; the relations it used to spell out are
   * in the issue's own Links section, one click away on the rail.
   */
  const [hover, setHover] = useState<string | null>(null);
  const focus = pinned ?? hover;
  /**
   * Everything the focused issue reaches, and how many hops away it is.
   *
   * The whole chain rather than one step, which the earlier version refused for
   * a good reason — everything downstream of a wave-0 issue is most of the
   * project. What makes the full reach readable is that it arrives as a
   * wavefront: each connector draws from its source toward its target and the
   * pill at the far end lights as the line lands. Distance is carried by *when*
   * a mark lights, so twelve hops read as twelve steps rather than as a blob.
   */
  const chain = useMemo(
    () => (focus === null ? null : reachFrom(model, focus)),
    [focus, model],
  );
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
   * Scrolling has to drop a *pointer* hover: the card's geometry is captured
   * when the hover lands, so a wheel turn leaves it describing a row that has
   * moved, and the cursor is no longer meaningfully on anything. It must not
   * drop a *keyboard* one — the focus ring travels with the element, arrow-key
   * navigation scrolls the target into view on purpose, and clearing on scroll
   * would cancel the preview of the very row it just moved to.
   */
  const fromKeyboard = useRef(false);
  const armHover = useCallback(
    (doc: string) => {
      clearIntent();
      intent.current = setTimeout(() => {
        fromKeyboard.current = false;
        setHover(doc);
      }, HOVER_INTENT_MS);
    },
    [setHover],
  );
  const dropHover = useCallback(() => {
    clearIntent();
    if (!fromKeyboard.current) setHover(null);
  }, [setHover]);
  /** Keyboard focus is a statement of intent already; it does not wait. */
  const setHoverNow = useCallback(
    (doc: string | null) => {
      clearIntent();
      fromKeyboard.current = doc !== null;
      setHover(doc);
    },
    [setHover],
  );
  useEffect(() => clearIntent, []);

  useEffect(() => {
    if (pinned === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPinned(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [pinned]);

  /**
   * How wide the rail may be, and whether either edge of the track is cut off.
   *
   * The only measurement in the file. It reads the scroll port, never a row, so
   * no connector is ever drawn against something the DOM has since moved.
   */
  const [portWidth, setPortWidth] = useState(0);
  const [edges, setEdges] = useState({ left: false, right: false });
  /**
   * Measure on attach, via a callback ref rather than a layout effect.
   *
   * The effect version has a hole in it that this chart was only lucky enough
   * to hide: the component returns an empty state on its first render, so a
   * `useLayoutEffect(…, [])` fires once against a null ref and is never invited
   * back, and the port's real width never arrives. The rail happened to clamp
   * to its floor either way here; the roadmap, drawing its whole scale off the
   * same measurement, drew every bar as a stub. A callback ref runs when the
   * node attaches, whenever that turns out to be.
   */
  const observer = useRef<ResizeObserver | null>(null);
  const attachPort = useCallback((node: HTMLDivElement | null) => {
    observer.current?.disconnect();
    observer.current = null;
    port.current = node;
    if (!node) return;
    setPortWidth(node.clientWidth);
    observer.current = new ResizeObserver(([entry]) => {
      setPortWidth(entry?.contentRect.width ?? node.clientWidth);
    });
    observer.current.observe(node);
  }, []);

  const waveCount = model.waves.length;
  const trackWidth = Math.max(waveCount, 1) * COL;
  // No feedback loop: the track's width does not depend on the rail's, so this
  // settles in one pass however the window is dragged.
  const rail = Math.min(RAIL_MAX, Math.max(RAIL_MIN, portWidth - trackWidth));

  const onScroll = useCallback(() => {
    const node = port.current;
    if (!node) return;
    const left = node.scrollLeft > 0;
    const right = node.scrollLeft + node.clientWidth < node.scrollWidth - 1;
    setEdges((current) =>
      current.left === left && current.right === right ? current : { left, right },
    );
  }, []);
  useLayoutEffect(onScroll, [onScroll, portWidth, trackWidth, rail]);

  /**
   * Walk the chain with the arrow keys.
   *
   * The chart had 68 tab stops across it and no way to get from an issue to the
   * thing blocking it — the one move the whole view is about. Left goes to a
   * blocker, right to something this blocks; both land on the bar, so the
   * highlight follows and the answer is on screen.
   */
  const step = useCallback(
    (from: SequenceNode, direction: "back" | "on") => {
      const candidates = direction === "back" ? from.blockedBy : from.blocks;
      const next = candidates.find((doc) => yByDoc.has(doc));
      if (next === undefined) return false;
      const target = port.current?.querySelector<HTMLElement>(`[data-bar="${next}"]`);
      if (!target) return false;
      target.focus();
      target.scrollIntoView({ block: "nearest", inline: "nearest" });
      return true;
    },
    [yByDoc],
  );

  if (allRows.length === 0) {
    return (
      <EmptyState
        icon={<GitBranch className="size-icon-lg" />}
        title={emptyTitle}
        body="The timeline orders work by what blocks what. Add an issue to start one."
      />
    );
  }
  // "Nothing matched" only if nothing matched. An issue in a loop survives a
  // filter like any other; it just has no column to stand in, so it appears
  // below the chart rather than on it — and reporting that as an empty filter
  // would hide the one row the person was looking for.
  const tangledOnScreen = [...model.cyclic, ...model.stalled].some((n) =>
    visible.has(n.row.doc_id),
  );
  if (lines.length === 0 && !tangledOnScreen) {
    return (
      <EmptyState
        kind="filtered-empty"
        title="No issues match the filter"
        body="The sequence is still there — nothing that survived the filter is on it."
      />
    );
  }

  const height = lines.length * ROW;


  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Legend model={model} waveCount={waveCount} shown={lines.length} filtered={filtered} />
      <div className="relative min-h-0 flex-1">
        <div ref={attachPort} onScroll={onScroll} className="h-full overflow-auto">
          <div className="relative" style={{ width: rail + trackWidth, minWidth: "100%" }}>
            <WaveScale
              waveCount={waveCount}
              lines={lines}
              rail={rail}
              scrolled={edges.left}
            />
            <div className="relative" style={{ height }}>
              {/* One layer for every line on the chart, under the rows so a
                  connector never crosses a title, and inert so it never eats a
                  click meant for one. */}
              <Wires
                model={model}
                chain={chain}
                yByDoc={yByDoc}
                widthOf={widthOf}
                height={height}
                rail={rail}
                width={rail + trackWidth}
              />
              {lines.map(({ node, y, startsWave, showMilestone }) => (
                <TimelineRow
                  key={node.row.doc_id}
                  node={node}
                  y={y}
                  rail={rail}
                  scrolled={edges.left}
                  startsWave={startsWave}
                  width={widthOf(node.row.estimate)}
                  state={stateById.get(node.row.status)}
                  model={model}
                  milestone={
                    node.row.milestone ? milestoneById.get(node.row.milestone) : undefined
                  }
                  startsMilestone={showMilestone}
                  tone={node.row.milestone ? milestoneTone.get(node.row.milestone) : undefined}
                  showSlack={model.edges.length > 0}
                  hop={chain?.get(node.row.doc_id)}
                  pinned={pinned === node.row.doc_id}
                  open={selection !== null && selection === node.row.reff}
                  onSelect={onSelect}
                  onPin={togglePin}
                  onHover={armHover}
                  onLeave={dropHover}
                  onFocusHover={setHoverNow}
                  onStep={step}
                />
              ))}
            </div>
            {model.edges.length === 0 && <NoDependenciesNote rail={rail} />}
            <TangleNote
              model={model}
              visible={visible}
              stateById={stateById}
              onSelect={onSelect}
            />
          </div>
        </div>
        {/* The track runs past the window long before a project is unusual — ten
            waves overflow a laptop — and nothing said so. A fade is the cheapest
            true statement that there is more chart than window. */}
        {edges.right && (
          <div className="from-bg pointer-events-none absolute inset-y-0 right-0 w-8 bg-gradient-to-l to-transparent" />
        )}
      </div>
    </div>
  );
}

/**
 * What the chart is claiming, and what its marks mean.
 *
 * The critical path is the headline: its length is the number of waves, so it
 * is the floor on how long this project takes however many people work on it.
 * That is the sentence the view exists to be able to say.
 *
 * The key beside it is the other half, and it was missing entirely. Four
 * channels carry this chart — bar position, bar width, bar colour, line weight
 * — and the legend used to decode none of them, so "why is that one blue" had
 * no answer anywhere on screen. It drops below `lg` because at that width the
 * facts matter more, and every bar still answers on hover.
 */
function Legend({
  model,
  waveCount,
  shown,
  filtered,
}: {
  model: SequenceModel;
  waveCount: number;
  shown: number;
  filtered: boolean;
}) {
  const nodes = [...model.byDoc.values()];
  const ready = nodes.filter((n) => n.ready).length;
  const impossible = nodes.filter((n) => n.impossible).length;
  const tangled = model.cyclic.length + model.stalled.length;
  const cantSlip = model.criticalCount;
  return (
    <div className="border-line/70 text-mute flex h-bar-md shrink-0 items-center gap-3 border-b px-4 text-xs">
      <span>
        <span className="text-fg font-medium">{ready}</span> ready to start
      </span>
      <span>
        <span className="text-fg font-medium">{waveCount}</span> round{waveCount === 1 ? "" : "s"} of work
      </span>
      {/* "Longest chain N" used to sit here beside "N rounds of work", and the
          two were always the same number — the longest chain's length *is* the
          wave count, by construction. One fact under two labels. What was worth
          saying is how much of the project has no room in it. */}
      {/* A count, with no swatch beside it, because there is no longer a mark
          on the chart to explain. Zero slack is a property of the whole graph —
          it flips when an unrelated estimate changes — so it belongs in a
          sentence about the project, not painted onto ten scattered bars. The
          `0`s in the rail's slack column say which ones. */}
      {cantSlip > 0 && (
        <span>
          <span className="text-fg font-medium">{cantSlip}</span> can&rsquo;t slip
        </span>
      )}
      {impossible > 0 && (
        <span className="text-warn flex items-center gap-1">
          <TriangleAlert className="size-icon-xs" />
          {impossible} due before a blocker
        </span>
      )}
      {tangled > 0 && (
        <span className="text-warn flex items-center gap-1">
          <TriangleAlert className="size-icon-xs" />
          {tangled} out of the order
        </span>
      )}
      {/* Said out loud, because the numbers to the left describe the project and
          the chart below describes a slice of it. The filter bar's "N hidden"
          is at the bottom of the window and answers a different question. */}
      {filtered && (
        <span className="text-dim">
          showing <span className="font-medium">{shown}</span> of{" "}
          <span className="font-medium">{nodes.length}</span>
        </span>
      )}
    </div>
  );
}

/**
 * The scale along the top — the only chrome the track gets.
 *
 * Column 0 is named rather than numbered. "Ready" is what it means, and it is
 * the column a person is looking for.
 *
 * The count sits *under* the label, on the same line as the bar that measures
 * it. Side by side they read as one number — "1 7", "2 7", "3 5" down the top
 * of the chart — which is the kind of defect that survives review because
 * everyone reading the code already knows which is which.
 */
function WaveScale({
  waveCount,
  lines,
  rail,
  scrolled,
}: {
  waveCount: number;
  lines: readonly Line[];
  rail: number;
  scrolled: boolean;
}) {
  // The profile: how much work sits in each round. Numbers alone make you read
  // and compare twelve of them; a bar makes the shape of the project — front
  // loaded, or a long thin tail — legible without reading anything. Counted
  // over what is drawn, so it describes the chart under it rather than a
  // project the filter is hiding.
  const counts = useMemo(() => {
    const tally = Array.from({ length: waveCount }, () => 0);
    for (const line of lines) {
      if (line.node.wave < waveCount) tally[line.node.wave] = (tally[line.node.wave] ?? 0) + 1;
    }
    return tally;
  }, [lines, waveCount]);
  const busiest = Math.max(1, ...counts);
  return (
    <div className="bg-bg border-line/70 sticky top-0 z-40 flex h-bar-md items-end border-b">
      <div
        className={cn(
          "bg-bg sticky left-0 z-10 h-full shrink-0",
          scrolled && "shadow-[1px_0_0_var(--color-line)]",
        )}
        style={{ width: rail }}
      />
      {counts.map((count, wave) => (
        <div key={wave} className="shrink-0 pb-1.5" style={{ width: COL }}>
          <div className={cn("text-2xs", wave === 0 ? "text-dim font-medium" : "text-mute")}>
            {wave === 0 ? "Ready" : wave}
          </div>
          <div className="mt-0.5 flex items-center gap-1.5">
            <div
              className={cn("h-0.5 rounded-full", wave === 0 ? "bg-dim" : "bg-line-strong")}
              style={{ width: `${Math.max(4, (count / busiest) * (COL - 34))}px` }}
            />
            <span className="text-mute text-2xs tabular-nums opacity-70">{count}</span>
          </div>
        </div>
      ))}
    </div>
  );
}

/**
 * Preview a chain on focus, but only when the focus came from the keyboard.
 *
 * A plain `onFocus` leaves a highlight standing after any mouse click: the
 * button keeps focus, the chain stays lit, and nothing on screen explains why —
 * which is exactly how a critical-path issue came to look permanently selected.
 * `:focus-visible` is the browser's own answer to "did they mean to focus
 * this", so a tabbing user still gets the preview and a clicking user does not
 * get a highlight they did not ask for and cannot dismiss.
 */
function focusPreview(doc: string, onHover: (doc: string | null) => void) {
  return (event: React.FocusEvent<HTMLElement>) => {
    if (event.currentTarget.matches(":focus-visible")) onHover(doc);
  };
}

/**
 * One issue: the rail says which, the bar says when and how big.
 *
 * Absolutely positioned rather than laid out in flow. Every row is the same
 * height by construction, so its y is arithmetic — which is what lets the wires
 * be drawn without measuring anything, and what keeps them correct when the
 * list scrolls.
 */
function TimelineRow({
  node,
  y,
  rail,
  scrolled,
  startsWave,
  width,
  state,
  model,
  milestone,
  startsMilestone,
  tone,
  showSlack,
  hop,
  pinned,
  open,
  onSelect,
  onPin,
  onHover,
  onLeave,
  onFocusHover,
  onStep,
}: {
  node: SequenceNode;
  y: number;
  rail: number;
  /** The track has been scrolled, so the frozen rail needs an edge to sit on. */
  scrolled: boolean;
  /** First row at this depth — takes the hairline that separates one round of
   *  work from the next. */
  startsWave: boolean;
  width: number;
  state: WorkflowState | undefined;
  model: SequenceModel;
  /** The row's milestone, on every row that has one. */
  milestone: MilestoneDto | undefined;
  /** First row of a milestone run — takes the fainter of the two rules. */
  startsMilestone: boolean;
  /** The milestone's dot colour. Bound to the name beside it — a catalog hue
   *  is never shown in this product without the word it belongs to. */
  tone: string | undefined;
  /** The project has dependencies, so slack is a number worth printing. With
   *  no edges every issue has zero slack and the column says nothing. */
  showSlack: boolean;
  /** How many hops from the focused issue, or `undefined` if it is not on the
   *  chain at all. Drives both whether this pill lights and when. */
  hop: number | undefined;
  /** The issue the chain is pinned to. */
  pinned: boolean;
  /** The issue open in the detail view. */
  open: boolean;
  onSelect: (reff: string) => void;
  onPin: (doc: string) => void;
  onHover: (doc: string) => void;
  onLeave: () => void;
  onFocusHover: (doc: string | null) => void;
  onStep: (from: SequenceNode, direction: "back" | "on") => boolean;
}) {
  const { row } = node;
  const stage = state?.category ?? "backlog";
  const done = stage === "done";
  const conflict = conflictSentence(node, model);
  return (
    <div
      onMouseEnter={() => onHover(row.doc_id)}
      onMouseLeave={onLeave}
      className={cn(
        interactiveRow({ selected: open }),
        "group absolute inset-x-0 flex items-center",
        // Two boundaries at two weights: a wave change is the staircase's own
        // step, a milestone change inside a wave is the quieter one. The second
        // is what recovers the sense of a *run* now that the coloured spine has
        // gone — a rule, not a hue.
        startsWave
          ? "border-line/60 border-t"
          : startsMilestone && "border-line/25 border-t",
        // The chain lights the ROW, not the mark.
        //
        // Three highlights have been tried on the pill itself and all three
        // were foreign to this product: a saturated blue, a substituted fill
        // that erased the state underneath it, and a 4px ring that belongs to
        // no other surface here. The app already has one way of saying "this
        // one" — the row's background — and it costs no new vocabulary at all.
        // It also sits *under* the connectors, so a lit row no longer erases
        // the lines crossing it, which was the other half of the same bug.
        hop === 0 && "bg-active",
        hop !== undefined && hop > 0 && "bg-hover",
      )}
      // `interactiveRow` carries a 32px minimum from the control ladder, which
      // matched the chart row exactly until the row grew; it states both so a
      // future change to either cannot silently overlap. The row's height is
      // the chart's arithmetic, not a control
      style={{
        top: y,
        height: ROW,
        minHeight: ROW,
        // Distance is carried by *when* a row lights, so a deep chain reads as
        // a wave travelling outward rather than as a blob that appeared all at
        // once. On the same clock as the lines above it, or the two halves of
        // one gesture would arrive at different speeds.
        transitionDuration: `${FADE_MS}ms`,
        transitionDelay: hop ? `${hop * HOP_MS}ms` : undefined,
      }}
    >
      {/* Two targets, because there are two things to want. The rail opens the
          issue; the pill pins its chain. Nesting one button inside another
          would have been invalid anyway, and separating them turns out to be
          the honest division: the rail is the issue, the track is the chart.

          The rail is frozen. It carries the only two things that say *which*
          issue a row is, and 74px of horizontal scroll used to take both. */}
      <button
        type="button"
        onClick={() => onSelect(row.reff)}
        onFocus={focusPreview(row.doc_id, onFocusHover)}
        onBlur={() => onFocusHover(null)}
        title={row.title}
        className={cn(
          "bg-bg group-hover:bg-hover sticky left-0 z-30 flex h-full shrink-0 items-center gap-2 pr-3 pl-3 text-left transition-colors",
          open && "bg-active group-hover:bg-active",
          scrolled && "shadow-[1px_0_0_var(--color-line)]",
        )}
        style={{ width: rail }}
      >
        {state && <StatusIcon category={state.category} color={catalogColor(state.color)} />}
        <span className="text-mute shrink-0 font-mono text-2xs tabular-nums">
          {row.key_alias ?? row.reff}
        </span>
        <span
          className={cn(
            "min-w-0 flex-1 truncate text-xs",
            done ? "text-mute line-through" : "text-fg",
          )}
        >
          {row.title}
        </span>
        {/* Slack, as a number rather than a colour. Zero means this issue sets
            the finish date; anything else is how many rounds it could slip. A
            quantity belongs in a column — painting it onto bars was what made
            ten scattered blue marks that no single one of which told you to do
            anything. */}
        {showSlack && (
          <span
            className="text-mute w-6 shrink-0 text-right text-2xs tabular-nums"
            title={
              node.slack === 0
                ? "No slack — if this slips, the project slips"
                : `${node.slack} round${node.slack === 1 ? "" : "s"} of slack`
            }
          >
            {node.slack === 0 ? "0" : `+${node.slack}`}
          </span>
        )}
        {/* The milestone, named, on every row.

            It was a 3px coloured spine down the left edge, and that edge is
            spoken for: a saturated vertical bar at the start of a table row
            means *selected* everywhere else in software, so a spine on every
            row read as every row being selected. It also could not do its own
            job — a 3px sliver at 55% alpha is below the size at which a reader
            maps a hue back to one of nine catalog colours, and there is no
            legend at the point of use.

            So the colour comes back bound to its name, which is the rule this
            product already follows everywhere else: a catalog swatch is never
            shown without the word beside it. Printed on every row rather than
            on run-changes only, because repetition is what removes the need for
            a legend — and the sort is wave-major, so a run breaks at every
            round and a run-start label left most rows looking like they
            belonged to nothing. */}
        {milestone && (
          <span
            className="text-mute flex max-w-24 shrink-0 items-center gap-1 text-2xs"
            title={milestone.name}
          >
            <span
              aria-hidden
              className="size-mark-xs shrink-0 rounded-full"
              style={{ background: tone }}
            />
            <span className="truncate">{milestone.name}</span>
          </span>
        )}
      </button>
      {/* The mark. Position is the wave, width is the estimate — one shape
          carrying both halves of "when, and how much". */}
      <span className="relative h-full flex-1">
        <button
          type="button"
          data-bar={row.doc_id}
          onClick={() => onPin(row.doc_id)}
          onFocus={focusPreview(row.doc_id, onFocusHover)}
          onBlur={() => onFocusHover(null)}
          onKeyDown={(event) => {
            const direction =
              event.key === "ArrowLeft" ? "back" : event.key === "ArrowRight" ? "on" : null;
            if (direction && onStep(node, direction)) event.preventDefault();
          }}
          aria-pressed={pinned}
          aria-label={`Show what ${row.key_alias ?? row.reff} blocks and waits on`}
          // The button is the full height of the row and the bar inside it is
          // 12px. They are separate elements because the bar's size is the
          // chart's business — it encodes the estimate — and the target's size
          // is the hand's. A 12px-tall hit area is a miss waiting to happen;
          // this one is the whole row and invisible.
          className="absolute top-0 z-20 flex h-full items-center"
          style={{ left: node.wave * COL + 6, width }}
        >
          <span
            title={node.impossible ? conflict : undefined}
            className={cn(
              "relative block w-full rounded-mark transition-[background-color]",
              // ONE PILL. Every bar on both charts is this shape, this height
              // and this tone; state is a whisper on top of it, never a
              // different component.
              //
              // The fill used to carry four things at once — workflow state,
              // readiness, structural criticality and selection — in four
              // loudly different treatments, one of them a saturated blue and
              // one an outline. That is what a legend was there to decode, and
              // a mark that needs a legend is a mark that has failed. The rail
              // beside it already states the exact status as a glyph and strikes
              // the title through when it is done; the bar does not need to say
              // it again at volume.
              //
              // So: `mute` is the pill, and every pill is the same height. A
              // finished issue was briefly drawn at half height — a second
              // channel, and a tempting one, but wrong: these rows are all
              // issues, and a bar that is shorter than its neighbour asserts
              // that it is a *smaller kind of thing*. Height is already spoken
              // for by the one meaning it can honestly carry here, which is
              // none. `dim` for work in flight is the only modulation: present
              // if you look for it, invisible if you are reading the shape of
              // the chart instead. Exact status is the rail's job — it has a
              // glyph for it and strikes the title through.
              stage === "active" ? "bg-dim" : "bg-mute",
              // The one exception that gets a hue, and it gets the whole mark.
              //
              // This warning has now been in three places. In the rail it could
              // only say "something is wrong with this row"; beside the bar it
              // sat exactly where every incoming connector lands, so a 12px
              // glyph competed with a knot of curves and lost. It is the bar
              // now: a contradiction between two dates a person set is a fact
              // about *this work*, so the work is what changes colour, and the
              // glyph knocked out of it says which fact.
              node.impossible && "bg-warn",
            )}
            style={{ height: BAR_H }}
          >
            {node.impossible && (
              <TriangleAlert
                className="text-bg absolute top-1/2 left-1 size-icon-2xs -translate-y-1/2"
                aria-label={conflict}
              />
            )}
          </span>
        </button>
      </span>
    </div>
  );
}

/**
 * The contradiction, with both dates in it.
 *
 * "1 due before a blocker" told you a contradiction existed and then refused to
 * say between what — which makes it an alarm rather than a finding. Naming the
 * blocker and printing the two dates is the whole difference between the two.
 */
function conflictSentence(node: SequenceNode, model: SequenceModel): string {
  const due = node.row.due_date;
  const first = node.conflicts[0];
  const blocker = first === undefined ? undefined : model.byDoc.get(first);
  if (due == null || !blocker || blocker.row.due_date == null) {
    return "Due no later than something that must precede it";
  }
  const key = blocker.row.key_alias ?? blocker.row.reff;
  const more = node.conflicts.length - 1;
  return (
    `Due ${dueLabel(due)}, but ${key} must finish first and is due ${dueLabel(blocker.row.due_date)}` +
    (more > 0 ? ` (and ${more} more)` : "")
  );
}

/**
 * The state most projects are actually in.
 *
 * A project with no `blocks` edges drew a single column of identical stubs down
 * the left of an empty window — technically the honest picture, and useless: it
 * is a worse list, it teaches nothing, and it does not say what would make it
 * into a chart. It draws the column still, because those are real rows, and
 * puts the missing sentence in the space the chart is not using.
 */
function NoDependenciesNote({ rail }: { rail: number }) {
  return (
    <div
      className="border-line bg-raised text-dim absolute top-16 max-w-sm rounded-surface border p-3 text-xs leading-5"
      style={{ left: rail + COL + 24 }}
    >
      <div className="text-fg mb-1 font-medium">Nothing blocks anything yet</div>
      This chart orders a project by what has to happen first, so with no
      dependencies it is one column: everything could start today. Open an issue
      and add a <span className="text-fg">Blocked by</span> link to start the
      sequence.
    </div>
  );
}

/**
 * Issues with no place in the order.
 *
 * Two different problems, and reporting them as one was a false statement. A
 * loop has no depth, so nothing in it can be placed; but everything *downstream*
 * of a loop also fails to resolve, and those are ordinary issues waiting behind
 * somebody else's mistake. Calling them "issues that block each other" — which
 * is what a fixture with a two-issue loop and one innocent dependent got told —
 * is wrong about the one row whose owner would go looking.
 *
 * `blocks` edges have no CRDT preventing a cycle — the sub-issue tree has one,
 * and this is not that tree — so this is reachable in normal use rather than a
 * corruption state. A note, not a panel: it is a small problem with a clear fix
 * and does not deserve a third of the view.
 */
function TangleNote({
  model,
  visible,
  stateById,
  onSelect,
}: {
  model: SequenceModel;
  /** The docs the filter left on screen. A tangle is a project-level fact, but
   *  the note lists rows — and listing rows the filter deliberately removed is
   *  the same contradiction the headline numbers used to commit in reverse. */
  visible: ReadonlySet<string>;
  stateById: ReadonlyMap<string, WorkflowState>;
  onSelect: (reff: string) => void;
}) {
  const nameOf = (doc: string) => {
    const node = model.byDoc.get(doc);
    return node ? (node.row.key_alias ?? node.row.reff) : doc;
  };
  // The loop sentence still names every member even when some are filtered
  // out: it is the explanation for why the visible one has no column, and an
  // explanation with a hole in it explains nothing.
  const loops = model.loops.filter((loop) => loop.some((doc) => visible.has(doc)));
  const cyclic = model.cyclic.filter((n) => visible.has(n.row.doc_id));
  const stalled = model.stalled.filter((n) => visible.has(n.row.doc_id));
  if (loops.length === 0 && cyclic.length === 0 && stalled.length === 0) return null;
  return (
    <div className="border-line/70 mt-2 border-t px-3 py-2">
      {loops.map((loop, i) => (
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
          Waiting behind {loops.length === 1 ? "that loop" : "a loop"}, so
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
  node: SequenceNode;
  stateById: ReadonlyMap<string, WorkflowState>;
  onSelect: (reff: string) => void;
}) {
  const state = stateById.get(node.row.status);
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={() => onSelect(node.row.reff)}
      onKeyDown={(e) => {
        if (e.key === "Enter") onSelect(node.row.reff);
      }}
      className={cn(interactiveRow(), "flex items-center gap-2 px-1")}
      style={{ height: ROW }}
    >
      {state && <StatusIcon category={state.category} color={catalogColor(state.color)} />}
      <span className="text-mute shrink-0 font-mono text-2xs tabular-nums">
        {node.row.key_alias ?? node.row.reff}
      </span>
      <span className="text-fg truncate text-xs">{node.row.title}</span>
    </div>
  );
}

/**
 * The lines between bars.
 *
 * Rows are sorted by depth, so a blocker is always above what it blocks and
 * every wire runs downward — which is why a plain curve is enough and no
 * routing is needed.
 */
function Wires({
  model,
  chain,
  yByDoc,
  widthOf,
  height,
  rail,
  width,
}: {
  model: SequenceModel;
  /** Hop distance from the focus, for every issue it reaches. */
  chain: ReadonlyMap<string, number> | null;
  yByDoc: ReadonlyMap<string, number>;
  widthOf: (estimate: number | null | undefined) => number;
  height: number;
  rail: number;
  width: number;
}) {
  /**
   * Every edge, routed once and kept.
   *
   * The first version drew only the critical path at rest and the rest on
   * hover, which kept it tidy and lost the point: a chart of the order of work
   * that shows no structure until you touch it is a list. All of them are drawn
   * now, at a weight where they read as *weave* rather than as lines you are
   * meant to trace — the eye gets the shape of the flow, and picking out one
   * thread is what hover is for.
   *
   * Routing does not depend on hover, so it is memoised without it: hover only
   * changes how a path is painted, never where it goes.
   */
  const wires = useMemo(() => {
    const routed: Array<{
      key: string;
      d: string;
      from: string;
      to: string;
      cleared: boolean;
    }> = [];
    for (const edge of model.edges) {
      const y1 = yByDoc.get(edge.from);
      const y2 = yByDoc.get(edge.to);
      const a = model.byDoc.get(edge.from);
      const b = model.byDoc.get(edge.to);
      if (y1 === undefined || y2 === undefined || !a || !b) continue;
      const x1 = rail + a.wave * COL + 6 + widthOf(a.row.estimate);
      const x2 = rail + b.wave * COL + 6;
      // A single cubic with horizontal handles: it leaves the bar going right
      // and arrives going right, so the eye follows it as flow rather than as a
      // wire that happens to join two points.
      const bend = Math.max(16, (x2 - x1) / 2);
      routed.push({
        key: `${edge.from}->${edge.to}`,
        d: `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`,
        from: edge.from,
        to: edge.to,
        cleared: edge.cleared,
      });
    }
    return routed;
  }, [model, yByDoc, widthOf, rail]);

  /**
   * Every wire, classified by how far it is from the focus.
   *
   * The order is the *wires'* order and never changes, which is the whole point
   * — see the two layers below.
   */
  const layered = useMemo(
    () =>
      wires.map((wire) => {
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
          // upstream it is the *target*, and the line has to draw backwards.
          // Both halves then travel away from the pill you picked.
          outward: depth === "chain" && to! < from!,
        };
      }),
    [chain, wires],
  );

  /**
   * Two layers, and the same wires drawn in both.
   *
   * SVG has no z-index — a path's depth *is* its position in the document — so
   * the obvious way to put a lit chain in front of the grey is to sort the
   * paths before rendering. That worked and it is why the dimming snapped:
   * re-sorting moves DOM nodes, and moving a node cancels every transition and
   * animation on it. Nothing could ease, because on each change of focus every
   * path was being torn out and reinserted somewhere else.
   *
   * So neither list is ever reordered. The base layer holds every wire at its
   * resting or dimmed weight; the chain layer sits on top and holds the same
   * wires again, invisible except where they are on the chain. Depth is which
   * *layer* a wire is showing in, which costs one extra path each and buys a
   * DOM that never moves — so a wire joining the chain fades up on top while its
   * base copy fades down, and leaving is the same thing backwards.
   */
  return (
    <>
      <svg
        className="pointer-events-none absolute top-0 left-0 z-10"
        width={width}
        height={height}
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
            // genuinely elsewhere. Grading those apart is what gives the
            // convergence some air — where six curves land on one pill, the two
            // that matter are no longer adding to the same knot.
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
        width={width}
        height={height}
        aria-hidden
      >
        {layered.map(({ wire, depth, hop, outward }) => {
          const lit = depth === "chain";
          return (
            <path
              key={wire.key}
              d={wire.d}
              fill="none"
              stroke="currentColor"
              strokeWidth={2}
              // Normalised, so one keyframe draws a curve of any real length
              // and nothing has to be measured back out of the DOM.
              pathLength={1}
              strokeDasharray={1}
              className="text-fg transition-opacity ease-out"
              style={
                lit
                  ? {
                      transitionDuration: `${FADE_MS}ms`,
                      animation: `${outward ? "lait-chain-draw-back" : "lait-chain-draw"} ${DRAW_MS}ms cubic-bezier(0.22, 1, 0.36, 1) ${hop * HOP_MS}ms both`,
                      // Trailing off on the same curve as the rows it lights, so
                      // a twelve-hop chain does not end as loudly as it began.
                      opacity: Math.max(0.16, 0.95 - hop * 0.16),
                    }
                  : { opacity: 0, transitionDuration: `${FADE_MS}ms` }
              }
            />
          );
        })}
      </svg>
    </>
  );
}
