import { useCallback, useEffect, useMemo, useState } from "react";
import { GitBranch, TriangleAlert } from "lucide-react";

import type { BoardView, MilestoneDto, ProjectGraphView, Row, WorkflowState } from "../types";
import { buildSequence, type SequenceModel, type SequenceNode } from "../core/sequence";
import { EmptyState } from "./AppState";
import { catalogColor } from "./colors";
import { StatusIcon } from "./icons";
import { cn, interactiveRow } from "./primitives";
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
 * the cursor has made it a lookup rather than a picture. At rest the edges read
 * as weave — where the graph is dense, where it braids, where it thins to a
 * single thread — with the critical path picked out on top. Hover is for
 * pulling one thread out of that weave, not for finding out there is one.
 *
 * The stack, since three things overlap and the order is load-bearing: row
 * backgrounds at the bottom, then the wires, then the pills, then the sticky
 * scale over all of it. Wires used to sit under the backgrounds, which cut
 * every curve into segments wherever it crossed a row.
 *
 * No geometry is measured. Rows are a fixed height and columns a fixed width,
 * so every position is arithmetic on an index. That is not only faster than
 * reading it back from the DOM, it removes the class of bug where a connector
 * is drawn against a layout that has since moved.
 */

/** Row height and column width. Everything on the chart is placed from these
 *  two numbers, so they are the only things to change to retune its density. */
const ROW = 30;
const COL = 92;
/** The rail: wide enough for a key and a readable title, narrow enough that the
 *  track — the part that is actually the chart — keeps the rest. */
const RAIL = 300;
/** A bar with no estimate still has to be visible and still has to read as
 *  "unsized" rather than as "small". */
const BAR_MIN = 26;
const BAR_MAX = COL - 14;

export function ProjectTimeline({
  board,
  graph,
  milestones,
  projectName,
  onSelect,
}: {
  board: BoardView;
  /** The project's whole edge set, from `project_graph`. Absent while it loads —
   *  the chart still draws, as a single column, which is the honest picture of
   *  "no dependencies known yet" and beats a spinner over a usable view. */
  graph: ProjectGraphView | null;
  milestones: MilestoneDto[];
  projectName: string;
  onSelect: (reff: string) => void;
}) {
  const states: WorkflowState[] = useMemo(
    () => board.columns.map((c) => c.state),
    [board.columns],
  );
  const rows: Row[] = useMemo(
    () => board.columns.flatMap((c) => c.rows).filter((r) => !r.tombstone),
    [board.columns],
  );
  const model = useMemo(
    () => buildSequence(rows, graph?.edges ?? [], states),
    [rows, graph, states],
  );
  const stateById = useMemo(() => indexBy(states, (s) => s.id), [states]);
  const milestoneById = useMemo(() => indexBy(milestones, (m) => m.id), [milestones]);

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
      .filter((n) => !n.cyclic)
      .sort(
        (a, b) =>
          a.wave - b.wave ||
          (rank.get(a.row.milestone ?? "") ?? milestones.length) -
            (rank.get(b.row.milestone ?? "") ?? milestones.length) ||
          a.row.reff.localeCompare(b.row.reff),
      );
    // Two flags, both about not repeating yourself. A milestone name printed on
    // all nineteen of its rows is nineteen times the ink for one fact, and it
    // was crowding the title down to "Shar…" — so it prints when it changes and
    // is otherwise implied by the rows above. Same for the wave: a hairline
    // where the depth changes is the only separator the staircase needs.
    return ordered.map((node, i) => {
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
  }, [model, milestones]);

  const yByDoc = useMemo(
    () => new Map(lines.map((l) => [l.node.row.doc_id, l.y + ROW / 2])),
    [lines],
  );

  /** Bar width is the estimate, scaled against the largest one present. An
   *  unsized issue draws the minimum, which reads as a tick rather than a
   *  claim that it is small. */
  const widthOf = useMemo(() => {
    const largest = Math.max(1, ...rows.map((r) => r.estimate ?? 0));
    return (estimate: number | null | undefined) =>
      estimate == null ? BAR_MIN : BAR_MIN + ((BAR_MAX - BAR_MIN) * Math.min(estimate, largest)) / largest;
  }, [rows]);

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
  const [pinned, setPinned] = useState<string | null>(null);
  const [hover, setHover] = useState<string | null>(null);
  const focus = pinned ?? hover;
  const togglePin = useCallback(
    (doc: string) => setPinned((current) => (current === doc ? null : doc)),
    [],
  );
  useEffect(() => {
    if (pinned === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setPinned(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [pinned]);
  const waveCount = model.waves.length;

  if (rows.length === 0) {
    return (
      <EmptyState
        icon={<GitBranch className="size-icon-lg" />}
        title={`No issues in ${projectName}`}
        body="The timeline orders a project by what blocks what. Add an issue to start one."
      />
    );
  }

  const height = lines.length * ROW;
  const trackWidth = Math.max(waveCount, 1) * COL;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Legend model={model} waveCount={waveCount} />
      <div className="min-h-0 flex-1 overflow-auto">
        <div className="relative" style={{ width: RAIL + trackWidth, minWidth: "100%" }}>
          <WaveScale waveCount={waveCount} model={model} />
          <div className="relative" style={{ height }}>
            {/* One layer for every line on the chart, under the rows so a
                connector never crosses a title, and inert so it never eats a
                click meant for one. */}
            <Wires
              model={model}
              yByDoc={yByDoc}
              widthOf={widthOf}
              focus={focus}
              height={height}
              width={RAIL + trackWidth}
            />
            {lines.map(({ node, y, startsWave, showMilestone }) => (
              <TimelineRow
                key={node.row.doc_id}
                node={node}
                y={y}
                startsWave={startsWave}
                width={widthOf(node.row.estimate)}
                state={stateById.get(node.row.status)}
                milestone={
                  showMilestone && node.row.milestone
                    ? milestoneById.get(node.row.milestone)
                    : undefined
                }
                linked={focus !== null && isNear(model, focus, node.row.doc_id)}
                pinned={pinned === node.row.doc_id}
                onSelect={onSelect}
                onPin={togglePin}
                onHover={setHover}
              />
            ))}
          </div>
          {model.cyclic.length > 0 && (
            <CycleNote nodes={model.cyclic} stateById={stateById} onSelect={onSelect} />
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * Is `doc` the focused issue or one step from it?
 *
 * One hop deep on purpose. A transitive highlight would light most of the chart
 * — everything downstream of a wave-0 issue is most of the project — and say
 * nothing about what actually blocks what.
 */
function isNear(model: SequenceModel, hovered: string, doc: string): boolean {
  if (hovered === doc) return true;
  const node = model.byDoc.get(hovered);
  if (!node) return false;
  return node.blockedBy.includes(doc) || node.blocks.includes(doc);
}

/**
 * What the chart is claiming, in one line.
 *
 * The critical path is the headline: its length is the number of waves, so it
 * is the floor on how long this project takes however many people work on it.
 * That is the sentence the view exists to be able to say.
 */
function Legend({ model, waveCount }: { model: SequenceModel; waveCount: number }) {
  const ready = [...model.byDoc.values()].filter((n) => n.ready).length;
  const impossible = [...model.byDoc.values()].filter((n) => n.impossible).length;
  return (
    <div className="border-line/70 text-mute flex h-bar-md shrink-0 items-center gap-3 border-b px-4 text-xs">
      <span>
        <span className="text-fg font-medium">{ready}</span> ready to start
      </span>
      {model.criticalPath.length > 0 && (
        <span className="flex items-center gap-1.5">
          <span className="bg-accent inline-block h-0.5 w-4 rounded-full" />
          longest chain <span className="text-fg font-medium">{model.criticalPath.length}</span>
        </span>
      )}
      <span>
        <span className="text-fg font-medium">{waveCount}</span> round{waveCount === 1 ? "" : "s"} of work
      </span>
      {impossible > 0 && (
        <span className="text-warn flex items-center gap-1">
          <TriangleAlert className="size-icon-xs" />
          {impossible} due before a blocker
        </span>
      )}
      {model.cyclic.length > 0 && (
        <span className="text-warn flex items-center gap-1">
          <TriangleAlert className="size-icon-xs" />
          {model.cyclic.length} in a loop
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
 */
function WaveScale({ waveCount, model }: { waveCount: number; model: SequenceModel }) {
  // The profile: how much work sits in each round. Numbers alone make you read
  // and compare twelve of them; a bar makes the shape of the project — front
  // loaded, or a long thin tail — legible without reading anything.
  const counts = Array.from({ length: waveCount }, (_, w) => (model.waves[w] ?? []).length);
  const busiest = Math.max(1, ...counts);
  return (
    <div className="bg-bg border-line/70 sticky top-0 z-30 flex h-bar-md items-end border-b">
      <div className="shrink-0" style={{ width: RAIL }} />
      {counts.map((count, wave) => (
        <div key={wave} className="shrink-0 pb-1.5" style={{ width: COL }}>
          <div className="text-mute flex items-baseline gap-1 text-2xs">
            <span className={cn(wave === 0 && "text-dim font-medium")}>
              {wave === 0 ? "Ready" : wave}
            </span>
            <span className="tabular-nums opacity-60">{count}</span>
          </div>
          <div
            className={cn("mt-1 h-0.5 rounded-full", wave === 0 ? "bg-dim" : "bg-line-strong")}
            style={{ width: `${Math.max(6, (count / busiest) * (COL - 16))}px` }}
          />
        </div>
      ))}
    </div>
  );
}

/**
 * One issue: the rail says which, the bar says when and how big.
 *
 * Absolutely positioned rather than laid out in flow. Every row is the same
 * height by construction, so its y is arithmetic — which is what lets the wires
 * be drawn without measuring anything, and what keeps them correct when the
 * list scrolls.
 */
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

function TimelineRow({
  node,
  y,
  startsWave,
  width,
  state,
  milestone,
  linked,
  pinned,
  onSelect,
  onPin,
  onHover,
}: {
  node: SequenceNode;
  y: number;
  /** First row at this depth — takes the hairline that separates one round of
   *  work from the next. */
  startsWave: boolean;
  width: number;
  state: WorkflowState | undefined;
  /** Present only on the first row of a run, so the name is stated once. */
  milestone: MilestoneDto | undefined;
  /** In the chain currently under focus — this issue or one step from it. */
  linked: boolean;
  /** The issue the chain is pinned to. */
  pinned: boolean;
  onSelect: (reff: string) => void;
  onPin: (doc: string) => void;
  onHover: (doc: string | null) => void;
}) {
  const { row } = node;
  const done = state?.category === "done";
  return (
    <div
      onMouseEnter={() => onHover(row.doc_id)}
      onMouseLeave={() => onHover(null)}
      className={cn(
        interactiveRow(),
        "group absolute inset-x-0 flex items-center",
        startsWave && "border-line/60 border-t",
      )}
      style={{ top: y, height: ROW }}
    >
      {/* Two targets, because there are two things to want. The rail opens the
          issue; the pill pins its chain. Nesting one button inside another
          would have been invalid anyway, and separating them turns out to be
          the honest division: the rail is the issue, the track is the chart. */}
      <button
        type="button"
        onClick={() => onSelect(row.reff)}
        onFocus={focusPreview(row.doc_id, onHover)}
        onBlur={() => onHover(null)}
        title={row.title}
        className="flex h-full shrink-0 items-center gap-2 px-3 text-left"
        style={{ width: RAIL }}
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
        {node.impossible && (
          <TriangleAlert
            className="text-warn size-icon-xs shrink-0"
            aria-label="Due no later than something that must precede it"
          />
        )}
        {milestone && (
          <span className="text-mute max-w-24 shrink-0 truncate text-2xs opacity-70">
            {milestone.name}
          </span>
        )}
      </button>
      {/* The mark. Position is the wave, width is the estimate — one shape
          carrying both halves of "when, and how much". */}
      <span className="relative h-full flex-1">
        <button
          type="button"
          onClick={() => onPin(row.doc_id)}
          onFocus={focusPreview(row.doc_id, onHover)}
          onBlur={() => onHover(null)}
          aria-pressed={pinned}
          aria-label={`Show what ${row.key_alias ?? row.reff} blocks and waits on`}
          // The button is the full height of the row and the bar inside it is
          // 8px. They are separate elements because the bar's size is the
          // chart's business — it encodes the estimate — and the target's size
          // is the hand's. An 8px-tall hit area is a miss waiting to happen;
          // this one is 30px and invisible.
          className="absolute top-0 z-20 flex h-full items-center"
          style={{ left: node.wave * COL + 6, width }}
        >
          <span
            className={cn(
              "h-2 w-full rounded-full transition-[background-color,box-shadow]",
              done && "opacity-40",
              // Linked takes the accent because its wires just did: the pill and
              // the curve leaving it are one highlight, and colouring only the
              // line makes you work out which bar it came from.
              //
              // `accent`, not `active`. They read as synonyms and are not:
              // `--color-active` is the near-white tint behind a selected row
              // (chroma 0.004), `--color-accent` is the blue (chroma 0.2). This
              // whole view was drawn in the former, which is why the critical
              // path once looked like a slightly paler grey line.
              // The longest chain carries the same full accent as a chain you
              // pick, not a paler one. A half-strength blue was tried and reads
              // as *less* important, which is backwards — it is the run of work
              // that sets the floor on finishing the project.
              //
              // So hue is not what separates "structural" from "selected"; the
              // ring on a pinned bar is. That leaves the accent meaning one
              // thing — this is on a chain that matters — and the ring saying
              // which chain you chose.
              linked || node.critical
                ? "bg-accent"
                : node.ready
                  ? "bg-dim"
                  : "bg-line-strong",
              // The pinned bar keeps a ring, so you can find what you are
              // looking at after scrolling away from it and back.
              pinned && "ring-accent/50 ring-[3px]",
              !pinned && "group-hover:ring-accent/20 group-hover:ring-[3px]",
            )}
          />
        </button>
      </span>
    </div>
  );
}

/**
 * The lines between bars.
 *
 * Two kinds, and the split is the whole reason this stays readable. The
 * critical path is permanent, because it is the one chain worth seeing without
 * asking. Everything else is drawn only for the issue under the cursor: 110
 * edges over 86 rows is a hairball, and a hairball hides the staircase it is
 * drawn on top of.
 *
 * Rows are sorted by depth, so a blocker is always above what it blocks and
 * every wire runs downward — which is why a plain curve is enough and no
 * routing is needed.
 */
function Wires({
  model,
  yByDoc,
  widthOf,
  focus,
  height,
  width,
}: {
  model: SequenceModel;
  yByDoc: ReadonlyMap<string, number>;
  widthOf: (estimate: number | null | undefined) => number;
  /** The pinned issue, or whatever the cursor is over. */
  focus: string | null;
  height: number;
  width: number;
}) {
  /**
   * Every edge, routed once and kept.
   *
   * The first version drew only the critical path at rest and the rest on
   * hover, which kept it tidy and lost the point: a chart of the order of work
   * that shows no structure until you touch it is a list. All of them are drawn
   * now, but at a weight where they read as *weave* rather than as lines you
   * are meant to trace — the eye gets the shape of the flow, and picking out
   * one thread is what hover is for.
   *
   * Routing does not depend on hover, so it is memoised without it: hover only
   * changes how a path is painted, never where it goes.
   */
  const wires = useMemo(() => {
    const routed: Array<{ key: string; d: string; from: string; to: string; critical: boolean }> = [];
    for (const edge of model.edges) {
      const y1 = yByDoc.get(edge.from);
      const y2 = yByDoc.get(edge.to);
      const a = model.byDoc.get(edge.from);
      const b = model.byDoc.get(edge.to);
      if (y1 === undefined || y2 === undefined || !a || !b) continue;
      const x1 = RAIL + a.wave * COL + 6 + widthOf(a.row.estimate);
      const x2 = RAIL + b.wave * COL + 6;
      // A single cubic with horizontal handles: it leaves the bar going right
      // and arrives going right, so the eye follows it as flow rather than as a
      // wire that happens to join two points. Rows are sorted by depth, so a
      // blocker is always above what it blocks and every curve runs downward —
      // which is why no routing around obstacles is needed.
      const bend = Math.max(16, (x2 - x1) / 2);
      routed.push({
        key: `${edge.from}->${edge.to}`,
        d: `M ${x1} ${y1} C ${x1 + bend} ${y1}, ${x2 - bend} ${y2}, ${x2} ${y2}`,
        from: edge.from,
        to: edge.to,
        critical: edge.critical,
      });
    }
    return routed;
  }, [model, yByDoc, widthOf]);

  return (
    <svg
      className="pointer-events-none absolute top-0 left-0 z-10"
      width={width}
      height={height}
      aria-hidden
    >
      {wires.map((wire) => {
        const lit = focus !== null && (wire.from === focus || wire.to === focus);
        // A lit edge outranks a critical one: when you are asking about a
        // specific issue, that question is the one on screen.
        const tone = lit || wire.critical ? "text-accent" : "text-line-strong";
        return (
          <path
            key={wire.key}
            d={wire.d}
            fill="none"
            stroke="currentColor"
            strokeWidth={lit ? 1.75 : wire.critical ? 1.25 : 1}
            className={tone}
            // Faint enough at rest to read as texture; any heavier and the
            // curves compete with the bars they are drawn between.
            // Nothing is dimmed to nothing any more. A pinned highlight
            // persists, so it does not have to win by silencing the chart —
            // the unfocused weave stays readable at its resting weight and the
            // focused chain simply comes forward.
            opacity={lit ? 1 : wire.critical ? 0.9 : 0.22}
          />
        );
      })}
    </svg>
  );
}

/**
 * Issues in a dependency loop.
 *
 * A loop has no depth, so there is no column these belong in, and picking one
 * would be a lie. `blocks` edges have no CRDT preventing a cycle — the
 * sub-issue tree has one, and this is not that tree — so this is reachable in
 * normal use rather than a corruption state. A note, not a panel: it is a small
 * problem with a clear fix and does not deserve a third of the view.
 */
function CycleNote({
  nodes,
  stateById,
  onSelect,
}: {
  nodes: SequenceNode[];
  stateById: ReadonlyMap<string, WorkflowState>;
  onSelect: (reff: string) => void;
}) {
  return (
    <div className="border-line/70 mt-2 border-t px-3 py-2">
      <div className="text-warn mb-1 flex items-center gap-1.5 text-2xs">
        <TriangleAlert className="size-icon-xs" />
        {nodes.length} issue{nodes.length === 1 ? "" : "s"} block each other, so they have no place
        in the order
      </div>
      {nodes.map((node) => (
        <div
          key={node.row.doc_id}
          role="button"
          tabIndex={0}
          onClick={() => onSelect(node.row.reff)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onSelect(node.row.reff);
          }}
          className={cn(interactiveRow(), "flex items-center gap-2 px-1")}
          style={{ height: ROW }}
        >
          {(() => {
            const state = stateById.get(node.row.status);
            return state ? (
              <StatusIcon category={state.category} color={catalogColor(state.color)} />
            ) : null;
          })()}
          <span className="text-mute shrink-0 font-mono text-2xs tabular-nums">
            {node.row.key_alias ?? node.row.reff}
          </span>
          <span className="text-fg truncate text-xs">{node.row.title}</span>
        </div>
      ))}
    </div>
  );
}
