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
