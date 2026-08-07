import { useState } from "react";
import { UserRound } from "lucide-react";

import type { LabelDto, MemberDto, Priority, Row, WorkflowState } from "../types";
import { PRIORITY_LABEL, PRIORITY_ORDER } from "../types";
import { Avatar, AvatarStack, memberName, stackFor } from "./Avatar";
import { catalogColor } from "./colors";
import { DatePicker } from "./DatePicker";
import { PriorityIcon, StatusIcon } from "./icons";
import { Combobox } from "./Picker";
import { cn, LabelChip, LabelDots } from "./primitives";
import { dueToInput } from "./time";

/**
 * Issue metadata as *triggers* — Linear's aggregate-view editing, where every
 * status glyph, priority mark, avatar stack, label run and due date on a list
 * row, board card or calendar entry opens its picker in place, without a trip
 * through the full issue page.
 *
 * These are the detail rail's `Combobox`/`DatePicker` fields wearing the row's
 * own faces: one chip per field, drawn exactly as the read-only view drew it
 * (`tone="bare" size="none"` — the face carries its own shape), so a view that
 * adopts them changes what a click *does* and nothing about how the row reads.
 *
 * Writes go through {@link IssueMutators} — the one set of callbacks the app
 * builds over `ProjectViewerStore`'s field methods — so a chip cannot invent
 * its own wire spelling, and every edit is optimistic the same way.
 */

/** The field writes a view receives — one object, not five props per view. */
export interface IssueMutators {
  setStatus(reff: string, status: string): void;
  setPriority(reff: string, priority: string): void;
  toggleAssignee(reff: string, key: string, add: boolean): void;
  toggleLabel(reff: string, name: string, add: boolean): void;
  /** Swap one label for another, or for nothing — a pill's own picker. */
  swapLabel(reff: string, from: string, to: string | null): void;
  /** `due` is `YYYY-MM-DD` UTC, or null to clear. */
  setDue(reff: string, due: string | null): void;
  /** `estimate` is a numeric string, or `"none"` to clear. */
  setEstimate(reff: string, estimate: string): void;
}

/**
 * Whether an event started inside a row control — a chip, a checkbox, a card's
 * menu button. The row surfaces guard their own click handlers with this
 * instead of the control calling `stopPropagation`, and the difference is not
 * cosmetic: Radix's dismissable layer defers outside-dismissal from the
 * pointerdown to the following `click` and *cancels it* when that click never
 * reaches the document — it reads an intercepted click as "someone else took
 * responsibility". A chip that swallowed its click therefore left every other
 * open picker standing. Nothing in a row may stop click propagation; controls
 * declare themselves, and the row declines to act.
 */
export function fromRowControl(event: {
  target: EventTarget | null;
  currentTarget: EventTarget | null;
}): boolean {
  const target = event.target;
  if (!(target instanceof Element)) return false;
  // A pick inside an open menu arrives here too: portals re-bubble React
  // events through the *component* tree, so a click on a menu item — mounted
  // under `document.body` — still reaches the row's onClick. Its DOM target is
  // not inside the row, which is exactly the test: a row acts only on clicks
  // that physically happened within it.
  const row = event.currentTarget;
  if (row instanceof Element && !row.contains(target)) return true;
  return target.closest("[data-row-control]") !== null;
}

/**
 * The containment shell every chip stands in. Board cards are `draggable`, so
 * the chip declares itself its *own* drag source — whose start is refused — to
 * keep a press-and-slip on a card's chip from lifting the whole card
 * (dragstart fires on the nearest draggable ancestor, so an inert child cannot
 * intercept it; a cancelled draggable child can). Clicks are NOT stopped here
 * — see {@link fromRowControl}.
 */
function ChipShell({
  className,
  children,
}: {
  className?: string | undefined;
  children: React.ReactNode;
}) {
  return (
    <span
      data-row-control=""
      className={cn("inline-flex min-w-0 shrink-0 items-center", className)}
      draggable
      onDragStart={(event) => {
        event.preventDefault();
        event.stopPropagation();
      }}
    >
      {children}
    </span>
  );
}

export function StatusChip({
  status,
  state,
  states,
  disabled,
  onPick,
  className,
}: {
  status: string;
  /** The resolved state, for the face. Unresolvable status → no chip at all,
   *  same as the read-only row: a glyph for a state we cannot name is noise. */
  state: WorkflowState | undefined;
  states: WorkflowState[];
  disabled?: boolean;
  onPick: (id: string) => void;
  className?: string;
}) {
  if (!state) return null;
  return (
    <ChipShell className={className}>
      <Combobox
        tone="bare"
        size="none"
        label="Change status"
        disabled={disabled ?? false}
        value={{ id: status, label: state.name }}
        face={<StatusIcon category={state.category} color={catalogColor(state.color)} />}
        options={states.map((s) => ({
          id: s.id,
          label: s.name,
          icon: <StatusIcon category={s.category} color={catalogColor(s.color)} />,
        }))}
        onPick={(id) => {
          if (id !== status) onPick(id);
        }}
      />
    </ChipShell>
  );
}

export function PriorityChip({
  priority,
  disabled,
  onPick,
  face,
  className,
}: {
  priority: Priority;
  disabled?: boolean;
  onPick: (id: string) => void;
  /** The surface's own rendering — a card wraps the glyph in its pill. */
  face?: React.ReactNode;
  className?: string;
}) {
  return (
    <ChipShell className={className}>
      <Combobox
        tone="bare"
        size="none"
        label="Change priority"
        disabled={disabled ?? false}
        className="capitalize"
        value={{ id: priority, label: priority }}
        face={face ?? <PriorityIcon priority={priority} />}
        // Highest first: the list you scan top-down starts where the urgency does.
        options={[...PRIORITY_ORDER].reverse().map((p) => ({
          id: p,
          label: PRIORITY_LABEL[p],
          icon: <PriorityIcon priority={p} />,
        }))}
        onPick={(id) => {
          if (id !== priority) onPick(id);
        }}
      />
    </ChipShell>
  );
}

export function AssigneeChip({
  assignees,
  members,
  disabled,
  onToggle,
  className,
}: {
  assignees: readonly string[];
  members: MemberDto[];
  disabled?: boolean;
  onToggle: (key: string, add: boolean) => void;
  className?: string;
}) {
  return (
    <ChipShell className={className}>
      <Combobox
        tone="bare"
        size="none"
        multi
        label="Assign"
        disabled={disabled ?? false}
        selected={assignees}
        emptyText={members.length ? "No matches" : "No members yet"}
        face={
          assignees.length > 0 ? (
            <AvatarStack members={stackFor(assignees, members)} />
          ) : (
            // Linear's dashed ghost: "unassigned" is a visible state — and now
            // a target — rather than an absence.
            <span
              className="border-line-strong text-mute flex size-avatar-md items-center justify-center rounded-full border border-dashed"
              title="Unassigned"
            >
              <UserRound className="size-icon-xs opacity-60" />
            </span>
          )
        }
        options={members.map((m) => ({
          id: m.key,
          label: memberName(m.key, m),
          icon: <Avatar deviceKey={m.key} alias={m.alias} me={m.me} size="sm" />,
          // The key prefix, because the petname is the *unverified* half of the
          // identity — Members.tsx makes the same point at full width.
          hint: m.key.slice(0, 6),
          keywords: [m.key, m.alias],
        }))}
        onToggle={(key) => onToggle(key, !assignees.includes(key))}
      />
    </ChipShell>
  );
}

export function LabelsChip({
  names,
  labels,
  disabled,
  onToggle,
  onSwap,
  max,
  showOverflow = true,
  dots,
  className,
}: {
  names: readonly string[];
  labels: LabelDto[];
  disabled?: boolean;
  /** Toggle within the whole set — the dots pill, which *is* the whole set. */
  onToggle: (name: string, add: boolean) => void;
  /** Swap or remove one label — a pill's picker speaks only for its pill. */
  onSwap: (from: string, to: string | null) => void;
  /** Passed through to `LabelChips` — the one thing surfaces differ in. */
  max?: number;
  /** Print `+N` for what `max` dropped — a card affords it, a list line doesn't. */
  showOverflow?: boolean;
  /** The narrow-width form: every label reduced to its dot, in one pill. */
  dots?: boolean;
  className?: string;
}) {
  if (names.length === 0) return null;
  const colorOf = (name: string) => labels.find((l) => l.name === name)?.color ?? "gray";

  // The dots form is one pill standing for the whole set, so its picker is the
  // whole set's: a multi-select that toggles.
  if (dots) {
    return (
      <ChipShell className={className}>
        <Combobox
          tone="bare"
          size="none"
          multi
          label="Change labels"
          disabled={disabled ?? false}
          selected={names}
          emptyText={labels.length ? "No matches" : "No labels yet"}
          face={<LabelDots names={names} colorOf={colorOf} />}
          options={labels.map((l) => ({
            id: l.name,
            label: l.name,
            swatch: catalogColor(l.color),
            keywords: [l.id],
          }))}
          onToggle={(name) => onToggle(name, !names.includes(name))}
        />
      </ChipShell>
    );
  }

  // Pills are Linear's way: each is its own trigger, and its picker speaks
  // only for the pill you clicked — swap this label, or remove it. The same
  // menu the detail rail hangs off its chips, so a pill means one thing
  // wherever you meet it.
  const shown = max === undefined ? names : names.slice(0, max);
  const rest = names.length - shown.length;
  return (
    <span className={cn("flex min-w-0 items-center gap-x-1.5 gap-y-2", className)}>
      {shown.map((name) => (
        <ChipShell key={name}>
          <Combobox
            tone="bare"
            size="none"
            label={`Change label ${name}`}
            disabled={disabled ?? false}
            value={{ id: name, label: name }}
            face={<LabelChip name={name} color={colorOf(name)} size="sm" />}
            options={[
              { id: "__remove__", label: `Remove ${name}` },
              ...labels
                .filter((l) => l.name === name || !names.includes(l.name))
                .map((l) => ({
                  id: l.name,
                  label: l.name,
                  swatch: catalogColor(l.color),
                  keywords: [l.id],
                })),
            ]}
            onPick={(next) => {
              if (next === name) return;
              onSwap(name, next === "__remove__" ? null : next);
            }}
          />
        </ChipShell>
      ))}
      {rest > 0 && showOverflow && (
        <span
          className="text-mute shrink-0 text-xs tabular-nums"
          title={names.slice(shown.length).join(", ")}
        >
          +{rest}
        </span>
      )}
    </span>
  );
}

export function DueChip({
  due,
  disabled,
  onChange,
  face,
  className,
}: {
  /** Unix seconds, or null/undefined for none — the row's own shape. */
  due: number | null | undefined;
  disabled?: boolean;
  onChange: (next: string | null) => void;
  /** The surface's own rendering of the date (or of its absence). */
  face: React.ReactNode;
  className?: string;
}) {
  return (
    <ChipShell className={className}>
      <DatePicker
        tone="bare"
        size="none"
        ariaLabel="Change due date"
        value={due != null ? dueToInput(due) : null}
        disabled={disabled ?? false}
        face={face}
        onChange={onChange}
      />
    </ChipShell>
  );
}

/**
 * The sub-issue progress pill as a door — Linear's "Open sub-issue…" menu.
 * Entirely the system's parts: the shared `Combobox` wearing its `wide` shell,
 * a section heading, and rows built from the option grammar every picker
 * speaks — status glyph, key as the kicker, title, priority on the trailing
 * edge. Picking a row *navigates*; this is the one chip whose menu opens
 * issues rather than writing a field.
 *
 * The children aren't on the `Row` (it carries only the done/total tally), so
 * they load through `loadChildren` when the menu opens — the graph resource
 * the detail pane already reads, fetched on demand and refreshed on each open.
 */
export function SubIssuesChip({
  states,
  face,
  loadChildren,
  onOpen,
  className,
}: {
  /** The workflow, for each child's status glyph. */
  states: WorkflowState[];
  /** The pill — the surface's ring-and-tally rendering. */
  face: React.ReactNode;
  loadChildren: () => Promise<Row[]>;
  onOpen: (reff: string) => void;
  className?: string;
}) {
  const [children, setChildren] = useState<Row[] | null>(null);
  return (
    <ChipShell className={className}>
      <Combobox
        tone="bare"
        size="none"
        wide
        label="Open sub-issue"
        heading="Sub-issues"
        value={null}
        emptyText={children === null ? "Loading sub-issues…" : "No sub-issues"}
        face={face}
        onOpenChange={(open) => {
          // Fetch on every open, keeping the previous list on screen while the
          // fresh one lands — a doorbell may have moved a child since last time.
          if (open) void loadChildren().then(setChildren).catch(() => setChildren([]));
        }}
        options={(children ?? []).map((child) => {
          const state = states.find((s) => s.id === child.status);
          return {
            id: child.reff,
            label: child.title,
            kicker: child.key_alias ?? child.reff,
            ...(state
              ? {
                  icon: (
                    <StatusIcon category={state.category} color={catalogColor(state.color)} />
                  ),
                }
              : {}),
            trailing: <PriorityIcon priority={child.priority} />,
            keywords: [child.reff, ...(child.key_alias ? [child.key_alias] : [])],
          };
        })}
        onPick={onOpen}
      />
    </ChipShell>
  );
}

export function EstimateChip({
  estimate,
  disabled,
  onPick,
  face,
  className,
}: {
  estimate: number | null | undefined;
  disabled?: boolean;
  onPick: (id: string) => void;
  face: React.ReactNode;
  className?: string;
}) {
  return (
    <ChipShell className={className}>
      <Combobox
        tone="bare"
        size="none"
        label="Change estimate"
        disabled={disabled ?? false}
        value={estimate != null ? { id: String(estimate), label: `${estimate} pt` } : null}
        face={face}
        // Fibonacci-ish, Linear's default scale; "None" clears. The engine
        // stores a bare number — the scale is a team convention.
        options={[
          { id: "none", label: "None" },
          ...[1, 2, 3, 5, 8, 13].map((n) => ({ id: String(n), label: `${n} pt` })),
        ]}
        onPick={onPick}
      />
    </ChipShell>
  );
}
