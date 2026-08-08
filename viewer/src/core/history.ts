/**
 * The viewer's history verbs.
 *
 * `pushState` and `replaceState` are two lines each, which is exactly why the
 * app grew fourteen call sites that did not agree with one another. Three rules
 * live here now, and each one closes a Back-button bug that was reproduced on
 * the running head:
 *
 * 1. **A push has to go somewhere.** `pushState` will happily stack the address
 *    you are already at, and every navigation verb did: re-picking the lit tab,
 *    the current sidebar entry, or the milestone already in the filter each
 *    added an entry. Three clicks of the current Issues tab took
 *    `history.length` from 2 to 5, so Back had to be pressed three times before
 *    the page moved — which reads as a dead button, then a jump.
 *
 * 2. **A replace has to keep the entry's state.** The address-sync effect wrote
 *    `replaceState(null, …)` on every selection change, which erased the marker
 *    rule 3 depends on. State belongs to the *entry*, not to the write, so a
 *    replace carries whatever the entry already held.
 *
 * 3. **A document opened over a surface is a place you were.** Opening an issue
 *    only ever replaced, so the list it was opened from was overwritten rather
 *    than kept: Overview → Issues → open TD-33 → Back landed on *Overview*, and
 *    the list — the page you were actually on — could not be returned to at
 *    all. Opening pushes now, and the entry is marked so closing can go back to
 *    the surface instead of pushing a second copy of it in front.
 *
 * Every address that reaches these functions comes from `formatRoute`. Nothing
 * here parses or builds one — that grammar has a single author and this is not
 * it.
 */

/** The address as `formatRoute` writes it: path and query, no origin. */
export function here(): string {
  return `${window.location.pathname}${window.location.search}`;
}

/**
 * What an entry was pushed to *show*, when what it shows is a document standing
 * over a surface that is still behind it.
 *
 * Only documents get a marker. A view change needs none: its surface is the
 * whole page, so Back to the previous view is already the right answer and
 * there is nothing to distinguish.
 */
export type Opened = "issue" | "spec";

interface EntryState {
  opened?: Opened;
}

/**
 * Go to `href`, keeping where you were.
 *
 * Silently does nothing when `href` is the address already showing — that is
 * rule 1, and it is why this is a function rather than a call to `pushState`.
 * Returns whether an entry was actually added, for callers that care.
 */
export function push(href: string, opened?: Opened): boolean {
  if (here() === href) return false;
  const state: EntryState | null = opened ? { opened } : null;
  window.history.pushState(state, "", href);
  return true;
}

/**
 * Correct the address without making a destination of the correction.
 *
 * For everything the address carries that is not somewhere you *went*: which
 * row the cursor is on, a filter facet, a project resolved asynchronously after
 * the page had already loaded.
 */
export function replace(href: string): boolean {
  if (here() === href) return false;
  window.history.replaceState(window.history.state, "", href);
  return true;
}

/** Whether the entry showing right now is one `push` marked with `opened`. */
export function openedHere(opened: Opened): boolean {
  return (window.history.state as EntryState | null)?.opened === opened;
}

/**
 * Close a document, returning to the surface it was opened over.
 *
 * Back rather than a new address, because the surface is *behind* you: pushing
 * another copy of the list in front of the issue would leave two entries for
 * one list and make Back re-open the issue you just closed. Replacing would be
 * worse still — it would spend the entry, so Back would skip the list entirely,
 * which is the bug this file exists to fix.
 *
 * A deep link straight into an issue has no such entry; nothing is behind it
 * but the page load. That case returns `false` and the caller closes the
 * document in place, which is the honest answer — there is no surface to go
 * back to.
 */
export function leave(opened: Opened): boolean {
  if (!openedHere(opened)) return false;
  window.history.back();
  return true;
}
