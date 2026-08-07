import { useEffect, useMemo, useRef, useState } from "react";
import { CalendarPlus, ChevronRight, Plus, Trash2 } from "lucide-react";

import type { RowGroup } from "../core/display";
import { indexBy } from "../core/performance";
import type { LabelDto, MemberDto, Row, WorkflowState } from "../types";
import { Avatar, memberName } from "./Avatar";
import { ApplicationState } from "./AppState";
import { catalogColor } from "./colors";
import {
  AssigneeChip,
  DueChip,
  fromRowControl,
  LabelsChip,
  PriorityChip,
  StatusChip,
  type IssueMutators,
} from "./fields";
import { PriorityIcon, StatusIcon } from "./icons";
import { IssueMenuItems } from "./IssueMenu";
import { GroupHeader } from "./layout";
import { Button, CheckboxInput, ContextMenu, IconButton } from "@astryxdesign/core";
import { interactiveRow } from "./primitives";
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
  mutators,
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
  /** In-place field writes — every chip and context submenu resolves here. */
  mutators: IssueMutators;
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
            states={states}
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
            mutators={mutators}
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
            <ul data-issue-collection>
              {deleted.map((row) => (
                <IssueRow
                  key={row.reff}
                  row={row}
                  anyDue={anyDue}
                  state={stateById.get(row.status)}
                  states={states}
                  members={members}
                  labels={labels}
                  selected={row.reff === selection}
                  checked={checked.has(row.reff)}
                  anyChecked={checked.size > 0}
                  pending={optimistic.has(row.doc_id)}
                  onSelect={onSelect}
                  onToggleCheck={checkRow}
                  onOpen={onOpen}
                  mutators={mutators}
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
            action={!deletedMode && !filtered && !readOnly && states[0] ? <Button
                                                                            onClick={() => onCreate(states[0]!.id)}
                                                                            icon={<Plus className="size-icon-sm" />}
                                                                            label="New issue"
                                                                            variant="primary"
                                                                            size="sm"
                                                                          /> : undefined}
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
  states,
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
  mutators,
  readOnly,
}: {
  group: RowGroup;
  rows: Row[];
  /** Whether this view reserves the due-date column — see the list's note. */
  anyDue: boolean;
  states: WorkflowState[];
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
  mutators: IssueMutators;
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
              variant="ghost"
              size="sm"
              tooltip={`${collapsed ? "Expand" : "Collapse"} ${title}`}
              icon={<ChevronRight className={`size-icon-xs transition-transform ${collapsed ? "" : "rotate-90"}`} />}
            />
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
              variant="ghost"
              size="sm"
              tooltip={`New issue in ${group.state.name}`}
              icon={<Plus className="size-icon-sm" />}
            />
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
            states={states}
            members={members}
            labels={labels}
            selected={row.reff === selection}
            checked={checked.has(row.reff)}
            anyChecked={checked.size > 0}
            pending={optimistic.has(row.doc_id)}
            onSelect={onSelect}
            onToggleCheck={onToggleCheck}
            onOpen={onOpen}
            mutators={mutators}
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
  states,
  members,
  labels,
  selected,
  checked,
  anyChecked,
  pending,
  onSelect,
  onToggleCheck,
  onOpen,
  mutators,
  readOnly,
}: {
  row: Row;
  /** Whether this view reserves the due-date column. */
  anyDue: boolean;
  state: WorkflowState | undefined;
  states: WorkflowState[];
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
  mutators: IssueMutators;
  readOnly: boolean;
}) {
  const el = useRef<HTMLLIElement>(null);
  // The same lock the detail rail applies: a read-only space cannot write, and
  // a provisional or deleted row is not yet (or no longer) a thing to edit.
  const locked = readOnly || row.provisional || row.tombstone;

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

  // The row's verbs. The definition lives in `IssueMenu` because the board
  // opens the same menu over the same issue — see the note there.
  const menu = (
    <IssueMenuItems
      reff={row.reff}
      status={row.status}
      priority={row.priority}
      assignees={row.assignees}
      labelNames={row.label_names ?? []}
      states={states}
      members={members}
      labels={labels}
      mutators={mutators}
      locked={locked}
      onOpen={onOpen}
      {...(readOnly ? {} : { selection: { checked, onToggle: (r: string) => onToggleCheck(r, false) } })}
    />
  );

  return (
    // Astryx's ContextMenu wraps its trigger in a `<div>` and offers no
    // `asChild`, which would put that div between the `<ul>` and its `<li>`s and
    // break the list semantics screen readers navigate by. `display: contents`
    // on it — see `[data-issue-collection] > div` in `styles.css` — takes the
    // wrapper out of BOTH layout and the accessibility tree, so the row is a
    // direct child of the list again in the only two senses that matter.
    <ContextMenu
      onOpenChange={(open) => {
        // Selecting, not opening: a right click asks "what can I do to this
        // one", so the row has to become the row the menu is about. Left click
        // additionally calls `onOpen`, and doing that here would answer the
        // question by navigating away from it.
        //
        // Selection rather than DOM focus, deliberately: the menu moves focus
        // into itself on open and hands it back on close, and forcing focus
        // onto the row would fight that and cost the menu its keyboard
        // navigation. `aria-current` and the active fill are what "this one"
        // needs to look like.
        if (open) onSelect(row.reff);
      }}
      menuContent={menu}
    >
    <li
      ref={el}
      className={clsxish([
        interactiveRow({ selected, size: "xl" }),
        // One step above the group header that introduces them. The header is
        // punctuation between piles; the rows are the thing you came to read,
        // and when the two were level the list had no figure and ground.
        "group/row flex h-ctl-xl items-center gap-2 px-4",
        checked && !selected && "bg-accent/5 shadow-[inset_2px_0_var(--color-accent)]",
        // The open-menu fill used to come from `data-[state=open]`, which Radix
        // wrote onto its trigger. Astryx's ContextMenu wraps the row instead of
        // becoming it, so nothing marks the `<li>` any more — and nothing needs
        // to: `onOpenChange` selects the row on open, so `selected` above is
        // already the fill, one render later. The attribute rule was doing the
        // same job a frame earlier and is now doing nothing at all.
        // A row whose body hasn't synced yet is real but not yet trustworthy;
        // say so quietly rather than rendering it as settled (UI.md §3.3).
        row.provisional && "opacity-60",
        row.tombstone && "opacity-60",
      ])}
      onClick={(event) => {
        // A click that began on a chip or the checkbox is that control's, not
        // the row's. Guarded here rather than stopped there — an intercepted
        // click makes Radix cancel another popover's outside-dismissal, so
        // nothing in a row is allowed to swallow one (see `fromRowControl`).
        if (fromRowControl(event)) return;
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
      <span data-row-control="" className="flex size-icon-md shrink-0 items-center justify-center">
        {!readOnly && (
          <CheckboxInput
            label={`Select ${row.key_alias ?? row.reff}`}
            isLabelHidden
            size="sm"
            value={checked}
            // One handler, not a click/change pair. This used to `preventDefault`
            // in `onClick` to stop Radix's button firing its own toggle after the
            // shift-range had already been applied. Astryx renders a real
            // `<input type="checkbox">`, whose change event is not cancellable
            // that way — it fired anyway and double-toggled the anchor row. The
            // shift test belongs in the one handler that sees the event.
            onChange={(_checked, event) =>
              onToggleCheck(row.reff, (event.nativeEvent as MouseEvent).shiftKey)
            }
            className={clsxish([
              !anyChecked && !checked &&
                "opacity-0 transition-opacity group-hover/row:opacity-100 focus-visible:opacity-100",
            ])}
          />
        )}
      </span>
      <PriorityChip
        priority={row.priority}
        disabled={locked}
        onPick={(p) => mutators.setPriority(row.reff, p)}
      />
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
      <StatusChip
        status={row.status}
        state={state}
        states={states}
        disabled={locked}
        onPick={(id) => mutators.setStatus(row.reff, id)}
      />
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
      <LabelsChip
        names={row.label_names ?? []}
        labels={labels}
        disabled={locked}
        onToggle={(name, add) => mutators.toggleLabel(row.reff, name, add)}
        onSwap={(from, to) => mutators.swapLabel(row.reff, from, to)}
        max={2}
        showOverflow={false}
        className="hidden shrink-0 flex-nowrap @min-[40rem]:flex"
      />
      <LabelsChip
        names={row.label_names ?? []}
        labels={labels}
        disabled={locked}
        onToggle={(name, add) => mutators.toggleLabel(row.reff, name, add)}
        onSwap={(from, to) => mutators.swapLabel(row.reff, from, to)}
        dots
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
      <AssigneeChip
        assignees={row.assignees}
        members={members}
        disabled={locked}
        onToggle={(key, add) => mutators.toggleAssignee(row.reff, key, add)}
        className="justify-end"
      />
      {/* Last, against the row's trailing edge. A date is the one field you
          scan down a column rather than read across a row, so it wants a
          straight right edge of its own — putting it before the faces gave it a
          ragged one that moved with however many people were assigned. The
          column exists per view (`anyDue`): every row reserves it when any row
          needs it, and no row pays 44px of empty edge when none does. */}
      {anyDue && (
        <DueChip
          due={row.due_date}
          disabled={locked}
          onChange={(next) => mutators.setDue(row.reff, next)}
          face={
            <span
              className={clsxish([
                // A fixed, right-aligned column: `Aug 14` and `Sep 3` are different
                // widths, and a date you read down a list has to start and end in
                // the same place on every row or the column stops being one.
                "flex w-11 shrink-0 items-center justify-end text-right text-2xs tabular-nums",
                row.due_date != null &&
                  { overdue: "text-danger", soon: "text-warn", later: "text-mute" }[
                    dueTone(row.due_date)
                  ],
              ])}
              title={row.due_date != null ? "Due date" : undefined}
            >
              {row.due_date != null ? (
                dueLabel(row.due_date)
              ) : !locked ? (
                // The reserved-but-empty cell is a target, said quietly: the
                // glyph surfaces with the row's other hover affordances.
                <CalendarPlus className="text-mute size-icon-xs opacity-0 transition-opacity group-hover/row:opacity-60" />
              ) : null}
            </span>
          }
        />
      )}

    </li>
    </ContextMenu>
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
