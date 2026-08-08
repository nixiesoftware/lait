import { useEffect, useRef, useState } from "react";
import { Calendar, ChevronLeft, ChevronRight, X } from "lucide-react";

import { IconButton, Popover } from "@astryxdesign/core";
import { cn, controlTrigger, navigationItem, type ControlSize, type ControlTone } from "./primitives";

/** The month grid's measure. Stated once, because the Popover needs the number
 *  to position with and the flip test needs the same number to reason about. */
const PANEL_WIDTH = 256;

/**
 * A date picker that belongs to the design system — the one control that used to
 * be the browser's native `<input type="date">`.
 *
 * The native input was defensible (its picker beats anything hand-rolled), and it
 * was also the single control that looked different on every OS and nothing like
 * the `Combobox` popover every other field opens. Worse, it made "tomorrow" a
 * calendar-driving chore: the deadlines people actually set are relative, and Linear
 * leads its date control with exactly those — Today, Tomorrow, Next week — before
 * the grid. So do we.
 *
 * **Everything is UTC.** The engine stores a due date as UTC midnight of the day you
 * named (see `time.ts::dueLabel`), and the wire format is `YYYY-MM-DD`. This whole
 * component computes in UTC so the day you tap is the day that gets stored, with no
 * timezone drift bending it a day either way. `value` and the argument to `onChange`
 * are that same `YYYY-MM-DD` string (or `null` for "no date") — the component never
 * touches unix seconds, so the caller's engine round-trip is a pure pass-through.
 */

// A day as its UTC calendar date, never a local one.
function utcDay(y: number, m: number, d: number): Date {
  return new Date(Date.UTC(y, m, d));
}
function todayUtc(): Date {
  const now = new Date();
  return utcDay(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
}
function parseInput(s: string): Date {
  const [y, m, d] = s.split("-").map(Number);
  return utcDay(y ?? 1970, (m ?? 1) - 1, d ?? 1);
}
function toInput(d: Date): string {
  return d.toISOString().slice(0, 10);
}
function addDays(d: Date, n: number): Date {
  return utcDay(d.getUTCFullYear(), d.getUTCMonth(), d.getUTCDate() + n);
}
function startOfMonth(d: Date): Date {
  return utcDay(d.getUTCFullYear(), d.getUTCMonth(), 1);
}
function addMonths(d: Date, n: number): Date {
  return utcDay(d.getUTCFullYear(), d.getUTCMonth() + n, 1);
}
// Monday-indexed weekday (0 = Monday), the week most product calendars open on.
function mondayIndex(d: Date): number {
  return (d.getUTCDay() + 6) % 7;
}

const WEEKDAYS = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

function labelFor(value: string): string {
  const d = parseInput(value);
  const sameYear = d.getUTCFullYear() === new Date().getUTCFullYear();
  return d.toLocaleDateString(undefined, {
    timeZone: "UTC",
    month: "short",
    day: "numeric",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

export function DatePicker({
  value,
  onChange,
  disabled,
  tone,
  size,
  ariaLabel = "Due date",
  placeholder = "None",
  className,
  face,
}: {
  value: string | null;
  onChange: (next: string | null) => void;
  disabled?: boolean;
  ariaLabel?: string;
  /** Trigger content (and colour) when `value` is null. A node rather than a
   *  string because both branches below already render it as children, and the
   *  issue rail's empty chip carries two wordings for a container query to
   *  choose between — see `EmptyValue`. */
  placeholder?: React.ReactNode;
  /** Extra trigger classes — the caller's tone colour rides here. */
  className?: string;
  tone?: ControlTone;
  size?: ControlSize;
  /** Trigger content. Defaults to the calendar glyph + label — a chip that
   *  already draws the date its own way (a list row's bare column, a card's
   *  `CalendarClock` run) passes its face and keeps its geometry. */
  face?: React.ReactNode;
}) {
  const [open, setOpen] = useState(false);
  /**
   * Which edge the calendar hangs from. Astryx's Popover does not flip, so a
   * Due date chip in the issue rail put a 256px grid against the window's right
   * edge and had it squeezed. Measured on open, for the same reasons set out in
   * `Picker.tsx` — the trigger moves, and a keybinding can open this.
   */
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [flipped, setFlipped] = useState(false);
  useEffect(() => {
    if (!open) return;
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) return;
    setFlipped(rect.left + PANEL_WIDTH > window.innerWidth - 8);
  }, [open]);
  const [view, setView] = useState<Date>(() =>
    startOfMonth(value ? parseInput(value) : todayUtc()),
  );

  const today = todayUtc();
  const selected = value;

  const pick = (next: string | null) => {
    onChange(next);
    setOpen(false);
  };

  // A read-only field still shows its value — it just can't open a menu over it.
  if (disabled) {
    return (
      <span className={cn(controlTrigger({ tone, size }), "text-dim", !value && "text-mute", className)}>
        {face ?? (value ? labelFor(value) : placeholder)}
      </span>
    );
  }

  const monthStart = view;
  const gridStart = addDays(monthStart, -mondayIndex(monthStart));
  const cells = Array.from({ length: 42 }, (_, i) => addDays(gridStart, i));

  const quick: { label: string; value: string | null }[] = [
    { label: "Today", value: toInput(today) },
    { label: "Tomorrow", value: toInput(addDays(today, 1)) },
    { label: "Next week", value: toInput(addDays(today, 7)) },
    { label: "No due date", value: null },
  ];

  return (
    <Popover
      isOpen={open}
      onOpenChange={(o) => {
        setOpen(o);
        // Re-centre on the current value each time it opens — you almost always
        // want to start from the date that is set, not wherever you last browsed.
        if (o) setView(startOfMonth(value ? parseInput(value) : todayUtc()));
      }}
      // Hangs from whichever edge has room, for the same reason and by the same
      // measurement as `Picker` — see the note there. This one sits in the issue
      // rail as Due date, which is exactly where the window runs out.
      alignment={flipped ? "end" : "start"}
      // On the Popover, not on the content: Astryx caps the panel at
      // `max-width: 100%`, so a width stated on the child is a width the
      // positioner never sees.
      width={PANEL_WIDTH}
      // Gated, for the same reason as Picker: Astryx keeps popover content in
      // the DOM whether or not it is showing, and a 42-cell month grid on every
      // due-date chip in a list is a lot of cells nobody asked for.
      content={
        !open ? null : (
        <div className="p-2">
        <div className="mb-1 flex flex-col gap-0.5">
          {quick.map((q) => (
            // A menu row, not a Button: full-width, flat, left-aligned. Astryx's
            // Button would make it a pill with its own padding, which is the
            // wrong shape for a list of choices stacked in a popover.
            <button
              key={q.label}
              type="button"
              onClick={() => pick(q.value)}
              className={cn(navigationItem(), "justify-between")}
            >
              {q.label}
              {q.value === null && selected && <X className="text-mute size-icon-xs" />}
            </button>
          ))}
        </div>

        <div className="border-line border-t pt-2">
          <div className="mb-1 flex items-center justify-between px-1">
            <IconButton
              label="Previous month"
              onClick={() => setView(addMonths(view, -1))}
              variant="ghost"
              size="sm"
              tooltip="Previous month"
              icon={<ChevronLeft className="size-icon-sm" />}
            />
            <span className="text-sm font-medium">
              {view.toLocaleDateString(undefined, {
                timeZone: "UTC",
                month: "long",
                year: "numeric",
              })}
            </span>
            <IconButton
              label="Next month"
              onClick={() => setView(addMonths(view, 1))}
              variant="ghost"
              size="sm"
              tooltip="Next month"
              icon={<ChevronRight className="size-icon-sm" />}
            />
          </div>

          <div className="grid grid-cols-7 gap-0.5">
            {WEEKDAYS.map((w) => (
              <span key={w} className="text-mute py-1 text-center text-2xs font-medium">
                {w}
              </span>
            ))}
            {cells.map((cell) => {
              const iso = toInput(cell);
              const inMonth = cell.getUTCMonth() === monthStart.getUTCMonth();
              const isSelected = iso === selected;
              const isToday = iso === toInput(today);
              return (
                // NOT an Astryx Button. A day cell is a 28px square in a
                // seven-column grid; Astryx's Button brings `padding: 8px 12px`
                // and a pill radius, which leaves ~3px of content box and clips
                // the digit. `Button` means "an action with a label" — this is a
                // grid cell that happens to be pressable.
                <button
                  key={iso}
                  type="button"
                  onClick={() => pick(iso)}
                  aria-label={cell.toLocaleDateString(undefined, {
                    timeZone: "UTC",
                    weekday: "long",
                    month: "long",
                    day: "numeric",
                    year: "numeric",
                  })}
                  aria-pressed={isSelected}
                  className={cn(
                    "size-ctl-md rounded-control text-sm tabular-nums",
                    isSelected
                      ? "bg-accent text-accent-fg"
                      : inMonth
                        ? "text-fg hover:bg-active"
                        : "text-mute hover:bg-active",
                    isToday && !isSelected && "ring-line-strong ring-1 ring-inset",
                  )}
                >
                  {cell.getUTCDate()}
                </button>
              );
            })}
          </div>
        </div>
        </div>
        )
      }
    >
      {/* Astryx requires the trigger children to contain a real button; Radix's
          Popover.Trigger used to render one for us. */}
      <button
        ref={triggerRef}
        type="button"
        aria-label={ariaLabel}
        className={cn(controlTrigger({ tone, size, open }), !value && "text-mute", className)}
      >
        {face ?? (
          <>
            <Calendar className="text-mute size-icon-sm shrink-0" />
            <span>{value ? labelFor(value) : placeholder}</span>
          </>
        )}
      </button>
    </Popover>
  );
}
