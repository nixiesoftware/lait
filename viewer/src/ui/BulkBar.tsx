import { RotateCcw, SignalHigh, Tag, Trash2, UserRound, X } from "lucide-react";

import type { BulkProgress } from "../core/bulk";
import type { LabelDto, MemberDto, WorkflowState } from "../types";
import { PRIORITY_ORDER } from "../types";
import { Avatar, memberName } from "./Avatar";
import { catalogColor } from "./colors";
import { DatePicker } from "./DatePicker";
import { PriorityIcon, StatusIcon } from "./icons";
import { Combobox } from "./Picker";
import { Button, IconButton } from "./primitives";

/**
 * The bulk-action bar — appears while any issue carries a check (`x`), floats
 * over the list, and vanishes with the last check.
 *
 * Every action here is N ordinary `Request`s, one per checked issue, with only a
 * few in flight at once. The engine's transaction unit remains one intent on one
 * issue, so "set 12 issues to Done" is still twelve honest commits. The bar reports
 * each outcome and retries only failures.
 *
 * **One pill, one set of controls.** It is the one surface that hovers *over* the
 * work rather than being part of the chrome, so it is fully rounded and carries a
 * deeper shadow than any popover — and everything in it is the same chip: the
 * pickers and the icon buttons round the same way, tint the same way, and lift the
 * same way under the pointer. Nothing separates them, because with one shape and
 * one treatment there are no groups for a rule to divide.
 */
export function BulkBar({
  count,
  progress,
  states,
  labels,
  members,
  onStatus,
  onPriority,
  onLabel,
  onAssign,
  onDue,
  onDelete,
  onRetryFailures,
  onClear,
}: {
  count: number;
  progress: BulkProgress | null;
  states: WorkflowState[];
  labels: LabelDto[];
  members: MemberDto[];
  onStatus: (id: string) => void;
  onPriority: (id: string) => void;
  onLabel: (name: string) => void;
  onAssign: (key: string) => void;
  onDue: (due: string) => void;
  onDelete: () => void;
  onRetryFailures: () => void;
  onClear: () => void;
}) {
  const pending = progress?.pending === true;

  return (
    // Two elements, because one cannot both scroll and size itself honestly. A
    // flex scroll container's intrinsic width drops its trailing inset — padding
    // or spacer — so the bar measured 609px of content into a 600px box and the
    // last control sat 3px from the edge while the first sat 13px in. The shell
    // scrolls; the row inside is `w-max`, a definite width that includes its own
    // padding, so the shell has something exact to size to.
    //
    // The inset is generous on purpose: a fully rounded shell curves in about 5px
    // at the height a 28px control's corner sits at, so a rectangle's padding
    // would read as wedged against the edge.
    //
    // `scrollbar-width: none` because the gutter is reserved whether or not it is
    // needed — the bar measured 52px tall for 40px of content on every screen wide
    // enough never to scroll, which is all of them.
    <div className="border-line-strong bg-raised shadow-float fixed bottom-6 left-1/2 z-40 max-w-[calc(100vw-2rem)] -translate-x-1/2 overflow-x-auto rounded-full border [scrollbar-width:none]">
      <div className="flex w-max items-center gap-1.5 px-3 py-1.5">
      {/* Dismiss leads: the first thing you want from a mode is the way out. */}
      <IconButton
        label="Clear selection"
        chord="Esc"
        variant="pill"
        className="size-7"
        onClick={onClear}
      >
        <X className="size-icon-sm" />
      </IconButton>
      {/* The count is the only text in the bar, so it gets the space a divider
          used to take — a gap reads as a break without drawing one. */}
      <span className="mr-1.5 shrink-0 text-sm font-medium tabular-nums">{count} selected</span>

      {progress && (
        <>
          <span
            className={
              progress.failures.length
                ? "text-danger mr-1.5 shrink-0 text-xs"
                : "text-mute mr-1.5 shrink-0 text-xs"
            }
            role="status"
            aria-live="polite"
            title={progress.failures
              .map((failure) => `${failure.label}: ${failure.message}`)
              .join("\n")}
          >
            {progress.pending
              ? `${progress.done}/${progress.total} complete`
              : progress.failures.length
                ? `${progress.successes.length} succeeded · ${progress.failures.length} failed`
                : `${progress.total} complete`}
          </span>
          {!progress.pending && progress.failures.length > 0 && (
            <Button variant="ghost" className="rounded-full" onClick={onRetryFailures}>
              <RotateCcw className="size-icon-xs" />
              Retry failed
            </Button>
          )}
        </>
      )}

      <Combobox
        label="Status"
        variant="pill"
        disabled={pending}
        value={null}
        face={<Face icon={<SignalHigh className="size-icon-sm" />} label="Status" />}
        options={states.map((s) => ({
          id: s.id,
          label: s.name,
          icon: <StatusIcon category={s.category} color={catalogColor(s.color)} />,
        }))}
        onPick={onStatus}
      />
      <Combobox
        label="Priority"
        variant="pill"
        disabled={pending}
        value={null}
        className="capitalize"
        face={<Face icon={<PriorityIcon priority="none" />} label="Priority" />}
        options={[...PRIORITY_ORDER].reverse().map((p) => ({
          id: p,
          label: p,
          icon: <PriorityIcon priority={p} />,
        }))}
        onPick={onPriority}
      />
      <Combobox
        label="Assign"
        variant="pill"
        disabled={pending}
        value={null}
        emptyText={members.length ? "No matches" : "No members yet"}
        face={<Face icon={<UserRound className="size-icon-sm" />} label="Assign" />}
        options={members.map((m) => ({
          id: m.key,
          label: memberName(m.key, m),
          icon: <Avatar deviceKey={m.key} alias={m.alias} me={m.me} size="sm" />,
          hint: m.key.slice(0, 6),
          keywords: [m.key, m.alias],
        }))}
        onPick={onAssign}
      />
      <Combobox
        label="Add label"
        variant="pill"
        disabled={pending}
        value={null}
        emptyText={labels.length ? "No matches" : "No labels yet"}
        face={<Face icon={<Tag className="size-icon-sm" />} label="Label" />}
        options={labels.map((l) => ({
          id: l.name,
          label: l.name,
          swatch: catalogColor(l.color),
        }))}
        onPick={onLabel}
        onCreate={onLabel}
      />
      <DatePicker
        variant="pill"
        value={null}
        placeholder="Due"
        ariaLabel="Set due date on selected"
        onChange={(next) => onDue(next ?? "none")}
      />

      {/* Destructive, and the only control that is: it earns its red on hover
          rather than sitting behind a rule that says "past here be dragons". */}
      <IconButton
        label="Delete selected"
        variant="pill"
        className="hover:bg-danger/10 hover:text-danger size-7"
        disabled={pending}
        onClick={onDelete}
      >
        <Trash2 className="size-icon-sm" />
      </IconButton>
      </div>
    </div>
  );
}

/** A verb in the bar: its glyph, then its name. The icons are what let the
 *  pickers read as actions rather than words wearing chevrons. */
function Face({ icon, label }: { icon?: React.ReactNode; label: string }) {
  return (
    <>
      {icon && (
        <span className="text-mute flex size-icon-sm shrink-0 items-center justify-center">{icon}</span>
      )}
      <span className="min-w-0 flex-1 truncate text-left">{label}</span>
    </>
  );
}
