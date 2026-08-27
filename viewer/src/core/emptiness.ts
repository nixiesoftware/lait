import type { ResourceState } from "./worldViewStore";

/**
 * Why a work area has no rows to draw.
 *
 * These were one fact and are three, and the three want different words. A
 * board is rowless while it loads, when the Space holds no project to draw one
 * for, and when the read failed — and the shell called all of them "this view
 * is unavailable — the local projection could not be loaded", under a warning
 * triangle, with a Retry that could not help two of them. On a Space that had
 * simply never had a project, the header said "Ready locally" while the body
 * said it had failed, and the body was the one that was wrong.
 *
 * The store has always known which it was: it publishes `cold | partial | ready
 * | refreshing | error` on every snapshot. Nothing read it. Every surface
 * inferred its state from `data ?? []`, which is exactly where "not asked yet"
 * and "asked, and there is nothing" become the same value.
 */
export type Emptiness =
  /** There are rows. Draw them. */
  | "none"
  /** A read failed, and somebody can retry it. */
  | "failed"
  /** Nothing has answered yet. */
  | "loading"
  /** The Space holds no project, so there is no board to ask for. */
  | "no-projects";

export function emptinessOf(input: {
  /** Whether the board has rows to draw. */
  hasRows: boolean;
  /** The board resource's own state. */
  board: ResourceState;
  /** The project list's state. */
  projects: ResourceState;
  /** How many live projects the Space holds. */
  projectCount: number;
}): Emptiness {
  if (input.hasRows) return "none";
  // A Space with no projects is asked first, and the order is the whole point.
  //
  // There is no board to ask for without a project, so the board request fails
  // — "no project chosen and no single default". That failure is *caused by*
  // the emptiness, so a check that reads it first answers "failed" every time
  // and the first-run case below is unreachable. I shipped exactly that, with a
  // test asserting it, and the screen went on saying a World had failed to load
  // when it had loaded perfectly and was simply new.
  //
  // Read from a project list that has actually answered: an empty list that has
  // not loaded says nothing at all, and concluding a first run from it is the
  // same defect one layer down.
  if (input.projects === "ready" && input.projectCount === 0) return "no-projects";
  // Then failures, which are the only one of the remaining cases anybody can
  // act on, and the only one where offering a retry is not a lie.
  if (input.board === "error") return "failed";
  if (input.projects === "error") return "failed";
  return "loading";
}
