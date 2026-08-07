import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Group, Panel, Separator, useDefaultLayout, usePanelRef } from "react-resizable-panels";
import {
  PanelLeft,
  PanelRight,
  Plus,
} from "lucide-react";

import { ConfirmRequired, hostRpc, LaitError, rpc, spaces as fetchSpaces } from "./api";
import { useDoorbell } from "./doorbell";
import { runBounded, type BulkProgress } from "./core/bulk";
import { groupRows, loadDisplay, saveDisplay, type DisplayState } from "./core/display";
import {
  contribute,
  registry,
  type AppApi,
  type Ctx,
  isIssueMode,
  isProjectView,
  type IssueMode,
  type IssueField,
  type View,
} from "./core/registry";
import {
  carriesFilter,
  DEFAULT_ROUTE,
  formatRoute,
  loadLastRoute,
  parseRoute,
  resolveLocalSpace,
  saveLastRoute,
  type ViewerRoute,
} from "./core/route";
import { useKeys } from "./core/useKeys";
import { neighbourState, workTarget } from "./core/workflow";
import { loadFavoriteProjects, toggleFavoriteProject } from "./core/personalNav";
import { loadRailOpen, saveRailOpen } from "./core/railState";
import { loadSavedViews, type SavedView } from "./core/savedViews";
import { SPEC_KIND_LABEL } from "./core/specs";
import { Activity } from "./ui/Activity";
import { classifyFailure, EmptyState, InlineError, recoveryForError, StandingNotice, TrustPopover } from "./ui/AppState";
import { Board } from "./ui/Board";
import { BulkBar } from "./ui/BulkBar";
import { Calendar } from "./ui/Calendar";
import { Timeline } from "./ui/Timeline";
import { DisplayOptions } from "./ui/DisplayOptions";
import { FilterMenu } from "./ui/FilterMenu";
import type { IssueMutators } from "./ui/fields";
import { Inbox } from "./ui/Inbox";
import { IssueSearch, rememberIssue } from "./ui/IssueSearch";
import { Projects } from "./ui/Projects";
import { ProjectOverview } from "./ui/ProjectOverview";
import { ProjectRail } from "./ui/ProjectRail";
import { ProjectTabs } from "./ui/ProjectTabs";
import {
  Breadcrumbs,
  DESTINATION_ICON,
  DestinationCrumb,
  HeaderActionsOutlet,
  HeaderSlotProvider,
  IssueCrumb,
  ProjectCrumb,
  SpecCrumb,
  SurfaceHeader,
  Toolbar,
  type BreadcrumbItem,
} from "./ui/layout";
import { Settings } from "./ui/Settings";
import { IssueDetail } from "./ui/IssueDetail";
import { IssueList } from "./ui/IssueList";
import { MyIssues } from "./ui/MyIssues";
import { RolesDialog, WorkflowDialog } from "./ui/Governance";
import { NewIssue } from "./ui/NewIssue";
import { NewProject } from "./ui/NewProject";
import { Palette } from "./ui/Palette";
import { Shortcuts } from "./ui/Shortcuts";
import { Specs } from "./ui/Specs";
import { Welcome } from "./ui/Welcome";
import { catalogColor } from "./ui/colors";
import * as ask from "./ui/dialogs";
import { DialogHost } from "./ui/dialogs";
import { Combobox } from "./ui/Picker";
import { Dialog, Theme } from "@astryxdesign/core";

import { laitTheme } from "./theme/lait";
import { Button, IconButton } from "@astryxdesign/core";
import { Sidebar } from "./ui/Sidebar";
import {
  applyFilter,
  EMPTY_FILTER,
  isActive,
  needsServer,
  type FilterState,
} from "./core/filter";
import { PREDICTION_TTL_MS, type Field } from "./core/overlay";
import {
  projectKeys,
  useProjectBoard,
  useProjectMilestones,
  useProjectRegistry,
  useProjectViewerStore,
  useSpec,
} from "./projectStore";
import {
  isReadOnly,
  type BoardPos,
  type Row,
  type SpaceRow,
  type SpecKind,
  type StatusInfo,
  type WhoamiInfo,
  type WorkflowState,
} from "./types";
import "./commands";

type Modal = "palette" | "issueSearch" | "shortcuts" | "workflow" | "roles" | null;
type ThemePreference = "system" | "light" | "dark";
type DensityPreference = "compact" | "comfortable";
const THEME_PREFERENCE = "lait.theme";
const DENSITY_PREFERENCE = "lait.density";
const LAYOUT_PANEL_IDS = ["sidebar", "main", "detail"];

/**
 * The two widths at which the shell sheds a side panel, in the order it sheds
 * them — and that order is the whole point.
 *
 * A window narrow enough for one panel is not narrow enough for none, and the
 * two panels are not worth the same. The project console is a *view* of the
 * project already on screen, and every row in it is reachable from the project's
 * own pages; the workspace rail is the only navigation there is. So the console
 * goes first and navigation survives to a much narrower window than it used to —
 * this used to be backwards, dropping all navigation at 960px while holding a
 * pinned 340px console that left the issue list narrower than it would have been
 * with both.
 *
 * `RAIL_DRAWER_QUERY` is the twin of the `max-[768px]:` utilities on the rail,
 * its separator and the drawer. It has to exist in both languages — Tailwind's
 * arbitrary variants take a literal, and the shell has to *know* which mode it is
 * in to draw the control that opens the drawer — so the number is written down
 * once here and the utilities are the copies to keep in step. The console has no
 * such twin: it is gated in JS alone, because the control that toggles it has to
 * disappear with it.
 */
const CONSOLE_QUERY = "(max-width: 960px)";
const RAIL_DRAWER_QUERY = "(max-width: 768px)";

/**
 * Track a media query as state.
 *
 * The shell cannot infer either threshold from what it already has. `Panel`'s
 * `onResize` cannot see the rail go: it is hidden by CSS, and `display: none` is
 * invisible to the layout library — it goes on reporting the same percentage it
 * had, so a shell that asks only the panel believes the rail is on screen while
 * the person is looking at a window with no navigation in it. That was the bug.
 */
function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const media = window.matchMedia(query);
    const sync = () => setMatches(media.matches);
    sync();
    media.addEventListener("change", sync);
    return () => media.removeEventListener("change", sync);
  }, [query]);
  return matches;
}

/**
 * The shell.
 *
 * It owns state and supplies an [`AppApi`]; it does not own keys. Every gesture —
 * a shortcut, a palette entry, a button — resolves to a command id and runs it, so
 * a behaviour is defined once and is overridable in one place. Buttons call
 * `registry.get(id)?.run(ctx)` rather than a local handler, which is what stops
 * "click" and "keypress" from drifting apart.
 */
export function App() {
  // Read once. The route contains canonical product identity only; this machine
  // resolves the space id to its own local replica after `/api/spaces` answers.
  const initialRoute = useRef((() => {
    const fromUrl = parseRoute(window.location);
    return fromUrl.spaceId ? fromUrl : (loadLastRoute() ?? fromUrl);
  })()).current;
  const [spaces, setSpaces] = useState<SpaceRow[]>([]);
  /** Canonical `ws_…` identity in the URL, distinct from the supervisor's
   * machine-local store handle used by RPC. */
  const [routeSpace, setRouteSpace] = useState<string | null>(initialRoute.spaceId);
  const [current, setCurrent] = useState<string | null>(null);
  // Asked for the formation surface while other spaces already exist, and which
  // tab to open on. Without this the only way to reach `Welcome` would be having
  // no store at all, which makes founding *or entering* a second space
  // impossible from the app — and the second one is how an existing user accepts
  // an invite. The switcher opens it on `found`; the tab strip does the rest,
  // and only the unknown-space empty state has a reason to pre-select `enter`.
  const [founding, setFounding] = useState<"found" | "enter" | null>(null);
  const [selection, setSelection] = useState<string | null>(initialRoute.issue);
  /** The open Spec on the Specs register. A document, not a row: there is no
   *  cursor-versus-open distinction to make, so one piece of state says both. */
  const [openSpec, setOpenSpec] = useState<string | null>(initialRoute.spec ?? null);
  /** The Spec composer: a kind to seed it with, `"any"` to let it ask. */
  const [composingSpec, setComposingSpec] = useState<SpecKind | "any" | null>(null);
  /** The open Baseline. Its own state rather than a second meaning for
   *  `openSpec`: they are different nouns with different readers. */
  const [openBaseline, setOpenBaseline] = useState<string | null>(initialRoute.baseline ?? null);
  const [modal, setModal] = useState<Modal>(null);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  /**
   * Is the issue view open?
   *
   * It is one bit, not two, because there is one way to read an issue: full
   * width, in the work area. `selection` says *which* row is under the cursor;
   * this says whether we are reading it. Arrowing down a list moves the former
   * without touching the latter.
   */
  const [detail, setDetail] = useState(true);
  const [view, setView] = useState<View>(initialRoute.view);
  const [unread, setUnread] = useState(0);
  /** The composer, and the column it was opened from (null = closed). */
  const [composing, setComposing] = useState<{ status?: string } | null>(null);
  const [composingProject, setComposingProject] = useState(false);
  const [filter, setFilter] = useState<FilterState>(initialRoute.filter ?? EMPTY_FILTER);
  const [filterOpen, setFilterOpen] = useState(false);
  // The console's own visibility, remembered across sessions. See core/railState.
  const [railOpen, setRailOpen] = useState(loadRailOpen);
  const [focusToken, setFocusToken] = useState(0);
  /** Group / order / show-deleted. Loaded once; every change is persisted. */
  const initialDisplayScope = `${initialRoute.spaceId ?? "none"}/${initialRoute.project ?? "all"}/${initialRoute.view}`;
  const [display, setDisplay] = useState<DisplayState>(() => loadDisplay(initialDisplayScope));
  const [displayOpen, setDisplayOpen] = useState(false);
  const [mobileNav, setMobileNav] = useState(false);
  /** Is the workspace rail collapsed? Only the shell's own header may answer
   *  that — a collapsed rail leaves ⌘B as the sole way back, which is a
   *  keystroke you have to already know. */
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  /** …and the other way the rail leaves the screen, which this state could not
   *  see: hidden by the drawer breakpoint rather than collapsed by a drag. */
  const railIsDrawer = useMediaQuery(RAIL_DRAWER_QUERY);
  /** Nothing on screen leads back to the rail. Both ways of losing it end here,
   *  because the reason the toggle exists is the same either way. */
  const railHidden = sidebarCollapsed || railIsDrawer;
  /** Is there room for the project console beside everything else? When there
   *  is not, the console does not render *and neither does the button that
   *  toggles it* — a control whose only effect is invisible is the same defect
   *  as a rail with nothing to bring it back. */
  const consoleFits = !useMediaQuery(CONSOLE_QUERY);
  const [personalNavRevision, setPersonalNavRevision] = useState(0);
  /** Bulk-selection checks, by canonical ref. Distinct from `selection`: the
   *  focus is one row, the checks are a set, and `x` is the socket. */
  const [checked, setChecked] = useState<ReadonlySet<string>>(new Set());
  const [bulkProgress, setBulkProgress] = useState<BulkProgress | null>(null);
  const bulkOperation = useRef<((reff: string) => Promise<unknown>) | null>(null);
  /** Which project's board is on screen. `null` = let the daemon pick, which at
   *  this call site means the only project there is — no branch hint reaches it
   *  and no default is configured for it. */
  const [project, setProject] = useState<string | null>(initialRoute.project);
  /** The picker a keybinding has asked for. Also an overlay: it owns the keymap. */
  const [field, setField] = useState<IssueField | null>(null);
  /** Doc-ids the daemon says qualify. `null` = the daemon wasn't asked, which is
   *  not the same as "nothing qualifies" — see core/filter.ts. */
  const [allowed, setAllowed] = useState<ReadonlySet<string> | null>(null);
  /** Tombstoned rows, fetched only while the display option shows them.
   *  Deleting an issue REMOVES it from `boards[P]` (the board genuinely does
   *  not know it), so the trash comes from `list all:true`, not the board. */
  const [deletedRows, setDeletedRows] = useState<Row[]>([]);
  const [mutationNotice, setMutationNotice] = useState("");
  /** Last doorbell epoch seen per space — the daemon-boot nonce (UI.md §5). */
  const epochs = useRef(new Map<string, number>());
  // Bumped on every doorbell for this space: the detail pane re-reads off it.
  const [revision, setRevision] = useState(0);
  const sidebar = usePanelRef();
  const detailPanel = usePanelRef();
  const [density, setDensity] = useState<DensityPreference>(() => loadDensity());
  /**
   * Appearance now lives in state because Astryx's `<Theme mode>` is the input
   * that drives it. Density deliberately does NOT: it is a cascade layer keyed
   * off `[data-density]`, so it costs one attribute write and no React render.
   * See `tool/generate-astryx-theme.mjs`.
   */
  const [theme, setThemeState] = useState<ThemePreference>(() => loadTheme());
  /**
   * The layout Issues were last drawn in.
   *
   * It has to outlive the route, or "remembering" it would only work while you
   * are already looking at it: leaving for Overview and coming back reads
   * `view` as `overview`, and any mode derived from the current route falls
   * back to the list. So it is state, updated whenever a layout is on screen —
   * the same shape as grouping and ordering, which is what it is.
   */
  const [issueLayout, setIssueLayout] = useState<IssueMode>("list");
  useEffect(() => {
    if (isIssueMode(view)) setIssueLayout(view);
  }, [view]);
  const projectStore = useProjectViewerStore();
  const boardSpace = isProjectView(view) ? current : null;
  const { board } = useProjectBoard(boardSpace, isProjectView(view) ? project : null);
  const labelsResource = useProjectRegistry(
    current ? projectKeys.labels(current) : "project:none/labels",
    useCallback(
      () => current ? projectStore.ensureLabels(current) : Promise.resolve([]),
      [current, projectStore],
    ),
  );
  const membersResource = useProjectRegistry(
    current ? projectKeys.members(current) : "project:none/members",
    useCallback(
      () => current ? projectStore.ensureMembers(current) : Promise.resolve([]),
      [current, projectStore],
    ),
  );
  const projectsResource = useProjectRegistry(
    current ? projectKeys.projects(current) : "project:none/projects",
    useCallback(
      () => current ? projectStore.ensureProjects(current) : Promise.resolve([]),
      [current, projectStore],
    ),
  );
  const statusResource = useProjectRegistry(
    current ? projectKeys.status(current) : "project:none/status",
    useCallback(
      () => current ? projectStore.ensureStatus(current) : Promise.resolve(null as never),
      [current, projectStore],
    ),
  );
  const standingResource = useProjectRegistry(
    current ? projectKeys.standing(current) : "project:none/standing",
    useCallback(
      () => current ? projectStore.ensureStanding(current) : Promise.resolve(null as never),
      [current, projectStore],
    ),
  );
  const labels = labelsResource.data ?? [];
  const projects = projectsResource.data ?? [];
  const statusInfo = (statusResource.data ?? null) as StatusInfo | null;
  const members = useMemo(() => {
    const source = membersResource.data ?? [];
    const nick = statusInfo?.nick.trim() ?? "";
    return nick
      ? source.map((member) => member.me && !member.alias ? { ...member, alias: nick } : member)
      : source;
  }, [membersResource.data, statusInfo?.nick]);
  /** Projects offered for navigation/creation. Archived ones stay cached but are
   * hidden from navigation until explicitly opened. */
  const liveProjects = useMemo(() => projects.filter((p) => !p.archived), [projects]);

  useEffect(() => {
    // Appearance is applied by `<Theme mode>`, not from here. Density still is:
    // it is a plain attribute and nothing else owns it.
    applyDensity(loadDensity());
  }, []);

  useEffect(() => {
    const prefetch = (event: Event) => {
      if (!current) return;
      const target = event.target instanceof Element
        ? event.target.closest<HTMLElement>("[data-issue-ref]")
        : null;
      const reff = target?.dataset.issueRef;
      if (reff) projectStore.prefetchIssue(current, reff);
    };
    document.addEventListener("pointerover", prefetch);
    document.addEventListener("focusin", prefetch);
    return () => {
      document.removeEventListener("pointerover", prefetch);
      document.removeEventListener("focusin", prefetch);
    };
  }, [current, projectStore]);
  const spacesRef = useRef(spaces);
  spacesRef.current = spaces;
  const routeSpaceRef = useRef(routeSpace);
  routeSpaceRef.current = routeSpace;
  const currentRef = useRef(current);
  currentRef.current = current;

  /** Apply browser history without waking a daemon or inventing local identity. */
  const applyRoute = useCallback((route: ViewerRoute) => {
    setRouteSpace(route.spaceId);
    const local = resolveLocalSpace(route.spaceId, spacesRef.current);
    setCurrent(local?.id ?? null);
    setProject(route.project);
    setView(route.view);
    setSelection(route.issue);
    setOpenSpec(route.spec ?? null);
    setOpenBaseline(route.baseline ?? null);
    setFilter(route.filter ?? EMPTY_FILTER);
    setDetail(route.issue !== null);
  }, []);

  useEffect(() => {
    const onPopState = () => applyRoute(parseRoute(window.location));
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, [applyRoute]);

  // Selection can be repaired after a board refresh and a multi-project space can
  // resolve its initial project asynchronously. Replace keeps the address honest
  // without turning those automatic corrections into Back-button destinations.
  //
  // `?issue=` means *this issue is open*, so it rides on `detail`, not on
  // `selection` alone: a highlighted row you have not opened is cursor position,
  // not a destination, and reloading its address must not put you inside it.
  useEffect(() => {
    const route = {
      spaceId: routeSpace,
      project,
      view,
      issue: detail ? selection : null,
      ...(openSpec ? { spec: openSpec } : {}),
      ...(openBaseline ? { baseline: openBaseline } : {}),
      filter,
    };
    const href = formatRoute(route);
    if (`${window.location.pathname}${window.location.search}` !== href) {
      window.history.replaceState(null, "", href);
    }
    saveLastRoute(route);
  }, [routeSpace, project, view, selection, detail, openSpec, openBaseline, filter]);

  // Settings is a page state, not a panel: its own left rail owns the hierarchy,
  // so the workspace sidebar steps aside while it's open and returns as you left
  // it on the way out. Only touches the desktop rail; the mobile drawer is modal
  // and already dismissed on navigation.
  const sidebarBeforeSettings = useRef<boolean | null>(null);
  useEffect(() => {
    const panel = sidebar.current;
    if (!panel || window.matchMedia(RAIL_DRAWER_QUERY).matches) return;
    if (view === "settings") {
      if (sidebarBeforeSettings.current === null) {
        sidebarBeforeSettings.current = panel.isCollapsed();
      }
      if (!panel.isCollapsed()) panel.collapse();
    } else if (sidebarBeforeSettings.current !== null) {
      if (!sidebarBeforeSettings.current && panel.isCollapsed()) panel.expand();
      sidebarBeforeSettings.current = null;
    }
    // `sidebar` is a stable ref; `view` is the trigger.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [view]);

  const space = spaces.find((s) => s.id === current) ?? null;
  const standing = (standingResource.data ?? null) as WhoamiInfo | null;
  /** Custody: this surface signs with a key it merely hosts (an agent's). */
  const custodyReadOnly = space ? isReadOnly(space) : false;
  /**
   * Standing: this node's own actor holds no write standing. `null` standing
   * (not yet resolved, or the probe failed) deliberately does NOT gate —
   * flashing a locked UI at a founder while whoami is in flight would be its
   * own lie; an actual denial still refuses at the engine.
   */
  const standingReadOnly = !custodyReadOnly && standing !== null && !standing.can_write;
  const readOnly = custodyReadOnly || standingReadOnly;

  // Safety net under the doorbell: a write refused for standing means our
  // cached standing may be stale (a revocation we haven't re-probed), so
  // re-resolve it — the gate then flips instead of every further affordance
  // failing one by one.
  useEffect(() => {
    if (error && current && classifyFailure(error) === "authorization") {
      void projectStore.ensureStanding(current, true).catch(() => undefined);
    }
  }, [error, current, projectStore]);
  const missingProject =
    isProjectView(view) &&
    project !== null &&
    projects.length > 0 &&
    !projects.some((candidate) => candidate.key === project);

  const { shown, optimistic } = useMemo(() => {
    if (!board) return { shown: null, optimistic: new Set<string>() as ReadonlySet<string> };
    return {
      shown: applyFilter(board, filter, allowed),
      optimistic: new Set(projectStore.overlay.docs()),
    };
  }, [allowed, board, filter, projectStore]);

  /** The list's arrangement (the board renders columns straight off `shown`). */
  const groups = useMemo(() => (shown ? groupRows(shown, display) : []), [shown, display]);

  // Motion follows what is *visible*, in the order it is visible: on the list,
  // j/k walks the display *groups*; on the board — which always lays out by
  // status regardless of the grouping option — it walks the columns. The trash
  // rows join the motion exactly when the display option shows them — a row you
  // can see but not land on is a trap.
  const rows: Row[] = useMemo(() => {
    const live =
      view === "board" && shown
        ? shown.columns.flatMap((c) => c.rows.filter((r) => !r.tombstone))
        : groups.flatMap((g) => g.rows.filter((r) => !r.tombstone));
    return display.deleted ? deletedRows : live;
  }, [view, shown, groups, display.deleted, deletedRows]);
  const favoriteProjects = useMemo(
    () => routeSpace ? loadFavoriteProjects(routeSpace) : [],
    [routeSpace, personalNavRevision],
  );
  const sidebarSavedViews = useMemo(
    () => routeSpace && project ? loadSavedViews(routeSpace, project) : [],
    [routeSpace, project, personalNavRevision],
  );
  const displayScope = `${routeSpace ?? "none"}/${project ?? "all"}/${view}`;
  const displayScopeRef = useRef(initialDisplayScope);

  useEffect(() => {
    if (displayScopeRef.current === displayScope) return;
    displayScopeRef.current = displayScope;
    setDisplay(loadDisplay(displayScope));
  }, [displayScope]);

  // Persisted per canonical project and surface: list grouping must not silently
  // rewrite the board's display contract, or another space's preference.
  useEffect(() => {
    saveDisplay(display, displayScopeRef.current);
  }, [display]);

  // `prefer` names a space that did not exist when this render started — the
  // one just founded or entered. The route ref cannot carry it: it is written
  // during render, so a caller that sets it and re-reads the catalog in the same
  // tick reads the value from before.
  const loadSpacesRaw = useCallback(async (prefer?: string) => {
    try {
      const { spaces } = await fetchSpaces();
      setSpaces(spaces);
      setError(null);
      setCurrent((cur) => {
        if (prefer) {
          const landed = resolveLocalSpace(prefer, spaces);
          if (landed) {
            setRouteSpace(landed.space);
            return landed.id;
          }
        }
        if (cur) return cur;
        const requested = routeSpaceRef.current;
        if (requested) {
          return resolveLocalSpace(requested, spaces)?.id ?? null;
        }
        // Attaching an agent brings that agent *online*, so auto-select only our
        // own single unambiguous space — never an agent.
        const mine = spaces.filter((s) => !isReadOnly(s));
        if (mine.length === 1 && mine[0]) {
          setRouteSpace(mine[0].space);
          return mine[0].id;
        }
        return null;
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const loadBoard = useCallback(async (id: string | null, proj: string | null): Promise<void> => {
    if (!id) return;
    try {
      await projectStore.ensureBoard(id, proj, true);
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [projectStore]);
  const loadSpaces = useCallback(() => loadSpacesRaw(), [loadSpacesRaw]);

  /**
   * Leave the catalog with nothing selected.
   *
   * `loadSpacesRaw` keeps whatever `current` already holds — it has to, or every
   * refresh would fight the person's choice — so a row that has just been
   * deregistered has to be let go here, *before* the reload. Otherwise `current`
   * names an id no longer in `spaces` and the shell renders a space that is not
   * on the list.
   */
  const deselectSpace = useCallback(() => {
    window.history.pushState(null, "", formatRoute(DEFAULT_ROUTE));
    setRouteSpace(null);
    setCurrent(null);
    setProject(null);
    setSelection(null);
  }, []);

  /**
   * Deregister one Orbit row.
   *
   * Navigation state only: `host_orbit_forget` never touches the store, and the
   * confirmation has to say so or it reads as a delete. What it does not say is
   * how to get the row back, because for a founder there is no in-app way —
   * entering re-registers a store that already holds its Space, but that needs
   * an invite link, and founding refuses an occupied directory.
   */
  const forgetSpace = useCallback(async (id: string) => {
    const row = spacesRef.current.find((space) => space.id === id);
    if (!row) return;
    const confirmed = await ask.confirm({
      title: `Forget ${row.name || row.space}?`,
      body: `This removes the space from the list on this device. The encrypted store at ${row.path} is left exactly as it is, and no other device is affected.`,
      confirmText: "Forget",
      danger: true,
    });
    if (!confirmed) return;
    try {
      await hostRpc({ cmd: "host_orbit_forget", selector: row.path });
      if (currentRef.current === id) deselectSpace();
      await loadSpacesRaw();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [deselectSpace, loadSpacesRaw]);

  /** Drop every row whose store is gone — the one remedy for a `missing` row. */
  const pruneSpaces = useCallback(async () => {
    const gone = spacesRef.current.filter((space) => space.status === "missing");
    if (gone.length === 0) return;
    const confirmed = await ask.confirm({
      title: gone.length === 1 ? "Remove 1 unavailable space?" : `Remove ${gone.length} unavailable spaces?`,
      // No "and it comes back if the store returns": the registry is only ever
      // written by founding and entering, so re-opening a store does not
      // re-register it. Promising a remedy this app does not have is worse than
      // saying nothing.
      body: `${gone.map((space) => space.name || space.space).join(", ")} — ${
        gone.length === 1 ? "the store this row names is" : "the store each row names is"
      } already gone from this machine. Removing ${
        gone.length === 1 ? "it" : "them"
      } clears the list; there is nothing left at ${gone.length === 1 ? "that path" : "those paths"} to delete.`,
      confirmText: "Remove",
      danger: true,
    });
    if (!confirmed) return;
    try {
      await hostRpc({ cmd: "host_orbit_prune" });
      if (gone.some((space) => space.id === currentRef.current)) deselectSpace();
      await loadSpacesRaw();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [deselectSpace, loadSpacesRaw]);

  // The project is read through a ref by the doorbell handler and the sweep, which
  // must not re-subscribe every time it changes.
  const projectRef = useRef(project);
  projectRef.current = project;

  useEffect(() => {
    void loadSpaces();
  }, [loadSpaces]);
  /**
   * Name a project once we know there is a choice.
   *
   * `Request::Board { project: null }` asks the daemon to resolve one, and its
   * chain is a **CLI** chain: the git branch's key → `project.default` → the only
   * project → a teaching error. A browser tab has no cwd and no branch, so on a
   * space with more than one project that chain reaches the error every time — and
   * the client sent `null` unconditionally, which is why a second project made the
   * board render "more than one project (ACME, DSN) — pass -p <KEY>" instead of
   * issues. The switcher in the header is only half the fix; this is the other.
   *
   * Left `null` for a single-project space on purpose: the chain resolves it fine,
   * and `project.default` keeps working for the one case a browser can honour it.
   * (Reading `project.default` outright is not possible from here — `config` is a
   * `Special` CLI handler, not a `Request`, so no HTTP endpoint reaches it.)
   */
  useEffect(() => {
    if (!isProjectView(view) || project !== null || projects.length === 0) return;
    setProject((projects.find((candidate) => !candidate.archived) ?? projects[0])!.key);
  }, [projects, project, view]);

  // The trash. Scoped to the board's project so the group matches the view,
  // re-read on every doorbell (a remote delete is exactly the news it carries).
  useEffect(() => {
    if (!current || !display.deleted) return setDeletedRows([]);
    let alive = true;
    void (async () => {
      try {
        const r = await rpc(current, {
          cmd: "list",
          project: board?.project.key ?? null,
          filter: { all: true },
        });
        if (alive && r.kind === "list") setDeletedRows(r.rows.filter((x) => x.tombstone));
      } catch {
        if (alive) setDeletedRows([]);
      }
    })();
    return () => {
      alive = false;
    };
  }, [current, display.deleted, board?.project.key, revision]);

  // `mine`/`label` are server truth: ask `list`, keep the doc-ids, intersect.
  useEffect(() => {
    if (!current || !needsServer(filter)) return setAllowed(null);
    let alive = true;
    void (async () => {
      try {
        const r = await rpc(current, {
          cmd: "list",
          project: null,
          filter: { mine: filter.mine, label: filter.label, all: true },
        });
        if (alive && r.kind === "list") setAllowed(new Set(r.rows.map((x) => x.doc_id)));
      } catch (e) {
        if (alive) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      alive = false;
    };
  }, [current, filter, revision]);

  // A selection that no longer exists (deleted, filtered away) must not linger.
  useEffect(() => {
    // Preserve a deep-linked issue until its board arrives. Treating the initial
    // empty rows array as authoritative would erase `?issue=…` before the first
    // request even had a chance to resolve it.
    if (!board || (view !== "list" && view !== "board")) return;
    // Keep an explicit unknown ref so the detail surface can say it is missing.
    // Redirecting it to the first row would rewrite a broken/shared link into a
    // different issue and make the failure invisible.
    setSelection((selected) => {
      if (display.deleted) {
        return selected && rows.some((row) => row.reff === selected) ? selected : null;
      }
      return selected ?? rows[0]?.reff ?? null;
    });
  }, [board, view, rows, display.deleted]);

  // Checks on rows that left the view are stale writes waiting to happen: a
  // bulk action must only ever touch what the user can currently see checked.
  useEffect(() => {
    setChecked((c) => {
      const live = new Set([...c].filter((reff) => rows.some((r) => r.reff === reff)));
      return live.size === c.size ? c : live;
    });
  }, [rows]);

  useEffect(() => {
    if (!toast) return;
    const t = window.setTimeout(() => setToast(null), 2400);
    return () => window.clearTimeout(t);
  }, [toast]);

  const liveness = useDoorbell(
    useCallback(
      (d) => {
        if (!d) {
          projectStore.overlay.clear();
          void loadSpaces();
          void loadBoard(current, projectRef.current);
          setRevision((r) => r + 1);
          return;
        }
        // `epoch` is a per-daemon-boot nonce: a change means that daemon
        // restarted, so our position in its stream is meaningless and nothing we
        // hold about the space is trustworthy — which is exactly what `reset`
        // says (UI.md §5). The server sends `reset` on the death it can see;
        // the epoch catches a restart it can't, where a daemon dies and returns
        // between two frames. Recorded for every space, not just the selected
        // one, so switching to a space doesn't compare against a stale nonce.
        const prev = epochs.current.get(d.space);
        epochs.current.set(d.space, d.epoch);
        const rebaseline = d.reset || (prev !== undefined && prev !== d.epoch);

        if (d.space !== current) return;
        void projectStore.handleDoorbell(rebaseline ? { ...d, reset: true } : d);
        setRevision((r) => r + 1);
        // On a rebaseline the space list is exactly as suspect as the board: a
        // daemon that restarted may have changed its own name, projects, or
        // whether it is up at all.
        if (rebaseline || d.invalidations.some((entry) => entry.planes.length > 0)) {
          void loadSpaces();
        }
      },
      [current, loadBoard, loadSpaces, projectStore],
    ),
  );

  /** The workflow, in board order — which is the order the work verbs resolve by. */
  const states: WorkflowState[] = useMemo(
    () => board?.columns.map((c) => c.state) ?? [],
    [board],
  );

  const rowsRef = useRef(rows);
  rowsRef.current = rows;
  const selRef = useRef(selection);
  selRef.current = selection;
  const statesRef = useRef(states);
  statesRef.current = states;
  const checkedRef = useRef(checked);
  checkedRef.current = checked;
  const membersRef = useRef(members);
  membersRef.current = members;
  // The *filtered* board: reordering has to land relative to a neighbour you can
  // actually see, or `J` jumps the card past rows a filter is hiding.
  const shownRef = useRef(shown);
  shownRef.current = shown;

  /** The selected row, or null. Read through refs so commands stay stable. */
  const selectedRow = useCallback(
    (): Row | null => rowsRef.current.find((r) => r.reff === selRef.current) ?? null,
    [],
  );

  /**
   * Predict, then send.
   *
   * The order is the point: the value is on screen before the request leaves, and
   * the doorbell — not a response — is what retires the guess. If the request is
   * refused we roll back immediately rather than wait for a doorbell that will
   * never come, because a refusal *is* the news.
   */
  const predict = useCallback(
    async (doc: string, field: Field, value: string, send: () => Promise<unknown>) => {
      setMutationNotice(`Saving ${field} on this device…`);
      try {
        await projectStore.predictValue(currentRef.current ?? "", doc, field, value, send);
        setMutationNotice(`${field} saved on this device`);
        return true;
      } catch (e) {
        setMutationNotice(`${field} was refused · local value restored`);
        if (!(e instanceof ConfirmRequired)) {
          setError(e instanceof LaitError ? e.message : String(e));
        }
        return false;
      }
    },
    [projectStore],
  );

  /** Writes never refetch — the daemon rings and the doorbell reloads. */
  const guard = useCallback(async (fn: () => Promise<unknown>) => {
    try {
      await fn();
    } catch (e) {
      if (e instanceof ConfirmRequired) return;
      setError(e instanceof LaitError ? e.message : String(e));
    }
  }, []);

  /** `predict`'s notice-and-rollback framing, for a store field method. */
  const commitField = useCallback(
    async (field: string, run: () => Promise<boolean>) => {
      setMutationNotice(`Saving ${field} on this device…`);
      try {
        await run();
        setMutationNotice(`${field} saved on this device`);
      } catch (e) {
        setMutationNotice(`${field} was refused · local value restored`);
        if (!(e instanceof ConfirmRequired)) {
          setError(e instanceof LaitError ? e.message : String(e));
        }
      }
    },
    [],
  );

  /**
   * The in-place field writes — one object, handed to the list, the board and
   * the calendar, so a chip on any of them commits through the same store
   * methods (and the same optimistic overlay) as the detail rail.
   */
  const issueMutators = useMemo<IssueMutators>(() => ({
    setStatus: (reff, status) =>
      void commitField("status", () =>
        projectStore.setStatus(currentRef.current ?? "", reff, status)),
    setPriority: (reff, priority) =>
      void commitField("priority", () =>
        projectStore.setPriority(currentRef.current ?? "", reff, priority)),
    toggleAssignee: (reff, key, add) =>
      void commitField("assignee", () =>
        projectStore.toggleAssignee(currentRef.current ?? "", reff, key, add)),
    toggleLabel: (reff, name, add) =>
      void commitField("label", () =>
        projectStore.toggleLabel(currentRef.current ?? "", reff, name, add)),
    swapLabel: (reff, from, to) =>
      void commitField("label", () =>
        projectStore.swapLabel(currentRef.current ?? "", reff, from, to)),
    setDue: (reff, due) =>
      void commitField("due date", () =>
        projectStore.setDue(currentRef.current ?? "", reff, due)),
    setEstimate: (reff, estimate) =>
      void commitField("estimate", () =>
        projectStore.setEstimate(currentRef.current ?? "", reff, estimate)),
  }), [commitField, projectStore]);

  /**
   * One request per checked issue with a small concurrency ceiling. Independent
   * refusals do not stop the run, and the itemized result targets retry precisely.
   */
  const bulk = useCallback(
    async (
      fn: (reff: string) => Promise<unknown>,
      retryRefs?: readonly string[],
    ) => {
      const requested = retryRefs
        ? new Set(retryRefs)
        : checkedRef.current;
      const targets = rowsRef.current.filter((row) => requested.has(row.reff));
      if (!targets.length) return null;
      bulkOperation.current = fn;
      setBulkProgress({
        done: 0,
        total: targets.length,
        pending: true,
        successes: [],
        failures: [],
      });
      const result = await runBounded(
        targets,
        (row) => fn(row.reff),
        3,
        (done, total) =>
          setBulkProgress((currentProgress) => ({
            done,
            total,
            pending: true,
            successes: currentProgress?.successes ?? [],
            failures: currentProgress?.failures ?? [],
          })),
      );
      const failures = result.failures.map(({ item, message }) => ({
        reff: item.reff,
        label: item.key_alias ?? item.reff,
        message,
      }));
      const finalProgress: BulkProgress = {
        done: targets.length,
        total: targets.length,
        pending: false,
        successes: result.successes.map((row) => row.reff),
        failures,
      };
      setBulkProgress(finalProgress);
      if (!failures.length) {
        window.setTimeout(() => setBulkProgress(null), 1600);
      }
      return finalProgress;
    },
    [],
  );

  const api: AppApi = useMemo(
    () => ({
      openPalette: () => setModal("palette"),
      openIssueSearch: () => setModal("issueSearch"),
      closePalette: () => setModal(null),
      toggleShortcuts: () => setModal((m) => (m === "shortcuts" ? null : "shortcuts")),
      toggleDetail: () => setDetail((d) => !d),
      /**
       * The one navigation verb: go to a view, optionally naming the project.
       *
       * There used to be three — `goto(view)`, `pickProject(key)` and
       * `openProjectView(key, view)` — which is one function's worth of work
       * split by which half of the destination the caller happened to know.
       * They had drifted: only one of them dropped the `mine` filter, only one
       * closed an open issue, and `pickProject` silently kept whichever view you
       * were last on, so "open Beacon" from the Projects page could land on a
       * calendar. Naming both halves in one call makes the destination the
       * argument instead of the choice of function.
       *
       * Omitting `toProject` keeps the project you are in (a workspace
       * destination clears it); passing `null` clears it explicitly.
       */
      goto: (v, toProject) => {
        const nextProject = isProjectView(v)
          ? (toProject !== undefined
              ? toProject
              : (project ?? board?.project.key ?? liveProjects[0]?.key ?? null))
          : null;
        // "My issues" is a destination, not a sticky scope: opening a project by
        // name drops the `mine` authorization filter so its board doesn't come up
        // mysteriously empty. Other facets (status/label/…) ride along.
        const scoped = toProject && filter.mine ? { ...filter, mine: false } : filter;
        window.history.pushState(
          null,
          "",
          formatRoute({ spaceId: routeSpace, project: nextProject, view: v, issue: null, filter: scoped }),
        );
        setProject(nextProject);
        setView(v);
        // Asking for the Board means the board, not the issue you were reading
        // drawn over it. The selection survives so returning to the list keeps
        // your place; the reading surface does not follow you between views.
        setSelection(null);
        setDetail(false);
        // Same rule, same reason: the Specs tab means the register. It is also
        // the way back out of a document, which a tab that returned you to
        // whatever you were last reading would not be.
        setOpenSpec(null);
        setOpenBaseline(null);
        setComposingSpec(null);
        if (scoped !== filter) setFilter(scoped);
      },
      /**
       * A milestone is a filter, not a destination.
       *
       * So this stays on whatever issue surface you were reading if that surface
       * can be narrowed — clicking a milestone while working the board should
       * narrow the board, not throw you into a list. Only Overview and Activity,
       * which draw no rows, hand you to Issues.
       */
      gotoMilestone: (toProject, milestone) => {
        const next = { ...filter, milestone };
        const nextView = carriesFilter(view) ? view : "list";
        const nextProject = toProject ?? project;
        window.history.pushState(
          null,
          "",
          formatRoute({
            spaceId: routeSpace,
            project: nextProject,
            view: nextView,
            issue: null,
            filter: next,
          }),
        );
        setProject(nextProject);
        setView(nextView);
        setSelection(null);
        setDetail(false);
        setFilter(next);
      },
      openFilter: () => {
        setFilterOpen(true);
        setFocusToken((t) => t + 1);
      },
      clearFilter: () => {
        window.history.replaceState(
          null,
          "",
          formatRoute({ spaceId: routeSpace, project, view, issue: selection, filter: EMPTY_FILTER }),
        );
        setFilter(EMPTY_FILTER);
      },
      toggleSidebar: () => {
        if (window.matchMedia(RAIL_DRAWER_QUERY).matches) {
          setMobileNav((open) => !open);
          return;
        }
        const p = sidebar.current;
        if (!p) return;
        if (p.isCollapsed()) p.expand();
        else p.collapse();
      },
      toast: (m) => setToast(m),
      refresh: () => {
        void loadSpaces();
        void loadBoard(current, projectRef.current);
        setToast("Refreshed");
      },
      // Row motion can update selection many times a second. It remains directly
      // linkable, but replaces the current entry rather than polluting Back with
      // every arrow-key stop — the address effect above does that write, and is
      // the single place that decides whether a selection is in the URL at all.
      select: (reff) => {
        if (routeSpace && reff) {
          rememberIssue(routeSpace, reff);
          setPersonalNavRevision((revision) => revision + 1);
        }
        setSelection(reff);
      },
      predict: (doc, field, value, send) => predict(doc, field, value, send),
      pickSpace: (id) => {
        const picked = spacesRef.current.find((space) => space.id === id);
        if (!picked) return;
        const next = { spaceId: picked.space, project: null, view: "projects" as const, issue: null };
        window.history.pushState(null, "", formatRoute(next));
        setRouteSpace(picked.space);
        setCurrent(picked.id);
        setProject(null);
        setView("projects");
        setSelection(null);
      },
      // A picker needs its subject visible: opening the assignee menu over a pane
      // you closed is a menu with no context.
      openField: (f) => {
        setDetail(true);
        setField(f);
      },

      /**
       * A work verb: one `Request`, one commit.
       *
       * Only `status` is predicted. The verbs also bundle assignment in the same
       * commit (`start` takes the issue, `stop` puts it down), but `Row` carries
       * `assignee_summary` — a string the *daemon* derives ("you", "alice +1") —
       * and re-deriving it here to predict it would be a second implementation of a
       * server rule for the sake of one frame. The doorbell brings the real one.
       */
      work: (action) => {
        const row = selectedRow();
        if (!row || !current) return;
        const cmd = `issue_${action}` as const;
        const target = workTarget(statesRef.current, action);
        if (!target) {
          // No state in that category — the daemon refuses with a better sentence
          // than we could write. Send it and show its words.
          void guard(() => rpc(current, { cmd, reff: row.reff }));
          return;
        }
        void predict(row.doc_id, "status", target.id, () =>
          rpc(current, { cmd, reff: row.reff }),
        );
      },

      /** `H`/`L` — the neighbouring workflow column. Clamps at both ends. */
      shiftStatus: (delta) => {
        const row = selectedRow();
        if (!row || !current) return;
        const next = neighbourState(statesRef.current, row.status, delta);
        if (!next) return;
        void predict(row.doc_id, "status", next.id, () =>
          rpc(current, { cmd: "issue_edit", reff: row.reff, status: next.id }),
        );
      },

      /**
       * `J`/`K` — reorder within the column.
       *
       * Position is `Catalog.boards[P]`'s to decide (A§9) and is not a field `Row`
       * carries, so there is nothing to predict: the doorbell repaints. Against a
       * daemon on a Unix socket that is a few milliseconds.
       *
       * Refused in a Done column, and that is not a nicety. Entering a done-category
       * status **removes the doc from `boards[P]`** (`replica.rs:858-869`); done
       * columns are rendered by the append rule instead, sorted `created_at desc`.
       * So a reorder there mutates a list the column isn't drawn from — the request
       * succeeds, the daemon rings, and the card lands exactly where it was. Doing
       * nothing is the honest outcome.
       */
      reorder: (delta) => {
        const row = selectedRow();
        const shownBoard = shownRef.current;
        if (!row || !current || !shownBoard) return;
        const col = shownBoard.columns.find((c) => c.state.id === row.status);
        if (!col || col.state.category === "done") return;

        const visible = col.rows.filter((r) => !r.tombstone);
        const i = visible.findIndex((r) => r.reff === row.reff);
        const target = visible[i + delta];
        if (i < 0 || !target) return;

        void guard(() =>
          rpc(current, {
            cmd: "issue_move",
            reff: row.reff,
            pos: delta < 0 ? { at: "before", reff: target.reff } : { at: "after", reff: target.reff },
          }),
        );
      },

      yankRef: () => {
        const row = selectedRow();
        if (!row) return;
        // The friendly handle if it has one — that is what a human pastes into a
        // branch name or a commit message.
        const ref = row.key_alias ?? row.reff;
        void navigator.clipboard
          .writeText(ref)
          .then(() => setToast(`Copied ${ref}`))
          .catch(() => setError("Clipboard blocked by the browser"));
      },
      moveSelection: (delta) => {
        const list = rowsRef.current;
        if (!list.length) return;
        const i = list.findIndex((r) => r.reff === selRef.current);
        const next = list[Math.max(0, Math.min(list.length - 1, (i < 0 ? 0 : i) + delta))];
        if (next) setSelection(next.reff);
      },
      createIssue: () => setComposing({}),
      createProject: () => setComposingProject(true),
      deleteIssue: (reff) =>
        void guard(async () => {
          if (!current) return;
          try {
            await rpc(current, { cmd: "issue_delete", reff });
          } catch (e) {
            // The engine hands back the CLI's own question rather than us
            // inventing one, so modal and terminal cannot disagree on the stakes.
            if (e instanceof ConfirmRequired) {
              // The engine's own words, in our dialog.
              if (await ask.confirm({ title: e.question, confirmText: "Delete", danger: true })) {
                await rpc(current, { cmd: "issue_delete", reff }, { confirm: true });
              }
              return;
            }
            throw e;
          }
        }),

      restoreIssue: (reff) => {
        if (!current) return;
        // `issue_restore` on a live issue still writes a "restored" event, so
        // refusing here keeps the history honest rather than politely noisy.
        const row = rowsRef.current.find((r) => r.reff === reff);
        if (row && !row.tombstone) return setToast("Not deleted");
        void guard(() => rpc(current, { cmd: "issue_restore", reff }));
      },

      /** Toggle, not set: `i` on an issue you hold puts it down (Linear's `I`
       *  self-assigns; the toggle is what a second press should honestly mean). */
      assignMe: () => {
        const row = selectedRow();
        const me = membersRef.current.find((m) => m.me);
        if (!row || !current || !me) return;
        const add = !row.assignees.includes(me.key);
        void guard(() => rpc(current, { cmd: "assign", reff: row.reff, who: [me.key], add }));
      },

      /** Column top/bottom. Same done-column refusal as `reorder`, same reason. */
      moveTo: (pos) => {
        const row = selectedRow();
        const shownBoard = shownRef.current;
        if (!row || !current || !shownBoard) return;
        const col = shownBoard.columns.find((c) => c.state.id === row.status);
        if (!col || col.state.category === "done") return;
        void guard(() => rpc(current, { cmd: "issue_move", reff: row.reff, pos: { at: pos } }));
      },

      toggleCheck: () => {
        const row = selectedRow();
        if (!row) return;
        setChecked((c) => {
          const next = new Set(c);
          if (!next.delete(row.reff)) next.add(row.reff);
          return next;
        });
      },
      checkAll: () => setChecked(new Set(rowsRef.current.map((r) => r.reff))),
      clearChecks: () => setChecked(new Set()),
      openDisplay: () => setDisplayOpen(true),
      openWorkflow: () => setModal("workflow"),
      openRoles: () => setModal("roles"),
      setTheme: (theme) => {
        setThemeState(theme);
        persistTheme(theme);
      },
    }),
    [
      applyRoute,
      current,
      routeSpace,
      project,
      view,
      selection,
      board,
      liveProjects,
      filter,
      guard,
      loadBoard,
      loadSpaces,
      predict,
      selectedRow,
    ],
  );

  // Deep-link / automation hook: a `lait:nav` CustomEvent drives navigation
  // without a synthetic DOM click. The handler DEFERS the actual navigation to a
  // fresh task, so the dispatcher's call stack (e.g. a headless `eval`) unwinds
  // before React re-renders — a re-render *inside* an eval detaches its execution
  // context, which is why clicking a nav item from automation is unreliable but a
  // dispatched event is not. Harmless in normal use: nothing dispatches it.
  //   window.dispatchEvent(new CustomEvent("lait:nav", { detail: { view, project, issue } }))
  useEffect(() => {
    const onNav = (event: Event) => {
      const detail = ((event as CustomEvent).detail ?? {}) as {
        view?: View;
        project?: string | null;
        issue?: string | null;
        milestone?: string | null;
      };
      setTimeout(() => {
        if (typeof detail.view === "string") api.goto(detail.view);
        if ("project" in detail) api.goto(isProjectView(view) ? view : "overview", detail.project ?? null);
        // After `project`, so `{ project, milestone }` in one detail scopes the
        // project it just named rather than the one you were on.
        if ("milestone" in detail) api.gotoMilestone(detail.project ?? null, detail.milestone ?? null);
        if ("issue" in detail) api.select(detail.issue ?? null);
      }, 0);
    };
    window.addEventListener("lait:nav", onNav as EventListener);
    return () => window.removeEventListener("lait:nav", onNav as EventListener);
  }, [api]);

  const ctx: Ctx = useMemo(
    () => ({
      view,
      spaceId: current,
      readOnly,
      selection,
      checkedCount: checked.size,
      // An open picker owns the keymap exactly as the palette does: `j` in the
      // assignee menu is cmdk's, not the board's.
      overlay: modal !== null || field !== null,
      app: api,
    }),
    [view, current, readOnly, selection, checked, modal, field, api],
  );

  /**
   * A card was dropped: set its status, then place it.
   *
   * **Two requests, and there is no way to make it one.** `issue_edit` carries
   * `status` but no position; `issue_move` carries `project` and `pos` but no
   * status. So a cross-column drag is two commits and two activity rows — the same
   * wrinkle the composer already documents for "file into a non-default column".
   * That is an honest record of what happened (moved, then placed) rather than a
   * fiction, and the alternative is a `Request` variant that does not exist.
   *
   * The **order is load-bearing**. Status first, position second:
   *
   * - Moving *into* a done status removes the doc from `boards[P]`; moving *out of*
   *   one re-inserts it at the top (`replica.rs:858-869`). Doing the placement first
   *   would have that re-insert stomp the position we just asked for.
   * - Dropping into a done column sends **no** `issue_move` at all (`pos` is null):
   *   done columns are rendered by the append rule and ignore the movable list, so
   *   the write would be invisible at best and a lie about ordering at worst.
   *
   * Only status is predicted — `applyOverlay` re-buckets the row into the new
   * column immediately, which is the part that has to feel instant. Position is not
   * a field `Row` carries, so it settles on the doorbell a few milliseconds later.
   */
  const dropCard = useCallback(
    (reff: string, status: string, pos: BoardPos | null) => {
      const id = currentRef.current;
      if (!id) return;
      const row = rowsRef.current.find((r) => r.reff === reff);
      if (!row) return;

      const changingStatus = row.status !== status;
      if (!changingStatus && !pos) return; // dropped where it already was

      const send = async () => {
        if (changingStatus) await rpc(id, { cmd: "issue_edit", reff, status });
        if (pos) await rpc(id, { cmd: "issue_move", reff, pos });
      };

      if (changingStatus) {
        void predict(row.doc_id, "status", status, send);
      } else {
        void guard(send);
      }
    },
    [guard, predict],
  );

  const pending = useKeys(ctx);
  const detailVisible = Boolean(
    detail &&
    selection &&
    current &&
    routeSpace &&
    board &&
    (view === "list" || view === "board" || view === "calendar"),
  );
  // Keep one panel topology for every view. The old two-id declaration mounted a
  // third panel only for issue-capable views, so the library rebalanced the
  // sidebar whenever that panel entered or left. Programmatic collapse/expand is
  // intentionally not persisted; only the user's resize choices are.
  const layout = useDefaultLayout({
    id: "lait.layout.v2",
    panelIds: LAYOUT_PANEL_IDS,
    onlySaveAfterUserInteractions: true,
  });

  /**
   * An open issue is a *view*, not an overlay and no longer a pane.
   *
   * It draws in the work area, beside the sidebar, exactly like the list and the
   * board — so the shell stays navigable while you read. It used to be a `fixed
   * inset-0` sheet, which took the sidebar with it and made the one surface
   * people dwell on the one they could not leave except by closing it.
   *
   * It also used to have a narrower twin: a third panel beside the list, with a
   * button to swap between the two. Two ways to read the same issue meant every
   * surface that opened one had to pick, and they did not pick alike — a board
   * card opened the pane, a list row opened full width. One reading surface, so
   * there is nothing left to pick.
   */
  // `founding` wins for the same reason it does over `projectShell`: the branch
  // that draws an open issue full width also hides the column the formation
  // surface lives in, so asking for it from an open issue would blank the app.
  const fullWidthDetail = detailVisible && founding === null;

  // The third panel survives so the layout keeps one topology for every view —
  // declaring it conditionally made the library rebalance the sidebar whenever it
  // came and went — but nothing is drawn in it, so it stays shut. Layout effects
  // run before paint, so its stored width never flashes.
  useLayoutEffect(() => {
    detailPanel.current?.collapse();
  }, [detailPanel, detailVisible]);

  useEffect(() => {
    registry.validate();
  }, []);

  // A prediction whose request neither errored nor rang is stuck: a dropped fetch,
  // a suspended tab.
  //
  // Sweeping is only half the job. Dropping the guess leaves the **pre-write**
  // value on screen with the uncertainty mark removed — the server's stale answer,
  // now presented as confirmed. That is worse than the guess was: at least the
  // guess admitted it was one. So a sweep re-reads.
  //
  // Deps are `[loadBoard]`, which is stable. Keying this on `predicted` tore the
  // interval down and rebuilt it on every prediction, so steady editing or a busy
  // doorbell stream could reset the timer indefinitely and it would never fire —
  // the one thing it exists to do.
  useEffect(() => {
    const t = window.setInterval(() => {
      if (!currentRef.current || !projectStore.expirePredictions(currentRef.current)) return;
      setMutationNotice("Local confirmation was delayed; refreshing authoritative state");
      void loadBoard(currentRef.current, projectRef.current);
      setRevision((r) => r + 1);
    }, PREDICTION_TTL_MS / 2);
    return () => window.clearInterval(t);
  }, [loadBoard, projectStore]);

  useEffect(() => {
    if (!mutationNotice || mutationNotice.startsWith("Saving")) return;
    const timeout = window.setTimeout(() => setMutationNotice(""), 3200);
    return () => window.clearTimeout(timeout);
  }, [mutationNotice]);

  const run = (id: string) => void registry.get(id)?.run(ctx);
  const openMyIssues = () => {
    window.history.pushState(
      null,
      "",
      formatRoute({ spaceId: routeSpace, project: null, view: "my-issues", issue: null }),
    );
    setProject(null);
    setView("my-issues");
    setSelection(null);
    setFilter(EMPTY_FILTER);
  };
  const openRecentIssue = (reff: string) => {
    const key = /^([A-Z][A-Z0-9]*)-\d+$/.exec(reff)?.[1] ?? project;
    window.history.pushState(null, "", formatRoute({ spaceId: routeSpace, project: key, view: "list", issue: reff, filter }));
    setProject(key);
    setView("list");
    setSelection(reff);
    setDetail(true);
  };
  const applySavedView = (saved: SavedView) => {
    const nextView = saved.view ?? "list";
    window.history.pushState(null, "", formatRoute({ spaceId: routeSpace, project, view: nextView, issue: null, filter: saved.filter }));
    setView(nextView);
    setSelection(null);
    setFilter(saved.filter);
    setDisplay(saved.display);
  };
  const toggleFavorite = (key: string) => {
    if (!routeSpace) return;
    toggleFavoriteProject(routeSpace, key);
    setPersonalNavRevision((revision) => revision + 1);
  };

  const activeProject =
    board?.project ?? projects.find((candidate) => candidate.key === project) ?? null;
  // The formation surface is not a project face. Without `founding` here, asking
  // for it from an open board would draw the project toolbar and the milestone
  // rail around a form for a space that does not exist yet.
  const projectShell =
    founding === null && isProjectView(view) && Boolean(project || activeProject);
  // The open project's milestones, for the filter menu's Milestone facet. The
  // same resource the overview and the issue rail read — one fetch for all three.
  const milestones = useProjectMilestones(current ?? "", activeProject?.id).data ?? [];
  const projectCounts = useMemo(() => {
    const counts = { backlog: 0, active: 0, done: 0, total: 0 };
    for (const column of board?.columns ?? []) {
      const count = column.rows.filter((row) => !row.tombstone).length;
      counts[column.state.category] += count;
      counts.total += count;
    }
    return counts;
  }, [board]);
  const projectDocIds = useMemo(
    () => new Set(board?.columns.flatMap((column) => column.rows.map((row) => row.doc_id)) ?? []),
    [board],
  );

  /**
   * The header trail names *what you are looking at* and its containers — never
   * the view mode. Inside a project the tab strip below already says Overview vs
   * Issues vs Board, so the trail stops at the project; a workspace destination
   * has no container to climb to, so it is its own root, wearing the sidebar's
   * icon for it. Every ancestor navigates; the leaf never does.
   *
   * The space itself is deliberately *not* a crumb. lait has no team layer under
   * it (Linear's first crumb is a team, not a workspace), so it would be a
   * constant on every surface — and one the sidebar already holds, permanently,
   * one row to the left. Settings is the exception, there the sidebar is gone.
   */
  /**
   * An open issue is the last hop of the same trail, not the start of a new one.
   *
   * The row is enough to name it — key and title are both on the board — so the
   * shell draws the crumb without waiting for the issue document to load, and the
   * bar never flickers between "Beacon" and "Beacon › BEACON-8" on the way in.
   */
  const openRow = fullWidthDetail
    ? (rows.find((row) => row.reff === selection) ??
      deletedRows.find((row) => row.reff === selection) ??
      null)
    : null;

  /**
   * The face of the project is the STRIP's to name, not the trail's — and this
   * has now gone both ways, so the reason matters more than the answer.
   *
   * The trail took the view when the strip was removed, on the grounds that the
   * sidebar's project tree already switched faces and a strip was a second
   * switcher for a settled choice. That was true of a project you *visit*. It is
   * not true of a project you are *inside*: the rail persists across the faces
   * now, a milestone click narrows the issue list without leaving the shell, and
   * the sidebar has no way to say "this project, Issues, scoped to M1". The
   * strip does, so the strip has it back and the trail stops at the project.
   *
   * An open issue is still a crumb — it is a different document, not a face.
   */
  /** The layout showing, when one is — `null` on Overview, Activity and the
   *  workspace destinations. Narrowed once so every gate below reads the same. */
  const issueMode = isIssueMode(view) ? view : null;
  /** The Spec being read, when one is. Its own resource, so a deep link resolves
   *  the title for the trail without the register having loaded. */
  const readingSpec = useSpec(current ?? "", view === "specs" ? openSpec : null).data ?? null;
  const belowProject = Boolean(openRow) || Boolean(readingSpec);

  const trail: BreadcrumbItem[] = projectShell
    ? [
        // A project that has become an ancestor climbs, and a crumb that climbs
        // is a link — so it stops being the switcher. Wrapping the picker in a
        // link would nest one control inside another; offering both would ask
        // which of two things a single click meant.
        liveProjects.length > 1 && !belowProject
          ? {
              key: "project",
              control: true,
              content: (
                <Combobox
                  tone="quiet" size="sm" swatchSlot="md"
                  label="Project"
                  swatchShape="square"
                  className="max-w-[min(32cqw,240px)] font-medium"
                  value={
                    activeProject
                      ? {
                          id: activeProject.key,
                          label: activeProject.name,
                          swatch: catalogColor(activeProject.color),
                        }
                      : null
                  }
                  options={[
                    ...liveProjects,
                    ...(activeProject &&
                    !liveProjects.some((candidate) => candidate.key === activeProject.key)
                      ? projects.filter((candidate) => candidate.key === activeProject.key)
                      : []),
                  ].map((candidate) => ({
                    id: candidate.key,
                    label: candidate.name,
                    swatch: catalogColor(candidate.color),
                    hint: candidate.key,
                  }))}
                  onPick={(key) => api.goto(isProjectView(view) ? view : "overview", key)}
                />
              ),
            }
          : {
              key: "project",
              content: (
                <ProjectCrumb
                  name={activeProject?.name ?? project ?? "Project"}
                  color={activeProject ? catalogColor(activeProject.color) : undefined}
                />
              ),
              // Only once it has something below it: a lone crumb is where you
              // already are, and `Breadcrumbs` never lets the leaf navigate.
              // The project's own hop is its home, not the view you came in on.
              ...(belowProject && (activeProject?.key ?? project)
                ? {
                    onNavigate: () =>
                      api.goto("overview", (activeProject?.key ?? project)!),
                  }
                : {}),
            },
      ]
    : [
        {
          key: view,
          content: (
            <DestinationCrumb
              icon={
                DESTINATION_ICON[view as keyof typeof DESTINATION_ICON] ??
                DESTINATION_ICON.workspace
              }
              label={workspaceTitle(view)}
            />
          ),
        },
      ];


  if (openRow) {
    trail.push({
      key: openRow.reff,
      content: <IssueCrumb id={openRow.key_alias ?? openRow.reff} title={openRow.title} />,
    });
  }

  if (readingSpec) {
    trail.push({
      key: readingSpec.spec,
      content: <SpecCrumb kind={SPEC_KIND_LABEL[readingSpec.kind]} title={readingSpec.title} />,
    });
  }

  /** The open issue. It has one home: the work area, at full width. */
  const issuePane =
    detailVisible && selection && current && routeSpace && board ? (
      rows.some((row) => row.reff === selection) ||
      deletedRows.some((row) => row.reff === selection) ? (
        <IssueDetail
          // Remount on a different issue: a stale draft must not survive into
          // the next one, and `key` says that in one line.
          key={selection}
          spaceId={current}
          canonicalSpaceId={routeSpace}
          reff={selection}
          states={states}
          members={members}
          labels={labels}
          projects={projects}
          readOnly={readOnly}
          // A deleted issue is not on the board at all, so the trash rows
          // are the only place its tombstone can be read from.
          tombstone={deletedRows.some((r) => r.reff === selection)}
          openField={field}
          onOpenField={setField}
          onError={setError}
          onDelete={api.deleteIssue}
          onPredict={api.predict}
          onNavigate={api.select}
          // A brief names the documents it is derived from, and following one
          // is a hop to a different surface — so it goes through `goto` rather
          // than opening a Spec inside an issue.
          onOpenSpec={(spec) => {
            api.goto("specs");
            setOpenSpec(spec);
          }}
          onClose={() => {
            api.select(null);
            setDetail(false);
          }}
          {...(rows.findIndex((row) => row.reff === selection) > 0
            ? {
                onPrevious: () =>
                  api.select(rows[rows.findIndex((row) => row.reff === selection) - 1]!.reff),
              }
            : {})}
          {...(rows.findIndex((row) => row.reff === selection) >= 0 &&
          rows.findIndex((row) => row.reff === selection) < rows.length - 1
            ? {
                onNext: () =>
                  api.select(rows[rows.findIndex((row) => row.reff === selection) + 1]!.reff),
              }
            : {})}
        />
      ) : (
        <EmptyState
          kind="unavailable"
          title="Issue not found in this local project"
          body={`${selection} is not present in the current local projection. It may belong to another project, still be arriving, or not exist on this replica.`}
          action={<Button
                    onClick={() => api.select(null)}
                    label="Clear selection"
                    variant="ghost"
                    size="sm"
                  />}
        />
      )
    ) : null;

  return (
    <Theme theme={laitTheme} mode={theme}>
    <HeaderSlotProvider>
    <Group
      orientation="horizontal"
      // Persisted per-user: a sidebar width you set once should survive a reload,
      // and the library already owns that — no state of ours to get wrong.
      {...layout}
      className="flex h-full"
    >
      <Panel
        id="sidebar"
        panelRef={sidebar}
        defaultSize="18%"
        minSize="180px"
        maxSize="32%"
        collapsible
        collapsedSize={0}
        groupResizeBehavior="preserve-pixel-size"
        onResize={(size) => setSidebarCollapsed(size.inPixels === 0)}
        className="bg-sunken max-[768px]:hidden"
      >
        <Sidebar
          spaces={spaces}
          current={current}
          projects={liveProjects}
          currentProject={board?.project.key ?? project}
          view={view}
          unread={unread}
          currentName={statusInfo?.name}
          favoriteProjects={favoriteProjects}
          savedViews={sidebarSavedViews}
          onPickSpace={api.pickSpace}
          onSearch={() => run("search.issues")}
          onOpenProjectView={(key, next) => api.goto(next, key)}
          onGo={api.goto}
          onMyIssues={openMyIssues}
          onApplySavedView={applySavedView}
          onToggleFavorite={toggleFavorite}
          onCreateProject={api.createProject}
          onAddSpace={() => setFounding("found")}
          onForgetSpace={(id) => void forgetSpace(id)}
          onPruneSpaces={() => void pruneSpaces()}
        />
      </Panel>

      {/* A 1px seam with a 7px hit area: thin to look at, big enough to grab. */}
      <Separator
        className={
          view === "settings"
            ? "pointer-events-none invisible relative w-px max-[768px]:hidden"
            : "bg-line data-[state=dragging]:bg-accent hover:bg-accent/60 relative w-px outline-none transition-colors max-[768px]:hidden"
        }
      >
        <span className="absolute inset-y-0 -left-[3px] w-[7px]" />
      </Separator>

      <Panel
        id="main"
        role="main"
        // On the board, the whole pane — breadcrumb bar and tab strip included —
        // stands on the raised canvas the columns are sunk into. A seam at the
        // toolbar's edge would split one surface into chrome-over-content; the
        // headers belong to the body they act on.
        className={`flex min-w-0 flex-col${view === "board" && !fullWidthDetail ? " bg-raised" : ""}`}
      >
        {/* One header for every view, mounted once and never swapped. Opening an
            issue extends the trail and hands the actions slot to the issue; it
            does not build a second bar with its own inset, its own controls and
            its own idea of where the title sits. Only Settings stands outside —
            it hides the sidebar, so it draws its own shell entirely. */}
        <div className={view === "settings" ? "hidden" : "shrink-0"}>
          <SurfaceHeader
            // No standing leading control — the bar's first ink is the thing
            // you are looking at, and a permanent toggle was window furniture
            // sitting where the trail should start. It returns for exactly the
            // states that need it: with the rail off screen there is nothing
            // that brings it back, and ⌘B only helps someone who already knows.
            // It leaves again the moment the rail is open.
            //
            // `railHidden`, not `sidebarCollapsed`: a narrow window hides the
            // rail with CSS, which the panel library never reports, so gating on
            // the collapse alone left every window under the drawer breakpoint
            // with no navigation and no way to ask for any.
            leading={
              // Its own space, so it reads as chrome acting on the window
              // rather than the first crumb of the trail.
              railHidden ? (
                <IconButton
                  label="Show sidebar"
                  tooltip="Show sidebar  ⌘B"
                  className="mr-2"
                  onClick={api.toggleSidebar}
                  variant="ghost"
                  size="sm"
                  icon={<PanelLeft className="size-icon-sm" />}
                />
              ) : undefined
            }
            trail={<Breadcrumbs items={trail} />}
            actions={
              // An open issue owns this end of the bar. Its buttons arrive from
              // `IssueDetail` through the outlet, so the shell does not need to
              // know what a duplicate or a restore is.
              fullWidthDetail ? (
                <HeaderActionsOutlet />
              ) : (
            <>
              {!projectShell && (
                <TrustPopover
                  liveness={liveness}
                  status={statusInfo}
                  space={space}
                  localReady={
                    statusInfo !== null &&
                    statusInfo.membership !== "pending" &&
                    statusInfo.counts_unavailable !== true
                  }
                  latestChange={mutationNotice}
                />
              )}

            </>
              )
            }
          />
          {/* The only band under the header. Filtering used to add a second one
              beneath it for as long as the filter was engaged; it is a panel
              now, so the chrome no longer changes height when you narrow. */}
          {projectShell && !fullWidthDetail && (
            <Toolbar>
              {/* The strip alone, then the tools at the tail. The status slices
                  used to sit here and no longer do: they are a filter, and six
                  identical pills on one bar answered two different questions —
                  which FACE of the project, and which ROWS that face draws. */}
              <ProjectTabs
                view={isProjectView(view) ? view : "list"}
                // Re-entering Issues keeps the layout you were last drawing them
                // in rather than snapping back to the list — the tree used to
                // carry this rule and the strip inherits it.
                onPick={(next) => api.goto(next === "list" ? issueLayout : next)}
              />
              {/* The controls belong beside the slices they act on, not up in the
                  trail: filtering, display and "new issue" are all about THIS
                  list, while the bar above names where you are. One row, the
                  slices at its head and the tools at its tail. */}
              <span className="ml-auto flex items-center gap-1">
              {projectShell && issueMode && (
                <FilterMenu
                  filter={filter}
                  labels={labels}
                  states={states}
                  members={members}
                  milestones={milestones}
                  open={filterOpen}
                  onOpenChange={setFilterOpen}
                  focusToken={focusToken}
                  resultCount={shown?.columns.reduce(
                    (count, column) => count + column.rows.filter((row) => !row.tombstone).length,
                    0,
                  ) ?? 0}
                  totalCount={board?.columns.reduce(
                    (count, column) => count + column.rows.filter((row) => !row.tombstone).length,
                    0,
                  ) ?? 0}
                  onChange={setFilter}
                />
              )}
              {projectShell && issueMode && (
                <span className="@max-[420px]:hidden">
                  <DisplayOptions
                    display={display}
                    view={issueMode}
                    onModeChange={(mode) => api.goto(mode)}
                    open={displayOpen}
                    onOpenChange={setDisplayOpen}
                    density={density}
                    onDensityChange={(nextDensity) => {
                      setDensity(nextDensity);
                      applyDensity(nextDensity);
                    }}
                    onChange={(nextDisplay) => {
                      if (nextDisplay.deleted !== display.deleted) {
                        api.select(null);
                        setDetail(false);
                      }
                      setDisplay(nextDisplay);
                      if (nextDisplay.deleted && view === "board") api.goto("list");
                    }}
                  />
                </span>
              )}

              {projectShell && !readOnly && current && (view === "list" || view === "board" || view === "calendar") && (
                <IconButton
                  label="New issue"
                  onClick={() => run("issue.create")}
                  variant="secondary"
                  elevation="low"
                  size="sm"
                  tooltip="New issue  C"
                  icon={<Plus className="size-icon-sm" />}
                />
              )}
              {/* No chord: `C` is the issue composer's everywhere, and a key that
                  makes a different kind of document depending on which tab is
                  lit is worse than a key that only makes issues. */}
              {projectShell && !readOnly && current && view === "specs" && !openSpec && !openBaseline && (
                <IconButton
                  label="New spec"
                  onClick={() => setComposingSpec("any")}
                  variant="secondary"
                  elevation="low"
                  size="sm"
                  tooltip="New spec"
                  icon={<Plus className="size-icon-sm" />}
                />
              )}
              {/* Last in the band, because it acts on the band's neighbour
                  rather than on the rows: everything to its left changes what
                  the list shows, this changes whether the console is beside it.
                  Gone entirely when there is no room for the console, rather
                  than left behind toggling something that cannot appear. */}
              {projectShell && consoleFits && (
                <IconButton
                  label={railOpen ? "Hide project panel" : "Show project panel"}
                  onClick={() =>
                    setRailOpen((was) => {
                      saveRailOpen(!was);
                      return !was;
                    })
                  }
                  variant={railOpen ? "active" : "secondary"}
                  elevation={railOpen ? "none" : "low"}
                  size="sm"
                  tooltip={railOpen ? "Hide project panel" : "Show project panel"}
                  icon={<PanelRight className="size-icon-sm" />}
                />
              )}
              </span>
            </Toolbar>
          )}
        </div>

        {standingReadOnly && standing && current && (
          <StandingNotice
            standing={standing}
            onRefresh={() =>
              void projectStore.ensureStanding(current, true).catch(() => undefined)
            }
          />
        )}

        {error && (
          <InlineError
            {...recoveryForError(error)}
            failureKind={classifyFailure(error)}
            message={error}
            onRetry={api.refresh}
            onDismiss={() => setError(null)}
            onCopy={() =>
              void navigator.clipboard.writeText(
                [`Viewer error`, error, window.location.href].join("\n"),
              )
            }
          />
        )}

        {fullWidthDetail && (
          <div className="ui-detail flex min-h-0 flex-1 flex-col">{issuePane}</div>
        )}


        {/* The project shell: whatever face you are on, beside the console.

            The rail is here rather than inside any one view because it has to
            survive the hop between them — clicking a milestone narrows the issue
            list, and a panel that vanished on the way would take the only way to
            clear the filter with it. Non-project surfaces render the content
            column alone and the row collapses to it. */}
        <div className={`flex min-h-0 flex-1${fullWidthDetail ? " hidden" : ""}`}>
        <div
          // Hidden rather than unmounted behind a full-width issue: coming back
          // should land you where you were in the list, not at the top of it.
          className="group/list flex min-w-0 min-h-0 flex-1 flex-col"
        >
          {/* Nothing to open, and the only surface that can change that. A
              machine with no store lands here on its first run, so this is
              where the app has to be able to found or enter a Space — there is
              no command surface left to do it from.

              An explicit ask wins over a selected space. Gating this on
              `!current` too is what used to make the surface unreachable the
              moment anything was open: one space auto-selects, and the only
              caller of `setFounding` was the empty state that a selected space
              replaces. Someone invited to a *second* space then had nowhere to
              paste the link. */}
          {founding !== null || (!current && !routeSpace && spaces.length === 0) ? (
            <Welcome
              initialMode={founding ?? "found"}
              // Only when there is somewhere to go back to.
              onCancel={spaces.length > 0 ? () => setFounding(null) : undefined}
              onArrived={(space) => {
                setFounding(null);
                void loadSpacesRaw(space);
              }}
            />
          ) : !current ? (
            <EmptyState
              icon={<PanelLeft className="size-icon-lg" />}
              title={routeSpace ? "This space is not on this device" : "Choose a local space"}
              body={
                routeSpace
                  ? `The link names ${routeSpace}, but no matching local replica is available. Enter the space from an invite on this device, then refresh.`
                  : "Select a space from the sidebar to open its local replica."
              }
              // A route naming a space this device does not hold is answered by
              // entering it, not by founding a second one under the same name.
              action={
                <Button
                  onClick={() => setFounding(routeSpace ? "enter" : "found")}
                  label={routeSpace ? "Use an invite" : "Start a space"}
                  variant="ghost"
                  size="sm"
                />
              }
            />
          ) : missingProject ? (
            <EmptyState
              kind="unavailable"
              title="Project not found in this local space"
              body={`${project} is not available in the current replica. Choose another project from the sidebar or wait for catalog data to arrive.`}
              action={
                projects[0] ? (
                  <Button
                    onClick={() => api.goto("overview", projects[0]!.key)}
                    label={`Choose ${projects[0].name}`}
                    variant="ghost"
                    size="sm"
                  />
                ) : (
                  <Button
                    onClick={api.refresh}
                    label="Refresh projects"
                    variant="ghost"
                    size="sm"
                  />
                )
              }
            />
          ) : view === "inbox" ? (
            <Inbox
              spaceId={current}
              revision={revision}
              onCountChange={setUnread}
              onOpen={openRecentIssue}
            />
          ) : view === "settings" ? (
            <Settings
              spaceId={current}
              spaceName={statusInfo?.name || space?.name || ""}
              spaceDescription={statusInfo?.description ?? ""}
              labels={labels}
              projects={projects}
              readOnly={readOnly}
              revision={revision}
              onError={setError}
              onExit={() => api.goto("list")}
            />
          ) : view === "my-issues" ? (
            <MyIssues
              spaceId={current}
              revision={revision}
              onError={setError}
              onOpen={openRecentIssue}
            />
          ) : view === "projects" ? (
            <Projects
              spaceId={current}
              projects={projects}
              revision={revision}
              spaceDescription={statusInfo?.description ?? ""}
              onOpen={(key) => api.goto("overview", key)}
            />
          ) : view === "overview" && activeProject ? (
            <ProjectOverview
              spaceId={current}
              project={activeProject}
              members={members}
              readOnly={readOnly}
              onError={setError}
            />
          ) : view === "specs" ? (
            <Specs
              spaceId={current}
              project={activeProject?.key ?? project}
              projectName={activeProject?.name ?? project ?? "this project"}
              readOnly={readOnly}
              spec={openSpec}
              baseline={openBaseline}
              members={members}
              composing={composingSpec}
              onCompose={setComposingSpec}
              onOpen={setOpenSpec}
              onOpenBaseline={setOpenBaseline}
              onError={setError}
            />
          ) : view === "activity" && board ? (
            <Activity
              spaceId={current}
              members={members}
              revision={revision}
              projectDocIds={projectDocIds}
              projectName={board.project.name}
              onError={setError}
              onOpen={(reff) => {
                api.goto("list");
                api.select(reff);
                setDetail(true);
              }}
            />
          ) : shown && view === "board" ? (
            <Board
              board={shown}
              display={display}
              members={members}
              labels={labels}
              selection={selection}
              optimistic={optimistic}
              onSelect={(reff) => {
                api.select(reff);
                setDetail(true);
              }}
              onCreate={(status) => setComposing({ status })}
              onDrop={dropCard}
              filtered={isActive(filter)}
              onClearFilter={() => api.clearFilter()}
              onReassign={(row, groupKey) => {
                const id = currentRef.current;
                if (!id) return;
                if (display.group === "priority") {
                  if (row.priority === groupKey) return;
                  void commitField("priority", () =>
                    projectStore.setPriority(id, row.reff, groupKey));
                } else if (display.group === "assignee") {
                  // Reassign = make the target the issue's sole assignee (or
                  // clear it for the unassigned lane).
                  const target = groupKey === "unassigned" ? null : groupKey;
                  void commitField("assignee", () =>
                    projectStore.setAssignees(id, row.reff, target ? [target] : []));
                }
              }}
              mutators={issueMutators}
              // The graph resource the detail pane reads, on demand: a card's
              // sub-issue menu asks only when it opens.
              onLoadChildren={(reff) => {
                const id = currentRef.current;
                if (!id) return Promise.resolve([]);
                return projectStore.ensureGraph(id, reff).then((graph) => graph.children);
              }}
              readOnly={readOnly}
            />
          ) : shown && view === "calendar" ? (
            <Calendar
              board={shown}
              members={members}
              labels={labels}
              onSelect={(reff) => {
                api.select(reff);
                setDetail(true);
              }}
              mutators={issueMutators}
              readOnly={readOnly}
            />
          ) : view === "timeline" ? (
            <Timeline
              projects={liveProjects}
              onOpenProject={(key) => api.goto("list", key)}
            />
          ) : shown && view === "list" ? (
            <IssueList
              groups={display.deleted ? [] : groups}
              deleted={display.deleted ? deletedRows : []}
              deletedMode={display.deleted}
              states={states}
              members={members}
              labels={labels}
              selection={selection}
              checked={checked}
              optimistic={optimistic}
              onSelect={api.select}
              onToggleCheck={(reff) =>
                setChecked((c) => {
                  const next = new Set(c);
                  if (!next.delete(reff)) next.add(reff);
                  return next;
                })
              }
              // Open the row that was acted on, not whatever happened to be
              // selected: Enter and the row menu reach here without a preceding
              // click, so selection may still be a row above.
              onOpen={(reff) => {
                api.select(reff);
                setDetail(true);
              }}
              onCreate={(status) => setComposing({ status })}
              mutators={issueMutators}
              readOnly={readOnly}
              filtered={isActive(filter)}
            />
          ) : (
            <EmptyState
              kind="unavailable"
              title="This view is unavailable"
              body="The local projection could not be loaded."
              action={<Button
                        onClick={api.refresh}
                        label="Retry loading"
                        variant="ghost"
                        size="sm"
                      />}
            />
          )}
        </div>
        {/* `consoleFits` before `railOpen`: the preference says what you want
            when there is room, not whether there is any. Keeping them separate
            is what lets the console come straight back at its old width instead
            of the window silently rewriting the setting on the way down. */}
        {projectShell && consoleFits && railOpen && activeProject && current && (
          <aside className="border-line w-rail shrink-0 overflow-y-auto border-l p-3">
            <ProjectRail
              spaceId={current}
              project={activeProject}
              members={members}
              counts={projectCounts}
              readOnly={readOnly}
              activeMilestone={filter.milestone}
              onError={setError}
              onOpenMilestone={(id: string | null) => api.gotoMilestone(activeProject.key, id)}
            />
          </aside>
        )}
        </div>
      </Panel>

      {/* The third panel and its handle are held open as declarations only: the
          layout library wants one panel topology for the life of the app, and
          removing an id it has already balanced against rebalances the sidebar.
          Nothing is drawn here and nothing can be dragged. */}
      <Separator disabled className="pointer-events-none invisible relative w-px" />
      <Panel
        id="detail"
        panelRef={detailPanel}
        defaultSize="34%"
        minSize="300px"
        maxSize="58%"
        collapsible
        collapsedSize="0%"
        className="ui-detail overflow-hidden"
      />

      {composing && current && routeSpace && board && (
        <NewIssue
          spaceId={current}
          canonicalSpaceId={routeSpace}
          projectKey={board.project.key}
          projects={liveProjects}
          states={states}
          labels={labels}
          members={members}
          defaultStatus={composing.status}
          onClose={() => setComposing(null)}
          onError={setError}
          onCreated={setToast}
        />
      )}
      {composingProject && current && (
        <NewProject
          spaceId={current}
          taken={projects.map((p) => p.key.toUpperCase())}
          onClose={() => setComposingProject(false)}
          // Land in what you just made. Creating a project and staying on the old
          // board is the app ignoring the thing you came to do.
          onCreated={(key) => {
            api.goto("overview", key);
            setToast(`Created ${key}`);
          }}
        />
      )}
      {/* A drawer, not a centred modal: pinned to the inline start and both
          block edges, which is what `position` expresses. It is only ever
          opened by a control that is itself hidden above 768px. */}
      <Dialog
        isOpen={mobileNav}
        onOpenChange={setMobileNav}
        width="min(320px, 88vw)"
        maxHeight="100vh"
        position={{ start: 0, top: 0, bottom: 0 }}
        className="ui-drawer bg-sunken pt-[env(safe-area-inset-top)] pb-[env(safe-area-inset-bottom)]"
      >
            <h2 className="sr-only">Workspace navigation</h2>
            <Sidebar
              spaces={spaces}
              current={current}
              projects={liveProjects}
              currentProject={board?.project.key ?? project}
              view={view}
              unread={unread}
              currentName={statusInfo?.name}
              favoriteProjects={favoriteProjects}
              savedViews={sidebarSavedViews}
              onPickSpace={(id) => {
                api.pickSpace(id);
                setMobileNav(false);
              }}
              onSearch={() => {
                run("search.issues");
                setMobileNav(false);
              }}
              onOpenProjectView={(key, next) => {
                api.goto(next, key);
                setMobileNav(false);
              }}
              onGo={(next) => {
                api.goto(next);
                setMobileNav(false);
              }}
              onMyIssues={() => {
                openMyIssues();
                setMobileNav(false);
              }}
              onApplySavedView={(saved) => {
                applySavedView(saved);
                setMobileNav(false);
              }}
              onToggleFavorite={toggleFavorite}
              onCreateProject={() => {
                api.createProject();
                setMobileNav(false);
              }}
              onAddSpace={() => {
                setFounding("found");
                setMobileNav(false);
              }}
              // The drawer closes on the *confirmation*, not on the click: the
              // dialog is rendered outside it, and dismissing the drawer first
              // takes the menu's focus context with it.
              onForgetSpace={(id) => {
                void forgetSpace(id).then(() => setMobileNav(false));
              }}
              onPruneSpaces={() => {
                void pruneSpaces().then(() => setMobileNav(false));
              }}
            />
      </Dialog>
      {checked.size > 0 && !readOnly && current && (
        <BulkBar
          count={checked.size}
          progress={bulkProgress}
          states={states}
          labels={labels}
          members={members}
          onStatus={(id) =>
            void bulk((reff) => rpc(current, { cmd: "issue_edit", reff, status: id }))
          }
          onPriority={(id) =>
            void bulk((reff) => rpc(current, { cmd: "issue_edit", reff, priority: id }))
          }
          onLabel={(name) => void bulk((reff) => rpc(current, { cmd: "label", reff, add: [name] }))}
          onAssign={(key) =>
            void bulk((reff) => rpc(current, { cmd: "assign", reff, who: [key], add: true }))
          }
          onDue={(due) => void bulk((reff) => rpc(current, { cmd: "issue_edit", reff, due }))}
          onDelete={() =>
            void (async () => {
              const n = checked.size;
              // The engine's per-issue question doesn't scale to a set, so the
              // dialog owns the aggregate phrasing and each request then rides
              // with `confirm` — the same consent, asked once.
              const ok = await ask.confirm({
                title: `Delete ${n} ${n === 1 ? "issue" : "issues"}?`,
                body: "Deletion tombstones — they can be restored later.",
                confirmText: "Delete",
                danger: true,
              });
              if (!ok) return;
              const result = await bulk((reff) =>
                rpc(current, { cmd: "issue_delete", reff }, { confirm: true }),
              );
              if (!result?.failures.length) setChecked(new Set());
            })()
          }
          onRetryFailures={() => {
            const operation = bulkOperation.current;
            if (!operation || !bulkProgress?.failures.length) return;
            void bulk(operation, bulkProgress.failures.map((failure) => failure.reff));
          }}
          onClear={() => setChecked(new Set())}
        />
      )}
      <DialogHost />
      {modal === "palette" && <Palette ctx={ctx} onClose={() => setModal(null)} />}
      {modal === "issueSearch" && current && routeSpace && board && (
        <IssueSearch
          spaceId={routeSpace}
          rpcSpaceId={current}
          rows={board.columns.flatMap((column) => column.rows).filter((row) => !row.tombstone)}
          projects={projects}
          states={states}
          onClose={() => setModal(null)}
          onOpen={(row) => {
            const destination = projects.find((candidate) => candidate.id === row.project_id);
            api.goto("list", destination?.key ?? board.project.key);
            setDetail(true);
            api.select(row.reff);
          }}
        />
      )}
      {modal === "shortcuts" && <Shortcuts ctx={ctx} onClose={() => setModal(null)} />}
      {modal === "workflow" && current && board && (
        <WorkflowDialog
          spaceId={current}
          projectKey={board.project.key}
          onClose={() => setModal(null)}
        />
      )}
      {modal === "roles" && current && (
        <RolesDialog spaceId={current} onClose={() => setModal(null)} />
      )}

      {/* A half-typed sequence must be visible, or `g` reads as a dropped key. */}
      {pending.length > 0 && (
        <div className="border-line-strong bg-raised text-dim shadow-overlay fixed bottom-4 left-4 rounded-surface border px-2 py-1 font-mono text-sm">
          {pending.join(" ")} …
        </div>
      )}
      {mutationNotice && (
        <div
          className="ui-surface border-line-strong bg-raised text-dim shadow-overlay fixed right-4 bottom-4 z-40 rounded-surface border px-3 py-1.5 text-sm"
          role="status"
          aria-live="polite"
        >
          {mutationNotice}
        </div>
      )}
      {toast && (
        <div
          className="border-line-strong bg-raised shadow-overlay fixed bottom-4 left-1/2 -translate-x-1/2 rounded-surface border px-3 py-1.5 text-sm"
          role="status"
          aria-live="polite"
        >
          {toast}
        </div>
      )}
    </Group>
    </HeaderSlotProvider>
    </Theme>
  );
}

function loadTheme(): ThemePreference {
  try {
    const saved = localStorage.getItem(THEME_PREFERENCE);
    return saved === "light" || saved === "dark" ? saved : "system";
  } catch {
    return "system";
  }
}

/**
 * Persist the appearance choice. It does NOT touch the DOM any more.
 *
 * Astryx's `<Theme>` syncs `data-theme` onto `<html>` itself when it is the
 * root theme, and it wins — anything we wrote here would be overwritten on its
 * next render. So the attribute has one owner, `mode` is the input, and this
 * function is reduced to the half that was always ours: remembering.
 *
 * The three states line up exactly: Astryx's `ThemeMode` is
 * `'light' | 'dark' | 'system'`, and `system` follows the OS the same way our
 * missing-attribute case did.
 */
function persistTheme(theme: ThemePreference): void {
  try {
    if (theme === "system") localStorage.removeItem(THEME_PREFERENCE);
    else localStorage.setItem(THEME_PREFERENCE, theme);
  } catch {
    // Appearance remains applied for this page even when storage is unavailable.
  }
}

function loadDensity(): DensityPreference {
  try {
    return localStorage.getItem(DENSITY_PREFERENCE) === "comfortable" ? "comfortable" : "compact";
  } catch {
    return "compact";
  }
}

function applyDensity(density: DensityPreference): void {
  if (density === "comfortable") document.documentElement.dataset.density = density;
  else delete document.documentElement.dataset.density;
  try {
    if (density === "comfortable") localStorage.setItem(DENSITY_PREFERENCE, density);
    else localStorage.removeItem(DENSITY_PREFERENCE);
  } catch {
    // The current page still reflects the choice when storage is unavailable.
  }
}

/**
 * The sidebar toggle is a command like everything else.
 *
 * Contributed here rather than in `commands/` because its `run` needs the panel
 * handle only this component holds — but it still goes through the same door, so
 * it lists in the palette, shows in `?`, and is rebindable. A component with a
 * private `keydown` would be a binding nobody could see or change.
 */
contribute({
  commands: [
    {
      id: "view.sidebar",
      title: "Toggle sidebar",
      group: "View",
      keys: ["mod+b"],
      run: (c) => c.app.toggleSidebar(),
    },
  ],
});


function workspaceTitle(view: View): string {
  if (view === "inbox") return "Inbox";
  if (view === "my-issues") return "My issues";
  if (view === "projects") return "Projects";
  if (view === "timeline") return "Roadmap";
  if (view === "settings") return "Settings";
  return "Workspace";
}
