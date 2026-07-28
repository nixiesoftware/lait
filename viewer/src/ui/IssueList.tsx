import { useEffect, useMemo, useRef, useState } from "react";
import { CheckSquare, ChevronRight, Copy, ExternalLink, Plus, Trash2, UserRound } from "lucide-react";

import type { RowGroup } from "../core/display";
import { indexBy } from "../core/performance";
import type { LabelDto, MemberDto, Row, WorkflowState } from "../types";
import { Avatar, AvatarStack, memberName, stackFor } from "./Avatar";
import { ApplicationState } from "./AppState";
import { catalogColor } from "./colors";
import { PriorityIcon, StatusIcon } from "./icons";
import { ContextMenu, ContextMenuContent, ContextMenuItem, GroupHeader } from "./layout";
import { Button, Checkbox, IconButton, interactiveRow, LabelChips, LabelDots } from "./primitives";
import { dueLabel, dueTone } from "./time";

/**
 * The default view: one flat, grouped list.
 *
 * The groups arrive from `core/display.ts` — by status they are the board's own
 * columns (one fetch, two renderings), and the other axes are client-side
 * rearrangements of the same rows. Group *shape* changes; row identity, motion,
 * and selection never do.
 *
 * The density is the feature. Rows are a fixed 40px with a fixed column rhythm,
 * so the eye tracks straight down the ids and the titles without re-finding them
 * on each line — which is exactly what stops being true the moment a row grows to
 * fit its content. Fixed, but not fixed in pixels: `h-ctl-xl` is a rung on the
 * control ladder, scaled by `--scale`, so comfortable density carries the whole
 * rhythm up with it.
 */
export function IssueList({
  groups,
  deleted,
  deletedMode,
  states,
  members,
  labels,
  selection,
  checked,
  optimistic,
  onSelect,
  onToggleCheck,
  onOpen,
  onCreate,
  readOnly,
  filtered,
}: {
  groups: RowGroup[];
  /** The trash — tombstoned rows from `list all:true`, rendered as their own
   *  group. Separate from `groups` because a deleted issue is *not on the
   *  board* (deletion removes it from `boards[P]`); empty = trash hidden. */
  deleted: Row[];
  /** Deleted rows are a recovery destination, never an appendix to live work. */
  deletedMode: boolean;
  /** Board-ordered workflow, for a row's status glyph under non-status grouping. */
  states: WorkflowState[];
  /** The ACL, for resolving assignee keys to faces. */
  members: MemberDto[];
  labels: LabelDto[];
  selection: string | null;
  /** Bulk-selection checks, by canonical ref. */
  checked: ReadonlySet<string>;
  /** Docs carrying an unconfirmed local prediction. */
  optimistic: ReadonlySet<string>;
  onSelect: (reff: string) => void;
  onToggleCheck: (reff: string) => void;
  onOpen: (reff: string) => void;
  onCreate: (status: string) => void;
  readOnly: boolean;
  filtered: boolean;
}) {
  const visible = (g: RowGroup) => g.rows.filter((r) => !r.tombstone);
  const stateById = useMemo(
    () => indexBy(states, (state) => state.id),
    [states],
  );
  const checkAnchor = useRef<string | null>(null);
  const orderedRows = useMemo(
    () => deletedMode ? deleted : groups.flatMap((group) => visible(group)),
    [deletedMode, deleted, groups],
  );
  const checkRow = (reff: string, range: boolean) => {
    const anchor = checkAnchor.current;
    if (range && anchor) {
      const from = orderedRows.findIndex((row) => row.reff === anchor);
      const to = orderedRows.findIndex((row) => row.reff === reff);
      if (from >= 0 && to >= 0) {
        const desired = !checked.has(reff);
        for (const row of orderedRows.slice(Math.min(from, to), Math.max(from, to) + 1)) {
          if (checked.has(row.reff) !== desired) onToggleCheck(row.reff);
        }
        return;
      }
    }
    checkAnchor.current = reff;
    onToggleCheck(reff);
  };
  const total = deletedMode
    ? deleted.length
    : groups.reduce((n, g) => n + visible(g).length, 0);
  // The key column's measure: the longest key in the view, in characters. The
  // key renders in the mono font, where every glyph is exactly 1ch, so this is
  // the column width — Linear's rule. Sizing to the view's own maximum is what
  // a hardcoded 64px could never do: `COMP-9` and `COMP-25` share one edge, and
  // an `ACCESS-100` view simply gets a wider column instead of truncating.
  const keyCh = useMemo(
    () =>
      Math.max(
        0,
        ...groups.flatMap((g) => g.rows).concat(deleted).map((r) => (r.key_alias ?? r.reff).length),
      ),
    [groups, deleted],
  );
  // The due-date column exists per view, like the key column: if any visible
  // row has a date, every row reserves the slot so the avatar column has a
  // straight wall to align against — and if none does, no row pays 44px of
  // empty edge for a column with nothing in it.
  const anyDue = useMemo(
    () => groups.flatMap((g) => g.rows).concat(deleted).some((r) => r.due_date != null),
    [groups, deleted],
  );

  return (
    // No total across the top. Every group header already carries its own count,
    // so the summary row spent a full band of the viewport restating their sum —
    // and it sat between the tab strip and the first group, which is exactly
    // where the eye starts.
    <div
      className="flex min-h-0 flex-1 flex-col"
      style={{ "--key-col": `${keyCh}ch` } as React.CSSProperties}
    >
      {/* `@container`: the row's trailing cluster adapts to the width of this
          pane (which halves when the detail opens), not the viewport. */}
      <div className="@container min-h-0 flex-1 overflow-y-auto">
        {!deletedMode && groups.map((group) => (
          <Group
            key={group.key}
            group={group}
            rows={visible(group)}
            anyDue={anyDue}
            stateById={stateById}
            members={members}
            labels={labels}
            selection={selection}
            checked={checked}
            optimistic={optimistic}
            onSelect={onSelect}
            onToggleCheck={checkRow}
            onOpen={onOpen}
            onCreate={onCreate}
            readOnly={readOnly}
          />
        ))}
        {deleted.length > 0 && (
          <section>
            <GroupHeader
              sticky
              icon={<Trash2 className="text-mute size-icon-sm" />}
              title="Deleted"
              count={deleted.length}
            />
            <ul>
              {deleted.map((row) => (
                <IssueRow
                  key={row.reff}
                  row={row}
                  anyDue={anyDue}
                  state={stateById.get(row.status)}
                  members={members}
            labels={labels}
                  selected={row.reff === selection}
                  checked={checked.has(row.reff)}
                  anyChecked={checked.size > 0}
                  pending={optimistic.has(row.doc_id)}
                  onSelect={onSelect}
                  onToggleCheck={checkRow}
                  onOpen={onOpen}
                  readOnly={readOnly}
                />
              ))}
            </ul>
          </section>
        )}
        {total === 0 && (
          <ApplicationState
            kind={deletedMode ? "empty" : filtered ? "filtered-empty" : "empty"}
            title={deletedMode ? "No deleted issues" : filtered ? "No matching issues" : "No issues yet"}
            body={deletedMode ? "Deleted issues will appear here so they can be inspected or restored." : filtered ? "Clear or adjust the current filters to see more." : "Create the first issue in this project."}
            action={!deletedMode && !filtered && !readOnly && states[0] ? <Button variant="primary" onClick={() => onCreate(states[0]!.id)}><Plus className="size-icon-sm" /> New issue</Button> : undefined}
            className="min-h-60"
          />
        )}
      </div>
    </div>
  );
}

/** The group header's leading glyph: whatever the group *is*. */
function GroupIcon({ group, members }: { group: RowGroup; members: MemberDto[] }) {
  if (group.state) {
    return (
      <StatusIcon category={group.state.category} color={catalogColor(group.state.color)} />
    );
  }
  if (group.kind === "priority") {
    return <PriorityIcon priority={group.label as Row["priority"]} />;
  }
  if (group.kind === "assignee" && group.key !== "unassigned") {
    const m = members.find((x) => x.key === group.label);
    return <Avatar deviceKey={group.label} alias={m?.alias ?? ""} me={m?.me ?? false} size="sm" />;
  }
  return null;
}

function Group({
  group,
  rows,
  anyDue,
  stateById,
  members,
  labels,
  selection,
  checked,
  optimistic,
  onSelect,
  onToggleCheck,
  onOpen,
  onCreate,
  readOnly,
}: {
  group: RowGroup;
  rows: Row[];
  /** Whether this view reserves the due-date column — see the list's note. */
  anyDue: boolean;
  stateById: ReadonlyMap<string, WorkflowState>;
  members: MemberDto[];
  labels: LabelDto[];
  selection: string | null;
  checked: ReadonlySet<string>;
  optimistic: ReadonlySet<string>;
  onSelect: (reff: string) => void;
  onToggleCheck: (reff: string, range: boolean) => void;
  onOpen: (reff: string) => void;
  onCreate: (status: string) => void;
  readOnly: boolean;
}) {
  const [collapsed, setCollapsed] = useState(false);
  // An emptied group stays visible under status grouping (a status that exists
  // is a column that exists — filter.ts's rule); a derived group with no rows
  // is nothing at all, so it goes.
  if (rows.length === 0 && group.kind !== "status") return null;

  // An assignee group is labeled by a KEY; the human name is resolved here,
  // where the member list is (same rule as every other naming site).
  const title =
    group.kind === "assignee" && group.key !== "unassigned"
      ? memberName(group.label, members.find((m) => m.key === group.label))
      : group.label;

  return (
    <section>
      <GroupHeader
        sticky
        leading={
          /* The visible slot is the same 16px column as a row checkbox. The
             control itself remains 24px and overflows the slot symmetrically, so
             alignment does not come at the cost of a usable pointer target. */
          <span className="relative flex size-icon-md shrink-0 items-center justify-center">
            <IconButton
              label={`${collapsed ? "Expand" : "Collapse"} ${title}`}
              onClick={() => setCollapsed((value) => !value)}
              aria-expanded={!collapsed}
              className="absolute"
            >
              <ChevronRight className={`size-icon-xs transition-transform ${collapsed ? "" : "rotate-90"}`} />
            </IconButton>
          </span>
        }
        icon={<GroupIcon group={group} members={members} />}
        title={<span className="capitalize">{title}</span>}
        count={rows.length}
        actions={
          !readOnly && group.state ? (
            // Always visible, like Linear's: creating an issue in a bucket is
            // the header's one action, and an affordance you must hover to
            // discover is one most people never do. The ghost variant keeps it
            // quiet enough not to compete with the title.
            <IconButton
              label={`New issue in ${group.state.name}`}
              onClick={() => onCreate(group.state!.id)}
            >
              <Plus className="size-icon-sm" />
            </IconButton>
          ) : undefined
        }
      />
      {!collapsed && <ul aria-label={`${title} issues`} data-issue-collection>
        {rows.map((row) => (
          <IssueRow
            key={row.reff}
            row={row}
            anyDue={anyDue}
            state={stateById.get(row.status)}
            members={members}
            labels={labels}
            selected={row.reff === selection}
            checked={checked.has(row.reff)}
            anyChecked={checked.size > 0}
            pending={optimistic.has(row.doc_id)}
            onSelect={onSelect}
            onToggleCheck={onToggleCheck}
            onOpen={onOpen}
            readOnly={readOnly}
          />
        ))}
      </ul>}
    </section>
  );
}

function IssueRow({
  row,
  anyDue,
  state,
  members,
  labels,
  selected,
  checked,
  anyChecked,
  pending,
  onSelect,
  onToggleCheck,
  onOpen,
  readOnly,
}: {
  row: Row;
  /** Whether this view reserves the due-date column. */
  anyDue: boolean;
  state: WorkflowState | undefined;
  members: MemberDto[];
  labels: LabelDto[];
  selected: boolean;
  checked: boolean;
  /** While any check exists the whole column shows, so targets stay aligned. */
  anyChecked: boolean;
  pending: boolean;
  onSelect: (reff: string) => void;
  onToggleCheck: (reff: string, range: boolean) => void;
  onOpen: (reff: string) => void;
  readOnly: boolean;
}) {
  const el = useRef<HTMLLIElement>(null);

  // Selection moves by keyboard, so it must drag the viewport with it — a
  // selected row below the fold is indistinguishable from a dropped keypress.
  useEffect(() => {
    if (selected) {
      el.current?.scrollIntoView({ block: "nearest" });
      if (document.activeElement?.closest("[data-issue-collection]")) {
        el.current?.focus({ preventScroll: true });
      }
    }
  }, [selected]);

  return (
    <ContextMenu.Root
      onOpenChange={(open) => {
        // Selecting, not opening: a right click asks "what can I do to this
        // one", so the row has to become the row the menu is about. Left click
        // additionally calls `onOpen`, and doing that here would answer the
        // question by navigating away from it.
        //
        // Selection rather than DOM focus, deliberately. Radix moves focus into
        // the menu on open and hands it back to the trigger on close; forcing
        // focus onto the row would fight it and cost the menu its keyboard
        // navigation. `aria-current` and the active fill are what "this one"
        // needs to look like.
        if (open) onSelect(row.reff);
      }}
    >
      {/* The whole row is the trigger. `asChild` keeps it an `<li>` — a wrapper
          element here would sit between the `<ul>` and its items and break the
          list semantics screen readers navigate by. */}
      <ContextMenu.Trigger asChild>
    <li
      ref={el}
      className={clsxish([
        interactiveRow({ selected, size: "xl" }),
        // One step above the group header that introduces them. The header is
        // punctuation between piles; the rows are the thing you came to read,
        // and when the two were level the list had no figure and ground.
        "group/row flex h-ctl-xl items-center gap-2 px-4",
        checked && !selected && "bg-accent/5 shadow-[inset_2px_0_var(--color-accent)]",
        // Radix marks its trigger while the menu is up. Matching the selected
        // fill means the row reads as the subject of the menu on the very first
        // frame, rather than after the selection round-trips through the app.
        "data-[state=open]:bg-active data-[state=open]:text-fg",
        // A row whose body hasn't synced yet is real but not yet trustworthy;
        // say so quietly rather than rendering it as settled (UI.md §3.3).
        row.provisional && "opacity-60",
        row.tombstone && "opacity-60",
      ])}
      onClick={(event) => {
        event.currentTarget.focus({ preventScroll: true });
        onSelect(row.reff);
        onOpen(row.reff);
      }}
      onKeyDown={(event) => {
        if (event.target === event.currentTarget && event.key === "Enter") {
          event.preventDefault();
          onOpen(row.reff);
        }
      }}
      aria-current={selected ? "true" : undefined}
      data-issue-ref={row.reff}
      data-bulk-selected={checked || undefined}
      tabIndex={selected ? 0 : -1}
    >
      {/* This 16px column is shared with the group chevron above it. Keeping the
          selection affordance in that column lets priority/status/title retain
          exactly the same geometry when the checkbox appears. */}
      <span className="flex size-icon-md shrink-0 items-center justify-center">
        {!readOnly && (
          <Checkbox
            checked={checked}
            onCheckedChange={() => onToggleCheck(row.reff, false)}
            onClick={(event) => {
              event.stopPropagation();
              if ((event.nativeEvent as MouseEvent).shiftKey) {
                event.preventDefault();
                onToggleCheck(row.reff, true);
              }
            }}
            aria-label={`Select ${row.key_alias ?? row.reff}`}
            className={clsxish([
              !anyChecked && !checked &&
                "opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100",
            ])}
          />
        )}
      </span>
      <PriorityIcon priority={row.priority} />
      {/* One column for every key in the view, sized by the list to its longest
          key (`--key-col`, in ch — exact in this mono font). Content-width was
          tried and misaligns: rows share a key *prefix* but not a digit count,
          so `COMP-9` sat one character narrower than `COMP-25` and everything
          after it drifted. A column sized to the view's own maximum keeps the
          status glyphs and titles on one edge without a hardcoded width to go
          stale or truncate. */}
      <span className="text-mute shrink-0 font-mono text-xs tabular-nums min-w-[var(--key-col)]">
        {row.key_alias ?? row.reff}
      </span>
      {state && <StatusIcon category={state.category} color={catalogColor(state.color)} />}
      <span
        className={clsxish(["min-w-0 flex-1 truncate font-medium", row.tombstone && "text-mute line-through"])}
      >
        {row.title}
      </span>
      {row.tombstone && (
        <Trash2 className="text-mute size-icon-xs shrink-0" aria-label="Deleted" />
      )}
      {/* Two is what a dense line affords, and the rest are simply not shown:
          a trailing `+2` is a tally of things you cannot see, competing for the
          same edge as the date. The full set is one click away in the detail.
          Below 40rem of *pane* (the detail opening halves it), pills don't fit
          beside a title at all, so the set collapses to Linear's dots-pill —
          the same labels as colour plus a count, instead of pills crushing the
          title or vanishing entirely. */}
      <LabelChips
        names={row.label_names ?? []}
        colorOf={(name) => labels.find((l) => l.name === name)?.color ?? "gray"}
        max={2}
        showOverflow={false}
        size="sm"
        className="hidden shrink-0 flex-nowrap @min-[40rem]:flex"
      />
      <LabelDots
        names={row.label_names ?? []}
        colorOf={(name) => labels.find((l) => l.name === name)?.color ?? "gray"}
        className="@min-[40rem]:hidden"
      />
      {/* Unconfirmed: shown as truth because that is what makes a write feel
          instant, but never *claimed* as truth. */}
      {pending && (
        <span
          className="bg-accent size-mark-xs shrink-0 animate-pulse rounded-full"
          title="Not confirmed by the daemon yet"
          aria-label="Pending"
        />
      )}
      {/* Faces, not `assignee_summary` — that string is the terminal's projection
          ("you +1"), and this row has a fixed 32px rhythm to keep. Every row
          renders the slot — an AvatarStack with nobody in it renders nothing,
          and a column that only exists when occupied is not a column. Linear's
          answer, taken whole: the empty slot draws the dashed ghost, so
          "unassigned" is a visible state rather than an absence. The slot is
          content-width, right edges flush: a wider stack grows leftward into
          the labels' slack, exactly as Linear's does — a fixed 56px column put
          36px of dead air between the labels and every single face. */}
      <span className="flex shrink-0 items-center justify-end">
        {row.assignees.length > 0 ? (
          <AvatarStack members={stackFor(row.assignees, members)} />
        ) : (
          <span
            className="border-line-strong text-mute flex size-avatar-md items-center justify-center rounded-full border border-dashed"
            title="Unassigned"
            aria-label="Unassigned"
          >
            <UserRound className="size-icon-xs opacity-60" />
          </span>
        )}
      </span>
      {/* Last, against the row's trailing edge. A date is the one field you
          scan down a column rather than read across a row, so it wants a
          straight right edge of its own — putting it before the faces gave it a
          ragged one that moved with however many people were assigned. The
          column exists per view (`anyDue`): every row reserves it when any row
          needs it, and no row pays 44px of empty edge when none does. */}
      {anyDue && (
        <span
          className={clsxish([
            // A fixed, right-aligned column: `Aug 14` and `Sep 3` are different
            // widths, and a date you read down a list has to start and end in
            // the same place on every row or the column stops being one.
            "w-11 shrink-0 text-right text-2xs tabular-nums",
            row.due_date != null &&
              { overdue: "text-danger", soon: "text-warn", later: "text-mute" }[
                dueTone(row.due_date)
              ],
          ])}
          title={row.due_date != null ? "Due date" : undefined}
        >
          {row.due_date != null && dueLabel(row.due_date)}
        </span>
      )}

    </li>
      </ContextMenu.Trigger>
      <ContextMenu.Portal>
        <ContextMenuContent>
          <ContextMenuItem onSelect={() => onOpen(row.reff)}>
            <ExternalLink className="size-icon-sm" />
            Open focused
          </ContextMenuItem>
          <ContextMenuItem
            onSelect={() => {
              const url = new URL(window.location.href);
              url.searchParams.set("issue", row.reff);
              url.searchParams.set("focus", "1");
              void navigator.clipboard.writeText(url.toString());
            }}
          >
            <Copy className="size-icon-sm" />
            Copy link
          </ContextMenuItem>
          {!readOnly && (
            <ContextMenuItem onSelect={() => onToggleCheck(row.reff, false)}>
              <CheckSquare className="size-icon-sm" />
              {checked ? "Remove from selection" : "Add to selection"}
            </ContextMenuItem>
          )}
        </ContextMenuContent>
      </ContextMenu.Portal>
    </ContextMenu.Root>
  );
}


/**
 * The row's labels, to the width the row can spare.
 *
 * A row that ended at its title left most of its width empty and made every
 * issue look alike; labels are the property that most often distinguishes two
 * issues with similar titles. Two chips, then a count — the third chip would
 * start competing with the title for the truncation budget, and a label you
 * cannot read is only noise with a colour.
 */

/** Tiny local join — `clsx` is a dependency, but a 3-line filter beats an import
 *  for the two call sites that need it. */
function clsxish(parts: Array<string | false | undefined>): string {
  return parts.filter(Boolean).join(" ");
}
