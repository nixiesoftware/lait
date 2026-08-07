import { Toolbar } from "./layout";
import { useMemo, useState } from "react";
import { ChevronLeft, ChevronRight } from "lucide-react";

import type { BoardView, LabelDto, MemberDto, Row, WorkflowState } from "../types";
import { tsToDate } from "../types";
import { fromRowControl, PriorityChip, StatusChip, type IssueMutators } from "./fields";
import { Button, ContextMenu, IconButton } from "@astryxdesign/core";
import { IssueMenuItems } from "./IssueMenu";

/**
 * The calendar view — the same filtered query as the list and board, placed on a
 * month grid by due date.
 *
 * No new data: `Row.due_date` already rides on every row (the list rows and board
 * cards render it too), so this is purely another arrangement of `board`'s rows —
 * exactly like `groupRows`, but two-dimensional. Dates are read in **UTC** because
 * the engine stores a due date as UTC midnight of the day the user named; a
 * local-time grid would file a deadline under the wrong square west of Greenwich.
 */
export function Calendar({
  board,
  members,
  labels,
  onSelect,
  mutators,
  readOnly,
}: {
  board: BoardView;
  /** The ACL and the label catalog — the entry's right-click menu offers both
   *  as fly-outs, exactly as the list row and the board card do. */
  members: MemberDto[];
  labels: LabelDto[];
  onSelect: (reff: string) => void;
  /** In-place field writes — the entry's priority/status chips resolve here. */
  mutators: IssueMutators;
  readOnly: boolean;
}) {
  const states: WorkflowState[] = useMemo(
    () => board.columns.map((c) => c.state),
    [board],
  );
  const rows = useMemo(
    () => board.columns.flatMap((c) => c.rows).filter((r) => !r.tombstone),
    [board],
  );

  // Bucket dated rows by their UTC day key; keep the undated ones for the footer.
  const { byDay, undated } = useMemo(() => {
    const byDay = new Map<string, Row[]>();
    const undated: Row[] = [];
    for (const r of rows) {
      if (r.due_date == null) {
        undated.push(r);
        continue;
      }
      const key = dayKey(tsToDate(r.due_date));
      byDay.set(key, [...(byDay.get(key) ?? []), r]);
    }
    return { byDay, undated };
  }, [rows]);

  // The month on screen, as a UTC anchor at day 1. Starts on the current month.
  const [anchor, setAnchor] = useState(() => {
    const now = new Date();
    return Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1);
  });
  const anchorDate = new Date(anchor);
  const year = anchorDate.getUTCFullYear();
  const month = anchorDate.getUTCMonth();

  const step = (delta: number) => setAnchor(Date.UTC(year, month + delta, 1));
  const toThisMonth = () => {
    const now = new Date();
    setAnchor(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1));
  };

  const weeks = useMemo(() => monthGrid(year, month), [year, month]);
  const todayKey = dayKey(new Date());
  const monthLabel = anchorDate.toLocaleDateString(undefined, {
    timeZone: "UTC",
    month: "long",
    year: "numeric",
  });

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      {/* A control band, not a header: the surface is already named "Issues" one
          row up, and this only says which month of it you are looking at. */}
      <Toolbar className="gap-2">
        <h2 className="text-sm font-semibold tabular-nums">{monthLabel}</h2>
        <div className="ml-2 flex items-center gap-0.5">
          <IconButton
            label="Previous month"
            onClick={() => step(-1)}
            variant="ghost"
            size="sm"
            tooltip="Previous month"
            icon={<ChevronLeft className="size-icon-md" />}
          />
          <IconButton
            label="Next month"
            onClick={() => step(1)}
            variant="ghost"
            size="sm"
            tooltip="Next month"
            icon={<ChevronRight className="size-icon-md" />}
          />
        </div>
        <Button
          onClick={toThisMonth}
          className="ml-1"
          label="Today"
          variant="secondary"
          elevation="low"
          size="sm"
        />
        <span className="text-mute ml-auto text-xs">
          {rows.length - undated.length} scheduled · {undated.length} undated
        </span>
      </Toolbar>

      <div className="grid shrink-0 grid-cols-7 border-b border-line">
        {WEEKDAYS.map((d) => (
          <div key={d} className="text-mute px-2 py-1 text-2xs font-semibold tracking-wider uppercase">
            {d}
          </div>
        ))}
      </div>

      <div className="grid min-h-0 flex-1 grid-cols-7 grid-rows-6">
        {weeks.flat().map((day) => {
          const key = dayKey(day);
          const inMonth = day.getUTCMonth() === month;
          const dayRows = byDay.get(key) ?? [];
          return (
            <div
              key={key}
              className={`border-line flex min-h-0 flex-col gap-0.5 overflow-hidden border-r border-b p-1 ${
                inMonth ? "" : "bg-bg/40"
              }`}
            >
              <span
                className={`text-2xs tabular-nums ${
                  key === todayKey
                    ? "bg-accent text-accent-fg flex size-4 items-center justify-center rounded-full"
                    : inMonth
                      ? "text-dim"
                      : "text-mute"
                }`}
              >
                {day.getUTCDate()}
              </span>
              <div data-calendar-entries className="flex min-h-0 flex-col gap-0.5 overflow-y-auto">
                {dayRows.map((r) => (
                  // The same verbs the list row and the board card carry — see
                  // `IssueMenu`. A calendar has no multi-select, so `selection`
                  // is omitted. `display: contents` on the wrapper keeps the
                  // entries as flex items of the day's stack; see
                  // `[data-calendar-entries] > div` in `styles.css`.
                  <ContextMenu
                    key={r.reff}
                    onOpenChange={(open) => {
                      if (open) onSelect(r.reff);
                    }}
                    menuContent={
                      <IssueMenuItems
                        reff={r.reff}
                        status={r.status}
                        priority={r.priority}
                        assignees={r.assignees}
                        labelNames={r.label_names ?? []}
                        states={states}
                        members={members}
                        labels={labels}
                        mutators={mutators}
                        locked={readOnly || r.provisional || r.tombstone}
                        onOpen={onSelect}
                      />
                    }
                  >
                  {/* A div wearing button semantics, not a `<button>`: the
                      priority and status chips inside are buttons of their own,
                      and buttons do not nest. */}
                  <div
                    role="button"
                    tabIndex={0}
                    data-issue-ref={r.reff}
                    onClick={(event) => {
                      // A chip's click is the chip's — guarded, never stopped
                      // (see `fromRowControl`).
                      if (fromRowControl(event)) return;
                      onSelect(r.reff);
                    }}
                    onKeyDown={(event) => {
                      if (event.target === event.currentTarget && event.key === "Enter") {
                        event.preventDefault();
                        onSelect(r.reff);
                      }
                    }}
                    title={r.title}
                    className="bg-raised border-line hover:border-line-strong flex items-center gap-1 rounded-control border px-1 py-0.5 text-left text-2xs"
                  >
                    <PriorityChip
                      priority={r.priority}
                      disabled={readOnly || r.provisional}
                      onPick={(p) => mutators.setPriority(r.reff, p)}
                    />
                    <StatusChip
                      status={r.status}
                      state={states.find((s) => s.id === r.status)}
                      states={states}
                      disabled={readOnly || r.provisional}
                      onPick={(id) => mutators.setStatus(r.reff, id)}
                    />
                    <span className="min-w-0 flex-1 truncate font-medium">{r.title}</span>
                  </div>
                  </ContextMenu>
                ))}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** The `YYYY-MM-DD` UTC key a due date files under. */
function dayKey(d: Date): string {
  return d.toISOString().slice(0, 10);
}

/** Six weeks of UTC days covering `month`, Monday-first, spilling into the
 *  neighbouring months so the grid is always a full rectangle. */
function monthGrid(year: number, month: number): Date[][] {
  const first = new Date(Date.UTC(year, month, 1));
  // JS: 0=Sun … 6=Sat. Shift so Monday=0.
  const lead = (first.getUTCDay() + 6) % 7;
  const start = Date.UTC(year, month, 1 - lead);
  const weeks: Date[][] = [];
  for (let w = 0; w < 6; w++) {
    const week: Date[] = [];
    for (let d = 0; d < 7; d++) {
      week.push(new Date(start + (w * 7 + d) * 86_400_000));
    }
    weeks.push(week);
  }
  return weeks;
}
