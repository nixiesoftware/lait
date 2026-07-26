import { useEffect, useRef, useState } from "react";
import * as Popover from "@radix-ui/react-popover";
import { ArrowLeft, Check, ChevronRight, ListFilter, Tag, UserRound } from "lucide-react";

import { EMPTY_FILTER, isActive, type FilterState } from "../core/filter";
import { PRIORITY_ORDER, type LabelDto, type MemberDto, type WorkflowState } from "../types";
import { Avatar, memberName } from "./Avatar";
import { catalogColor } from "./colors";
import { PriorityIcon, StatusIcon } from "./icons";
import { cn, IconButton, PopoverContent } from "./primitives";

/** Toggle one id in a multi-select filter axis. */
const toggle = (list: readonly string[], id: string): string[] =>
  list.includes(id) ? list.filter((x) => x !== id) : [...list, id];

type Facet = "status" | "priority" | "assignees" | "label";

/**
 * The filter menu.
 *
 * **One control per dimension**, not one menu holding several. `mine`, `status`, and
 * `label` answer different questions, and a single "Any ▾" menu that mixed them
 * could only ever show one of them in its trigger — so a board narrowed by both a
 * label and `mine` looked, from the outside, like it was narrowed by one. The
 * dimensions are still separate here; they are just stacked in one panel instead
 * of laid out across a band of the window that was there whether or not you were
 * filtering.
 *
 * It **drills down in place** rather than flying submenus out sideways. A flyout
 * has to find room next to a panel that is already at the right edge of the
 * window, and it puts the values you are choosing and the list you are narrowing
 * on top of each other. Replacing the panel keeps one column, so the back arrow
 * is the only thing you have to learn.
 *
 * The kinds are still marked by where they live: text is the box you type into and
 * is client-side; everything else is a control. Which of those cost a round trip and
 * which do not is `core/filter.ts`'s call, not this file's — see the note there on
 * why `status` is the one that looks server-shaped and isn't.
 *
 * Escape restores what the filter was on open rather than just clearing it, which
 * is the TUI's rule: a filter you can't back out of is one you stop using.
 */
export function FilterMenu({
  filter,
  labels,
  states,
  members,
  open,
  onOpenChange,
  focusToken,
  resultCount,
  totalCount,
  onChange,
}: {
  filter: FilterState;
  labels: LabelDto[];
  states: WorkflowState[];
  members: MemberDto[];
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Bumped by the `/` command; opens and focuses without owning the binding. */
  focusToken: number;
  resultCount: number;
  totalCount: number;
  onChange: (f: FilterState) => void;
}) {
  const input = useRef<HTMLInputElement>(null);
  const restore = useRef(filter);
  const [facet, setFacet] = useState<Facet | null>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (!open) return;
    restore.current = filter;
    setFacet(null);
    // The panel animates in; focusing on the same tick lands on a node that is
    // still being positioned, and the caret jumps when it settles.
    const id = window.setTimeout(() => input.current?.select(), 0);
    return () => window.clearTimeout(id);
    // Only on open and on `/` — not on every keystroke, or it would re-select
    // as you type.
  }, [open, focusToken]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => setQuery(""), [facet]);

  const active = isActive(filter);
  const label = labels.find((l) => l.name === filter.label);

  const counts: Record<Facet, number> = {
    status: filter.status.length,
    priority: filter.priority.length,
    assignees: filter.assignees.length,
    label: filter.label ? 1 : 0,
  };

  const matches = (text: string) => text.toLowerCase().includes(query.trim().toLowerCase());

  return (
    <Popover.Root open={open} onOpenChange={onOpenChange}>
      <Popover.Trigger asChild>
        <IconButton label="Filter" chord="/" variant={active ? "active" : "ghost"}>
          <ListFilter className="size-icon-md" />
        </IconButton>
      </Popover.Trigger>
      <PopoverContent
        align="end"
        className="w-72 p-0"
        onKeyDown={(event) => {
          if (event.key !== "Escape") return;
          // Inside a facet, Escape is "back", not "give up on the whole filter".
          if (facet) {
            event.preventDefault();
            event.stopPropagation();
            setFacet(null);
            return;
          }
          onChange(restore.current);
        }}
      >
        {facet === null ? (
          <div className="flex flex-col">
            {/* Text leads because it is the one filter with no menu to open —
                you are already typing by the time the panel has settled. */}
            <div className="border-line flex items-center gap-2 border-b px-3 py-2">
              <ListFilter className="text-mute size-icon-sm shrink-0" aria-hidden />
              <input
                ref={input}
                value={filter.text}
                placeholder="Filter issues…"
                onChange={(e) => onChange({ ...filter, text: e.target.value })}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.stopPropagation();
                    e.currentTarget.blur();
                  }
                }}
                className="placeholder:text-mute min-w-0 flex-1 bg-transparent text-sm outline-none"
                aria-label="Filter issues"
              />
            </div>
            <p className="text-mute px-3 pt-2 pb-1 text-2xs">
              spaces for AND, <span className="font-mono">|</span> for OR,{" "}
              <span className="font-mono">-</span> to exclude
            </p>

            <div className="flex flex-col p-1">
              <Row
                icon={<UserRound className="size-icon-sm" />}
                label="Mine"
                onClick={() => onChange({ ...filter, mine: !filter.mine })}
                trailing={
                  <span
                    role="switch"
                    aria-checked={filter.mine}
                    className={cn(
                      "flex h-4 w-7 shrink-0 items-center rounded-full p-0.5 transition-colors",
                      filter.mine ? "bg-accent" : "bg-active",
                    )}
                  >
                    <span
                      className={cn(
                        "bg-bg size-3 rounded-full transition-transform",
                        filter.mine && "translate-x-3",
                      )}
                    />
                  </span>
                }
              />
              <Facet
                name="Status"
                icon={<StatusIcon category="backlog" color="var(--color-mute)" />}
                count={counts.status}
                onOpen={() => setFacet("status")}
              />
              <Facet
                name="Priority"
                icon={<PriorityIcon priority="none" />}
                count={counts.priority}
                onOpen={() => setFacet("priority")}
              />
              {members.length > 0 && (
                <Facet
                  name="Assignee"
                  icon={<UserRound className="size-icon-sm" />}
                  count={counts.assignees}
                  onOpen={() => setFacet("assignees")}
                />
              )}
              {labels.length > 0 && (
                <Facet
                  name="Label"
                  icon={<Tag className="size-icon-sm" />}
                  count={counts.label}
                  value={filter.label ?? undefined}
                  swatch={label ? catalogColor(label.color) : undefined}
                  onOpen={() => setFacet("label")}
                />
              )}
            </div>

            {active && (
              <div className="border-line flex items-center gap-2 border-t px-3 py-2">
                <span className="text-mute text-2xs tabular-nums" aria-live="polite">
                  {resultCount} of {totalCount} · AND across facets
                </span>
                <button
                  className="text-dim hover:text-fg ml-auto text-xs"
                  onClick={() => onChange(EMPTY_FILTER)}
                >
                  Clear all
                </button>
              </div>
            )}
          </div>
        ) : (
          <div className="flex flex-col">
            <div className="border-line flex items-center gap-1 border-b px-2 py-1.5">
              <IconButton label="Back to filters" onClick={() => setFacet(null)}>
                <ArrowLeft className="size-icon-sm" />
              </IconButton>
              <span className="text-fg text-sm font-medium">{FACET_NAME[facet]}</span>
              {counts[facet] > 0 && (
                <button
                  className="text-dim hover:text-fg ml-auto text-xs"
                  onClick={() => onChange(clearFacet(filter, facet))}
                >
                  Clear
                </button>
              )}
            </div>
            <div className="border-line border-b px-3 py-2">
              <input
                autoFocus
                value={query}
                placeholder={`${FACET_NAME[facet]}…`}
                onChange={(e) => setQuery(e.target.value)}
                className="placeholder:text-mute w-full bg-transparent text-sm outline-none"
                aria-label={`Search ${FACET_NAME[facet]}`}
              />
            </div>
            <div className="max-h-64 overflow-y-auto p-1">
              {facet === "status" &&
                states
                  .filter((s) => matches(s.name))
                  .map((s) => (
                    <Value
                      key={s.id}
                      icon={<StatusIcon category={s.category} color={catalogColor(s.color)} />}
                      label={s.name}
                      selected={filter.status.includes(s.id)}
                      onClick={() => onChange({ ...filter, status: toggle(filter.status, s.id) })}
                    />
                  ))}
              {facet === "priority" &&
                [...PRIORITY_ORDER]
                  .reverse()
                  .filter((p) => matches(p))
                  .map((p) => (
                    <Value
                      key={p}
                      icon={<PriorityIcon priority={p} />}
                      label={p}
                      className="capitalize"
                      selected={filter.priority.includes(p)}
                      onClick={() => onChange({ ...filter, priority: toggle(filter.priority, p) })}
                    />
                  ))}
              {facet === "assignees" &&
                members
                  .filter((m) => matches(memberName(m.key, m)) || matches(m.key))
                  .map((m) => (
                    <Value
                      key={m.key}
                      icon={<Avatar deviceKey={m.key} alias={m.alias} me={m.me} size="sm" />}
                      label={memberName(m.key, m)}
                      selected={filter.assignees.includes(m.key)}
                      onClick={() =>
                        onChange({ ...filter, assignees: toggle(filter.assignees, m.key) })
                      }
                    />
                  ))}
              {/* Single-valued because `Filter.label` is: the daemon resolves one
                  name to one `LabelId`. Offering multi-select here would be
                  promising an intersection the `Request` cannot carry. */}
              {facet === "label" &&
                labels
                  .filter((l) => matches(l.name))
                  .map((l) => (
                    <Value
                      key={l.id}
                      swatch={catalogColor(l.color)}
                      label={l.name}
                      selected={filter.label === l.name}
                      onClick={() =>
                        onChange({ ...filter, label: filter.label === l.name ? null : l.name })
                      }
                    />
                  ))}
            </div>
          </div>
        )}
      </PopoverContent>
    </Popover.Root>
  );
}

const FACET_NAME: Record<Facet, string> = {
  status: "Status",
  priority: "Priority",
  assignees: "Assignee",
  label: "Label",
};

function clearFacet(filter: FilterState, facet: Facet): FilterState {
  if (facet === "label") return { ...filter, label: null };
  return { ...filter, [facet]: [] };
}

function Row({
  icon,
  label,
  trailing,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  trailing?: React.ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="text-dim hover:bg-hover hover:text-fg flex h-8 w-full items-center gap-2 rounded-md px-2 text-left text-sm outline-none focus-visible:ring-accent/50 focus-visible:ring-1"
    >
      <span className="text-mute flex size-icon-md shrink-0 items-center justify-center">{icon}</span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {trailing}
    </button>
  );
}

/** A dimension you can open. The count is the whole point of the row when the
 *  panel is shut — it is what tells you the filter is on without opening it. */
function Facet({
  name,
  icon,
  count,
  value,
  swatch,
  onOpen,
}: {
  name: string;
  icon: React.ReactNode;
  count: number;
  value?: string | undefined;
  swatch?: string | undefined;
  onOpen: () => void;
}) {
  return (
    <Row
      icon={icon}
      label={name}
      onClick={onOpen}
      trailing={
        <>
          {count > 0 && (
            <span className="text-accent flex min-w-0 items-center gap-1 text-xs">
              {swatch && (
                <span className="size-1.5 shrink-0 rounded-full" style={{ background: swatch }} />
              )}
              <span className="truncate">{value ?? count}</span>
            </span>
          )}
          <ChevronRight className="text-mute size-icon-xs shrink-0" aria-hidden />
        </>
      }
    />
  );
}

function Value({
  icon,
  swatch,
  label,
  selected,
  className,
  onClick,
}: {
  icon?: React.ReactNode;
  swatch?: string | undefined;
  label: string;
  selected: boolean;
  className?: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      aria-pressed={selected}
      className={cn(
        "hover:bg-hover flex h-7 w-full items-center gap-2 rounded-md px-2 text-left text-sm outline-none focus-visible:ring-accent/50 focus-visible:ring-1",
        selected ? "text-fg" : "text-dim",
        className,
      )}
    >
      {swatch ? (
        <span className="size-2 shrink-0 rounded-full" style={{ background: swatch }} />
      ) : (
        <span className="flex size-icon-md shrink-0 items-center justify-center">{icon}</span>
      )}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {selected && <Check className="text-accent size-icon-sm shrink-0" aria-hidden />}
    </button>
  );
}
