import { cva, type VariantProps } from "class-variance-authority";
import { clsx, type ClassValue } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";
import * as CheckboxPrimitive from "@radix-ui/react-checkbox";
import * as Popover from "@radix-ui/react-popover";
import * as SwitchPrimitive from "@radix-ui/react-switch";
import * as Tooltip from "@radix-ui/react-tooltip";
import { Check, LoaderCircle } from "lucide-react";

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

const button = cva(
  // Shared: the parts that are true of every button, including the focus ring,
  // which is not optional in a keyboard-first app. `transition` (not just
  // `transition-colors`) so the `active:` press scales back smoothly on release;
  // the scale is a transform, so it costs no layout.
  "inline-flex shrink-0 select-none items-center justify-center gap-1.5 font-medium transition-colors disabled:pointer-events-none disabled:opacity-45",
  {
    variants: {
      variant: {
        /** The default. Invisible until hovered — for chrome. */
        ghost: "text-mute hover:bg-hover hover:text-fg",
        /** A visible affordance without shouting. */
        outline: "border-line bg-bg hover:border-line-strong hover:bg-hover text-fg border",
        /** Exactly one per screen, at most. A neutral inverse commit keeps blue
         *  available for focus and state instead of making every save look like
         *  a Jira call-to-action. */
        primary: "bg-fg text-bg hover:bg-fg/85",
        /** A quiet destructive affordance — the inline "X" that only reddens on
         *  hover. For the button that actually confirms a destroy, use
         *  `destructive`. */
        danger: "text-mute hover:bg-danger/10 hover:text-danger",
        /** The filled destructive commit — the confirm button in a delete dialog.
         *  White-on-danger clears AA (see the palette note). Replaces the old
         *  `primary` + `bg-danger` override that every call site had to remember. */
        destructive: "bg-danger text-accent-fg hover:bg-danger/85",
        /** Selected state in a segmented group. Bordered, because the thing
         *  it alternates with is `outline`: a state change should move the
         *  fill, not the silhouette. Without the border a selected tab was a
         *  pixel narrower than its neighbours and the row shifted as you
         *  moved between them. */
        active: "border-line-strong bg-active text-fg border",
        /** A named action inside dense chrome. Unlike `primary`, this sits beside
         * icon buttons without turning the toolbar into a callout banner. */
        toolbar:
          "border-line bg-raised text-dim hover:border-line-strong hover:bg-hover hover:text-fg border",
        /** Text action embedded in prose or metadata. It never grows a capsule. */
        inline: "text-dim hover:text-fg hover:underline underline-offset-2",
        /** A circle on the floating bulk pill — the same chip treatment
         *  `controlTrigger`'s `pill` gives the pickers beside it, so the icon
         *  buttons and the menus read as one set of controls rather than two
         *  kinds of thing sharing a bar. Fill answers the pointer, not
         *  elevation: the bar is already the thing that is lifted, and a control
         *  that lifts again inside it claims a second plane that is not there. */
        pill: "bg-active/60 text-dim hover:bg-hover hover:text-fg rounded-full",
      },
      size: {
        /** Icon-only chrome: a 24px circle, the toolbar unit. Fully rounded for
         *  the same reason the property rail is — the only shape these carry is a
         *  hover fill, so a circle reads as deliberate where a rounded square
         *  reads as a box that appeared under the pointer. */
        icon: "size-ctl-sm rounded-full",
        /** Pills, not boxes. A button carries no border of its own in the
         *  common variants, so its shape is whatever the fill describes — and a
         *  row of buttons has to agree: a ghost "Cancel" beside a primary "Save"
         *  cannot be a pill next to a box. Shape therefore rides on size, which
         *  every variant passes through, rather than on the ones that happen to
         *  be quiet. */
        sm: "h-ctl-sm rounded-full px-2 text-sm",
        md: "h-ctl-md rounded-full px-2 text-sm",
        /** No capsule at all — this is a text action inside prose. */
        inline: "h-auto p-0 text-xs",
      },
    },
    defaultVariants: { variant: "ghost", size: "sm" },
  },
);

export type ButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> &
  VariantProps<typeof button> & {
    /**
     * Show a spinner and go inert. Callers used to hand-roll this — a manual
     * `aria-busy`, a disabled toggle, and a text swap to "Saving…" — and each did
     * two of the three. `loading` does all three from one flag: the spinner leads,
     * the button disables so a second click can't fire, and `aria-busy` tells a
     * screen reader why. The label stays the caller's to change (or not).
     */
    loading?: boolean;
  };

export function Button({ className, variant, size, loading, disabled, children, ...rest }: ButtonProps) {
  return (
    <button
      className={cn(button({ variant, size }), className)}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      {...rest}
    >
      {loading && <LoaderCircle className="size-icon-sm animate-spin" aria-hidden />}
      {children}
    </button>
  );
}

export function InlineAction(props: Omit<ButtonProps, "variant" | "size">) {
  return <Button variant="inline" size="inline" {...props} />;
}

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
    <Tooltip.Root>
      <Tooltip.Trigger asChild>
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
      </Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content
          sideOffset={OverlayGap.tip}
          style={{ transformOrigin: "var(--radix-tooltip-content-transform-origin)" }}
          className="ui-surface border-line-strong bg-raised shadow-overlay z-50 flex flex-col gap-1 rounded-control border px-2 py-1.5 text-xs"
        >
          {names.map((name) => (
            <span key={name} className="flex items-center gap-1.5">
              <span
                className="size-mark-xs shrink-0 rounded-full"
                style={{ background: catalogColor(colorOf(name)) }}
              />
              <span className="capitalize">{name}</span>
            </span>
          ))}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
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

const field = cva(
  "border-line bg-[var(--field-bg)] placeholder:text-mute w-full rounded-control border text-sm outline-none transition-colors focus:border-line-strong focus:ring-1 focus:ring-line-strong/30 disabled:cursor-not-allowed disabled:opacity-50 aria-invalid:border-danger aria-invalid:focus:ring-danger/20",
  {
    variants: {
      size: {
        sm: "h-ctl-md px-2",
        md: "h-ctl-lg px-2.5",
      },
    },
    defaultVariants: { size: "md" },
  },
);

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
  // emitted both classes and left the winner to CSS source order.
  "inline-flex items-center gap-1.5 text-sm outline-none transition-colors disabled:pointer-events-none disabled:opacity-45 data-[state=open]:bg-active",
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
        outline:
          "border-line bg-bg hover:border-line-strong hover:bg-hover rounded-control border px-2",
        /** Inside a floating bar. It lifts on hover: the bar is the one surface
         *  that is over the work rather than part of it, so its controls answer
         *  the pointer with elevation instead of only a fill. */
        pill: "bg-active/60 text-dim hover:bg-hover hover:text-fg rounded-full px-2.5",
        /** No box at all — the child already is one. Wrapping a label chip in a
         *  second shape would put a rectangle around a pill, so hover dims
         *  rather than fills and nothing shifts when it opens. Pair with
         *  `size="none"`: the chip carries its own height. */
        bare: "min-w-0 rounded-full transition-opacity hover:opacity-75 data-[state=open]:opacity-75",
      },
    },
    defaultVariants: { tone: "outline", size: "md" },
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

export type InputProps = Omit<React.InputHTMLAttributes<HTMLInputElement>, "size"> &
  VariantProps<typeof field>;

/** The single-line field recipe. Validation is driven by `aria-invalid`, so
 * callers do not need to rebuild border and focus states for each form. */
export function Input({ className, size, ...props }: InputProps) {
  return <input className={cn(field({ size }), className)} {...props} />;
}

export type TextareaProps = React.TextareaHTMLAttributes<HTMLTextAreaElement> & {
  resize?: "none" | "vertical";
};

/** Multi-line counterpart to `Input`; it shares the same surface, radius,
 * validation and focus language without forcing a fixed height. */
export function Textarea({ className, resize = "vertical", ...props }: TextareaProps) {
  return (
    <textarea
      className={cn(
        field(),
        "h-auto min-h-16 px-2.5 py-2",
        resize === "vertical" ? "resize-y" : "resize-none",
        className,
      )}
      {...props}
    />
  );
}

/** Compact form caption used by settings and dialogs. Sentence case keeps form
 * hierarchy quieter than the old all-caps Jira-like labels. */
export function FieldLabel({
  children,
  className,
  ...props
}: React.LabelHTMLAttributes<HTMLLabelElement>) {
  return (
    <label className={cn("text-dim flex flex-col gap-1.5 text-sm", className)} {...props}>
      {children}
    </label>
  );
}

/**
 * Prose you can type into.
 *
 * This used to be a hover target: the whole body lit up with a tinted panel and
 * grew a pencil in the corner, so reading an issue meant a rectangle following
 * your pointer down the page. The affordance was the wrong size — an issue body
 * is the largest thing on the surface, and treating it as one big button made
 * the page feel like a form.
 *
 * Now the text simply takes a caret. Clicking puts you in the editor at the
 * point you clicked; nothing highlights beforehand, because `cursor-text` has
 * already said everything a hover panel was trying to.
 *
 * The keyboard route stays explicit. A `div` that opens an editor on click is
 * fine for a pointer and invisible to everything else, so there is still a real
 * button — it is just `sr-only` until focused, rather than painted over the
 * prose for everyone.
 */
export function EditableSurface({
  children,
  onEdit,
  label = "Edit",
  className,
}: {
  children: React.ReactNode;
  /** `offset` is a best-effort caret position in the *source* text. */
  onEdit: (offset?: number) => void;
  label?: string;
  className?: string;
}) {
  return (
    <div
      className={cn("relative cursor-text", className)}
      onClick={(event) => {
        // Links, checkboxes and the code block's copy button keep their own
        // behaviour — clicking a link should follow it, not start an edit.
        if ((event.target as HTMLElement).closest("a, button, input, select, textarea")) return;
        onEdit(caretOffsetFromPoint(event.clientX, event.clientY));
      }}
    >
      {children}
      <button
        type="button"
        onClick={(event) => {
          event.stopPropagation();
          onEdit();
        }}
        className="border-line bg-raised focus:ring-accent/50 sr-only rounded-control border px-2 py-1 text-sm focus:not-sr-only focus:absolute focus:top-0 focus:right-0 focus:z-10 focus:ring-1"
      >
        {label}
      </button>
    </div>
  );
}

/**
 * Where in the rendered text the pointer landed, as a character count within
 * that text node.
 *
 * Rendered prose and its Markdown source are different strings — `**bold**` is
 * six characters longer on the way in — so this cannot be an exact mapping, and
 * the caller reconciles it against the source. `undefined` means "no idea",
 * which the caller reads as "put the caret at the end".
 */
function caretOffsetFromPoint(x: number, y: number): number | undefined {
  const doc = document as Document & {
    caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
  };
  const pos = doc.caretPositionFromPoint?.(x, y);
  if (pos?.offsetNode.nodeType === Node.TEXT_NODE) {
    return sourceOffset(pos.offsetNode.textContent ?? "", pos.offset);
  }
  const range = doc.caretRangeFromPoint?.(x, y);
  if (range?.startContainer.nodeType === Node.TEXT_NODE) {
    return sourceOffset(range.startContainer.textContent ?? "", range.startOffset);
  }
  return undefined;
}

/** Packed so the caller can find the clicked run in the source itself. */
const CARET_RUNS = new Map<number, string>();
let caretSeq = 0;

function sourceOffset(text: string, offset: number): number {
  const key = caretSeq++;
  CARET_RUNS.set(key, text.slice(0, offset));
  // Only the most recent click can be pending; anything older is noise.
  if (CARET_RUNS.size > 4) CARET_RUNS.delete(key - 4);
  return key;
}

/**
 * Turn a token from `EditableSurface` into a real offset in `source`.
 *
 * The rendered run is searched for in the source. A paragraph's words survive
 * rendering unchanged, so this lands on the right line far more often than not;
 * when the run is absent (it was inside a table cell, say) the answer is the end
 * of the text, which is where a caret with nothing better to do belongs.
 */
export function resolveCaret(token: number | undefined, source: string): number {
  if (token === undefined) return source.length;
  const run = CARET_RUNS.get(token);
  CARET_RUNS.delete(token);
  if (!run) return source.length;
  const tail = run.slice(-24);
  if (!tail.trim()) return source.length;
  const at = source.indexOf(tail);
  return at === -1 ? source.length : at + tail.length;
}

/**
 * An icon button with a tooltip carrying its shortcut.
 *
 * The label is required and does double duty: `aria-label` for anyone not looking
 * at it, and the tooltip for anyone who is. An icon-only control without a label
 * is a puzzle; `title` alone is one that screen readers read inconsistently and
 * that never mentions the key.
 */
export function IconButton({
  label,
  chord,
  className,
  variant,
  children,
  ...rest
}: Omit<ButtonProps, "size"> & { label: string; chord?: string }) {
  return (
    <Tooltip.Root>
      <Tooltip.Trigger asChild>
        <button
          aria-label={label}
          className={cn(button({ variant, size: "icon" }), className)}
          {...rest}
        >
          {children}
        </button>
      </Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content
          sideOffset={OverlayGap.tip}
          style={{ transformOrigin: "var(--radix-tooltip-content-transform-origin)" }}
          // `rounded-control`, not `rounded-surface`. A tooltip is a floating
          // label at control scale, not a panel: at 26px tall the 12px surface
          // corner is essentially half the height, which stops being a corner
          // and renders as a pill. The surface rung is for things with enough
          // body to carry it.
          className="ui-surface border-line-strong bg-raised shadow-overlay z-50 flex items-center gap-1.5 rounded-control border px-2 py-1 text-xs"
        >
          {label}
          {chord && <Kbd>{chord}</Kbd>}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  );
}

/**
 * A checkbox that belongs to the theme.
 *
 * The native control was the only form element still rendering as the OS drew it —
 * a blue-by-default box that ignores `--color-accent` and looks pasted-in beside
 * everything else. Radix hands us the box's behaviour (focus, space-to-toggle, the
 * indeterminate state) with no appearance, so the appearance is ours: the accent
 * fill and the check we use everywhere a boolean is set.
 */
export function Checkbox({
  className,
  ...props
}: React.ComponentProps<typeof CheckboxPrimitive.Root>) {
  return (
    <CheckboxPrimitive.Root
      className={cn(
        // 14px, matching the status glyph it shares a row with. At 16px it was
        // the largest mark on a list line — heavier than the status circle and
        // the priority bars either side of it — which put the most emphasis on
        // the one control that is only there when you are selecting.
        "border-line-strong bg-bg text-accent-fg data-[state=checked]:bg-accent data-[state=checked]:border-accent data-[state=indeterminate]:bg-accent data-[state=indeterminate]:border-accent flex size-icon-sm shrink-0 items-center justify-center rounded-mark border transition-colors disabled:opacity-50",
        className,
      )}
      {...props}
    >
      <CheckboxPrimitive.Indicator>
        <Check className="size-icon-2xs" strokeWidth={3} />
      </CheckboxPrimitive.Indicator>
    </CheckboxPrimitive.Root>
  );
}

/**
 * A switch, for a setting that takes effect the instant you flip it.
 *
 * A checkbox says "this will be true when you submit"; a switch says "this is on
 * now." That is the only reason to prefer one — so the switch is for the live
 * toggles (a preference, a "create another"), never for a form you still have to
 * confirm. Same accent, so on-ness reads the same as a checked box.
 */
export function Switch({ className, ...props }: React.ComponentProps<typeof SwitchPrimitive.Root>) {
  return (
    <SwitchPrimitive.Root
      className={cn(
        "border-line-strong bg-active data-[state=checked]:bg-accent data-[state=checked]:border-accent relative inline-flex h-4 w-7 shrink-0 items-center rounded-full border transition-colors disabled:opacity-50",
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb className="bg-fg data-[state=checked]:bg-accent-fg pointer-events-none block size-icon-xs translate-x-0.5 rounded-full transition-transform data-[state=checked]:translate-x-[13px]" />
    </SwitchPrimitive.Root>
  );
}

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

/**
 * The floating shell every Popover shares — the counterpart to `MenuContent`.
 *
 * There was a `MenuContent` for dropdowns but no equivalent for popovers, so the
 * shell string (`border-line-strong bg-raised shadow-overlay z-50 rounded-surface
 * border`) was hand-copied into every picker, display panel, and status popover —
 * and it had already drifted (the inbox popover reached for a `bg-overlay` token
 * that doesn't exist, so it rendered with no fill). One component ends that: the
 * chrome is decided here, callers pass only what differs (width, padding, align).
 *
 * It owns the `Portal` too, so a caller writes `<PopoverContent>` rather than
 * `<Popover.Portal><Popover.Content …>` — one less nesting to get wrong, and the
 * portal is not an opinion any single popover should be re-making.
 *
 * `ui-surface` gives the entrance the modal surfaces already had; the
 * transform-origin is pinned to Radix's computed anchor so the scale grows from the
 * trigger's edge instead of the popover's center.
 */
export function PopoverContent({
  className,
  sideOffset = OverlayGap.panel,
  style,
  ...props
}: React.ComponentProps<typeof Popover.Content>) {
  return (
    <Popover.Portal>
      <Popover.Content
        sideOffset={sideOffset}
        style={{ transformOrigin: "var(--radix-popover-content-transform-origin)", ...style }}
        className={cn(
          "ui-surface border-line-strong bg-raised shadow-overlay z-50 rounded-surface border outline-none",
          className,
        )}
        {...props}
      />
    </Popover.Portal>
  );
}

/** Tooltips need one provider; delay is short because these are chrome hints,
 *  not explanations you should have to wait for. */
export function TooltipProvider({ children }: { children: React.ReactNode }) {
  return (
    <Tooltip.Provider delayDuration={400} skipDelayDuration={200}>
      {children}
    </Tooltip.Provider>
  );
}
