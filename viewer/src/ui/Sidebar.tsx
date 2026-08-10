import { useState } from "react";
import {
  ArrowLeftRight,
  Bookmark,
  Bot,
  ChevronRight,
  CircleDot,
  Cog,
  Copy,
  ExternalLink,
  EyeOff,
  Folder,
  FolderPlus,
  Inbox,
  GanttChart,
  Plus,
  Search,
  Star,
  StarOff,
  Trash2,
  UserRound,
} from "lucide-react";

import {
  isProjectView,
  type ProjectView,
  type View,
} from "../core/registry";
import type { SavedView } from "../core/savedViews";
import type { ProjectDto, SpaceRow, TeamDto } from "../types";
import { ungrouped } from "../core/teams";
import { catalogColor } from "./colors";
import { ProjectIcon } from "./icons";

import { Badge, ContextMenu, Divider, DropdownMenu, DropdownMenuItem, DropdownMenuSubMenu, IconButton } from "@astryxdesign/core";
import { cn, navigationItem } from "./primitives";

/** Linear-shaped navigation over lait's local identities and projects. */
export function Sidebar({
  spaces,
  current,
  projects,
  teams,
  currentTeam,
  currentProject,
  view,
  unread,
  currentName,
  favoriteProjects,
  savedViews,
  onPickSpace,
  onSearch,
  onOpenProjectView,
  onGo,
  onGoTeam,
  onMyIssues,
  onApplySavedView,
  onToggleFavorite,
  onCreateProject,
  onAddSpace,
  onForgetSpace,
  onPruneSpaces,
}: {
  spaces: SpaceRow[];
  current: string | null;
  projects: ProjectDto[];
  teams: TeamDto[];
  /** The team the address is scoped to, by KEY. */
  currentTeam: string | null;
  currentProject: string | null;
  view: View;
  unread: number;
  /** The current space's authoritative catalog name (from `status`), which
   *  refreshes on every doorbell — so a rename shows without reloading, unlike the
   *  spaces-list `name` that only refetches on a catalog-dirty doorbell. */
  currentName?: string | undefined;
  favoriteProjects: readonly string[];
  savedViews: readonly SavedView[];
  onPickSpace: (id: string) => void;
  onSearch: () => void;
  onOpenProjectView: (key: string, view: ProjectView) => void;
  onGo: (view: View, project?: string | null) => void;
  onGoTeam: (team: string | null, view?: View) => void;
  onMyIssues: () => void;
  onApplySavedView: (view: SavedView) => void;
  onToggleFavorite: (key: string) => void;
  onCreateProject: () => void;
  /** Open the formation surface — founding and entering from an invite both.
   *  Here rather than only on the empty state, which a selected space replaces;
   *  see `App.tsx`'s formation gate. */
  onAddSpace: () => void;
  /** Deregister one Orbit row. Navigation state only — never the store. */
  onForgetSpace: (id: string) => void;
  /** Drop every row whose store is gone. */
  onPruneSpaces: () => void;
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

  /**
   * Which teams are unfolded.
   *
   * Stored as the set of *collapsed* ids rather than open ones, so a team
   * created after this was last written opens by default. The alternative — a
   * set of open ids — hides every new team behind a chevron nobody knows to
   * click.
   */
  const [foldedTeams, setFoldedTeams] = useState<ReadonlySet<string>>(() => {
    try {
      const stored = JSON.parse(localStorage.getItem("lait.sidebar.teams") ?? "[]");
      return new Set(Array.isArray(stored) ? (stored as string[]) : []);
    } catch {
      return new Set();
    }
  });
  const toggleTeam = (id: string) => {
    setFoldedTeams((folded) => {
      const next = new Set(folded);
      if (!next.delete(id)) next.add(id);
      try {
        localStorage.setItem("lait.sidebar.teams", JSON.stringify([...next]));
      } catch {
        // A fold state is a convenience; navigation is unaffected.
      }
      return next;
    });
  };

  const looseProjects = ungrouped(teams, projects);

  const activeView = isProjectView(view) ? view : null;

  const projectNode = (project: ProjectDto) => (
    <ProjectRow
      key={project.id}
      project={project}
      active={project.key === currentProject}
      favorited={favoriteProjects.includes(project.key)}
      // Re-entering a project keeps the face you were last on, so the tree
      // answers "which project" and the strip answers "which face" without
      // either resetting the other's answer.
      onPick={(key) => onOpenProjectView(key, activeView ?? "overview")}
      onToggleFavorite={onToggleFavorite}
    />
  );

  return (
    <nav aria-label="Workspace" className="flex h-full min-h-0 flex-col">
      {/* The same band `SurfaceHeader` draws, so the space and the surface it is
          showing sit on one line across the whole window. It carries no bottom
          border: the rule under the work area's header separates that header
          from its content, and the sidebar has no content to separate it from.

          `px-3` here and on the body below, up from `px-2`. Every row already
          holds its own 8px, so at an 8px wall the glyphs sat 16px in and the
          hover fills ran to within 8px of the edge — close enough that the rail
          read as a list pressed against the window rather than one set into it.
          12px is the step that separates them, and it is the same wall the
          drawer now stands the whole column against. */}
      <div className="flex h-bar-lg shrink-0 items-center gap-0.5 px-3">
        <SpaceSwitcher
          spaces={spaces}
          current={current}
          currentName={currentName}
          onPick={onPickSpace}
          onSearch={onSearch}
          onOpenSettings={() => onGo("settings")}
          onAddSpace={onAddSpace}
          onForgetSpace={onForgetSpace}
          onPruneSpaces={onPruneSpaces}
        />
      </div>

      <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-3 pb-2">
      {agent && (
        <div className="border-line bg-bg text-dim mx-1 mt-2 flex items-start gap-2 rounded-surface border p-2 text-xs">
          <Bot className="mt-0.5 size-icon-sm shrink-0" />
          <span>
            Observing as <strong className="text-fg">{agent}</strong>. Writes are disabled.
          </span>
        </div>
      )}

      <div className="mt-3 flex flex-col gap-0.5">
        <NavItem icon={<Inbox />} label="Inbox" active={view === "inbox"} badge={unread} onClick={() => onGo("inbox")} />
        <NavItem icon={<UserRound />} label="My issues" active={view === "my-issues"} onClick={onMyIssues} />
      </div>

      <Section title="Workspace" />
      <div className="flex flex-col gap-0.5">
        {/* `!currentTeam` on both, and it is the whole of the second fix.

            A team's Projects and the workspace's Projects are the same *view*
            at two scopes, so `view === "projects"` was true for both and both
            lit. Selection has to follow the entry point you used, not the
            destination you landed on — otherwise the sidebar says you are in
            two places, and neither of them is wrong enough to be obviously
            wrong. */}
        <NavItem icon={<ProjectIcon />} label="Projects" active={view === "projects" && !currentTeam} onClick={() => onGo("projects")} />
        {/* `null` and not the default. `goto` keeps the project you are in for
            any project-capable view, and Timeline is one — so without this the
            workspace Roadmap landed on whichever project you last had open, and
            the space-wide chart was unreachable while any project existed. */}
        <NavItem icon={<GanttChart />} label="Roadmap" active={view === "timeline" && !currentTeam} onClick={() => onGo("timeline", null)} />
        {savedViews.length > 0 && <MiniSection title="Saved views" />}
        {savedViews.map((saved) => (
          <NavItem key={saved.id} icon={<Bookmark />} label={saved.name} onClick={() => onApplySavedView(saved)} compact />
        ))}
      </div>

      {/* Teams.

          A team is a set of projects, so every destination under one is the
          destination it names *scoped to those projects*. That is the answer to
          a flat list of seventeen projects: three entries a team rather than one
          a project, and the projects themselves reachable through the team's
          own Projects page.

          No icon on the header, and none was ever needed — a team has a name,
          and a glyph beside it would be one more thing to look at in a column
          whose whole problem was having too many. */}
      {teams.length > 0 && <Section title="Teams" count={teams.length} />}
      <div className="flex flex-col gap-0.5">
        {teams.map((candidate) => {
          const open = !foldedTeams.has(candidate.id);
          const scoped = currentTeam === candidate.key;
          return (
            // The block carries the same 2px its rows do, so the header sits
            // off its first child by exactly what separates any two rows. It
            // had no gap at all, which read as the header and the first item
            // being one welded object rather than as a heading over a list.
            <div key={candidate.id} className="flex flex-col gap-0.5">
              {/* A fold toggle, never a destination — so it never takes the
                  selected fill. It used to, which is how three things came to
                  be highlighted at once: the header, the child under it, and
                  the workspace entry for the same view. Being *inside* a team
                  is an ancestor relationship, and an ancestor is worth marking
                  quietly — the name brightens, and that is all. */}
              <button
                onClick={() => toggleTeam(candidate.id)}
                aria-expanded={open}
                className={cn(
                  navigationItem({ size: "md" }),
                  "font-medium",
                  scoped && "text-fg",
                )}
              >
                <span className="min-w-0 flex-1 truncate">{candidate.name}</span>
                <ChevronRight
                  className={cn(
                    "text-mute size-icon-xs shrink-0 transition-transform",
                    open && "rotate-90",
                  )}
                  aria-hidden
                />
              </button>
              {/* Indented, and the indent is the only thing saying these belong
                  to the team above them — which is enough, and is what the
                  reference does. */}
              {open && (
                <div className="flex flex-col gap-0.5 pl-4">
                  {/* Full height, not `compact`. The indent already says these
                      belong to the team above them, so shrinking them as well
                      was saying it twice — and it left the children reading as
                      a denser, lesser kind of row than every other entry in the
                      column. Every nav row in this sidebar is one height now;
                      indent alone carries the hierarchy. */}
                  <NavItem
                    icon={<CircleDot />}
                    label="Issues"
                    active={scoped && (view === "list" || view === "board")}
                    onClick={() => onGoTeam(candidate.key, "list")}
                  />
                  <NavItem
                    icon={<ProjectIcon />}
                    label="Projects"
                    active={scoped && view === "projects"}
                    onClick={() => onGoTeam(candidate.key, "projects")}
                  />
                  <NavItem
                    icon={<GanttChart />}
                    label="Roadmap"
                    active={scoped && view === "timeline"}
                    onClick={() => onGoTeam(candidate.key, "timeline")}
                  />
                </div>
              )}
            </div>
          );
        })}
      </div>

      {/* What no team owns.

          Always rendered, even with no teams at all — which is the whole of a
          fresh space, and the heading is how you learn there is a grouping to
          opt into. These are listed as projects rather than as a fourth team
          section because there is no team here to scope a destination *to*. */}
      <Section
        title={teams.length > 0 ? "No team" : "Projects"}
        count={looseProjects.length}
        open={projectsOpen}
        onToggle={toggleProjects}
        action={
          !agent && (
            <IconButton
              label="New project"
              onClick={onCreateProject}
              variant="ghost"
              size="sm"
              tooltip="New project"
              icon={<Plus className="size-icon-xs" />}
            />
          )
        }
      />
      <div>
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
          looseProjects.length === 0 ? (
            <p className="text-mute px-2 py-1 text-base">
              {teams.length > 0 ? "Every project has a team." : "No projects yet."}
            </p>
          ) : (
            looseProjects.map(projectNode)
          )
        ) : (
          // Collapsed still anchors you: the project you're in stays visible
          // unless it's already pinned under Favorites.
          (() => {
            const active = looseProjects.find(
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
  onAddSpace,
  onForgetSpace,
  onPruneSpaces,
}: {
  spaces: SpaceRow[];
  current: string | null;
  currentName?: string | undefined;
  onPick: (id: string) => void;
  onSearch: () => void;
  onOpenSettings: () => void;
  onAddSpace: () => void;
  onForgetSpace: (id: string) => void;
  onPruneSpaces: () => void;
}) {
  const selected = spaces.find((s) => s.id === current) ?? null;
  const title = (currentName?.trim() || selected?.name) || selected?.space || "Choose a space";
  // A row whose store is gone. It has no remedy anywhere else in the app: the
  // registry is only ever *written* by founding and entering, so nothing else
  // clears one and it sits in the switcher for good.
  const unavailable = spaces.filter((s) => s.status === "missing").length;
  return (
    <div className="flex min-w-0 flex-1 items-center gap-0.5">
      {/* One line, because it has to sit on the header's baseline.
          The second line said "Member · 8 people" — a role that does not change
          and a headcount nobody navigates by, costing 12px of every screen and,
          more to the point, making the sidebar's first row a different height
          from the work area's. Both are `h-bar-lg` now, so the space and the thing
          you are looking at read across at the same level.

          The trigger's composite face — mark, title, status dot — flattens into
          Button props: `icon` takes the mark, `label` the title. The status dot
          moves OUT of the trigger and sits beside it, because `endContent` is
          typed to an Icon or a Badge and ours is neither. It reads the same and
          it is no longer inside the control's hit area, which is arguably
          better: a status is not something you click.

          Verbs first, replicas behind a submenu. This used to inline every
          local space and hang "Workspace settings" off the bottom, so the one
          row you open the menu for was the row a scrollbar hid. */}
      <DropdownMenu
        alignment="start"
        hasChevron={false}
        menuWidth={224}
        button={{
          label: title,
          "aria-label": "Space menu",
          // Sized to the name, and standing in the same column as the rows
          // below it.
          //
          // It used to be `flex-1`, which stretched the trigger across the rail
          // — so the hover fill ran the full width for a control whose content
          // is one folder glyph and a word, and the fill was the only thing on
          // the rail claiming that much room. `min-w-0` without the grow keeps
          // the truncation: a long space name still shortens rather than
          // pushing the dot and the search button off the end.
          //
          // `justify-start` stays and is still load-bearing. `.astryx-button`
          // sets `justify-content: center`, which is right for a control that
          // wraps its label and wrong for one that may be shrunk by a narrow
          // rail: the truncated name would centre itself in what is left. No
          // utility here names `justify-content`, so Astryx's rule stands
          // unopposed without it.
          //
          // The geometry is `navigationItem`'s — `gap-2 px-2`, no negative
          // margin — rather than the `gap-1.5 px-1.5 -mx-1` it carried. Those
          // differences were visible: the folder glyph sat 6px left of the
          // Inbox glyph directly beneath it, so the rail's first row started in
          // a different column from every other row.
          className:
            "hover:bg-hover flex h-ctl-md min-w-0 items-center justify-start gap-2 rounded-row px-2",
          variant: "ghost",
          size: "sm",
          // `icon-sm`, which is what `NavItem` draws. At `icon-xs` the glyph was
          // 12px centred in Astryx's 16px icon slot, so it began 2px right of
          // every glyph under it — the two pixels that keep a column from
          // reading as a column.
          icon:
            selected?.identity.kind === "agent" ? (
              <Bot className="text-mute size-icon-sm" />
            ) : (
              <Folder className="text-mute size-icon-sm" />
            ),
        }}
      >
        <DropdownMenuItem
          label="Workspace settings"
          icon={<Cog className="size-icon-sm" />}
          onClick={onOpenSettings}
        />
        <DropdownMenuItem
          label="Copy space ID"
          icon={<Copy className="size-icon-sm" />}
          isDisabled={!selected}
          onClick={() => selected && void navigator.clipboard.writeText(selected.space)}
        />
        {spaces.length > 1 && (
          <>
            <Divider />
            <DropdownMenuSubMenu
              label="Switch space"
              icon={<ArrowLeftRight className="size-icon-sm" />}
            >
              {spaces.map((space) => (
                <DropdownMenuItem
                  key={`${space.id}-${space.identity.kind === "agent" ? space.identity.name : "own"}`}
                  onClick={() => onPick(space.id)}
                  icon={
                    space.identity.kind === "agent" ? (
                      <Bot className="size-icon-sm shrink-0" />
                    ) : (
                      <Folder className="size-icon-sm shrink-0" />
                    )
                  }
                  label={space.name || space.space}
                  // An agent replica is a different *identity* on the same data,
                  // which is the only thing worth saying twice.
                  {...(space.identity.kind === "agent"
                    ? { description: space.identity.name }
                    : {})}
                  endContent={<StatusDot status={space.status} />}
                />
              ))}
            </DropdownMenuSubMenu>
          </>
        )}
        {/* One entry, not one per formation verb: founding and entering are two
            answers to "add a space", and the surface behind this already asks
            which with a tab strip. */}
        <Divider />
        <DropdownMenuItem
          label="Add space"
          icon={<FolderPlus className="size-icon-sm" />}
          onClick={onAddSpace}
        />
        {/* Registry upkeep. Both are navigation state and neither touches a
            store, which is what makes them safe to offer next to the verbs that
            create one. */}
        {(selected || unavailable > 0) && <Divider />}
        {selected && (
          <DropdownMenuItem
            label="Forget this space"
            icon={<EyeOff className="size-icon-sm" />}
            onClick={() => onForgetSpace(selected.id)}
          />
        )}
        {unavailable > 0 && (
          <DropdownMenuItem
            label={
              unavailable === 1
                ? "Remove 1 unavailable space"
                : `Remove ${unavailable} unavailable spaces`
            }
            icon={<Trash2 className="size-icon-sm" />}
            onClick={onPruneSpaces}
          />
        )}
      </DropdownMenu>
      {/* Pinned right by `ml-auto`, which the trigger's `flex-1` used to do for
          them by taking all the slack itself. Now that it takes only what its
          name needs, the free space has to be claimed by the thing that wants
          to be at the far end. */}
      <span className="ml-auto flex shrink-0 items-center gap-0.5">
        {selected && <StatusDot status={selected.status} />}
        <IconButton
          label="Search issues"
          onClick={onSearch}
          variant="ghost"
          size="sm"
          tooltip="Search issues  Q"
          icon={<Search className="size-icon-md" />}
        />
      </span>
    </div>
  );
}

/**
 * One project in the nav — colour dot, name, and a hover star to (un)pin.
 *
 * Flat, and this used to be a tree. The tree existed because a flat list could
 * only answer "which project" while the face you wanted was another click away
 * on a strip you could not see yet. The project shell has that strip now, and
 * it is always on screen — so the tree was offering the same three faces the
 * strip offers, one pane apart, with two highlights claiming to be the current
 * page. The nav answers "which project"; the strip answers "which face".
 *
 * Shared by Favorites and the all-projects list so the two render identically
 * and a pin just moves the row up.
 */
function ProjectRow({
  project,
  active,
  favorited,
  onPick,
  onToggleFavorite,
}: {
  project: ProjectDto;
  active: boolean;
  favorited: boolean;
  onPick: (key: string) => void;
  onToggleFavorite: (key: string) => void;
}) {
  // The row's verbs. Only the two this row already wires plus the link — a menu
  // that offered "Project settings" would be promising plumbing that does not
  // exist here, and a dead entry is worse than an absent one.
  //
  // No `display: contents` escape hatch, unlike the issue surfaces: this row is
  // not a list item and its container is not a flex row, so Astryx's trigger
  // wrapper costs nothing here. The hatch is for the cases that need it.
  const menu = (
    <>
      <DropdownMenuItem
        label="Open project"
        icon={<ExternalLink className="size-icon-sm" />}
        onClick={() => onPick(project.key)}
      />
      <DropdownMenuItem
        label="Copy link"
        icon={<Copy className="size-icon-sm" />}
        onClick={() => {
          const url = new URL(window.location.href);
          url.searchParams.set("project", project.key);
          void navigator.clipboard.writeText(url.toString());
        }}
      />
      <Divider />
      <DropdownMenuItem
        label={favorited ? "Remove from favorites" : "Add to favorites"}
        icon={
          favorited ? <StarOff className="size-icon-sm" /> : <Star className="size-icon-sm" />
        }
        onClick={() => onToggleFavorite(project.key)}
      />
    </>
  );

  return (
    <ContextMenu menuContent={menu}>
    <div className="mb-0.5">
      <div className="group/project relative flex items-center">
        <button
          onClick={() => onPick(project.key)}
          title={`${project.name} · ${project.key}`}
          aria-current={active ? "page" : undefined}
          className={cn(navigationItem({ selected: active }), "px-1.5")}
        >
          <ProjectIcon
            color={catalogColor(project.color)}
            className={cn("size-icon-sm opacity-75", active && "opacity-100")}
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
          variant="ghost"
          size="sm"
          tooltip={favorited ? `Remove ${project.name} from favorites` : `Add ${project.name} to favorites`}
          icon={favorited ? <StarOff className="size-icon-xs" /> : <Star className="size-icon-xs" />}
        />
      </div>
    </div>
    </ContextMenu>
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
          className="text-mute hover:text-fg flex min-w-0 items-center gap-1 text-xs font-semibold tracking-[0.08em] uppercase"
          onClick={onToggle}
          aria-expanded={open}
        >
          {label}
        </button>
      ) : (
        <h2 className="text-mute flex min-w-0 items-center gap-1 text-xs font-semibold tracking-[0.08em] uppercase">
          {label}
        </h2>
      )}
      {action && <span className="ml-auto">{action}</span>}
    </div>
  );
}

function MiniSection({ title }: { title: string }) {
  return <p className="text-mute mt-1 px-2 text-2xs font-semibold tracking-[0.08em] uppercase">{title}</p>;
}

function NavItem({ icon, label, active, badge, compact, onClick }: { icon: React.ReactElement; label: string; active?: boolean; badge?: number; compact?: boolean; onClick: () => void }) {
  return (
    <button
      onClick={onClick}
      aria-current={active ? "page" : undefined}
      className={cn(
        navigationItem({
          selected: active,
          size: compact ? "sm" : "md",
        }),
      )}
    >
      <span className="text-mute [&>svg]:size-icon-sm">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {!!badge && <Badge variant="blue" label={badge} className="justify-center tabular-nums" />}
    </button>
  );
}

function StatusDot({ status }: { status: SpaceRow["status"] }) {
  const cls = { up: "bg-ok", idle: "bg-mute", missing: "bg-danger" }[status];
  const label = { up: "Local daemon running", idle: "Local daemon idle", missing: "Local replica unavailable" }[status];
  return <span className={cn("size-mark-xs shrink-0 rounded-full", cls)} title={label} role="img" aria-label={label} />;
}
