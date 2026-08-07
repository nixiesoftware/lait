const PREFIX = "lait.board-scroll.";

export function loadBoardScroll(projectId: string): number {
  try {
    const value = Number(localStorage.getItem(`${PREFIX}${projectId}`));
    return Number.isFinite(value) && value > 0 ? value : 0;
  } catch {
    return 0;
  }
}

export function saveBoardScroll(projectId: string, left: number): void {
  try {
    localStorage.setItem(`${PREFIX}${projectId}`, String(Math.max(0, Math.round(left))));
  } catch {
    // Durable window state is a convenience, never a board dependency.
  }
}

/**
 * Which status columns this project hides.
 *
 * Per project and local, exactly like the scroll position above: what a board
 * looks like on this machine is not a fact about the work, and pushing it
 * through the engine would make one person's tidy-up everyone's.
 *
 * Stored as the ids that are HIDDEN rather than the ones shown, so a workflow
 * that gains a state shows it by default. The opposite encoding silently hides
 * every new status from everyone who had ever touched this control.
 */
const HIDDEN_PREFIX = "lait.board-hidden.";

export function loadHiddenColumns(projectId: string): string[] {
  try {
    const raw = localStorage.getItem(`${HIDDEN_PREFIX}${projectId}`);
    const parsed: unknown = raw ? JSON.parse(raw) : [];
    return Array.isArray(parsed) ? parsed.filter((id): id is string => typeof id === "string") : [];
  } catch {
    return [];
  }
}

export function saveHiddenColumns(projectId: string, ids: readonly string[]): void {
  try {
    localStorage.setItem(`${HIDDEN_PREFIX}${projectId}`, JSON.stringify([...ids]));
  } catch {
    // Durable window state is a convenience, never a board dependency.
  }
}
