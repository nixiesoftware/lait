import type { MilestoneDto } from "../types";

/**
 * How far along a milestone is.
 *
 * Derived from the counts, never stored — the same rule the engine follows for
 * `total`/`done` themselves (`src/world/issues.rs`, the `Milestones` query
 * counts live issues at read time and keeps no progress on the record). A
 * milestone with a status field would be a second answer to a question the
 * issues already answer, and the two would drift the first time someone reopened
 * an issue.
 */
export type MilestoneProgress = "not-started" | "in-progress" | "complete";

export function milestoneProgress({ done, total }: Pick<MilestoneDto, "done" | "total">): MilestoneProgress {
  // `total === 0` is not-started, not complete: a milestone nobody has scoped
  // any work into has not been achieved, and `0 >= 0` would say it had.
  if (total === 0) return "not-started";
  if (done >= total) return "complete";
  return done > 0 ? "in-progress" : "not-started";
}

/** Whole-percent completion, 0 when nothing is scoped. */
export function milestonePercent({ done, total }: Pick<MilestoneDto, "done" | "total">): number {
  return total === 0 ? 0 : Math.round((done / total) * 100);
}
