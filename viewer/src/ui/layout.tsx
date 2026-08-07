import { createContext, useContext, useId, useMemo, useState } from "react";

import type { ProjectView } from "../core/registry";
import { loadRailCollapsed, saveRailCollapsed } from "../core/railState";
import { createPortal } from "react-dom";

import { cn, crumbGlyph } from "./primitives";
import {
  Activity,
  Bot,
  Calendar,
  ChevronRight,
  FileText,
  Folder,
  FolderKanban,
  GanttChart,
  House,
  Inbox,
  List,
  SquareKanban,
  UserRound,
} from "lucide-react";

/**
 * Every surface's title bar: one frame, three slots.
 *
 * The frame is not negotiable — height, padding, gap and container are the same
 * on every surface, so the bar does not move when you switch views. What each
 * view *puts* in it is: a leading control (the sidebar toggle, Settings' way
 * back), the trail, and whatever actions that view owns. Callers used to render
 * their own children into a bare `<header>` and each drifted its own gap and
 * inset; the slots are what stop that from being expressible.
 *
 * The `@container` belongs here rather than on each caller's wrapper. The trail
 * inside measures itself in container units — a crumb ceiling of `32cqw`, an
 * ancestor that drops at `@max-[560px]` — and when each surface declared its own
 * container those queries silently measured a different box per surface, so the
 * same trail collapsed at three different widths.
 */
export function SurfaceHeader({
  leading,
  trail,
  actions,
  className,
}: {
  leading?: React.ReactNode;
  trail?: React.ReactNode;
  actions?: React.ReactNode;
  className?: string;
}) {
  return (
    <header
      className={cn(
        // 44px, not 32. The trail's leaf is a wrapping issue title, and two lines
        // of it need the room; a bar sized to one line would have made the title
        // the only thing the bar could not hold.
        "border-line/70 @container flex h-bar-lg shrink-0 items-center gap-1 border-b px-2",
        className,
      )}
    >
      {leading}
      {trail}
      {actions && <span className="ml-auto flex items-center gap-0.5">{actions}</span>}
    </header>
  );
}

/**
 * A band of controls hanging under a `SurfaceHeader`.
 *
 * Deliberately shorter than the header above it. It does not *name* anything —
 * it qualifies what the header already named — and a band at the header's own
 * height reads as a second header, which is how the filter row and the status
 * slices used to look like two titles stacked. Height is the whole argument, so
 * it is the one thing this component will not let a caller override.
 */
export function Toolbar({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        // No bottom rule: the bar and the rows under it are one surface —
        // the slices act on the list directly, and a line between them read
        // as a boundary between two things rather than a header for one.
        "flex shrink-0 items-center gap-1 px-2",
        className,
        // 32px, unchanged — the AIR came from the controls, not from here.
        // They were 28px in this band and left 2px above and below, which is
        // why it read as packed; at 24px (`toolbarControl`) the same 32px band
        // gives 4px. Double the padding for no extra header, which is the whole
        // point: a shorter control buys breathing room a taller bar would have
        // charged the page for.
        "h-bar-sm",
      )}
    >
      {children}
    </div>
  );
}

/**
 * The header of a section *within* a surface — a list's status group, a board's
 * column.
 *
 * Not a `SurfaceHeader`: that one names the surface and there is exactly one of
 * it, at the top of the window. This one names a pile of rows and repeats down
 * the page. They were both bare `<header>` tags carrying their geometry inline,
 * which is why growing "all the headers" meant finding five string literals in
 * three files; the distinction they were drawing was real but written nowhere.
 */
export function GroupHeader({
  leading,
  icon,
  title,
  count,
  meta,
  actions,
  sticky = false,
  className,
}: {
  /** A control before the glyph — the collapse chevron. */
  leading?: React.ReactNode;
  /** What the group *is*: a status, a priority, a face. */
  icon?: React.ReactNode;
  title: React.ReactNode;
  count?: number | undefined;
  /** Inline after the count — a note about the group, not an action on it. */
  meta?: React.ReactNode;
  /** Pushed to the trailing edge. */
  actions?: React.ReactNode;
  /**
   * Pinned while its rows scroll under it. A list says yes — losing track of
   * which bucket you are reading is the one piece of context a long list
   * silently takes away. A board column scrolls in its own axis and its header
   * never leaves, so it says no.
   */
  sticky?: boolean;
  className?: string;
}) {
  return (
    <header
      className={cn(
        // Below the surface header, above nothing: it sits *inside* the surface
        // and repeats down it, so matching the bar that names the whole page
        // made the divider between two piles of issues as loud as the page. It
        // stays one step under the rows it introduces — the header is
        // punctuation, the rows are the text.
        "flex h-bar-md shrink-0 items-center gap-2",
        sticky
          ? "bg-bg/95 border-line/70 sticky top-0 z-10 border-b px-4 backdrop-blur-sm"
          : "px-1",
        className,
      )}
    >
      {leading}
      {icon}
      {/* A collapsed board column passes no title, and an empty `h2` would be a
          heading announcing nothing — worse than no heading at all. */}
      {title != null && <h2 className="text-base font-semibold">{title}</h2>}
      {count !== undefined && <span className="text-mute text-sm tabular-nums">{count}</span>}
      {meta}
      {actions && <span className="ml-auto flex items-center gap-0.5">{actions}</span>}
    </header>
  );
}

/**
 * The header's actions slot, filled from wherever the view happens to live.
 *
 * A view's own controls belong to that view — the issue pager needs the issue's
 * neighbours, its overflow menu needs the mutation state sitting inside
 * `IssueDetail` — but they have to *draw* in the one header the shell owns and
 * never unmounts. Lifting that state into the shell to place four buttons would
 * be paying for the wrong thing, so the buttons travel instead: the shell hangs
 * an outlet, the view renders `HeaderActions`, and React puts them in the right
 * box without either side learning about the other.
 */
const HeaderSlot = createContext<{
  node: HTMLElement | null;
  attach: (el: HTMLElement | null) => void;
} | null>(null);

export function HeaderSlotProvider({ children }: { children: React.ReactNode }) {
  const [node, setNode] = useState<HTMLElement | null>(null);
  const value = useMemo(() => ({ node, attach: setNode }), [node]);
  return <HeaderSlot.Provider value={value}>{children}</HeaderSlot.Provider>;
}

/** Rendered by the shell, into `SurfaceHeader`'s `actions` slot. */
export function HeaderActionsOutlet() {
  const slot = useContext(HeaderSlot);
  return <span ref={slot?.attach} className="flex items-center gap-0.5" />;
}

/** Rendered by a view. Its children draw in the shell's header. */
export function HeaderActions({ children }: { children: React.ReactNode }) {
  const slot = useContext(HeaderSlot);
  return slot?.node ? createPortal(children, slot.node) : null;
}

/**
 * One hop of the trail.
 *
 * The trail is an *object* path — workspace, project, issue — never the route you
 * took to get here. A surface with a tab strip (a project's views, Settings' rail)
 * therefore ends its trail at the object the tabs belong to: the strip already says
 * which face of it you are looking at, and naming it twice was the old duplication.
 */
export type BreadcrumbItem = {
  key: string;
  content: React.ReactNode;
  /** Ancestors climb; the leaf is where you already are and never navigates. */
  onNavigate?: (() => void) | undefined;
  /** Accessible name, for crumbs whose content mixes a glyph with text. */
  label?: string | undefined;
  /** Content that brings its own trigger geometry (a picker) — don't pad it twice. */
  control?: boolean | undefined;
  /** Ancestors may drop out on a narrow surface. The leaf never does. */
  optional?: boolean | undefined;
};

/** Shared crumb geometry: a link, a static crumb and a picker crumb must land on
 *  the same baseline and the same padding, or the trail visibly steps. The picker
 *  crumb gets here as `tone="quiet" size="sm"`, which resolves to these same
 *  values — the `sm` rung IS this height, so the two cannot drift apart the way
 *  they could when the crumb's height was hard-coded inside its own variant. */
const crumbFace = "flex min-h-ctl-sm min-w-0 items-center gap-1.5 rounded-full transition-colors";


export function Breadcrumbs({
  items,
  className,
}: {
  items: BreadcrumbItem[];
  className?: string;
}) {
  return (
    <nav aria-label="Breadcrumb" className={cn("min-w-0 flex-1 overflow-hidden", className)}>
      <ol className="flex min-w-0 items-center text-sm">
        {items.map((item, index) => {
          const leaf = index === items.length - 1;
          return (
            <li
              key={item.key}
              className={cn(
                // Named group so a crumb that carries a control can reveal its
                // affordance on hover — the crumb, not some outer row, is what
                // the pointer is addressing.
                "group/crumb flex min-w-0 items-center",
                // The leaf takes the slack and truncates last; ancestors hold their
                // width up to a ceiling so a long project name can't erase them.
                leaf ? "flex-1" : "max-w-[min(32cqw,240px)] shrink-0",
                !leaf && item.optional && "@max-[560px]:hidden",
              )}
            >
              {item.onNavigate ? (
                <button
                  type="button"
                  aria-label={item.label}
                  onClick={item.onNavigate}
                  className={cn(
                    crumbFace,
                    "text-dim hover:bg-hover hover:text-fg focus-visible:ring-accent/50 -mx-1 px-1.5 outline-none focus-visible:ring-1",
                  )}
                >
                  {item.content}
                </button>
              ) : (
                <span
                  aria-current={leaf ? "page" : undefined}
                  aria-label={item.label}
                  className={cn(
                    crumbFace,
                    leaf ? "text-fg font-medium" : "text-dim",
                    !item.control && "-mx-1 px-1.5",
                  )}
                >
                  {item.content}
                </span>
              )}
              {/* The separator belongs to the crumb before it: a dropped ancestor
                  takes its chevron with it, so the trail never opens with a stray ›. */}
              {!leaf && <ChevronRight className="text-mute mx-0.5 size-icon-xs shrink-0" aria-hidden />}
            </li>
          );
        })}
      </ol>
    </nav>
  );
}

/** The space, drawn the way the sidebar's space switcher draws it. */
export function WorkspaceCrumb({ name, agent }: { name: string; agent?: boolean | undefined }) {
  const Glyph = agent ? Bot : Folder;
  return (
    <>
      <span className={cn(crumbGlyph, "bg-active rounded-mark")}>
        <Glyph className="text-mute size-icon-2xs" aria-hidden />
      </span>
      <span className="truncate">{name}</span>
    </>
  );
}

/** A project, drawn the way the sidebar, the project cards and the project picker
 *  draw it — so the crumb doesn't change shape when a second project turns it
 *  into a picker. */
export function ProjectCrumb({ name, color }: { name: string; color?: string | undefined }) {
  return (
    <>
      <span className={crumbGlyph} aria-hidden>
        <span
          className="size-mark-sm rounded-mark"
          style={{ background: color ?? "var(--color-mute)" }}
        />
      </span>
      <span className="truncate">{name}</span>
    </>
  );
}

/**
 * An issue: the key is the identity, the title is context.
 *
 * Both are the trail's own text size. The key used to be `font-mono text-xs`,
 * which put two type scales inside a single crumb and made the leaf read as a
 * label glued to a heading rather than one name. Tabular figures keep the keys
 * from dancing as you page between issues; that much the mono font was right
 * about.
 */
export function IssueCrumb({ id, title }: { id: string; title?: string | undefined }) {
  return (
    <>
      <span className="text-mute shrink-0 tabular-nums">{id}</span>
      {/* Wraps rather than running the width of the bar and ending in an
          ellipsis. A title is the sentence that says what the issue *is*, and a
          single line of it cut at the window edge is the half you can already
          guess; two lines usually reach the end. It still clamps, because a bar
          that grows with its content is a bar whose contents move. */}
      {title && <span className="line-clamp-2 min-w-0">{title}</span>}
    </>
  );
}

/**
 * A Spec: the kind is the identity, the title is the document.
 *
 * Same two-part shape as an issue's crumb, and for the same reason — but where
 * an issue leads with a key you can quote, a Spec leads with what it *is*. There
 * is no per-project alias to put there, and the revision coordinate that does
 * identify it exactly has no business in a header bar.
 */
export function SpecCrumb({ kind, title }: { kind: string; title?: string | undefined }) {
  return (
    <>
      <span className="text-mute shrink-0">{kind}</span>
      {title && <span className="line-clamp-2 min-w-0">{title}</span>}
    </>
  );
}

/** A workspace destination — its own root, carrying the sidebar's icon for it. */
export function DestinationCrumb({ icon, label }: { icon?: React.ReactNode; label: string }) {
  return (
    <>
      {icon && <span className={cn(crumbGlyph, "text-mute [&>svg]:size-icon-sm")}>{icon}</span>}
      <span className="truncate">{label}</span>
    </>
  );
}

/** One icon per project face, so the sidebar's tree names them the way the rest
 *  of the app draws them. */
export const PROJECT_VIEW_ICON: Record<ProjectView, React.ReactElement> = {
  overview: <House />,
  list: <List />,
  board: <SquareKanban />,
  calendar: <Calendar />,
  activity: <Activity />,
  specs: <FileText />,
};

/** One icon per destination, shared by the sidebar and the header crumb so the two
 *  can't drift apart. */
export const DESTINATION_ICON = {
  inbox: <Inbox />,
  "my-issues": <UserRound />,
  projects: <FolderKanban />,
  timeline: <GanttChart />,
  workspace: <Folder />,
} as const;

export function SectionHeader({
  title,
  meta,
  action,
  className,
}: {
  title: React.ReactNode;
  meta?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex min-h-ctl-sm items-center gap-2", className)}>
      <h3 className="text-mute text-2xs font-semibold tracking-wider uppercase">{title}</h3>
      {meta && <span className="text-mute text-xs">{meta}</span>}
      {action && <span className="ml-auto">{action}</span>}
    </div>
  );
}

export function PropertyRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="issue-property group/prop flex min-h-ctl-md items-center gap-2">
      <dt className="text-mute w-20 shrink-0">{label}</dt>
      <dd className="min-w-0 flex-1">{children}</dd>
    </div>
  );
}

/**
 * The issue's property aside, after Linear's.
 *
 * Two decisions carry it, and both are departures from the label/value grid
 * this replaces.
 *
 * **A row has no visible term.** The glyph and the value carry the field —
 * a coloured status dot beside "Done" is not ambiguous — so the rail spends one
 * column instead of two, and the values start at the rail's left edge rather
 * than 88px into it. The term stays in the `<dl>` for assistive technology and
 * comes back as the row's tooltip, so nothing is actually lost; it just stops
 * being printed nine times.
 *
 * **An unset property reads as a verb.** `Set priority`, not `Priority · None`.
 * The old rail spent five rows on an empty issue announcing that priority,
 * assignees, estimate, due date and milestone were all absent. A verb spends
 * the same row offering to fix it — which is the only reason you were looking.
 *
 * Grouping comes from captions between runs of rows. Each group is its own
 * `<dl>` so the caption can be a real heading rather than a `<dt>` pretending.
 */
export function RailSection({
  title,
  children,
}: {
  /** Omit on the leading group — Linear leaves its first run uncaptioned. */
  title?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="rail-section flex flex-col">
      {title && (
        // The caption now carries ALL the separation between groups, because the
        // rail itself no longer has a gap. Deliberately asymmetric: more above
        // than below, so a caption belongs to the rows under it instead of
        // floating midway between two groups. That is also what removes the
        // seam between the properties and the metadata below them — the rail
        // used to spend the gap twice, once as its own `gap-3` and again under
        // every caption, which read as a break rather than a heading.
        <h3 className="rail-caption text-mute text-2xs mt-3 mb-1 font-semibold tracking-wider uppercase">
          {title}
        </h3>
      )}
      <dl className="flex flex-col">{children}</dl>
    </section>
  );
}

/**
 * A card in the project rail — and deliberately *not* the grammar `RailSection`
 * above uses.
 *
 * An issue's rail is metadata about a document you are reading, so it recedes:
 * captions over a hairline column, no borders, nothing to click. A project's
 * rail is the console for something you are running, so it does the opposite.
 * Each card is a bordered surface with a header that carries its own verb, its
 * rows go somewhere, and the whole thing folds when you are done with it.
 *
 * The two surfaces used to share one aside on the theory that a project and an
 * issue are the same kind of object. They are not: one is a thing you read, the
 * other is a thing you steer.
 *
 * `id` keys the fold preference and must stay stable — it is not the title,
 * which is a display string.
 */
export function RailCard({
  title,
  id,
  action,
  children,
}: {
  title: string;
  id: string;
  /** A trailing control in the header, typically the `+` that adds to the card. */
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  const [collapsed, setCollapsed] = useState(() => loadRailCollapsed(id));
  const bodyId = useId();

  const toggle = () => {
    setCollapsed((was) => {
      saveRailCollapsed(id, !was);
      return !was;
    });
  };

  return (
    <section className="border-line bg-raised rounded-surface border">
      <div className="flex h-ctl-lg items-center gap-1 px-2">
        <button
          type="button"
          onClick={toggle}
          aria-expanded={!collapsed}
          aria-controls={bodyId}
          className="text-dim hover:text-fg flex min-w-0 items-center gap-1 rounded-control px-1 text-sm font-medium outline-none focus-visible:ring-1 focus-visible:ring-accent/50"
        >
          <span className="truncate">{title}</span>
          <ChevronRight
            className={cn("size-icon-sm shrink-0 transition-transform", !collapsed && "rotate-90")}
            aria-hidden
          />
        </button>
        {/* Always visible, unlike the document's `Disclosure`. A console's verbs
            do not hide until you find them with the pointer. */}
        {action && <span className="ml-auto flex items-center">{action}</span>}
      </div>
      {!collapsed && <div id={bodyId} className="px-2 pb-2">{children}</div>}
    </section>
  );
}

export function RailRow({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    // `title` restores the term to the pointer. It is the same string as the
    // `<dt>`, so the tooltip and the screen reader agree by construction.
    // The vertical padding is the ONLY space between two rows — each row is a
    // 28px control, so `py` is doubled into the gap between neighbours. Kept
    // non-zero rather than removed: a wrapped run of labels overflows the 28px
    // minimum, and at zero the first and last chip sit flush against the rows
    // above and below. 2px is the least that still reads as a gap between two
    // adjacent hover pills.
    <div className="issue-property group/prop flex min-h-ctl-md items-center gap-2 py-0.5" title={label}>
      <dt className="sr-only">{label}</dt>
      <dd className="min-w-0 flex-1">{children}</dd>
    </div>
  );
}

/**
 * A collapsible group in a document body — Linear's `▸ Sub-issues 0/1` and
 * Jira's `⌄ Linked work items`, which are the same control.
 *
 * This replaces a stack of uppercase captions. The band between an issue's
 * description and its activity could put seven of them on screen at once
 * (attachments, parent, sub-issues, blocked by, blocks, related, duplicates),
 * each a caption over one or two rows, and captions do not compress: seven of
 * them read as seven sections regardless of how little is in each.
 *
 * A disclosure header is one line that carries its own count, so a group with
 * nothing in it costs nothing and a group with three rows costs a line. Open by
 * default, because the point is to remove chrome, not to hide the work — the
 * collapse is there for a body with a long sub-issue list.
 */
export function Disclosure({
  title,
  count,
  action,
  defaultOpen = true,
  children,
}: {
  title: string;
  /** Shown beside the title — `3`, or `2/5` for a done-over-total. */
  count?: React.ReactNode;
  /** A trailing control, typically the `+` that adds to this group. */
  action?: React.ReactNode;
  defaultOpen?: boolean;
  children: React.ReactNode;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const id = useId();

  return (
    <section className="flex flex-col">
      <div className="group/disc flex h-ctl-lg items-center gap-1">
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          aria-expanded={open}
          aria-controls={id}
          className="text-dim hover:text-fg -ml-1 flex min-w-0 items-center gap-1 rounded-control px-1 py-0.5 text-sm font-medium outline-none focus-visible:ring-1 focus-visible:ring-accent/50"
        >
          <ChevronRight
            className={cn("size-icon-sm shrink-0 transition-transform", open && "rotate-90")}
            aria-hidden
          />
          <span className="truncate">{title}</span>
          {count !== undefined && (
            <span className="text-mute ml-1 shrink-0 tabular-nums">{count}</span>
          )}
        </button>
        {action && (
          // Revealed on hover like the rest of the row affordances, but always
          // present once the group is focused or the pointer is anywhere in it.
          <span className="ml-auto opacity-0 transition-opacity group-hover/disc:opacity-100 focus-within:opacity-100">
            {action}
          </span>
        )}
      </div>
      <div id={id} hidden={!open} className="flex flex-col gap-1 pb-1">
        {children}
      </div>
    </section>
  );
}

export function Toast({
  children,
  action,
  className,
}: {
  children: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn("border-line bg-raised text-dim flex items-center gap-3 rounded-control border px-3 py-2 text-sm", className)}
      role="status"
      aria-live="polite"
    >
      <span className="min-w-0 flex-1">{children}</span>
      {action}
    </div>
  );
}


/** The context-menu spelling of `MenuSub*` — same surface, summoned by the
 *  right button. Radix ships the two menus as separate primitives, so each
 *  needs its own wrapper to keep them indistinguishable on screen. */

/**
 * A menu row that opens another menu.
 *
 * The list a menu *could* inline is often the part you need least: a space
 * switcher that enumerates every replica pushes the settings you came for below
 * a scrollbar. One row that admits there is more behind it keeps the menu the
 * size of its verbs.
 */
