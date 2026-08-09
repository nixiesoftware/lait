import { useCallback, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Flag, TriangleAlert } from "lucide-react";

import type {
  BoardView,
  MemberDto,
  MilestoneDto,
  ProjectDto,
  StatusCategory,
  WorkflowState,
} from "../types";
import {
  layOutProject,
  measureThroughput,
  type MilestoneStop,
  type ProjectWork,
} from "../core/throughput";
import { EmptyState } from "./AppState";
import { Avatar } from "./Avatar";
import { catalogColor } from "./colors";
import { ProjectIcon } from "./icons";
import { cn, interactiveRow } from "./primitives";
import { dueLabel } from "./time";

/**
 * The workspace roadmap — every project on one axis of **work**.
 *
 * The axis is scale, not calendar time, and that is the whole design. lait has
 * no issue start dates, so a time-axis Gantt can only be drawn by inventing
 * them; what it does have is scope — issues, their estimates, the milestones
 * that partition them and the workflow stages that say which are moving. So x
 * is cumulative work, and the origin is **now**: left of zero is what has been
 * finished, to scale, and right of it is what is left.
 *
 * Time is the second reading of the same axis. The space's measured rate — work
 * actually closed, over the window since the earliest project start — converts a
 * distance into weeks, so the ruler above the chart is a projection with a
 * stated basis rather than a date somebody typed once and never revisited. Where
 * a milestone carries a target as well, the two readings can disagree, and that
 * disagreement is the most useful thing on the page: *the plan says the 5th, the
 * work says the 2nd of next month*.
 *
 * Three consequences worth stating, because they are what make this readable at
 * a glance and none of them is obvious from the code:
 *
 * - **Every bar is centred on the same instant.** Two Gantt bars with different
 *   invented start dates cannot be compared; two arms measured from now can.
 * - **A long left arm is spend, not progress.** The chart says how much a
 *   project has consumed and how much it still holds, in one unit.
 * - **The unit is issues unless the work is really estimated.** A points axis
 *   over half-unsized issues is a chart of how much estimating happened.
 */

const PROJECT_ROW = 44;
/** The bar, at 12/44 — the proportion a duration bar reads at. It was 10/52,
 *  which is inline-meter territory. Also its own minimum width: below that a
 *  bar is a smear, so the shortest run clamps to a square. */
const BAR_H = 16;
/** The frozen identity column. */
const RAIL = 280;
/**
 * Zoom, as a multiple of *fit*.
 *
 * The axis counts issues, so there is no natural pixels-per-unit the way there
 * is for a day — a fixed constant drew a five-issue space as a thumbnail in an
 * empty canvas and a five-hundred-issue one as a mile of scroll. The base is
 * therefore whatever makes the widest project fill the port, and zoom is a
 * multiplier on it: 1× shows everything, the rest trade overview for room.
 */
const ZOOMS = [
  { id: "fit", label: "Fit", factor: 1 },
  { id: "half", label: "½", factor: 2 },
  { id: "quarter", label: "¼", factor: 4 },
] as const;
type Zoom = (typeof ZOOMS)[number]["id"];
/** Floors and ceilings on the derived scale. Below the floor a bar is a smear;
 *  above the ceiling a handful of issues sprawls across a metre of canvas. */
const PX_MIN = 3;
const PX_MAX = 40;
const WEEK = 604_800;
/** Roughly how wide a `~Feb 5, 2027` runs at `text-2xs`. Used to decide how
 *  many of them fit rather than to lay any of them out. */
const DATE_LABEL_PX = 62;

export function Roadmap({
  projects,
  boards,
  milestones,
  states,
  members,
  onOpenProject,
}: {
  projects: ProjectDto[];
  /** Each project's board by KEY. Absent means not loaded yet. */
  boards: Record<string, BoardView>;
  /** Each project's milestones by project id. */
  milestones: Record<string, MilestoneDto[]>;
  states: WorkflowState[];
  members: MemberDto[];
  onOpenProject: (key: string) => void;
}) {
  const [zoom, setZoom] = useState<Zoom>("fit");
  const port = useRef<HTMLDivElement | null>(null);
  const [portWidth, setPortWidth] = useState(0);
  /**
   * Measure on attach, via a callback ref rather than a layout effect.
   *
   * The effect version silently never ran. On the first render `projects` is
   * still empty, so the component returns its empty state, the scroll port does
   * not exist, and a `useLayoutEffect(…, [])` fires once against a null ref and
   * is never invited back. The port then mounted at 1217px and the chart went
   * on drawing itself against a width of zero — which is why "Fit" fitted
   * nothing and every bar was a stub in an empty canvas.
   *
   * A callback ref runs when the node attaches, whenever that turns out to be.
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
  const [scrolled, setScrolled] = useState(false);

  // Pinned once rather than read at each call site: a chart whose origin moves
  // between two of its own measurements is not a chart.
  const now = useMemo(() => Math.floor(Date.now() / 1000), []);
  const memberByKey = useMemo(() => new Map(members.map((m) => [m.key, m])), [members]);

  const rate = useMemo(
    () => measureThroughput(projects, boards, states, now),
    [boards, now, projects, states],
  );

  const work = useMemo(
    () =>
      projects.map((project) =>
        layOutProject(
          project,
          boards[project.key],
          milestones[project.id] ?? [],
          states,
          rate?.perWeek ?? null,
          now,
        ),
      ),
    [boards, milestones, now, projects, rate, states],
  );

  /** How far the axis runs each way from now, in units, padded so the longest
   *  arm is not flush against the edge. */
  const span = useMemo(() => {
    const left = Math.max(1, ...work.map((w) => w.done));
    const right = Math.max(1, ...work.map((w) => w.remaining));
    return { left: left * 1.08, right: right * 1.08 };
  }, [work]);

  // Fit first, then zoom. Measured off the port so the default really is "all
  // of it, on screen" whatever the window is doing.
  const total = span.left + span.right;
  const factor = ZOOMS.find((z) => z.id === zoom)?.factor ?? 1;
  const px = Math.min(
    PX_MAX,
    Math.max(PX_MIN, ((Math.max(portWidth, 480) - RAIL) / total) * factor),
  );
  const width = total * px;
  const originX = span.left * px;
  /** Work → x. The one placement rule; everything else is drawn from it. */
  const xOf = useCallback((units: number) => originX + units * px, [originX, px]);

  const onScroll = useCallback(() => {
    const node = port.current;
    if (!node) return;
    const next = node.scrollLeft > 0;
    setScrolled((current) => (current === next ? current : next));
  }, []);

  const scrollToNow = useCallback(() => {
    const node = port.current;
    if (!node) return;
    node.scrollTo({ left: Math.max(0, RAIL + originX - node.clientWidth / 3), behavior: "smooth" });
  }, [originX]);

  const landed = useRef(false);
  useLayoutEffect(() => {
    const node = port.current;
    if (!node || landed.current || width === 0) return;
    landed.current = true;
    node.scrollLeft = Math.max(0, RAIL + originX - node.clientWidth / 3);
  }, [originX, width]);

  // One row per project, and no expander.
  //
  // There was a disclosure chevron here that unfolded a project's issues in
  // place. It went because the row already had a better answer to the same
  // gesture: clicking it opens that project's own chart, which draws the issues
  // *and* their dependencies rather than a flat list of them. Two ways down
  // into the same place, one of them worse, is a choice nobody wants to make.
  const lines = useMemo(
    () => work.map((entry, i) => ({ key: entry.project.id, work: entry, y: i * PROJECT_ROW })),
    [work],
  );
  const height = lines.length * PROJECT_ROW;
  const ticks = useMemo(() => buildTicks(span, px), [px, span]);
  const weekMarks = useMemo(
    () => dateMarks(ticks, rate?.perWeek ?? null, px, now),
    [now, px, rate, ticks],
  );

  if (projects.length === 0) {
    return (
      <EmptyState
        icon={<Flag className="size-icon-lg" />}
        title="No projects yet"
        body="The roadmap measures every project on one scale of work. Create a project to start one."
      />
    );
  }

  const remaining = work.reduce((n, w) => n + w.remaining, 0);
  const doneTotal = work.reduce((n, w) => n + w.done, 0);

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="border-line/70 text-mute flex h-bar-md shrink-0 items-center gap-3 border-b px-4 text-xs">
        <span>
          <span className="text-fg font-medium">{fmt(remaining)}</span> issues left
        </span>
        <span>
          <span className="text-fg font-medium">{fmt(doneTotal)}</span> done
        </span>
        {/* The basis, stated. A projected date whose rate is not on screen is
            the same unaccountable guess as a date somebody typed. */}
        {rate ? (
          <span className="text-dim">
            at <span className="text-fg font-medium">{fmt(rate.perWeek)}</span> issues/week over{" "}
            {fmt(rate.sinceWeeks)} weeks
          </span>
        ) : (
          <span className="text-dim">
            no rate yet — set a project start date and close some work to project dates
          </span>
        )}
        <div className="ml-auto flex items-center gap-2">
          <button
            type="button"
            onClick={scrollToNow}
            className="border-line text-fg hover:bg-hover rounded-control border px-2 py-0.5 text-2xs transition-colors"
          >
            Now
          </button>
          <div
            className="bg-active/40 flex gap-0.5 rounded-control p-0.5"
            role="group"
            aria-label="Zoom"
          >
            {ZOOMS.map((candidate) => (
              <button
                key={candidate.id}
                type="button"
                aria-pressed={zoom === candidate.id}
                onClick={() => setZoom(candidate.id)}
                title={
                  candidate.factor === 1
                    ? "Fit every project on screen"
                    : `Show ${candidate.label} of the widest project`
                }
                className={cn(
                  "rounded-control px-2 py-0.5 text-2xs transition-colors",
                  zoom === candidate.id ? "bg-raised text-fg" : "text-dim hover:text-fg",
                )}
              >
                {candidate.label}
              </button>
            ))}
          </div>
        </div>
      </div>

      <div ref={attachPort} onScroll={onScroll} className="min-h-0 flex-1 overflow-auto">
        <div className="relative" style={{ width: RAIL + width, minWidth: "100%" }}>
          <Scales
            ticks={ticks}
            weekMarks={weekMarks}
            xOf={xOf}
            originX={originX}
            projecting={rate !== null}
            scrolled={scrolled}
          />
          <div className="relative" style={{ height }}>
            <Gridlines ticks={ticks} xOf={xOf} originX={originX} height={height} />
            {lines.map((line) => (
              <ProjectBand
                key={line.key}
                work={line.work}
                y={line.y}
                xOf={xOf}
                lead={
                  line.work.project.lead ? memberByKey.get(line.work.project.lead) : undefined
                }
                loaded={boards[line.work.project.key] !== undefined}
                scrolled={scrolled}
                onOpen={() => onOpenProject(line.work.project.key)}
              />
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

interface Tick {
  units: number;
  label: string;
  /** The origin — drawn as a rule rather than a tick. */
  origin: boolean;
}

/** Ticks on the work axis, at a round step chosen so they land roughly every
 *  110px whatever the zoom. */
function buildTicks(span: { left: number; right: number }, px: number): Tick[] {
  const rough = 110 / px;
  const step = [1, 2, 5, 10, 20, 25, 50, 100, 200, 500, 1000].find((s) => s >= rough) ?? 1000;
  const ticks: Tick[] = [];
  for (let units = -Math.ceil(span.left / step) * step; units <= span.right; units += step) {
    ticks.push({
      units,
      // Signed on purpose: "-40" is forty units already spent, and dropping the
      // sign would make the left arm read as more work still to come.
      label: units === 0 ? "now" : `${units > 0 ? "+" : ""}${fmt(units)}`,
      origin: units === 0,
    });
  }
  if (!ticks.some((tick) => tick.origin)) ticks.push({ units: 0, label: "now", origin: true });
  return ticks.sort((a, b) => a.units - b.units);
}

/**
 * The date row labels the *work* row's ticks, and never generates its own.
 *
 * Two independent tick generators is what produced `NovFeb 5, 20277, 20276,
 * 2027`: each row chose its own positions, so a date could land anywhere,
 * including on top of the last one. Here a date can only appear where a work
 * tick already is — it may be dropped, but it can never arrive somewhere
 * nothing else is.
 *
 * Thinned until the labels clear each other. A date at `text-2xs` is about 62px
 * wide with a year on it and needs ~24px of air, so the stride doubles until
 * the spacing is enough — and if even every fourth is too tight, only the far
 * anchor survives. Work labels are never dropped; the measurement outranks the
 * projection.
 */
function dateMarks(
  ticks: Tick[],
  perWeek: number | null,
  px: number,
  now: number,
): Map<number, string> {
  const marks = new Map<number, string>();
  if (perWeek === null || perWeek <= 0) return marks;
  const forward = ticks.filter((tick) => tick.units > 0);
  if (forward.length === 0) return marks;
  const spacingPx = (forward[1]?.units ?? forward[0]!.units) * px;
  let stride = 1;
  while (spacingPx * stride < DATE_LABEL_PX + 24 && stride < forward.length) stride *= 2;
  if (spacingPx * stride < DATE_LABEL_PX + 24) {
    const last = forward[forward.length - 1]!;
    marks.set(last.units, `~${dueLabel(now + (last.units / perWeek) * WEEK)}`);
    return marks;
  }
  for (let i = stride - 1; i < forward.length; i += stride) {
    const tick = forward[i]!;
    marks.set(tick.units, `~${dueLabel(now + (tick.units / perWeek) * WEEK)}`);
  }
  return marks;
}

/**
 * Two rulers on one axis.
 *
 * The work scale is the measurement and reads as one: plain numbers, solid
 * origin. The date scale above it is a *projection*, so it is drawn as one —
 * lighter, italic, and it simply stops existing when there is no measured rate
 * to derive it from. A projection that looks exactly like a measurement is the
 * failure this whole view is built to avoid.
 */
function Scales({
  ticks,
  weekMarks,
  xOf,
  originX,
  projecting,
  scrolled,
}: {
  ticks: Tick[];
  weekMarks: ReadonlyMap<number, string>;
  xOf: (units: number) => number;
  originX: number;
  projecting: boolean;
  scrolled: boolean;
}) {
  return (
    <div className="bg-bg border-line/70 sticky top-0 z-30 flex h-bar-lg items-stretch border-b">
      <div
        className={cn(
          "bg-bg sticky left-0 z-10 flex shrink-0 flex-col justify-end pb-1 pl-3",
          scrolled && "shadow-[1px_0_0_var(--color-line)]",
        )}
        style={{ width: RAIL }}
      >
        <span className="text-mute text-2xs">
          issues
          {projecting && <span className="opacity-60"> · dates projected</span>}
        </span>
      </div>
      <div className="relative flex-1">
        {[...weekMarks].map(([units, label]) => (
          <span
            key={units}
            className="text-mute absolute top-1 text-2xs tabular-nums"
            style={{ left: xOf(units), transform: "translateX(-50%)" }}
          >
            {label}
          </span>
        ))}
        {ticks.map((tick) => (
          <span
            key={tick.units}
            className={cn(
              "absolute bottom-1 text-2xs tabular-nums",
              tick.origin ? "text-accent font-medium" : "text-mute",
            )}
            style={{ left: xOf(tick.units), transform: "translateX(-50%)" }}
          >
            {tick.label}
          </span>
        ))}
        <span className="bg-accent absolute bottom-0 h-2 w-px" style={{ left: originX }} />
      </div>
    </div>
  );
}

function Gridlines({
  ticks,
  xOf,
  originX,
  height,
}: {
  ticks: Tick[];
  xOf: (units: number) => number;
  originX: number;
  height: number;
}) {
  return (
    <div className="pointer-events-none absolute inset-0" style={{ height }}>
      {ticks
        .filter((tick) => !tick.origin)
        .map((tick) => (
          <div
            key={tick.units}
            className="border-line/60 absolute top-0 h-full border-l border-dashed"
            style={{ left: RAIL + xOf(tick.units) }}
          />
        ))}
      <div className="bg-accent/70 absolute top-0 h-full w-px" style={{ left: RAIL + originX }} />
    </div>
  );
}

/**
 * The same pill as the sequence chart's, in the same two registers.
 *
 * `mute` is the pill; `dim` is the one step down the ramp that says work is in
 * flight. Nothing else modulates — exact status is the rail's business on both
 * charts. Both of the treatments this replaced were broken in opposite ways —
 * `bg-mute/25` measured about 1.4:1 against the light ground, so the done run,
 * usually the largest part of a bar, was under the threshold at which a shape
 * reads as a mark at all; and the backlog run was a 1px outline over a ~1.2:1
 * wash, which is to say a wireframe.
 */
const STAGE_CLASS: Record<StatusCategory, string> = {
  done: "bg-mute",
  active: "bg-dim",
  backlog: "bg-mute",
};

function ProjectBand({
  work,
  y,
  xOf,
  lead,
  loaded,
  scrolled,
  onOpen,
}: {
  work: ProjectWork;
  y: number;
  xOf: (units: number) => number;
  lead: MemberDto | undefined;
  loaded: boolean;
  scrolled: boolean;
  onOpen: () => void;
}) {
  const { project } = work;
  const tone = catalogColor(project.color);
  const total = work.done + work.remaining;
  const milestoneOrder = new Map(
    work.stops.map((stop, index) => [stop.milestone.id, index]),
  );
  return (
    <div
      className={cn(interactiveRow(), "group absolute inset-x-0 flex cursor-pointer items-center")}
      style={{ top: y, height: PROJECT_ROW, minHeight: PROJECT_ROW }}
    >
      <div
        className={cn(
          "bg-bg group-hover:bg-hover sticky left-0 z-20 flex h-full shrink-0 items-center gap-2 pr-3 pl-1 transition-colors",
          scrolled && "shadow-[1px_0_0_var(--color-line)]",
        )}
        style={{ width: RAIL }}
      >
        <button
          type="button"
          onClick={onOpen}
          className="flex h-full min-w-0 flex-1 flex-col justify-center gap-0.5 text-left"
          title={project.name}
        >
          <span className="flex items-center gap-2">
            <ProjectIcon color={tone} />
            <span className="text-fg group-hover:text-bright min-w-0 flex-1 truncate text-xs font-medium transition-colors">
              {project.name}
            </span>
          </span>
          {/* The numerals carry, the words recede — this line is scanned, not
              read. The KEY comes down here: beside a fully spelled-out name at
              44px it was saying the same thing twice. */}
          <span className="text-mute pl-4 text-2xs tabular-nums">
            <span className="font-mono">{project.key}</span>
            {" · "}
            {!loaded ? (
              "…"
            ) : total === 0 ? (
              "no issues"
            ) : (
              <>
                <span className="text-fg">{fmt(work.remaining)}</span> of {fmt(total)} left ·{" "}
                <span className="text-fg">{fmt(work.active)}</span> moving
              </>
            )}
          </span>
        </button>
        {lead && <Avatar deviceKey={lead.key} alias={lead.alias} me={lead.me} size="sm" />}
      </div>

      <span className="relative h-full flex-1">
        {/* The milestone partition, as bands under the bar. The diamonds mark
            where each one ends; these say how much of the project each one *is*,
            which is the question a stop on its own cannot answer. Alternating
            rather than coloured: they are a grouping, and a hue here would
            compete with the stage tones that carry the actual state. */}
        {work.segments
          .filter((segment) => segment.milestone !== null)
          .map((segment) => {
            const index = milestoneOrder.get(segment.milestone!) ?? 0;
            return (
              <span
                key={`band:${segment.from}:${segment.milestone}`}
                className={cn(
                  "absolute inset-y-1 rounded-control",
                  index % 2 === 0 ? "bg-hover/70" : "bg-transparent",
                )}
                style={{
                  left: xOf(segment.from),
                  width: Math.max(2, xOf(segment.to) - xOf(segment.from)),
                }}
              />
            );
          })}
        {/* Runs, on the one ramp. The project's own catalog hue used to fill
            the in-flight run, which put N arbitrary saturated colours on the
            one segment that is supposed to mean "happening" — the roadmap's
            version of the same complaint the sequence chart's blue drew.
            Identity moved to the dot beside the name, where a catalog colour
            belongs: bound to the word it colours. */}
        {work.segments.map((segment) => (
          <span
            key={`${segment.from}:${segment.stage}:${segment.milestone ?? ""}`}
            className={cn(
              "absolute top-1/2 -translate-y-1/2 rounded-mark",
              STAGE_CLASS[segment.stage],
            )}
            style={{
              left: xOf(segment.from),
              // One height, like the sequence chart's pills. A run is a run of
              // issues whatever state they are in, and a shorter bar would say
              // the finished ones are a smaller kind of thing rather than a
              // finished one. Tone carries the only distinction there is.
              height: BAR_H,
              // Clamped to its own height. Below that a run is not a bar, and a
              // sub-pixel sliver is worse than an honest square.
              width: Math.max(BAR_H, xOf(segment.to) - xOf(segment.from)),
            }}
          />
        ))}
        {/* Labels only where they fit.

            The names used to be printed unconditionally and overlapped into
            mush the moment two milestones were close — `FoundatiSync ∆ Oct 4
            30, 2023un 14, 2027`. No mainstream roadmap persistently labels
            dense point markers; they degrade instead. So does this: the full
            `Name ~date` if there is room for it, the date alone if not, and
            nothing at all below that. The diamond is never dropped, and the
            tooltip always carries the whole story. Evaluated left to right so
            the leftmost stop wins a contested gap. */}
        {(() => {
          let lastLabelEnd = -Infinity;
          return [...work.stops]
            .sort((a, b) => a.at - b.at)
            .map((stop) => {
              const x = xOf(stop.at);
              const full = labelWidth(stop.milestone.name) + DATE_LABEL_PX;
              const short = DATE_LABEL_PX;
              const detail =
                x - full / 2 > lastLabelEnd + 8
                  ? "full"
                  : x - short / 2 > lastLabelEnd + 8
                    ? "date"
                    : "none";
              if (detail !== "none") lastLabelEnd = x + (detail === "full" ? full : short) / 2;
              return (
                <MilestoneStopMark
                  key={stop.milestone.id}
                  stop={stop}
                  x={x}
                  detail={detail}
                />
              );
            });
        })()}
      </span>
    </div>
  );
}

/**
 * Where a milestone's work runs out, and what the calendar thinks of that.
 *
 * The mark's *position* is the measurement — this much work stands between now
 * and the end of this milestone. The date beside it is the second reading, and
 * when the project also carries a target for it, the two can disagree. That
 * disagreement is the point of the chart, so it is the one thing here allowed
 * to raise its voice.
 */
/** About how wide a name runs at `text-2xs`. An estimate on purpose — measuring
 *  text to lay out a chart means a layout pass per frame, and being one
 *  character out only costs a slightly wider gap. */
const labelWidth = (name: string) => name.length * 5.6 + 10;

function MilestoneStopMark({
  stop,
  x,
  detail,
}: {
  stop: MilestoneStop;
  x: number;
  /** How much of the label there is room for. */
  detail: "full" | "date" | "none";
}) {
  const { milestone, projected, target, late } = stop;
  const tip = [
    projected !== null ? `work lands ~${dueLabel(projected)}` : null,
    target !== null ? `target ${dueLabel(target)}` : null,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <span
      className="absolute inset-y-0 flex flex-col items-center justify-center"
      style={{ left: x }}
    >
      <span
        className={cn("size-mark-sm rotate-45 border", late ? "border-warn bg-warn" : "border-dim bg-dim")}
        title={`${milestone.name}${tip ? ` — ${tip}` : ""}`}
      />
      {detail !== "none" && (
        <span className="pointer-events-none absolute top-1/2 mt-2 flex items-center gap-1 text-2xs whitespace-nowrap">
          {detail === "full" && <span className="text-mute">{milestone.name}</span>}
          {late && <TriangleAlert className="text-warn size-icon-2xs" />}
          {projected !== null && (
            <span className={cn("tabular-nums", late ? "text-warn" : "text-mute")}>
              ~{dueLabel(projected)}
            </span>
          )}
        </span>
      )}
    </span>
  );
}

/** One decimal only when it earns it — "5" beats "5.0" and "0.8" beats "1". */
function fmt(n: number): string {
  if (!Number.isFinite(n)) return "0";
  return Number.isInteger(n) ? String(n) : n.toFixed(1);
}
