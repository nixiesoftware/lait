const PREFIX = "lait.rail-card.";

/**
 * Whether a rail card is collapsed.
 *
 * Keyed on the card, **not** on the project. Collapsing "Properties" is a
 * statement about how you read a project overview, not about one project — and
 * per-project state would mean collapsing the same card again on every project
 * you opened, which is the opposite of a preference.
 *
 * View state, so it lives in `localStorage` and never in the catalog: it is not
 * something a teammate should receive on a sync.
 */
export function loadRailCollapsed(card: string): boolean {
  try {
    return localStorage.getItem(`${PREFIX}${card}`) === "1";
  } catch {
    return false;
  }
}

export function saveRailCollapsed(card: string, collapsed: boolean): void {
  try {
    if (collapsed) localStorage.setItem(`${PREFIX}${card}`, "1");
    else localStorage.removeItem(`${PREFIX}${card}`);
  } catch {
    // A remembered fold is a convenience; the rail renders the same without it.
  }
}

const RAIL_OPEN = "lait.project-rail";

/**
 * Whether the project rail is showing at all.
 *
 * Separate from the per-card folds above, and global for the same reason: it is
 * a statement about how you work, not about one project. The board is why it
 * exists — a fixed 340px console is worth its width on a document and expensive
 * on horizontally scrolling columns, so the width has to be reclaimable.
 *
 * Open by default: the rail is the point of the project shell, and a console
 * nobody knows to turn on is a console nobody has.
 */
export function loadRailOpen(): boolean {
  try {
    return localStorage.getItem(RAIL_OPEN) !== "0";
  } catch {
    return true;
  }
}

export function saveRailOpen(open: boolean): void {
  try {
    if (open) localStorage.removeItem(RAIL_OPEN);
    else localStorage.setItem(RAIL_OPEN, "0");
  } catch {
    // A remembered panel is a convenience; the shell renders the same without it.
  }
}
