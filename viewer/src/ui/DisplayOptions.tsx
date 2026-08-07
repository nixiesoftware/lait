import { Calendar, List, SlidersHorizontal, SquareKanban } from "lucide-react";

import type { DisplayState, GroupBy, OrderBy } from "../core/display";
import { ISSUE_MODES, ISSUE_MODE_LABEL, type IssueMode } from "../core/registry";
import { Button, IconButton, Popover } from "@astryxdesign/core";
import { cn, toolbarIconControl } from "./primitives";

/** The layout switcher's glyphs. Same icons the sidebar gives the destination,
 *  so a board is drawn as a board wherever it is named. */
const MODE_ICON = { list: List, board: SquareKanban, calendar: Calendar } as const;

/**
 * The display-options popover — Linear's `Shift+V` surface, reduced to the axes
 * this client actually has: grouping, ordering, and whether deleted issues show.
 *
 * Controlled from the App so the keybinding can open it: an uncontrolled
 * popover would be the one overlay the registry couldn't reach.
 *
 * Grouping applies to the list (the board's columns *are* the status grouping);
 * ordering applies to both. Deleted issues are a dedicated list recovery mode;
 * choosing it from the board moves to that destination.
 */
export function DisplayOptions({
  display,
  view,
  open,
  onOpenChange,
  onChange,
  onModeChange,
  density,
  onDensityChange,
}: {
  display: DisplayState;
  /** Which layout is showing — grouping is disabled on the board. */
  view: IssueMode;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onChange: (d: DisplayState) => void;
  /** Switch layout. It is a route, so this navigates — but it belongs here,
   *  with the other choices about how the same rows are drawn. */
  onModeChange: (mode: IssueMode) => void;
  density: "compact" | "comfortable";
  onDensityChange: (density: "compact" | "comfortable") => void;
}) {
  const changed =
    display.group !== "status" || display.order !== "board" || display.deleted;

  return (
    <Popover
      isOpen={open}
      onOpenChange={onOpenChange}
      alignment="end"
      content={
        <div className="flex w-64 flex-col gap-3 p-3">
          {/* Layout leads, because everything under it is read in its terms:
              grouping means columns on a board and headers in a list, and on a
              calendar it means nothing at all. */}
          <div
            className="bg-active/40 flex gap-0.5 rounded-surface p-0.5"
            role="group"
            aria-label="Layout"
          >
            {ISSUE_MODES.map((mode) => {
              const Glyph = MODE_ICON[mode];
              const active = view === mode;
              return (
                <button
                  key={mode}
                  aria-pressed={active}
                  onClick={() => onModeChange(mode)}
                  className={cn(
                    "flex h-ctl-md flex-1 items-center justify-center gap-1.5 rounded-control text-xs transition-colors",
                    active ? "bg-raised text-fg" : "text-dim hover:text-fg",
                  )}
                >
                  <Glyph className="size-icon-sm" aria-hidden />
                  {ISSUE_MODE_LABEL[mode]}
                </button>
              );
            })}
          </div>

          <Axis label="Group by">
            {(
              [
                ["status", "Status"],
                ["assignee", "Assignee"],
                ["priority", "Priority"],
                ["none", "None"],
              ] as const
            )
              // "None" is a list-only shape — a single-column board is just the
              // list; the board's other axes (status/assignee/priority) become
              // its columns.
              .filter(([id]) => !(view === "board" && id === "none"))
              .map(([id, label]) => (
                <Choice
                  key={id}
                  label={label}
                  active={display.group === id}
                  onClick={() => onChange({ ...display, group: id as GroupBy })}
                />
              ))}
          </Axis>

          <Axis label="Order by">
            {(
              [
                ["board", "Board order"],
                ["priority", "Priority"],
                ["title", "Title"],
              ] as const
            ).map(([id, label]) => (
              <Choice
                key={id}
                label={label}
                active={display.order === id}
                onClick={() => onChange({ ...display, order: id as OrderBy })}
              />
            ))}
          </Axis>

          <Axis label="Issue mode">
            <Choice
              label="Active"
              active={!display.deleted}
              onClick={() => onChange({ ...display, deleted: false })}
            />
            <Choice
              label="Deleted"
              active={display.deleted}
              onClick={() => onChange({ ...display, deleted: true })}
            />
          </Axis>

          <Axis label="Density">
            <Choice label="Compact" active={density === "compact"} onClick={() => onDensityChange("compact")} />
            <Choice label="Comfortable" active={density === "comfortable"} onClick={() => onDensityChange("comfortable")} />
          </Axis>
        </div>
      }
    >
      <IconButton
        label="Display options"
        variant={changed ? "active" : "secondary"}
        elevation={changed ? "none" : "low"}
        size="sm"
        className={toolbarIconControl}
        tooltip="Display options  ⇧V"
        icon={<SlidersHorizontal className="size-icon-sm" />}
      />
    </Popover>
  );
}

function Axis({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-1">
      <span className="text-mute text-2xs font-semibold tracking-wider uppercase">{label}</span>
      <div className="flex flex-wrap gap-1">{children}</div>
    </div>
  );
}

function Choice({
  label,
  active,
  disabled,
  onClick,
}: {
  label: string;
  active: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  return (
    <Button
      aria-pressed={active}
      isDisabled={disabled ?? false}
      onClick={onClick}
      label={`${label}`}
      variant={active ? "active" : "secondary"}
      elevation={active ? "none" : "low"}
      size="sm"
    />
  );
}
