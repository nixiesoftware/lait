import { Tooltip } from "@astryxdesign/core";
import { cva, type VariantProps } from "class-variance-authority";
import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

import { catalogColor, labelSurface } from "./colors";

/**
 * The primitives every surface builds from.
 *
 * Radix ships no Button, and that is correct — a native `<button>` needs no
 * behaviour wrapper, and Radix has no styling opinion to give. What we were
 * missing was never a component library; it was a **variant system**. Without one
 * every button grows its own class string, and they drift: we had five different
 * "button" recipes disagreeing about padding, border, and weight, which is how a
 * header ends up looking assembled rather than designed.
 *
 * So: `cva` maps intent (`variant`, `size`) to classes, once. A caller says
 * `<Button variant="ghost">`, never `px-2 py-1 rounded border …`. `cn` merges any
 * override through `tailwind-merge`, so a later `px-3` actually replaces the
 * variant's padding instead of both landing in the class list and letting source
 * order decide.
 *
 * The default is **ghost**. Chrome should recede until you need it — a toolbar of
 * bordered buttons competes with the content it exists to serve.
 */

/**
 * Our named ladders are invisible to `tailwind-merge` out of the box.
 *
 * It resolves conflicts by knowing which classes belong to the same group, and
 * that knowledge is a fixed vocabulary: it recognises `rounded-full` but has
 * never heard of `rounded-control`, so it files them separately and keeps BOTH.
 * Two border-radius declarations then reach the element and CSS source order —
 * not the caller's intent — decides the corner.
 *
 * That failed quietly rather than loudly, which is the dangerous kind: a recipe
 * whose variant asked for a pill kept the base's box, or didn't, depending on
 * which rule Tailwind happened to emit last. Registering the ladders here fixes
 * the whole class of bug once, instead of restructuring every recipe to avoid
 * ever emitting two.
 */
const LADDERS = [
  "ctl-xs", "ctl-sm", "ctl-md", "ctl-lg", "ctl-xl",
  "bar-sm", "bar-md", "bar-lg",
  "avatar-sm", "avatar-md", "avatar-lg",
  "icon-2xs", "icon-xs", "icon-sm", "icon-md", "icon-lg",
  "mark-xs", "mark-sm", "mark-md", "mark-lg", "mark-xl",
];

const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      rounded: [{ rounded: ["mark", "control", "surface"] }],
      h: [{ h: LADDERS }],
      "min-h": [{ "min-h": LADDERS }],
      w: [{ w: LADDERS }],
      size: [{ size: LADDERS }],
    },
  },
});

/**
 * How far a floating surface sits from the thing that opened it.
 *
 * A number rather than a token because Radix takes `sideOffset` as a prop, not
 * a class — but the reason for naming it is the same as every other axis: one
 * value per CATEGORY of surface, so "move the menus further out" is one edit
 * instead of a hunt through call sites. These were five loose literals across
 * six files, and two popovers had drifted 2px off the shared default with no
 * stated reason.
 */
export const OverlayGap = {
  /** Anchored panels: popovers, pickers, date pickers, filters. */
  panel: 4,
  /** Dropdown menus. Same as `panel` today, split out so the menu family can
   *  be retuned without touching every popover. */
  menu: 4,
  /** Tooltips. Deliberately looser: a tip carries no border-to-border
   *  relationship with its trigger, so it needs the extra breathing room to
   *  read as a label about the control rather than part of it. */
  tip: 6,
} as const;

export function cn(...parts: ClassValue[]): string {
  return twMerge(clsx(parts));
}

/**
 * `Button`, `IconButton` and the ten-variant `button` recipe used to live here.
 *
 * They are Astryx's now — `import { Button, IconButton } from "@astryxdesign/core"`.
 * Six of our ten variants turned out to be components rather than variants
 * (`active` was a SegmentedControl, `inline` a Link, `pill` and `size="icon"`
 * an IconButton); `primary` kept our neutral-inverse meaning through a theme
 * override, and `danger` is the one piece of genuinely new vocabulary, added
 * to `ButtonVariantMap` by `astryx theme build`. See `src/theme/README.md`.
 */

const badge = cva(
  "inline-flex min-w-0 items-center gap-1 whitespace-nowrap rounded-full border px-1.5 text-2xs leading-4",
  {
    variants: {
      tone: {
        neutral: "border-line bg-raised text-dim",
        accent: "border-accent/30 bg-accent/10 text-accent",
        danger: "border-danger/30 bg-danger/5 text-danger",
        success: "border-ok/30 bg-ok/10 text-ok",
      },
    },
    defaultVariants: { tone: "neutral" },
  },
);

/** True compact metadata: counts, tags, reactions and state labels. */
export function Badge({
  tone,
  className,
  ...props
}: React.HTMLAttributes<HTMLSpanElement> & VariantProps<typeof badge>) {
  return <span className={cn(badge({ tone }), className)} {...props} />;
}

/** An actual interactive pill: reactions and removable/filter chips only. */
/**
 * A label, as it appears on an issue.
 *
 * One component because there were three: the detail rail, the list row and the
 * board card each carried their own copy, and they had drifted to two different
 * border tokens, two text colours, and one height between them (16px in two
 * places, 16px-with-`h-4` in the third). Nothing chose those differences.
 *
 * Sized to belong to the document language rather than to hide from it. The old
 * chip was 10px type in a 16px pill sitting in a 28px row beside 14px values —
 * legible, but reading as a footnote about the issue instead of a property of
 * it. `dot + tint + name` after Linear's shape and GitHub's weight: the dot is
 * the crisp colour anchor, the tint gives the pill presence, and the name stays
 * on a text token, so contrast never depends on which colour was picked.
 */
export function LabelChip({
  name,
  color,
  size = "md",
  className,
}: {
  name: string;
  /** A catalog colour name (or hex). Unknown names resolve to the muted token. */
  color: string;
  /** `sm` for dense rows — a list line, a board card. */
  size?: "sm" | "md";
  className?: string;
}) {
  return (
    <span
      title={name}
      style={labelSurface(color)}
      className={cn(
        "inline-flex min-w-0 items-center rounded-full border font-medium whitespace-nowrap",
        // `text-sm` is 12px on this scale, the same size as the rail values a
        // label sits among — a label *is* a value, and setting it a step down
        // was what made the old 16px chip read as a footnote.
        //
        // Both sizes are 24px — Linear's measure, and the roominess is the
        // point: at 20px with an 8px inset the pill read as a cramped ticket
        // stub next to every other control on the page. The rail's rows wrap a
        // hair looser for it, which is the trade Linear makes too.
        //
        // What the sizes differ in is voice. In the rail a label is a property
        // you are reading, so it takes the foreground at the same 12px as the
        // values around it; in a list row it is metadata you scan past next to
        // a date and a project, so it drops to `text-dim` and 11px and sits at
        // their weight rather than above it.
        size === "md" ? "text-fg h-ctl-sm gap-2 px-2.5 text-sm" : "text-dim h-ctl-sm gap-1.5 px-2.5 text-xs",
        className,
      )}
    >
      <span
        className="size-mark-sm shrink-0 rounded-full"
        style={{ background: catalogColor(color) }}
      />
      {/* `capitalize` is a text transform, not a rewrite: the DOM still holds
          the stored name, so copying a chip yields the string the engine
          resolves — `Request::Label` matches on the name, and a display
          convention must never become an identity change. */}
      <span className="truncate capitalize">{name}</span>
    </span>
  );
}

/**
 * A run of labels, with the tail folded into a count.
 *
 * `max` is the one thing that legitimately differs by surface — a list line has
 * less room than a rail — so it is a parameter rather than three components.
 */
export function LabelChips({
  names,
  colorOf,
  max,
  showOverflow = true,
  size = "md",
  className,
}: {
  names: readonly string[];
  /** Resolve a label name to its catalog colour. */
  colorOf: (name: string) => string;
  /** Show at most this many. Omit to show all. */
  max?: number;
  /**
   * Print `+N` for what `max` dropped. On by default — silently hiding data is
   * worse than a counter — but a list line trades that away: the row already
   * carries an id, a status and a title, and a trailing tally of things you
   * cannot see competes with all three.
   */
  showOverflow?: boolean;
  size?: "sm" | "md";
  className?: string;
}) {
  if (names.length === 0) return null;
  const shown = max === undefined ? names : names.slice(0, max);
  const rest = names.length - shown.length;
  return (
    // A wider row gap than column gap: wrapped chips otherwise stack at a
    // tighter pitch than the rows they share a rail with, and the block reads
    // as detached from the column.
    <span className={cn("flex min-w-0 flex-wrap items-center gap-x-1.5 gap-y-2", className)}>
      {shown.map((name) => (
        <LabelChip key={name} name={name} color={colorOf(name)} size={size} />
      ))}
      {rest > 0 && showOverflow && (
        <span className="text-mute shrink-0 text-xs tabular-nums" title={names.slice(shown.length).join(", ")}>
          +{rest}
        </span>
      )}
    </span>
  );
}

/**
 * Linear's narrow-width form of a label set: one pill, every label reduced to
 * its dot, the dots overlapped, the count after. Shown instead of `LabelChips`
 * when the container is too narrow for pills — the labels stay present as
 * *colour*, which is the half that scans. The names ride a real tooltip (the
 * app's, not the browser's): a pill whose whole point is compression owes the
 * hover the uncompressed answer.
 */
export function LabelDots({
  names,
  colorOf,
  className,
}: {
  names: readonly string[];
  colorOf: (name: string) => string;
  className?: string;
}) {
  if (names.length === 0) return null;
  return (
    <Tooltip
      // The content is a legend, not a sentence: one swatch and one name per
      // line, so the chip's colours can be read back as words.
      content={
        <div className="flex flex-col gap-1">
          {names.map((name) => (
            <span key={name} className="flex items-center gap-1.5">
              <span
                className="size-mark-xs shrink-0 rounded-full"
                style={{ background: catalogColor(colorOf(name)) }}
              />
              <span className="capitalize">{name}</span>
            </span>
          ))}
        </div>
      }
    >
      <span
          aria-label={`Labels: ${names.join(", ")}`}
          className={cn(
            "border-line text-dim inline-flex h-ctl-sm shrink-0 items-center gap-1.5 rounded-full border px-2.5 text-xs font-medium",
            className,
          )}
        >
          <span className="flex -space-x-1">
            {names.map((name) => (
              <span
                key={name}
                className="size-mark-sm rounded-full ring-1 ring-[var(--color-bg)]"
                style={{ background: catalogColor(colorOf(name)) }}
              />
            ))}
          </span>
          <span className="tabular-nums">{names.length}</span>
        </span>
    </Tooltip>
  );
}

export function ChipButton({
  className,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement>) {
  return (
    <button
      className={cn(
        "border-line bg-bg text-dim hover:border-line-strong hover:bg-hover aria-pressed:border-accent/40 aria-pressed:bg-accent/10 aria-pressed:text-fg inline-flex h-ctl-sm items-center gap-1 rounded-full border px-1.5 text-xs outline-none transition-colors focus-visible:ring-accent/50 focus-visible:ring-1 disabled:pointer-events-none disabled:opacity-45",
        className,
      )}
      {...props}
    />
  );
}

/**
 * Shared trigger geometry for controls that open a popover. Opening a menu is
 * behaviour, not a visual role: property values should remain quiet, while a
 * standalone composer/filter control needs a visible boundary. Neither is a
 * semantic pill; true tags and reactions get their own primitive.
 */
/**
 * The leading glyph slot of a breadcrumb crumb.
 *
 * Objects keep the mark the rest of the app gives them — a project is an 8px
 * swatch on cards and in the sidebar, a workspace destination is a 14px lucide
 * icon — but the *slot* is one size everywhere, so a crumb's text starts at the
 * same offset whatever kind of thing it names. Without it the trail shifted
 * sideways as you moved between a project view and a workspace destination, and
 * again between a project crumb and the picker that replaces it in a
 * multi-project space.
 *
 * It lives here rather than in `layout` because the picker draws its own face
 * and needs the same slot; `layout` and `Picker` both already depend on this
 * module, and neither should depend on the other.
 */
export const crumbGlyph = "flex size-icon-md shrink-0 items-center justify-center";

/**
 * Trigger geometry, decomposed.
 *
 * This was seven `variant`s, which is what happens when one axis has to encode
 * two things: `property` and `crumb` were the same face at different heights,
 * and `chip` and `filter` the same box on different fills. Height was welded in,
 * so "a property row, but taller" had no expression except an eighth variant.
 *
 * Now TONE says what a control looks like and SIZE says how tall it is, and the
 * two compose. The call-site audit that preceded this also retired `filter` and
 * `toolbar` outright — both had zero usages anywhere in the app.
 */
export const controlTrigger = cva(
  // No radius in the base: a corner is part of a tone's identity, not a default
  // it inherits. Keeping it here meant a tone that wanted a different shape
  // emitted both classes and left the winner to CSS source order. The open-state
  // fill moved out for the same reason: `bare`'s whole identity is "no box", and
  // a base that painted one behind every open trigger overruled it.
  "inline-flex items-center gap-1.5 text-sm outline-none transition-colors disabled:pointer-events-none disabled:opacity-45",
  {
    variants: {
      /** How tall. Declared before `tone` only for readability; `cn` resolves
       *  any overlap through the registered ladders, not through source order. */
      size: {
        /** No height of its own — for a tone that brings its own shape. */
        none: "",
        xs: "min-h-ctl-xs",
        sm: "min-h-ctl-sm",
        md: "min-h-ctl-md",
        lg: "min-h-ctl-lg",
        xl: "min-h-ctl-xl",
      },
      tone: {
        /** Chrome that is only there when you point at it — the property rail
         *  and the breadcrumb switcher. Fully rounded: a hover fill is the only
         *  shape it has, so a pill reads as deliberate where a rounded box reads
         *  as something that appeared under the pointer. */
        quiet: "hover:bg-hover -mx-1 min-w-0 rounded-full px-1.5 text-left",
        /** A standing control with a border. Stays a box — a border makes the
         *  shape explicit rather than only appearing on hover, and a pilled
         *  border starts reading as a tag rather than a control. */
        outline: "border-line hover:border-line-strong hover:bg-hover rounded-control border px-2",
        /** Inside a floating bar. It lifts on hover: the bar is the one surface
         *  that is over the work rather than part of it, so its controls answer
         *  the pointer with elevation instead of only a fill. */
        pill: "text-dim hover:bg-hover hover:text-fg rounded-full px-2.5",
        /** No box at all — the child already is one, and no state may paint one
         *  either: a glyph on a row answers the pointer and the open menu by
         *  getting *lighter*, never by growing a fill or dimming away. Pair
         *  with `size="none"`: the chip carries its own height. */
        bare: "min-w-0 rounded-full transition-[filter] hover:brightness-125",
      },
      /**
       * The menu is open, and the trigger has to say so.
       *
       * This used to ride on `data-[state=open]` — Radix's attribute, written
       * onto its own Trigger. Astryx's popover writes `aria-expanded` onto the
       * button instead, and re-pointing the selector at that got the rule
       * generated but not applied: in the popover's own subtree the variant
       * lost, while the identical markup cloned onto `<body>` won. Rather than
       * keep guessing at whose attribute wins where, the state comes from us —
       * `Combobox` and `DatePicker` already hold it in React, so styling from a
       * boolean is both simpler and honest about who knows.
       */
      open: { true: "", false: "" },
    },
    compoundVariants: [
      // The resting fill lives here rather than on the tone, so a trigger only
      // ever carries ONE background class.
      { tone: "outline", open: false, class: "bg-bg" },
      { tone: "pill", open: false, class: "bg-active/60" },
      { tone: "quiet", open: true, class: "bg-active" },
      { tone: "outline", open: true, class: "bg-active" },
      { tone: "pill", open: true, class: "bg-active" },
      // The bare tone paints no fill in any state — a glyph on a row answers
      // the open menu by getting lighter, never by growing a box.
      { tone: "bare", open: true, class: "brightness-125" },
    ],
    defaultVariants: { tone: "outline", size: "md", open: false },
  },
);

export type ControlTone = NonNullable<VariantProps<typeof controlTrigger>["tone"]>;
export type ControlSize = NonNullable<VariantProps<typeof controlTrigger>["size"]>;

/** Shared list interaction states. Content layout remains the caller's concern;
 * hover, selection, focus and dividers do not. */
export const interactiveRow = cva(
  "group cursor-default outline-none transition-colors focus-visible:bg-hover focus-visible:ring-accent/50 focus-visible:ring-1 focus-visible:ring-inset",
  {
    variants: {
      surface: {
        list: "border-line/35 border-b",
        contained: "rounded-control",
      },
      selected: {
        true: "bg-active text-fg",
        false: "hover:bg-hover",
      },
      /** Rungs, not adjectives. `compact`/`normal`/`roomy` described a row
       *  relative to its neighbours, which is why two of them silently
       *  collapsed onto one height when D5 retired the 36px step and nothing
       *  could say so. A rung names the height itself, so a call site asking
       *  for `lg` gets `lg` and two call sites agreeing is visible rather than
       *  hidden behind two words that mean the same thing. */
      size: {
        md: "min-h-ctl-md",
        lg: "min-h-ctl-lg",
        xl: "min-h-ctl-xl",
      },
    },
    defaultVariants: {
      surface: "list",
      selected: false,
      size: "lg",
    },
  },
);

/** One navigation hit-area and state language for the app rail and settings. */
export const navigationItem = cva(
  // Fully rounded, same family as the property rail and the crumb it sits
  // under: a nav row carries no border, so a hover or selected fill is the
  // only thing describing its shape.
  "flex w-full min-w-0 items-center gap-2 rounded-full px-2 text-left text-sm outline-none transition-colors focus-visible:ring-accent/50 focus-visible:ring-1",
  {
    variants: {
      selected: {
        true: "bg-active text-fg",
        false: "text-dim hover:bg-hover hover:text-fg",
      },
      size: {
        sm: "h-ctl-sm",
        md: "h-ctl-md",
        lg: "h-ctl-lg",
      },
    },
    defaultVariants: { selected: false, size: "md" },
  },
);

/** A key hint. One spelling, everywhere it appears. */
export function Kbd({ children, className }: { children: React.ReactNode; className?: string }) {
  return (
    <kbd
      className={cn(
        "border-line-strong bg-bg text-dim rounded-mark border px-1 font-mono text-2xs leading-4",
        className,
      )}
    >
      {children}
    </kbd>
  );
}
