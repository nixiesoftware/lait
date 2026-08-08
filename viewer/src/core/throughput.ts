import type { BoardView, MilestoneDto, ProjectDto, Row, StatusCategory, WorkflowState } from "../types";

/**
 * The roadmap's model: a project measured in **work**, not in calendar days.
 *
 * Every roadmap in every tracker puts time on the x axis, and lait cannot —
 * honestly — join them. An issue carries an estimate and a due date; it has no
 * start date, so there is no bar to place on a time axis that somebody actually
 * typed. The usual workaround is to invent starts and then quietly present the
 * invention as a plan.
 *
 * So the axis is **scale**. x is cumulative work: how much there is, in the
 * order it has to happen. That is a number the tracker genuinely holds — issues
 * exist, estimates exist, milestones partition them, workflow stages say which
 * are moving. Nothing is derived to draw it.
 *
 * Time then arrives *second*, and derived rather than typed: given how fast the
 * space has actually been closing work, a distance along the work axis converts
 * to a number of weeks. That makes the calendar a **projection with a stated
 * basis** instead of a date somebody set in March and never revisited — and it
 * is the one reading a tracker with full history is entitled to make.
 *
 * The origin is now. Left of it is work already done, to scale; right of it is
 * what is left. Every project's bar is centred on the same instant, so the arms
 * are comparable across projects in a way that two Gantt bars with different
 * invented starts never are.
 */

/**
 * The unit is one issue, and there is no second option.
 *
 * A points axis was offered alongside it and withdrawn. Estimates are optional
 * in this tracker, so a points bar is only as true as the fraction of issues
 * somebody sized — a project half-estimated draws half a bar, and the missing
 * half looks like less work rather than like a blank field. "One issue is one
 * unit" needs nobody to have typed anything, which is the only footing a
 * headline number should stand on. Estimates still earn their keep inside a
 * project (the sequence chart draws a bar's width from one); they just do not
 * get to set the scale of the whole space.
 */
export const UNIT = "issues";

/** One block of the bar: a run of work of one stage inside one milestone. */
export interface WorkSegment {
  /** Cumulative work at the segment's left edge, relative to now (negative =
   *  already done). */
  from: number;
  to: number;
  stage: StatusCategory;
  /** The milestone this run belongs to, or `null` for the no-milestone bucket. */
  milestone: string | null;
}

export interface MilestoneStop {
  milestone: MilestoneDto;
  /** Cumulative work where this milestone's own run begins. With `at`, this is
   *  the milestone's *share* of the project — which is the reading the bands
   *  under the bar exist to give: a milestone that is a third of the work looks
   *  like a third of the work. */
  from: number;
  /** Cumulative work at which this milestone's last issue is finished. */
  at: number;
  /** Units of this milestone's work still outstanding. */
  remaining: number;
  /** What the calendar says, if anyone set it. */
  target: number | null;
  /**
   * When the work says it lands, from the measured rate. `null` when there is
   * no rate to project with — an unknown is left unknown rather than filled in
   * with the target it is supposed to be checking.
   */
  projected: number | null;
  /** Projected to land after its own target. The one claim worth an alarm. */
  late: boolean;
}

export interface ProjectWork {
  project: ProjectDto;
  /** Units already finished — the left arm's length. */
  done: number;
  /** Units still to do — the right arm's length. */
  remaining: number;
  /** Of `remaining`, how much is in flight rather than untouched. */
  active: number;
  segments: WorkSegment[];
  stops: MilestoneStop[];
  /** Issues in the order the bar lays them out, with their own spans. */
  blocks: Array<{ row: Row; from: number; to: number; stage: StatusCategory }>;
}

const WEEK = 604_800;

/**
 * How fast the space actually closes work, in units per week.
 *
 * Measured over the window the space has been running: from the earliest
 * project start anybody set to now. That is the only start date lait holds, and
 * using it keeps the rate an observation rather than a setting.
 *
 * `null` when there is nothing to measure — no start date, no elapsed time, or
 * nothing finished yet. A missing rate is reported as missing; a roadmap that
 * invents one is back to inventing dates, which is the thing this design exists
 * to avoid.
 */
export function measureThroughput(
  projects: readonly ProjectDto[],
  boards: Record<string, BoardView>,
  states: readonly WorkflowState[],
  now: number,
): { perWeek: number; sinceWeeks: number; done: number } | null {
  const starts = projects.map((p) => p.start_date).filter((d): d is number => d != null);
  if (starts.length === 0) return null;
  const elapsed = (now - Math.min(...starts)) / WEEK;
  if (elapsed <= 0) return null;

  const category = new Map(states.map((s) => [s.id, s.category]));
  let done = 0;
  for (const project of projects) {
    for (const row of rowsOf(boards[project.key])) {
      if (category.get(row.status) === "done") done += 1;
    }
  }
  if (done === 0) return null;
  return { perWeek: done / elapsed, sinceWeeks: elapsed, done };
}

const rowsOf = (board: BoardView | undefined): Row[] =>
  board ? board.columns.flatMap((c) => c.rows).filter((r) => !r.tombstone) : [];

/**
 * Lay one project out along the work axis.
 *
 * Order is the order the work has to happen in: milestones in the project's own
 * order, and inside each one, finished work first, then what is moving, then
 * what has not been picked up. That ordering is what makes the left arm mean
 * "spent" and the boundary at zero mean "now" — without it, the bar would be a
 * bag of issues and its midpoint would mean nothing.
 */
export function layOutProject(
  project: ProjectDto,
  board: BoardView | undefined,
  milestones: readonly MilestoneDto[],
  states: readonly WorkflowState[],
  rate: number | null,
  now: number,
): ProjectWork {
  const category = new Map(states.map((s) => [s.id, s.category]));
  const stageOf = (row: Row): StatusCategory => category.get(row.status) ?? "backlog";
  const rows = rowsOf(board);

  const rank = new Map(milestones.map((m, i) => [m.id, i]));
  const STAGE_ORDER: Record<StatusCategory, number> = { done: 0, active: 1, backlog: 2 };
  const ordered = [...rows].sort(
    (a, b) =>
      (rank.get(a.milestone ?? "") ?? milestones.length) -
        (rank.get(b.milestone ?? "") ?? milestones.length) ||
      STAGE_ORDER[stageOf(a)] - STAGE_ORDER[stageOf(b)] ||
      a.reff.localeCompare(b.reff),
  );

  const done = ordered.filter((r) => stageOf(r) === "done").length;
  const total = ordered.length;
  const active = ordered.filter((r) => stageOf(r) === "active").length;

  // Everything is placed relative to now, which sits where finished work ends.
  let cursor = -done;
  const blocks: ProjectWork["blocks"] = [];
  const segments: WorkSegment[] = [];
  for (const row of ordered) {
    const stage = stageOf(row);
    blocks.push({ row, from: cursor, to: cursor + 1, stage });
    const last = segments[segments.length - 1];
    if (last && last.stage === stage && last.milestone === (row.milestone ?? null)) {
      last.to = cursor + 1;
    } else {
      segments.push({ from: cursor, to: cursor + 1, stage, milestone: row.milestone ?? null });
    }
    cursor += 1;
  }

  const stops: MilestoneStop[] = [];
  for (const milestone of milestones) {
    const mine = blocks.filter((b) => b.row.milestone === milestone.id);
    if (mine.length === 0) continue;
    const at = Math.max(...mine.map((b) => b.to));
    const from = Math.min(...mine.map((b) => b.from));
    const remaining = mine
      .filter((b) => b.stage !== "done")
      .reduce((n, b) => n + (b.to - b.from), 0);
    const target = milestone.target_date ?? null;
    // Projected only forward: work already behind us landed when it landed, and
    // reporting a "projection" for it would be a guess about the past.
    const projected = rate !== null && rate > 0 && at > 0 ? now + (at / rate) * WEEK : null;
    stops.push({
      milestone,
      from,
      at,
      remaining,
      target,
      projected,
      late: projected !== null && target !== null && projected > target,
    });
  }

  return { project, done, remaining: total - done, active, segments, stops, blocks };
}
