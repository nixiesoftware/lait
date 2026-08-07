import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { GroupHeader } from "./layout";
import { ArrowRight, CalendarClock, FilterX, Gauge, Info, MoreHorizontal, Plus } from "lucide-react";

import { loadBoardScroll, saveBoardScroll } from "../core/boardState";
import { groupRows, type DisplayState, type RowGroup } from "../core/display";
import type { BoardColumn, BoardPos, BoardView, LabelDto, MemberDto, Row } from "../types";
import { AvatarStack, memberName, stackFor } from "./Avatar";
import { EmptyState } from "./AppState";
import { catalogColor } from "./colors";
import {
  AssigneeChip,
  DueChip,
  EstimateChip,
  fromRowControl,
  LabelsChip,
  PriorityChip,
  StatusChip,
  SubIssuesChip,
  type IssueMutators,
} from "./fields";
import { PriorityIcon, ProgressRing, StatusIcon } from "./icons";
import { IssueMenuItems } from "./IssueMenu";
import { Button, ContextMenu, DropdownMenu, DropdownMenuItem, IconButton } from "@astryxdesign/core";
import { cn } from "./primitives";
import { dueLabel, dueTone } from "./time";

const DUE_TONE = { overdue: "text-danger", soon: "text-warn", later: "text-mute" } as const;

/** A card's property mini-chip — `ChipButton`'s measure, worn as a face: the
 *  same 24px pill as a label chip, so one row reads as one family. */
const cardChip = "border-line text-dim flex h-ctl-sm items-center gap-1 rounded-full border px-2 text-xs";

/**
 * The board — the same fetch as the list, laid out sideways.
 *
 * `BoardView.columns` are status buckets in board order, so this and `IssueList`
 * are two renderings of one `Request`. Switching views costs nothing and cannot
 * show you two different truths.
 *
 * Ordering is the daemon's: `Catalog.boards[P]` is a movable list and the
 * authority for position (A§9, S§5.5). This never sorts.
 *
 * ## Dragging
 *
 * Native HTML5 drag-and-drop, not a library. The board is four columns of one card
 * shape, the platform already owns the drag image, the cursor, and the escape key —
 * and this bundle is committed into the binary (`src/serve/assets`), so a 40KB drag
 * engine is 40KB every `lait` install carries forever. The keyboard path (`J`/`K`,
 * `H`/`L`) is separate and primary; this is the mouse affordance for the same verbs.
 */
export function Board({
  board,
  display,
  members,
  labels,
  selection,
  optimistic,
  onSelect,
  onCreate,
  onDrop,
  onReassign,
  mutators,
  onLoadChildren,
  readOnly,
  filtered,
  onClearFilter,
}: {
  board: BoardView;
  /** How the board is grouped. `status` = workflow columns (the default and the
   *  only axis with drag-ordering); `assignee`/`priority` = swimlane columns
   *  whose drop reassigns that field instead of moving status. */
  display: DisplayState;
  /** The ACL, for resolving assignee keys to faces. */
  members: MemberDto[];
  labels: LabelDto[];
  selection: string | null;
  /** Docs carrying an unconfirmed local prediction. */
  optimistic: ReadonlySet<string>;
  onSelect: (reff: string) => void;
  onCreate: (status: string) => void;
  /** A card landed. `pos` is null when the target column can't be ordered. */
  onDrop: (reff: string, status: string, pos: BoardPos | null) => void;
  /** A card was dragged into a non-status swimlane: reassign it to `groupKey`
   *  (a priority string, an assignee key, or `"unassigned"`). */
  onReassign: (row: Row, groupKey: string) => void;
  /** In-place field writes — every chip on a card resolves here. */
  mutators: IssueMutators;
  /** The sub-issue rows behind a card's tally, fetched when its menu opens. */
  onLoadChildren: (reff: string) => Promise<Row[]>;
  readOnly: boolean;
  /** A filter is narrowing this board (`mine`, status, label, …). */
  filtered: boolean;
  /** Reset that filter — offered on the empty state so a board emptied by a
   *  leftover filter (e.g. "My issues") is never a silent blank. */
  onClearFilter: () => void;
}) {
  // A board with rows in the space but none after filtering must say so, exactly
  // as the list does — an empty grid of columns reads as "no issues", when the
  // truth is "a filter is hiding them" (the classic leftover-`mine` trap).
  const anyRows = board.columns.some((col) => col.rows.some((row) => !row.tombstone));
  if (!anyRows && filtered) {
    return (
      <EmptyState
        kind="filtered-empty"
        title="No matching issues"
        body="Every issue in this project is hidden by the current filter."
        action={
          <Button
            onClick={onClearFilter}
            icon={<FilterX className="size-icon-sm" />}
            label="Clear filter"
            variant="primary"
            size="sm"
          />
        }
        className="min-h-60"
      />
    );
  }

  if (display.group === "assignee" || display.group === "priority") {
    return (
      <GroupedBoard
        board={board}
        display={display}
        members={members}
        labels={labels}
        selection={selection}
        optimistic={optimistic}
        onSelect={onSelect}
        onReassign={onReassign}
        mutators={mutators}
        onLoadChildren={onLoadChildren}
        readOnly={readOnly}
      />
    );
  }
  /** The card in flight, and the column it left. */
  const [drag, setDrag] = useState<{ reff: string; from: string } | null>(null);
  /** Where it would land. Rendered as the gap. */
  const [over, setOver] = useState<{ col: string; pos: BoardPos } | null>(null);
  const [announcement, setAnnouncement] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);

  useLayoutEffect(() => {
    if (scrollRef.current) scrollRef.current.scrollLeft = loadBoardScroll(board.project.id);
  }, [board.project.id]);

  const finish = (col: BoardColumn) => {
    if (!drag || !over) return reset();
    const isDone = col.state.category === "done";
    // A done column is not drawn from `boards[P]` — entering a done status removes
    // the doc from the movable list and the column is rendered by the append rule
    // instead (`replica.rs:858-869`). So there is no position to ask for, and
    // asking anyway would write to a list this column ignores.
    onDrop(drag.reff, col.state.id, isDone ? null : over.pos);
    setAnnouncement(`Moved ${drag.reff} to ${col.state.name}`);
    reset();
  };

  const reset = () => {
    setDrag(null);
    setOver(null);
  };

  return (
    // The canvas is the PAGE. It used to take `raised`, one rung above every
    // other view's background, and that was the tell: the board looked right and
    // the rest of the app looked hollow. The ladder moved instead — dark's `bg`
    // now sits where this canvas sat — so the board keeps exactly the surface it
    // had while every other view joins it, and this stops being a special case.
    // Spacing on the 4px rhythm: a 16px margin around the board, columns 12px
    // apart — the seam between wells stays narrower than the shore around them.
    <div
      ref={scrollRef}
      className="bg-bg flex min-h-0 flex-1 gap-3 overflow-x-auto p-4"
      aria-label="Issue board"
      tabIndex={0}
      onScroll={(event) => saveBoardScroll(board.project.id, event.currentTarget.scrollLeft)}
    >
      <p className="sr-only" aria-live="polite">{announcement}</p>
      {board.columns.map((col) => (
        <Column
          key={col.state.id}
          col={col}
          members={members}
          labels={labels}
          selection={selection}
          optimistic={optimistic}
          drag={drag}
          over={over?.col === col.state.id ? over.pos : null}
          onSelect={onSelect}
          onCreate={onCreate}
          onDragStart={(reff) => setDrag({ reff, from: col.state.id })}
          onDragEnd={reset}
          onOver={(pos) =>
            setOver((current) => {
              const next = { col: col.state.id, pos };
              return sameBoardTarget(current, next) ? current : next;
            })
          }
          onDrop={() => finish(col)}
          mutators={mutators}
          onLoadChildren={onLoadChildren}
          columns={board.columns}
          readOnly={readOnly}
        />
      ))}
    </div>
  );
}

/**
 * The board grouped by a field that is *not* status — assignee or priority.
 *
 * Columns come from `groupRows` (the same swimlane buckets the list uses), so the
 * two views agree. The drop verb is different from the status board's: there is no
 * `boards[P]` position for these axes, so a card dropped into a column reassigns
 * that field (`onReassign`) rather than moving its status and its order. The card's
 * own status chip still changes status — the two verbs stay distinct.
 */
function GroupedBoard({
  board,
  display,
  members,
  labels,
  selection,
  optimistic,
  onSelect,
  onReassign,
  mutators,
  onLoadChildren,
  readOnly,
}: {
  board: BoardView;
  display: DisplayState;
  members: MemberDto[];
  labels: LabelDto[];
  selection: string | null;
  optimistic: ReadonlySet<string>;
  onSelect: (reff: string) => void;
  onReassign: (row: Row, groupKey: string) => void;
  mutators: IssueMutators;
  onLoadChildren: (reff: string) => Promise<Row[]>;
  readOnly: boolean;
}) {
  const [drag, setDrag] = useState<{ reff: string; from: string } | null>(null);
  const [overCol, setOverCol] = useState<string | null>(null);
  const [announcement, setAnnouncement] = useState("");

  const axis = display.group === "priority" ? "priority" : "assignee";
  const groups = groupRows(board, display);
  const columns = board.columns;
  const rowByReff = new Map(board.columns.flatMap((c) => c.rows).map((r) => [r.reff, r]));

  const drop = (group: RowGroup) => {
    if (!drag) return;
    const row = rowByReff.get(drag.reff);
    if (row && group.key !== drag.from) {
      onReassign(row, group.key);
      setAnnouncement(`Moved ${row.key_alias ?? row.reff} to ${group.label}`);
    }
    setDrag(null);
    setOverCol(null);
  };

  return (
    <div className="bg-raised flex min-h-0 flex-1 gap-3 overflow-x-auto p-4" aria-label="Issue board" tabIndex={0}>
      <p className="sr-only" aria-live="polite">{announcement}</p>
      {groups.map((group) => (
        <GroupedColumn
          key={group.key}
          group={group}
          axis={axis}
          members={members}
          labels={labels}
          selection={selection}
          optimistic={optimistic}
          columns={columns}
          active={drag !== null && !readOnly}
          over={overCol === group.key}
          readOnly={readOnly}
          onSelect={onSelect}
          onDragStart={(reff) => setDrag({ reff, from: group.key })}
          onDragEnd={() => {
            setDrag(null);
            setOverCol(null);
          }}
          onOver={() => setOverCol(group.key)}
          onDrop={() => drop(group)}
          mutators={mutators}
          onLoadChildren={onLoadChildren}
        />
      ))}
    </div>
  );
}

function GroupedColumn({
  group,
  axis,
  members,
  labels,
  selection,
  optimistic,
  columns,
  active,
  over,
  readOnly,
  onSelect,
  onDragStart,
  onDragEnd,
  onOver,
  onDrop,
  mutators,
  onLoadChildren,
}: {
  group: RowGroup;
  axis: "assignee" | "priority";
  members: MemberDto[];
  labels: LabelDto[];
  selection: string | null;
  optimistic: ReadonlySet<string>;
  columns: BoardColumn[];
  active: boolean;
  over: boolean;
  readOnly: boolean;
  onSelect: (reff: string) => void;
  onDragStart: (reff: string) => void;
  onDragEnd: () => void;
  onOver: () => void;
  onDrop: () => void;
  mutators: IssueMutators;
  onLoadChildren: (reff: string) => Promise<Row[]>;
}) {
  const rows = group.rows.filter((r) => !r.tombstone);
  const unassigned = axis === "assignee" && group.key === "unassigned";
  return (
    // The same fixed-width sunken box as the status columns — a lane is a
    // column whatever axis it buckets by.
    <section className="bg-sunken flex w-80 shrink-0 flex-col rounded-surface pt-1">
      <GroupHeader
        className="px-3"
        icon={
          axis === "priority" ? (
            <PriorityIcon priority={rows[0]?.priority ?? "none"} />
          ) : unassigned ? (
            <span className="border-line text-mute flex size-avatar-sm items-center justify-center rounded-full border border-dashed text-[9px]">
              ?
            </span>
          ) : (
            <AvatarStack members={stackFor([group.key], members)} />
          )
        }
        title={
          <span className="capitalize">
            {axis === "assignee" && !unassigned
              ? memberName(group.key, members.find((m) => m.key === group.key))
              : group.label}
          </span>
        }
        count={rows.length}
      />
      <ul
        aria-label={`${group.label} issues`}
        data-board-collection
        className={[
          "flex min-h-0 flex-1 flex-col gap-2.5 overflow-y-auto rounded-surface p-3 pt-1 transition-colors",
          active && over ? "bg-hover" : "",
        ].join(" ")}
        onDragOver={(e) => {
          if (!active) return;
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          onOver();
        }}
        onDrop={(e) => {
          if (!active) return;
          e.preventDefault();
          onDrop();
        }}
      >
        {rows.map((row) => (
          <Card
            key={row.reff}
            row={row}
            members={members}
            labels={labels}
            selected={row.reff === selection}
            pending={optimistic.has(row.doc_id)}
            dragging={false}
            gap={null}
            draggable={!readOnly && !row.tombstone}
            onSelect={onSelect}
            onDragStart={onDragStart}
            onDragEnd={onDragEnd}
            onOver={() => onOver()}
            columns={columns}
            mutators={mutators}
            onLoadChildren={onLoadChildren}
            readOnly={readOnly}
          />
        ))}
        {rows.length === 0 && (
          <li
            className={[
              "text-mute rounded-surface border border-dashed p-4 text-center text-sm transition-colors",
              active && over ? "border-accent text-accent" : "border-line",
            ].join(" ")}
          >
            {active && over ? "Drop here" : "—"}
          </li>
        )}
      </ul>
    </section>
  );
}

function sameBoardTarget(
  left: { col: string; pos: BoardPos } | null,
  right: { col: string; pos: BoardPos },
): boolean {
  if (!left || left.col !== right.col || left.pos.at !== right.pos.at) return false;
  if (left.pos.at === "before" || left.pos.at === "after") {
    return right.pos.at === left.pos.at && right.pos.reff === left.pos.reff;
  }
  return true;
}

/** Done is append-only in the daemon; live columns accept an explicit tail. */
export function boardMovePosition(col: BoardColumn): BoardPos | null {
  return col.state.category === "done" ? null : { at: "bottom" };
}

function Column({
  col,
  members,
  labels,
  selection,
  optimistic,
  drag,
  over,
  onSelect,
  onCreate,
  onDragStart,
  onDragEnd,
  onOver,
  onDrop,
  mutators,
  onLoadChildren,
  columns,
  readOnly,
}: {
  col: BoardColumn;
  members: MemberDto[];
  labels: LabelDto[];
  selection: string | null;
  optimistic: ReadonlySet<string>;
  drag: { reff: string; from: string } | null;
  over: BoardPos | null;
  onSelect: (reff: string) => void;
  onCreate: (status: string) => void;
  onDragStart: (reff: string) => void;
  onDragEnd: () => void;
  onOver: (pos: BoardPos) => void;
  onDrop: () => void;
  mutators: IssueMutators;
  onLoadChildren: (reff: string) => Promise<Row[]>;
  columns: BoardColumn[];
  readOnly: boolean;
}) {
  const rows = col.rows.filter((r) => !r.tombstone);
  const active = drag !== null && !readOnly;

  return (
    // One fixed measure per column, and the column is a *box*: header and rows
    // share one sunken panel, a shade darker than the canvas behind it, so the
    // board reads as Linear's does — dark wells holding raised cards. Nothing
    // collapses any more: a board's whole point is every bucket visible at
    // once, and the 40px rail the chevron bought was a place issues went to
    // hide. Content-sized widths went with it — `Backlog` and an empty
    // `Canceled` are the same shelf, which is what lets the eye track a card
    // across columns without the geometry shifting under it.
    <section className="bg-sunken flex w-80 shrink-0 flex-col rounded-surface pt-1">
      {/* 12px inset for the header and the list both, so the state glyph and
          every card share one left edge — the alignment is what makes the box
          read as one object rather than a header floating over a list. */}
      <GroupHeader
        className="px-3"
        icon={<StatusIcon category={col.state.category} color={catalogColor(col.state.color)} />}
        title={col.state.name}
        count={rows.length}
        meta={col.state.category === "done" ? (
          <Info
            className="text-mute size-icon-sm"
            role="img"
            aria-label="Completed issues follow completion order. Move an issue here; its completion time determines its position."
          />
        ) : undefined}
        actions={
          <>
            {!readOnly && (
              <IconButton
                label={`New issue in ${col.state.name}`}
                onClick={() => onCreate(col.state.id)}
                variant="ghost"
                size="sm"
                tooltip={`New issue in ${col.state.name}`}
                icon={<Plus className="size-icon-sm" />}
              />
            )}
            {/* Compound mode — `items` omitted. The data form has no
                `endContent`, and the count belongs on the trailing edge where
                every other tally in the app sits. */}
            <DropdownMenu
              alignment="end"
              hasChevron={false}
              button={{
                label: `${col.state.name} column actions`,
                variant: "ghost",
                size: "sm",
                isIconOnly: true,
                icon: <MoreHorizontal className="size-icon-sm" />,
                tooltip: `${col.state.name} column actions`,
              }}
            >
              {!readOnly && (
                <DropdownMenuItem
                  label="New issue"
                  icon={<Plus className="size-icon-sm" />}
                  onClick={() => onCreate(col.state.id)}
                />
              )}
              <DropdownMenuItem
                label="Open first issue"
                icon={<ArrowRight className="size-icon-sm" />}
                isDisabled={!rows[0]}
                onClick={() => rows[0] && onSelect(rows[0].reff)}
                endContent={<span className="text-mute tabular-nums">{rows.length}</span>}
              />
            </DropdownMenu>
          </>
        }
      />
      <ul
        aria-label={`${col.state.name} issues`}
        data-board-collection
        className={[
          "flex min-h-0 flex-1 flex-col gap-2.5 overflow-y-auto rounded-surface p-3 pt-1 transition-colors",
          // The whole column lights up as a target, because the drop is a *status*
          // change first and a position second — the column is the thing you are
          // choosing.
          active && over !== null ? "bg-hover" : "",
        ].join(" ")}
        onDragOver={(e) => {
          if (!active) return;
          // Without this the browser refuses the drop and snaps the card back.
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
          // Past the last card — or over an empty column — means the end.
          if (rows.length === 0) onOver({ at: "top" });
        }}
        onDrop={(e) => {
          if (!active) return;
          e.preventDefault();
          onDrop();
        }}
      >
        {rows.map((row) => (
          <Card
            key={row.reff}
            row={row}
            members={members}
          labels={labels}
            selected={row.reff === selection}
            pending={optimistic.has(row.doc_id)}
            dragging={drag?.reff === row.reff}
            gap={gapFor(over, row.reff)}
            draggable={!readOnly && !row.tombstone}
            onSelect={onSelect}
            onDragStart={onDragStart}
            onDragEnd={onDragEnd}
            onOver={onOver}
            columns={columns}
            mutators={mutators}
            onLoadChildren={onLoadChildren}
            readOnly={readOnly}
          />
        ))}
        {rows.length === 0 && (
          <li
            className={[
              "text-mute rounded-surface border border-dashed p-4 text-center text-sm transition-colors",
              active && over !== null ? "border-accent text-accent" : "border-line",
            ].join(" ")}
          >
            {active && over !== null ? "Drop here" : "—"}
          </li>
        )}
        {/* The tail target. A card dropped below the last one has to land
            *somewhere*, and the list's own padding is not a drop zone the eye can
            find — this is, and it only exists while something is in flight. */}
        {active && rows.length > 0 && (
          <li
            className="min-h-ctl-lg flex-1"
            onDragOver={(e) => {
              e.preventDefault();
              onOver({ at: "bottom" });
            }}
          >
            {over?.at === "bottom" && <DropLine />}
          </li>
        )}
      </ul>
    </section>
  );
}

/** Whether the insertion line sits above or below this card, if at all. */
function gapFor(over: BoardPos | null, reff: string): "before" | "after" | null {
  if (!over) return null;
  if (over.at === "before" && over.reff === reff) return "before";
  if (over.at === "after" && over.reff === reff) return "after";
  if (over.at === "top") return null;
  return null;
}

/** The insertion point, drawn where the card will land. */
function DropLine() {
  return <div className="bg-accent my-0.5 h-0.5 rounded-full" aria-hidden="true" />;
}

function Card({
  row,
  members,
  labels,
  selected,
  pending,
  dragging,
  gap,
  draggable,
  onSelect,
  onDragStart,
  onDragEnd,
  onOver,
  columns,
  mutators,
  onLoadChildren,
  readOnly,
}: {
  row: Row;
  members: MemberDto[];
  labels: LabelDto[];
  selected: boolean;
  pending: boolean;
  dragging: boolean;
  gap: "before" | "after" | null;
  draggable: boolean;
  onSelect: (reff: string) => void;
  onDragStart: (reff: string) => void;
  onDragEnd: () => void;
  onOver: (pos: BoardPos) => void;
  /** The workflow, for the title-row status chip's options. */
  columns: BoardColumn[];
  mutators: IssueMutators;
  onLoadChildren: (reff: string) => Promise<Row[]>;
  readOnly: boolean;
}) {
  const el = useRef<HTMLLIElement>(null);
  const locked = readOnly || row.provisional || row.tombstone;
  // Selection moves by keyboard, so it has to drag the viewport with it.
  useEffect(() => {
    if (selected) {
      el.current?.scrollIntoView({ block: "nearest" });
      if (document.activeElement?.closest("[data-board-collection]")) {
        el.current?.focus({ preventScroll: true });
      }
    }
  }, [selected]);

  // The same verbs the list row carries — see `IssueMenu`. A card and a row are
  // two pictures of one issue, so the right click over either offers the same
  // things. The board has no multi-select, so `selection` is omitted rather than
  // passed empty: the row is absent, not disabled.
  const menu = (
    <IssueMenuItems
      reff={row.reff}
      status={row.status}
      priority={row.priority}
      assignees={row.assignees}
      labelNames={row.label_names ?? []}
      states={columns.map((c) => c.state)}
      members={members}
      labels={labels}
      mutators={mutators}
      locked={locked}
      onOpen={onSelect}
    />
  );

  return (
    <>
      {gap === "before" && <DropLine />}
      {/* `display: contents` on the wrapper — see `[data-board-collection] > div`
          in `styles.css`. The column is a flex container, so without it the
          cards stop being its flex items and the gap between them collapses. */}
      <ContextMenu
        // Right click asks "what can I do to this one", so the card becomes the
        // selected card first. Same reasoning as the list row.
        onOpenChange={(open) => {
          if (open) onSelect(row.reff);
        }}
        menuContent={menu}
      >
      <li
        ref={el}
        data-issue-ref={row.reff}
        draggable={draggable}
        onClick={(event) => {
          // A chip's or the menu's click is not a card click — guarded, never
          // stopped, so Radix's outside-dismissal sees every click (see
          // `fromRowControl`).
          if (fromRowControl(event)) return;
          event.currentTarget.focus({ preventScroll: true });
          onSelect(row.reff);
        }}
        onKeyDown={(event) => {
          if (event.target === event.currentTarget && event.key === "Enter") {
            event.preventDefault();
            onSelect(row.reff);
          }
        }}
        onDragStart={(e) => {
          // Firefox ignores a drag whose dataTransfer carries nothing.
          e.dataTransfer.setData("text/plain", row.reff);
          e.dataTransfer.effectAllowed = "move";
          onDragStart(row.reff);
        }}
        onDragEnd={onDragEnd}
        onDragOver={(e) => {
          e.preventDefault();
          // Which half of the card the pointer is in decides the side. Measuring
          // per-event rather than on drag start, because the card under the cursor
          // moves as the placeholder opens gaps above it.
          const box = e.currentTarget.getBoundingClientRect();
          const below = e.clientY > box.top + box.height / 2;
          onOver({ at: below ? "after" : "before", reff: row.reff });
        }}
        aria-current={selected ? "true" : undefined}
        tabIndex={selected ? 0 : -1}
        className={[
          "bg-raised group/card cursor-default rounded-surface border p-3 transition-[border-color,opacity] duration-150",
          selected
            ? "border-accent ring-accent ring-1"
            : "border-line hover:border-line-strong",
          row.provisional ? "opacity-60" : "",
          row.tombstone ? "opacity-60" : "",
          // The card left the deck: dim the hole it came from rather than removing
          // it, so the column doesn't reflow under the cursor mid-drag.
          dragging ? "opacity-40" : "",
        ].join(" ")}
      >
        {/* Linear's card, three rows top to bottom: who and which (key leading,
            faces trailing), what (status + title), then one pill row where
            every property is its own trigger. */}
        <div className="mb-1 flex items-center gap-2">
          {/* The same measure the list rows set the key in — one identity, one
              size, whichever surface it names. */}
          <span className="text-mute font-mono text-xs tabular-nums">
            {row.key_alias ?? row.reff}
          </span>
          <span className="ml-auto flex items-center gap-2">
            {pending && (
              <span
                className="bg-accent size-mark-xs animate-pulse rounded-full"
                title="Not confirmed by the daemon yet"
                aria-label="Pending"
              />
            )}
            {/* Faces, not `assignee_summary`. The summary is the *terminal's*
                projection — "you +1" is a sentence, and a card wants a glance. */}
            <AssigneeChip
              assignees={row.assignees}
              members={members}
              disabled={locked}
              onToggle={(key, add) => mutators.toggleAssignee(row.reff, key, add)}
            />
          </span>
        </div>
        {/* Status leads the title. The glyph is the picker, so the state a card
            is in and the way to change it are one mark. */}
        <div className="mb-1.5 flex items-start gap-1.5">
          <StatusChip
            status={row.status}
            state={columns.find((c) => c.state.id === row.status)?.state}
            states={columns.map((c) => c.state)}
            disabled={locked}
            onPick={(id) => mutators.setStatus(row.reff, id)}
            className="mt-0.5"
          />
          <p className={`min-w-0 flex-1 line-clamp-2 font-medium ${row.tombstone ? "text-mute line-through" : ""}`}>
            {row.title}
          </p>
        </div>
        {/* One pill row, one family — the bordered mini-chip (the `ChipButton`
            measure: 24px, pilled, quiet), each pill its own trigger. Priority
            leads as an icon pill, Linear's spelling; the due date keeps its
            urgency colour — that is data — and everything else stays dim. */}
        <div className="flex flex-wrap items-center gap-1.5">
          <PriorityChip
            priority={row.priority}
            disabled={locked}
            onPick={(p) => mutators.setPriority(row.reff, p)}
            face={
              <span className={cn(cardChip, "px-1.5")}>
                <PriorityIcon priority={row.priority} size="sm" />
              </span>
            }
          />
          {row.due_date != null && (
            <DueChip
              due={row.due_date}
              disabled={locked}
              onChange={(next) => mutators.setDue(row.reff, next)}
              face={
                <span
                  className={cn(
                    cardChip,
                    dueTone(row.due_date) !== "later" && DUE_TONE[dueTone(row.due_date)],
                  )}
                >
                  <CalendarClock className="size-icon-xs" />
                  {dueLabel(row.due_date)}
                </span>
              }
            />
          )}
          {row.estimate != null && (
            <EstimateChip
              estimate={row.estimate}
              disabled={locked}
              onPick={(id) => mutators.setEstimate(row.reff, id)}
              face={
                <span className={cardChip}>
                  <Gauge className="size-icon-xs" />
                  {row.estimate}
                </span>
              }
            />
          )}
          {(row.child_total ?? 0) > 0 && (
            <SubIssuesChip
              states={columns.map((c) => c.state)}
              loadChildren={() => onLoadChildren(row.reff)}
              onOpen={onSelect}
              face={
                <span
                  className={cn(cardChip, row.child_done === row.child_total && "text-ok")}
                  title={`${row.child_done} of ${row.child_total} sub-issues done`}
                >
                  <ProgressRing done={row.child_done ?? 0} total={row.child_total ?? 0} />
                  {row.child_done}/{row.child_total}
                </span>
              }
            />
          )}
          {/* Three fits a card's width; the rest fold into `+N`. */}
          <LabelsChip
            names={row.label_names ?? []}
            labels={labels}
            disabled={locked}
            onToggle={(name, add) => mutators.toggleLabel(row.reff, name, add)}
            onSwap={(from, to) => mutators.swapLabel(row.reff, from, to)}
            max={3}
            className="flex-wrap"
          />
        </div>
      </li>
      </ContextMenu>
      {gap === "after" && <DropLine />}
    </>
  );
}
