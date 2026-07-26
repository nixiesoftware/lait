import { useState } from "react";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";
import {
  ArrowLeftRight,
  Bookmark,
  Bot,
  ChevronDown,
  ChevronRight,
  Cog,
  Copy,
  Folder,
  Inbox,
  FolderKanban,
  GanttChart,
  Plus,
  Search,
  Star,
  StarOff,
  UserRound,
} from "lucide-react";

import {
  isProjectView,
  navViewFor,
  PROJECT_NAV_VIEWS,
  PROJECT_VIEW_LABEL,
  type IssueMode,
  type ProjectView,
  type View,
} from "../core/registry";
import type { SavedView } from "../core/savedViews";
import type { ProjectDto, SpaceRow } from "../types";
import { catalogColor } from "./colors";
import {
  MenuContent,
  MenuItem,
  MenuSeparator,
  MenuSub,
  MenuSubContent,
  MenuSubTrigger,
  PROJECT_VIEW_ICON,
} from "./layout";
import { Badge, cn, IconButton, navigationItem } from "./primitives";

/** Linear-shaped navigation over lait's local identities and projects. */
export function Sidebar({
  spaces,
  current,
  projects,
  currentProject,
  view,
  unread,
  currentName,
  favoriteProjects,
  savedViews,
  issueLayout,
  onPickSpace,
  onSearch,
  onOpenProjectView,
  onGo,
  onMyIssues,
  onApplySavedView,
  onToggleFavorite,
  onCreateProject,
}: {
  spaces: SpaceRow[];
  current: string | null;
  projects: ProjectDto[];
  currentProject: string | null;
  view: View;
  unread: number;
  /** The current space's authoritative catalog name (from `status`), which
   *  refreshes on every doorbell — so a rename shows without reloading, unlike the
   *  spaces-list `name` that only refetches on a catalog-dirty doorbell. */
  currentName?: string | undefined;
  favoriteProjects: readonly string[];
  savedViews: readonly SavedView[];
  /** The layout Issues were last drawn in, so re-entering them keeps it. */
  issueLayout: IssueMode;
  onPickSpace: (id: string) => void;
  onSearch: () => void;
  onOpenProjectView: (key: string, view: ProjectView) => void;
  onGo: (view: View) => void;
  onMyIssues: () => void;
  onApplySavedView: (view: SavedView) => void;
  onToggleFavorite: (key: string) => void;
  onCreateProject: () => void;
}) {
  const space = spaces.find((s) => s.id === current) ?? null;
  const agent = space?.identity.kind === "agent" ? space.identity.name : null;
  // Linear's density model: favorites are the always-visible projects; the full
  // list folds behind its section header. Default-collapsed once you have
  // favorites or an active project (curation/context replaces enumeration), open
  // on the workspace portfolio so a fresh space never hides its projects. The
  // versioned key deliberately resets the old always-open default once.
  const [projectsOpen, setProjectsOpen] = useState<boolean>(() => {
    const stored = localStorage.getItem("lait.sidebar.projects.v2");
    return stored !== null
      ? stored === "1"
      : favoriteProjects.length === 0 && currentProject === null;
  });
  const toggleProjects = () => {
    setProjectsOpen((open) => {
      localStorage.setItem("lait.sidebar.projects.v2", open ? "0" : "1");
      return !open;
    });
  };

  // Which projects show their faces. Deliberately not persisted: an expanded
  // tree is about the project you are working in right now, and restoring six
  // of them from last week would be the enumeration the collapse is there to
  // avoid. The project you are in opens itself.
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const toggleExpanded = (key: string) =>
    setExpanded((open) => {
      const next = new Set(open);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  const isExpanded = (key: string) => expanded.has(key) || (key === currentProject && isProjectView(view));
  const activeView = isProjectView(view) ? view : null;

  const projectNode = (project: ProjectDto) => (
    <ProjectRow
      key={project.id}
      project={project}
      active={project.key === currentProject}
      activeView={activeView}
      issueMode={issueLayout}
      favorited={favoriteProjects.includes(project.key)}
      expanded={isExpanded(project.key)}
      onToggleExpand={toggleExpanded}
      onPick={(key) => onOpenProjectView(key, activeView ?? "overview")}
      onOpenView={onOpenProjectView}
      onToggleFavorite={onToggleFavorite}
    />
  );

  return (
    <nav aria-label="Workspace" className="flex h-full min-h-0 flex-col">
      {/* The same band `SurfaceHeader` draws, so the space and the surface it is
          showing sit on one line across the whole window. It carries no bottom
          border: the rule under the work area's header separates that header
          from its content, and the sidebar has no content to separate it from. */}
      <div className="flex h-bar-lg shrink-0 items-center gap-0.5 px-2">
        <SpaceSwitcher
          spaces={spaces}
          current={current}
          currentName={currentName}
          onPick={onPickSpace}
          onSearch={onSearch}
          onOpenSettings={() => onGo("settings")}
        />
      </div>

      <div className="flex min-h-0 flex-1 flex-col px-2 pb-2">
      {agent && (
        <div className="border-line bg-bg text-dim mx-1 mt-2 flex items-start gap-2 rounded border p-2 text-xs">
          <Bot className="mt-0.5 size-icon-sm shrink-0" />
          <span>
            Observing as <strong className="text-fg">{agent}</strong>. Writes are disabled.
          </span>
        </div>
      )}

      <div className="mt-3 flex flex-col gap-px">
        <NavItem icon={<Inbox />} label="Inbox" active={view === "inbox"} badge={unread} onClick={() => onGo("inbox")} />
        <NavItem icon={<UserRound />} label="My issues" active={view === "my-issues"} onClick={onMyIssues} />
      </div>

      <Section title="Workspace" />
      <div className="flex flex-col gap-px">
        <NavItem icon={<FolderKanban />} label="Projects" active={view === "projects"} onClick={() => onGo("projects")} />
        <NavItem icon={<GanttChart />} label="Roadmap" active={view === "timeline"} onClick={() => onGo("timeline")} />
        {savedViews.length > 0 && <MiniSection title="Saved views" />}
        {savedViews.map((saved) => (
          <NavItem key={saved.id} icon={<Bookmark />} label={saved.name} onClick={() => onApplySavedView(saved)} compact />
        ))}
      </div>

      <Section
        title="Projects"
        count={projects.length}
        open={projectsOpen}
        onToggle={toggleProjects}
        action={
          !agent && (
            <IconButton label="New project" onClick={onCreateProject}>
              <Plus className="size-icon-xs" />
            </IconButton>
          )
        }
      />
      <div className="min-h-0 flex-1 overflow-y-auto">
        {favoriteProjects.length > 0 && (
          <div className="mb-2">
            <MiniSection title="Favorites" />
            {favoriteProjects.map((key) => {
              const favorite = projects.find((candidate) => candidate.key === key);
              return favorite ? projectNode(favorite) : null;
            })}
          </div>
        )}
        {projectsOpen ? (
          projects.length === 0 ? (
            <p className="text-mute px-2 py-1 text-sm">No projects yet.</p>
          ) : (
            projects.map(projectNode)
          )
        ) : (
          // Collapsed still anchors you: the project you're in stays visible
          // unless it's already pinned under Favorites.
          (() => {
            const active = projects.find(
              (project) => project.key === currentProject && !favoriteProjects.includes(project.key),
            );
            return active ? projectNode(active) : null;
          })()
        )}
      </div>
      </div>
    </nav>
  );
}

function SpaceSwitcher({
  spaces,
  current,
  currentName,
  onPick,
  onSearch,
  onOpenSettings,
}: {
  spaces: SpaceRow[];
  current: string | null;
  currentName?: string | undefined;
  onPick: (id: string) => void;
  onSearch: () => void;
  onOpenSettings: () => void;
}) {
  const selected = spaces.find((s) => s.id === current) ?? null;
  const title = (currentName?.trim() || selected?.name) || selected?.space || "Choose a space";
  return (
    <div className="flex min-w-0 flex-1 items-center gap-0.5">
      <DropdownMenu.Root>
        {/* One line, because it has to sit on the header's baseline.
            The second line said "Member · 8 people" — a role that does not change
            and a headcount nobody navigates by, costing 12px of every screen and,
            more to the point, making the sidebar's first row a different height
            from the work area's. Both are `h-bar-lg` now, so the space and the thing
            you are looking at read across at the same level. Membership itself
            has better homes: the status dot, the agent banner below, and the
            members tab in Settings. */}
        <DropdownMenu.Trigger
          className="hover:bg-hover data-[state=open]:bg-active -mx-1 flex h-ctl-md min-w-0 flex-1 items-center gap-1.5 rounded-md px-1.5 outline-none"
          aria-label="Space menu"
        >
          <span className="bg-active flex size-ctl-xs shrink-0 items-center justify-center rounded">
            {selected?.identity.kind === "agent" ? <Bot className="text-mute size-icon-xs" /> : <Folder className="text-mute size-icon-xs" />}
          </span>
          <strong className="min-w-0 flex-1 truncate text-left text-sm">{title}</strong>
          {selected && <StatusDot status={selected.status} />}
          <ChevronDown className="text-mute size-icon-xs shrink-0" aria-hidden />
        </DropdownMenu.Trigger>
        {/* Verbs first, replicas behind a submenu.
            This used to inline every local space and hang "Workspace settings"
            off the bottom, so the one row you open the menu for was the row a
            scrollbar hid — and with each space captioned "Your local actor" the
            list read as repetition rather than choice. It was also a
            `<details>` pretending to be a menu: no roving focus, no Escape, no
            flipping when it met the bottom of the window. */}
        <DropdownMenu.Portal>
          <MenuContent align="start" className="min-w-56">
            <MenuItem onSelect={onOpenSettings}>
              <Cog className="size-icon-sm" /> Workspace settings
            </MenuItem>
            <MenuItem
              disabled={!selected}
              onSelect={() => selected && void navigator.clipboard.writeText(selected.space)}
            >
              <Copy className="size-icon-sm" /> Copy space ID
            </MenuItem>
            {spaces.length > 1 && (
              <>
                <MenuSeparator />
                <MenuSub>
                  <MenuSubTrigger>
                    <ArrowLeftRight className="size-icon-sm" /> Switch space
                  </MenuSubTrigger>
                  <MenuSubContent>
                    {spaces.map((space) => (
                      <MenuItem
                        key={`${space.id}-${space.identity.kind === "agent" ? space.identity.name : "own"}`}
                        onSelect={() => onPick(space.id)}
                        className={cn(space.id === current && "text-fg")}
                      >
                        {space.identity.kind === "agent" ? <Bot className="size-icon-sm shrink-0" /> : <Folder className="size-icon-sm shrink-0" />}
                        <span className="min-w-0 flex-1 truncate">{space.name || space.space}</span>
                        {/* An agent replica is a different *identity* on the same
                            data, which is the only thing worth saying twice. */}
                        {space.identity.kind === "agent" && (
                          <span className="text-mute shrink-0 text-2xs">{space.identity.name}</span>
                        )}
                        <StatusDot status={space.status} />
                      </MenuItem>
                    ))}
                  </MenuSubContent>
                </MenuSub>
              </>
            )}
          </MenuContent>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>
      <IconButton label="Search issues" chord="Q" onClick={onSearch}>
        <Search className="size-icon-md" />
      </IconButton>
    </div>
  );
}

/** One project in the nav — color dot, name, key, and a hover star to (un)pin.
 *  Shared by Favorites and the collapsible all-projects list so the two render
 *  identically and a pin just moves the row up. */
/**
 * One project in the nav, and — when opened — its five faces beneath it.
 *
 * A flat list of names could only answer "which project"; the view you actually
 * wanted was another click away, on a tab strip you could not see yet. Hanging
 * the views off the project makes the second question answerable from the same
 * place as the first, and the indent rule on the left says which project is
 * answering it.
 */
function ProjectRow({
  project,
  active,
  activeView,
  issueMode,
  favorited,
  expanded,
  onToggleExpand,
  onPick,
  onOpenView,
  onToggleFavorite,
}: {
  project: ProjectDto;
  active: boolean;
  activeView: ProjectView | null;
  issueMode: IssueMode;
  favorited: boolean;
  expanded: boolean;
  onToggleExpand: (key: string) => void;
  onPick: (key: string) => void;
  onOpenView: (key: string, view: ProjectView) => void;
  onToggleFavorite: (key: string) => void;
}) {
  // "You are on one of this project's faces" — the child that owns the highlight.
  const onFace = active && expanded && activeView !== null;
  return (
    <div className="mb-0.5">
      <div className="group/project relative flex items-center">
        {/* Its own hit target: the chevron reveals, the name navigates. One
            button doing both would make "see what is in here" and "go there"
            the same gesture. */}
        <button
          onClick={() => onToggleExpand(project.key)}
          aria-expanded={expanded}
          aria-label={expanded ? `Collapse ${project.name}` : `Expand ${project.name}`}
          className="text-mute hover:text-fg flex size-ctl-xs shrink-0 items-center justify-center rounded outline-none"
        >
          <ChevronRight
            className={cn("size-icon-xs transition-transform", expanded && "rotate-90")}
            aria-hidden
          />
        </button>
        {/* Exactly one row in the tree is where you are. When a face of this
            project is open that face is the answer, so the project itself steps
            back — it used to keep its own highlight *and* its own
            `aria-current`, which drew a two-row block and told a screen reader
            there were two current pages. */}
        <button
          onClick={() => onPick(project.key)}
          title={`${project.name} · ${project.key}`}
          aria-current={active && !onFace ? "page" : undefined}
          className={cn(navigationItem({ selected: active && !onFace }), "px-1.5")}
        >
          <span
            className={cn("size-mark-xs shrink-0 rounded-sm opacity-75", active && "opacity-100")}
            style={{ background: catalogColor(project.color) }}
          />
          <span className="min-w-0 flex-1 truncate">{project.name}</span>
        </button>
        <IconButton
          label={favorited ? `Remove ${project.name} from favorites` : `Add ${project.name} to favorites`}
          className={cn(
            "absolute top-0.5 right-0.5 size-ctl-sm opacity-0 group-hover/project:opacity-100 focus-visible:opacity-100",
            active ? "bg-active hover:bg-hover" : "bg-hover",
          )}
          onClick={() => onToggleFavorite(project.key)}
        >
          {favorited ? <StarOff className="size-icon-xs" /> : <Star className="size-icon-xs" />}
        </IconButton>
      </div>
      {expanded && (
        /**
         * A project's faces are pages, drawn like every other page in this nav:
         * an icon, a label, and a full-width row that lights up when you are on
         * it. No connecting rule — the indent is the nesting, and a tree of
         * elbows here would be scaffolding around five items that already read
         * as a group.
         *
         * The rule is what the *next* level down gets, if one ever exists: the
         * reference draws Cycles' Current/Upcoming with a plain vertical line
         * and no icons, precisely because they are subordinate to a page that
         * already has one.
         */
        <div className="ml-[18px] flex flex-col gap-px">
          {PROJECT_NAV_VIEWS.map((v) => (
            <NavItem
              key={v}
              icon={PROJECT_VIEW_ICON[v]}
              label={PROJECT_VIEW_LABEL[v]}
              // A board *is* Issues, so the row stays lit while you are on one.
              active={active && activeView !== null && navViewFor(activeView) === v}
              // …and re-entering Issues keeps the layout you were last drawing
              // them in, rather than snapping you back to the list.
              onClick={() => onOpenView(project.key, v === "list" ? issueMode : v)}
            />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * One section header, collapsible or not, in one grammar.
 *
 * "Workspace" was a static uppercase label and "PROJECTS 17" was a button with a
 * chevron — two typographic registers a row apart, which read as two different
 * kinds of thing rather than two instances of the same one. A section that
 * cannot collapse simply renders without the disclosure; it does not get its own
 * typeface.
 */
function Section({
  title,
  count,
  open,
  onToggle,
  action,
}: {
  title: string;
  count?: number | undefined;
  open?: boolean | undefined;
  onToggle?: (() => void) | undefined;
  action?: React.ReactNode;
}) {
  const label = (
    <>
      {onToggle && (
        <ChevronRight
          className={cn("size-icon-xs shrink-0 transition-transform", open && "rotate-90")}
          aria-hidden
        />
      )}
      <span className="truncate">{title}</span>
      {count !== undefined && <span className="font-normal tabular-nums">{count}</span>}
    </>
  );
  return (
    <div className="mt-4 mb-1 flex h-ctl-xs items-center px-2">
      {onToggle ? (
        <button
          className="text-mute hover:text-fg flex min-w-0 items-center gap-1 text-2xs font-semibold tracking-[0.08em] uppercase"
          onClick={onToggle}
          aria-expanded={open}
        >
          {label}
        </button>
      ) : (
        <h2 className="text-mute flex min-w-0 items-center gap-1 text-2xs font-semibold tracking-[0.08em] uppercase">
          {label}
        </h2>
      )}
      {action && <span className="ml-auto">{action}</span>}
    </div>
  );
}

function MiniSection({ title }: { title: string }) {
  return <p className="text-mute mt-1 px-2 text-[9px] font-semibold tracking-[0.08em] uppercase">{title}</p>;
}

function NavItem({ icon, label, active, badge, compact, onClick }: { icon: React.ReactElement; label: string; active?: boolean; badge?: number; compact?: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      aria-current={active ? "page" : undefined}
      className={cn(
        navigationItem({
          selected: active,
          density: compact ? "compact" : "normal",
        }),
      )}
    >
      <span className="text-mute [&>svg]:size-icon-sm">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {!!badge && <Badge tone="accent" className="justify-center tabular-nums">{badge}</Badge>}
    </button>
  );
}

function StatusDot({ status }: { status: SpaceRow["status"] }) {
  const cls = { up: "bg-ok", idle: "bg-mute", missing: "bg-danger" }[status];
  const label = { up: "Local daemon running", idle: "Local daemon idle", missing: "Local replica unavailable" }[status];
  return <span className={cn("size-mark-xs shrink-0 rounded-full", cls)} title={label} role="img" aria-label={label} />;
}
