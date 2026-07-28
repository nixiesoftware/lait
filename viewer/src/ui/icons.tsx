import type { Priority, StatusCategory } from "../types";

/**
 * The two glyphs a tracker row lives or dies by.
 *
 * Hand-drawn rather than pulled from an icon set, and this is the exception that
 * proves the "don't build what exists" rule: priority and status are *data*
 * encoded as shape, not decoration. lucide has no bar-chart-priority or
 * partial-progress-ring, and approximating them with the nearest generic icon is
 * how a dense list stops being scannable — which is the whole point of the row.
 */

/** Priority as ascending bars, dimmed when unset. Linear's grammar, and it reads
 *  at a glance in a way a word never does at this density.
 *
 *  `size="sm"` is the pill-row measure: a card's property pills draw their
 *  glyphs at 12px, and the urgent badge at its full 16px sat among them like a
 *  thumb — so it steps down with everything else, and goes fully round: at
 *  12px the mark radius reads as a squashed square, and a circle is the shape
 *  the other small glyphs already are. */
export function PriorityIcon({
  priority,
  size = "md",
  className = "",
}: {
  priority: Priority;
  size?: "md" | "sm";
  className?: string;
}) {
  const box = size === "sm" ? "size-icon-xs" : "size-icon-md";
  if (priority === "urgent") {
    return (
      <span
        className={`inline-flex ${box} shrink-0 items-center justify-center ${size === "sm" ? "rounded-full" : "rounded-mark"} bg-urgent ${className}`}
        role="img"
        aria-label="Urgent priority"
      >
        <svg
          viewBox="0 0 16 16"
          className={`${size === "sm" ? "size-[8px]" : "size-icon-xs"} fill-white`}
          aria-hidden="true"
        >
          <rect x="7" y="3.5" width="2" height="6" rx="1" />
          <rect x="7" y="11" width="2" height="2" rx="1" />
        </svg>
      </span>
    );
  }
  // 3 bars; `lit` counts how many are filled.
  const lit = { none: 0, low: 1, medium: 2, high: 3, urgent: 3 }[priority];
  const label = priority === "none" ? "No priority" : `${priority} priority`;
  return (
    <svg
      viewBox="0 0 16 16"
      className={`${box} shrink-0 ${className}`}
      role="img"
      aria-label={label}
    >
      {[0, 1, 2].map((i) => (
        <rect
          key={i}
          x={2 + i * 5}
          y={11 - i * 3}
          width="3"
          height={3 + i * 3}
          rx="1"
          // An unset priority still draws all three bars, faintly: the shape has
          // to stay constant or the column jitters between rows.
          className={i < lit ? "fill-dim" : "fill-line-strong"}
        />
      ))}
    </svg>
  );
}

/**
 * Status as a progress ring, shaped by category so it reads without colour —
 * which matters for the ~8% of users who would otherwise see three identical
 * circles.
 */
export function StatusIcon({
  category,
  color,
  className = "",
}: {
  category: StatusCategory;
  color: string;
  className?: string;
}) {
  const label = { backlog: "Backlog", active: "In progress", done: "Done" }[category];
  return (
    <svg
      viewBox="0 0 14 14"
      className={`size-icon-sm shrink-0 ${className}`}
      role="img"
      aria-label={label}
      style={{ color }}
    >
      <circle
        cx="7"
        cy="7"
        r="6"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        // Backlog is not started, and a dashed ring says so before the colour does.
        strokeDasharray={category === "backlog" ? "2.5 2" : undefined}
        opacity={category === "backlog" ? 0.65 : 1}
      />
      {category === "active" && (
        // A half-filled pie: "started, not finished".
        <path d="M7 7 L7 2.5 A4.5 4.5 0 0 1 7 11.5 Z" fill="currentColor" />
      )}
      {category === "done" && (
        <>
          <circle cx="7" cy="7" r="6" fill="currentColor" />
          {/* Scaled out from the centre by about a quarter. The old check sat
              in the middle of a 12px disc looking like a tick someone had
              dropped in rather than the mark the disc was drawn for. Its
              furthest point reaches 3.9 from centre and the stroke adds 0.9,
              so it still clears the r=6 rim with room to spare. */}
          <path
            d="M3.8 7.2 L6.1 9.5 L10.2 4.8"
            fill="none"
            stroke="var(--color-bg)"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </>
      )}
    </svg>
  );
}

/**
 * Sub-issue progress as a filling ring — Linear's mark, and the same grammar as
 * the status icon above: a circle whose fill *is* the number. A checklist glyph
 * next to "2/7" said the same thing twice in different alphabets; the ring lets
 * the fraction be read as shape before it is read as digits. Strokes in
 * `currentColor`, so the chip it sits in decides the voice — dim while under
 * way, `ok` when the ring closes.
 */
export function ProgressRing({
  done,
  total,
  className = "",
}: {
  done: number;
  total: number;
  className?: string;
}) {
  const r = 5.5;
  const circumference = 2 * Math.PI * r;
  const arc = circumference * (total > 0 ? Math.min(1, done / total) : 0);
  return (
    <svg
      viewBox="0 0 14 14"
      // Rotated so the fill grows from 12 o'clock, the way every dial is read.
      className={`size-icon-xs shrink-0 -rotate-90 ${className}`}
      role="img"
      aria-label={`${done} of ${total} sub-issues done`}
    >
      <circle cx="7" cy="7" r={r} fill="none" stroke="currentColor" strokeWidth="1.5" opacity="0.3" />
      {arc > 0 && (
        <circle
          cx="7"
          cy="7"
          r={r}
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
          strokeLinecap="round"
          strokeDasharray={`${arc} ${circumference}`}
        />
      )}
    </svg>
  );
}
