import type { MilestoneProgress } from "../core/milestone";
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
  tone = "signal",
  className = "",
}: {
  priority: Priority;
  size?: "md" | "sm";
  /** `neutral` drops the urgent badge's red for the same ink the bars use. In a
   *  row, red is the signal — it is what makes an urgent issue findable. In a
   *  menu of five choices it signals nothing (you are looking *at* the choices),
   *  and one red mark among four grey ones just reads as the odd one out. */
  tone?: "signal" | "neutral";
  className?: string;
}) {
  const box = size === "sm" ? "size-icon-xs" : "size-icon-md";
  if (priority === "urgent") {
    const neutral = tone === "neutral";
    return (
      <span
        className={`inline-flex ${box} shrink-0 items-center justify-center ${size === "sm" ? "rounded-full" : "rounded-mark"} ${neutral ? "bg-dim" : "bg-urgent"} ${className}`}
        role="img"
        aria-label="Urgent priority"
      >
        <svg
          viewBox="0 0 16 16"
          // Knocked out of the surface rather than painted white: `bg-dim` is
          // dark in light mode and light in dark mode, and white only survives
          // one of those.
          className={`${size === "sm" ? "size-[8px]" : "size-icon-xs"} ${neutral ? "fill-raised" : "fill-white"}`}
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

export const STATUS_LABEL: Record<StatusCategory, string> = {
  backlog: "Backlog",
  active: "In progress",
  done: "Done",
};

/**
 * The ring, as data rather than as JSX.
 *
 * There are two renderers now — the React one below, and a plain-DOM builder
 * the CodeMirror live preview needs, because a widget in a CodeMirror
 * decoration is a `Node` and there is no React underneath it. The glyph is the
 * product's most-repeated mark; two hand-kept copies of it would disagree
 * within a release. So the shape lives here once and both renderers spell it
 * out from the same list.
 */
export function statusRing(
  category: StatusCategory,
): Array<{ tag: "circle" | "path"; attrs: Record<string, string> }> {
  const rim = {
    tag: "circle" as const,
    attrs: {
      cx: "7",
      cy: "7",
      r: "6",
      fill: "none",
      stroke: "currentColor",
      "stroke-width": "1.5",
      // Backlog is not started, and a dashed ring says so before the colour does.
      ...(category === "backlog" ? { "stroke-dasharray": "2.5 2", opacity: "0.65" } : {}),
    },
  };
  if (category === "active") {
    return [
      rim,
      // A half-filled pie: "started, not finished".
      { tag: "path", attrs: { d: "M7 7 L7 2.5 A4.5 4.5 0 0 1 7 11.5 Z", fill: "currentColor" } },
    ];
  }
  if (category === "done") {
    return [
      rim,
      { tag: "circle", attrs: { cx: "7", cy: "7", r: "6", fill: "currentColor" } },
      // Scaled out from the centre by about a quarter. The old check sat in the
      // middle of a 12px disc looking like a tick someone had dropped in rather
      // than the mark the disc was drawn for. Its furthest point reaches 3.9
      // from centre and the stroke adds 0.9, so it still clears the r=6 rim.
      {
        tag: "path",
        attrs: {
          d: "M3.8 7.2 L6.1 9.5 L10.2 4.8",
          fill: "none",
          stroke: "var(--color-bg)",
          "stroke-width": "1.8",
          "stroke-linecap": "round",
          "stroke-linejoin": "round",
        },
      },
    ];
  }
  return [rim];
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
  return (
    <svg
      viewBox="0 0 14 14"
      className={`size-icon-sm shrink-0 ${className}`}
      role="img"
      aria-label={STATUS_LABEL[category]}
      style={{ color }}
    >
      {statusRing(category).map(({ tag: Tag, attrs }, i) => (
        <Tag key={i} {...attrs} />
      ))}
    </svg>
  );
}

/** The same ring, built as DOM. See `statusRing` for why there are two. */
export function statusIconElement(category: StatusCategory, color: string): SVGSVGElement {
  const NS = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(NS, "svg");
  svg.setAttribute("viewBox", "0 0 14 14");
  svg.setAttribute("class", "size-icon-sm shrink-0");
  svg.setAttribute("role", "img");
  svg.setAttribute("aria-label", STATUS_LABEL[category]);
  svg.style.color = color;
  for (const { tag, attrs } of statusRing(category)) {
    const child = document.createElementNS(NS, tag);
    for (const [name, value] of Object.entries(attrs)) child.setAttribute(name, value);
    svg.append(child);
  }
  return svg;
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

/**
 * A milestone's state as a diamond — Linear's mark, in the same grammar as the
 * status circle above: outline while untouched, half-filled once work lands,
 * solid when it closes. The shape says "milestone" where the circle says
 * "status", so the two never read as the same field even at 14px.
 *
 * Colour follows the project's own progress bar rather than Linear's palette —
 * `mute` / `accent` / `ok` are what a lait progress bar already means, and a
 * milestone that turned amber next to a bar that turned blue would be two
 * vocabularies for one idea.
 *
 * The fill is derived from counts (`core/milestone.ts`), so this draws whatever
 * the issues currently say. There is no state to get out of sync.
 */
export function MilestoneIcon({
  progress,
  className = "",
}: {
  /**
   * `"none"` is the No-milestone bucket — the *absence* of a milestone, not a
   * milestone that has not started. It draws dashed, borrowing the rule the
   * status circle above already established: a broken outline says "this is not
   * a thing yet" before the colour says anything.
   */
  progress: MilestoneProgress | "none";
  className?: string;
}) {
  const label = {
    none: "No milestone",
    "not-started": "Not started",
    "in-progress": "In progress",
    complete: "Complete",
  }[progress];
  const tone = {
    none: "var(--color-mute)",
    "not-started": "var(--color-mute)",
    "in-progress": "var(--color-accent)",
    complete: "var(--color-ok)",
  }[progress];
  return (
    <svg
      viewBox="0 0 14 14"
      className={`size-icon-sm shrink-0 ${className}`}
      role="img"
      aria-label={label}
      style={{ color: tone }}
    >
      {/* A rotated square rather than a `<rect transform>`: the path keeps the
          points on exact half-pixels, which is what stops the rim shimmering at
          this size. */}
      <path
        d="M7 1.4 L12.6 7 L7 12.6 L1.4 7 Z"
        fill={progress === "complete" ? "currentColor" : "none"}
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinejoin="round"
        // Dots, not dashes: a zero-length segment under a round cap renders as a
        // disc of the stroke width. Two per side — the side is 5.6·√2 ≈ 7.92, so
        // a 3.96 period puts a dot exactly on every corner, which is what keeps
        // the points and stops the ring reading as a circle.
        //
        // Two, not three: the dot is 1.5 wide and the glyph is 14px, so a third
        // per side leaves ~1px of gap and the discs anti-alias into a continuous
        // fuzzy ring — the one shape this must not be, because the *circle* is
        // the status icon.
        strokeDasharray={progress === "none" ? "0.01 3.96" : undefined}
        strokeLinecap={progress === "none" ? "round" : undefined}
      />
      {progress === "in-progress" && (
        // The left half, so the fill grows the way a bar does — from the start
        // edge inward — rather than from an arbitrary side.
        <path d="M7 2.6 L7 11.4 L2.6 7 Z" fill="currentColor" />
      )}
    </svg>
  );
}
