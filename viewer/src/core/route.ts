import type { View } from "./registry";
import { isReadOnly, type SpaceRow } from "../types";
import { EMPTY_FILTER, isActive, type FilterState } from "./filter";

/**
 * The shareable part of the viewer's location.
 *
 * Space and issue identity are canonical product identifiers. A route must never
 * contain a local store path, daemon selector, bearer token, or signing secret:
 * another lait installation resolves the same identifiers against its own local
 * replicas and identities.
 */
export interface ViewerRoute {
  spaceId: string | null;
  project: string | null;
  view: View;
  issue: string | null;
  /** The open Spec, on the Specs register. Absent rather than null when closed,
   *  so a route without one compares equal to a route that never had the key. */
  spec?: string;
  /** The open Baseline. A separate key rather than a second meaning for `spec`:
   *  they are different nouns and a link should say which one it opens. */
  baseline?: string;
  /**
   * The composer, expanded out of its dialog and onto the page.
   *
   * NOT a `View`. The union in `registry.ts` is the set of *destinations* — the
   * sidebar lists them, the command registry projects them, and every surface
   * switches on them. An unsaved draft is none of those: it is the Issues view
   * with a composer standing in front of it, which is exactly what `issue` is
   * for an issue. So it rides as a flag on the route the way `?issue=` does,
   * and `view` stays `list`.
   *
   * It is a path segment rather than a query parameter because it is a place
   * you can be sent, and because `/issues/new` is the address a reader would
   * guess. `formatRoute` only ever emits it under a project: the composer needs
   * a project key to file into, so a workspace-scoped draft has no meaning.
   */
  composing?: boolean;
  /**
   * The open Settings sub-page.
   *
   * Settings already wrote `?tab=` and already read it back on mount, and it
   * still did not work: this formatter is the sole author of the address, and
   * anything it does not know about is erased on the next render. So a settings
   * sub-page was linkable in the address bar and gone on reload.
   *
   * Same shape as `spec` and `baseline` — a sub-selection that only exists
   * under one view, so it is parsed and emitted only there. `general` is the
   * default and stays out of the URL, so the plain `/settings` address means
   * what it always did.
   */
  tab?: string;
  filter?: FilterState;
}

/** The Settings sub-pages that have an address. Mirrors `Settings.tsx`'s `Tab`;
 *  an unknown value falls back to `general` rather than rendering nothing. */
const SETTINGS_TABS = new Set(["members", "devices", "labels", "workflow", "access"]);

export const DEFAULT_ROUTE: ViewerRoute = {
  spaceId: null,
  project: null,
  view: "list",
  issue: null,
};

const VIEWS = new Set<View>([
  "overview",
  "list",
  "board",
  "calendar",
  "timeline",
  "projects",
  "inbox",
  "my-issues",
  "activity",
  "specs",
  "settings",
]);
const LAST_ROUTE = "lait.last-route";

/**
 * Canonical URL grammar:
 *
 *   /spaces/:space/:workspace-view
 *   /spaces/:space/projects/:project/:project-view
 *
 * Project identity is structural because it owns Overview, Issues, Board,
 * Calendar, and Activity. Query parameters carry optional issue/filter state.
 * Unknown parameters are deliberately preserved by neither parser nor formatter:
 * the route is a small product contract, not a bag of component state.
 */
export function parseRoute(location: Pick<Location, "pathname" | "search">): ViewerRoute {
  const parts = location.pathname.split("/").filter(Boolean).map(decode);
  if (parts[0] !== "spaces" || !parts[1]) return DEFAULT_ROUTE;

  const candidate = parts[2];
  const projectCandidate = candidate === "projects" && parts[3] ? clean(parts[3]) : null;
  const projectViewCandidate = projectCandidate ? projectView(parts[4]) : null;
  // `/projects/:key/issues/new` — the trailing segment is the only one the
  // grammar takes after a project view, and only after `issues`. Anything else
  // there is not a route we wrote, so it is ignored rather than guessed at.
  const composing =
    projectCandidate !== null && projectViewCandidate === "list" && parts[5] === "new";
  // Members used to be a root destination. It now lives inside workspace
  // settings; old bookmarks still land in Settings instead of a project list.
  const view =
    projectViewCandidate ??
    (candidate === "members"
      ? "settings"
      : candidate && VIEWS.has(candidate as View)
        ? (candidate as View)
        : "list");
  const query = new URLSearchParams(location.search);
  const legacyOverview =
    candidate === "projects" && !projectCandidate ? clean(query.get("overview")) : null;
  const filter: FilterState = {
    text: clean(query.get("q")) ?? "",
    mine: query.get("mine") === "1",
    label: clean(query.get("label")),
    status: query.getAll("status").filter(Boolean),
    priority: query.getAll("priority").filter(Boolean),
    assignees: query.getAll("assignee").filter(Boolean),
    // `?milestone=` present-but-empty is the No-milestone bucket, so the test is
    // `has`, not truthiness. `clean()` would fold "" back to null and lose it.
    milestone: query.has("milestone") ? (query.get("milestone") ?? "") : null,
  };
  // `focus=1` used to pick full width over the split pane. There is no split any
  // more — an open issue is always full width — so the parameter is accepted and
  // dropped rather than rejected: old links still open the issue they name.
  const issue = displaysIssue(view) ? clean(query.get("issue")) : null;
  const spec = view === "specs" ? clean(query.get("spec")) : null;
  const baseline = view === "specs" ? clean(query.get("baseline")) : null;
  const tabParam = view === "settings" ? clean(query.get("tab")) : null;
  const tab = tabParam && SETTINGS_TABS.has(tabParam) ? tabParam : null;

  return {
    spaceId: parts[1],
    project: projectCandidate ?? legacyOverview ?? (isProjectDestination(view) ? clean(query.get("project")) : null),
    view: legacyOverview ? "overview" : view,
    issue,
    ...(spec ? { spec } : {}),
    ...(baseline ? { baseline } : {}),
    ...(composing ? { composing: true } : {}),
    ...(tab ? { tab } : {}),
    ...(carriesFilter(view) && isActive(filter) ? { filter } : {}),
  };
}

export function formatRoute(route: ViewerRoute): string {
  if (!route.spaceId) return "/";

  const query = new URLSearchParams();
  // Not while composing: the draft page stands in front of the rows, so an
  // `?issue=` beside it would claim two things are open at once. The selection
  // underneath is cursor position, and cursor position is not addressable.
  if (route.issue && displaysIssue(route.view) && !route.composing) {
    query.set("issue", route.issue);
  }
  if (route.spec && route.view === "specs") {
    query.set("spec", route.spec);
  }
  if (route.baseline && route.view === "specs") {
    query.set("baseline", route.baseline);
  }
  if (route.tab && route.view === "settings" && SETTINGS_TABS.has(route.tab)) {
    query.set("tab", route.tab);
  }
  if (carriesFilter(route.view) && route.filter && isActive(route.filter)) {
    if (route.filter.text.trim()) query.set("q", route.filter.text.trim());
    if (route.filter.mine) query.set("mine", "1");
    if (route.filter.label) query.set("label", route.filter.label);
    for (const status of route.filter.status) query.append("status", status);
    for (const priority of route.filter.priority) query.append("priority", priority);
    for (const assignee of route.filter.assignees) query.append("assignee", assignee);
    if (route.filter.milestone !== null) query.set("milestone", route.filter.milestone);
  }

  const underProject = route.project && isProjectDestination(route.view);
  // The composer only has an address under a project's Issues: it files into a
  // project key, so there is nowhere else for the segment to hang.
  const draft = underProject && route.composing && route.view === "list" ? "/new" : "";
  const path = underProject
    ? `/spaces/${encodeURIComponent(route.spaceId)}/projects/${encodeURIComponent(route.project!)}/${projectSegment(route.view)}${draft}`
    : `/spaces/${encodeURIComponent(route.spaceId)}/${route.view}`;
  const search = query.toString();
  return search ? `${path}?${search}` : path;
}

export function sameRoute(a: ViewerRoute, b: ViewerRoute): boolean {
  return (
    a.spaceId === b.spaceId &&
    a.project === b.project &&
    a.view === b.view &&
    a.issue === b.issue &&
    (a.spec ?? null) === (b.spec ?? null) &&
    (a.baseline ?? null) === (b.baseline ?? null) &&
    (a.composing ?? false) === (b.composing ?? false) &&
    (a.tab ?? null) === (b.tab ?? null) &&
    JSON.stringify(a.filter ?? EMPTY_FILTER) === JSON.stringify(b.filter ?? EMPTY_FILTER)
  );
}

/** Resolve portable space identity to this machine's supervisor target. When
 * both our actor and an agent hold the space, portable links open as us. */
export function resolveLocalSpace(canonical: string | null, spaces: SpaceRow[]): SpaceRow | null {
  if (!canonical) return null;
  const newestFirst = spaces
    .filter((space) => space.space === canonical)
    .sort((a, b) => b.last_opened - a.last_opened || a.id.localeCompare(b.id));
  return newestFirst.find((space) => !isReadOnly(space)) ?? newestFirst[0] ?? null;
}

export function loadLastRoute(): ViewerRoute | null {
  try {
    const href = localStorage.getItem(LAST_ROUTE);
    return href ? parseRoute(new URL(href, window.location.origin)) : null;
  } catch {
    return null;
  }
}

export function saveLastRoute(route: ViewerRoute): void {
  if (!route.spaceId) return;
  try {
    localStorage.setItem(LAST_ROUTE, formatRoute(route));
  } catch {
    // Continuity is a convenience; navigation remains fully functional.
  }
}

function clean(value: string | null): string | null {
  const trimmed = value?.trim();
  return trimmed ? trimmed : null;
}

function decode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

/** Project-home destinations carry structural project identity; workspace
 * destinations must never inherit stale project state. */
function isProjectDestination(view: View): boolean {
  return view === "overview" || view === "list" || view === "board" || view === "calendar" || view === "activity" || view === "specs";
}

/** Whether this view draws rows a filter can narrow. */
export function carriesFilter(view: View): boolean {
  return view === "list" || view === "board" || view === "calendar";
}

function displaysIssue(view: View): boolean {
  return view === "list" || view === "board" || view === "calendar";
}

function projectSegment(view: View): string {
  return view === "list" ? "issues" : view;
}

function projectView(segment: string | undefined): View | null {
  if (!segment) return "overview";
  if (segment === "issues" || segment === "list") return "list";
  return segment === "overview" ||
    segment === "board" ||
    segment === "calendar" ||
    segment === "activity" ||
    segment === "specs"
    ? segment
    : null;
}
