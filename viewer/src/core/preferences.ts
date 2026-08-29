import { useSyncExternalStore } from "react";

/**
 * Personal preferences — how *this person* wants the client to behave, on
 * *this device*.
 *
 * They are private and local by construction: nothing here is synced, signed,
 * or visible to another member, which is the line between a preference and a
 * setting. A Space setting (its name, its labels, its workflow) is shared truth
 * that the engine records; a preference is a private convenience that the
 * browser records. Theme and density are preferences too, but they live in
 * `App` because Astryx's `<Theme mode>` and the `[data-density]` attribute are
 * the inputs that drive them — this module holds the ones that nothing else
 * already owns.
 *
 * One key, one JSON object, and a hook that re-renders every reader when any
 * field changes: a Calendar honouring the week start must redraw the moment
 * the Preferences page changes it, or the page is a form that lies.
 */

/** Which surface opens when the client launches into a Space. `last` is the
 *  route you were on when you left, which is what the client always did. */
export type HomeView =
  | "last"
  | "list"
  | "board"
  | "calendar"
  | "projects"
  | "inbox"
  | "my-issues"
  | "activity"
  | "specs";

export type WeekStart = "monday" | "sunday";

/** Which key press submits a comment. `mod-enter` is ⌘↵ / Ctrl+↵. */
export type CommentSubmit = "enter" | "mod-enter";

export interface Preferences {
  homeView: HomeView;
  weekStart: WeekStart;
  commentSubmit: CommentSubmit;
}

export const DEFAULT_PREFERENCES: Preferences = {
  homeView: "last",
  weekStart: "monday",
  commentSubmit: "mod-enter",
};

export const HOME_VIEW_OPTIONS: readonly { id: HomeView; label: string }[] = [
  { id: "last", label: "Where you left off" },
  { id: "list", label: "Issues" },
  { id: "board", label: "Board" },
  { id: "calendar", label: "Calendar" },
  { id: "projects", label: "Projects" },
  { id: "inbox", label: "Inbox" },
  { id: "my-issues", label: "My issues" },
  { id: "activity", label: "Activity" },
  { id: "specs", label: "Specs" },
];

const KEY = "lait.prefs";
const EVENT = "lait:prefs";

const HOME_VIEWS = new Set<string>(HOME_VIEW_OPTIONS.map((o) => o.id));

/** Read the stored preferences, defaulting every field the store does not
 *  carry or carries with a value this build does not know. */
export function loadPreferences(): Preferences {
  let raw: unknown = null;
  try {
    const text = localStorage.getItem(KEY);
    raw = text ? JSON.parse(text) : null;
  } catch {
    raw = null;
  }
  const stored = (raw && typeof raw === "object" ? raw : {}) as Record<string, unknown>;
  return {
    homeView:
      typeof stored.homeView === "string" && HOME_VIEWS.has(stored.homeView)
        ? (stored.homeView as HomeView)
        : DEFAULT_PREFERENCES.homeView,
    weekStart: stored.weekStart === "sunday" ? "sunday" : DEFAULT_PREFERENCES.weekStart,
    commentSubmit: stored.commentSubmit === "enter" ? "enter" : DEFAULT_PREFERENCES.commentSubmit,
  };
}

/** Write one field and tell every reader. Returns the whole new set. */
export function savePreference<K extends keyof Preferences>(
  key: K,
  value: Preferences[K],
): Preferences {
  const next = { ...loadPreferences(), [key]: value };
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    // Preferences are a convenience: the page keeps the value for its own
    // lifetime even when storage refuses it.
  }
  cache = next;
  window.dispatchEvent(new Event(EVENT));
  return next;
}

/** The snapshot `useSyncExternalStore` hands out. Cached so an unchanged store
 *  returns the same object and React can skip the render. */
let cache: Preferences | null = null;

function snapshot(): Preferences {
  if (!cache) cache = loadPreferences();
  return cache;
}

function subscribe(notify: () => void): () => void {
  const onChange = () => {
    cache = null;
    notify();
  };
  window.addEventListener(EVENT, onChange);
  // Another tab of the same head changed them.
  window.addEventListener("storage", onChange);
  return () => {
    window.removeEventListener(EVENT, onChange);
    window.removeEventListener("storage", onChange);
  };
}

/** The live preferences. Re-renders the caller when any field changes. */
export function usePreferences(): Preferences {
  return useSyncExternalStore(subscribe, snapshot, snapshot);
}

/** Test seam: drop the cached snapshot so the next read hits storage. */
export function resetPreferencesCache(): void {
  cache = null;
}

// ---- week geometry ----------------------------------------------------------

/**
 * The column a date falls in, 0..6, for a week that starts on `weekStart`.
 *
 * JS numbers days Sunday-first (0=Sun … 6=Sat). Monday-first shifts by six so
 * Monday lands on 0 and Sunday on 6; Sunday-first is the native numbering.
 */
export function weekColumn(jsDay: number, weekStart: WeekStart): number {
  return weekStart === "monday" ? (jsDay + 6) % 7 : jsDay;
}

const MONDAY_FIRST = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as const;

/** Weekday headers in column order, in the width the surface wants. */
export function weekdayLabels(weekStart: WeekStart, width: "short" | "tiny" = "short"): string[] {
  const days =
    weekStart === "monday" ? [...MONDAY_FIRST] : [MONDAY_FIRST[6], ...MONDAY_FIRST.slice(0, 6)];
  return width === "tiny" ? days.map((d) => d.slice(0, 2)) : days;
}
