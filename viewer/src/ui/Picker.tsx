import { Popover } from "@astryxdesign/core";
import { useEffect, useRef, useState } from "react";
import { Command } from "cmdk";
import { Check, Plus } from "lucide-react";

import { cmdkFilter } from "../core/fuzzy";
import { cn, controlTrigger, crumbGlyph, menuRow, railGlyph, type ControlSize, type ControlTone } from "./primitives";

/**
 * A pill that opens a searchable menu — the tracker's workhorse control.
 *
 * **One control, not one per field.** Status, priority, assignees, labels, and
 * project are all "pick from a set", and the moment they stop being the same
 * component they start disagreeing: about where the check mark goes, whether the
 * menu closes on pick, whether search exists. That drift is invisible in review and
 * obvious in use, so there is exactly one of these.
 *
 * **The search field is always there**, including over four statuses where it looks
 * like overkill. Two reasons, and the second is the real one:
 *
 * 1. It makes every picker keyboard-complete — `s` `d` `o` `n` `↵` sets Done without
 *    the hand leaving the keys, which is the whole point of this client.
 * 2. `cmdk` drives its list from the input's focus. Hiding the input below some
 *    threshold means the popover focuses its own content instead, and the keydown
 *    never reaches `Command`'s handler — arrow keys silently stop working on
 *    exactly the small menus that looked too simple to break.
 *
 * Astryx's Popover owns focus trapping, escape, outside-click, and collision
 * flipping; `cmdk` owns roving focus, `aria-activedescendant`, and scroll-into-view.
 * The *ranking* stays ours (`cmdkFilter`), so the palette and every picker agree on
 * what "matches" means.
 *
 * `open`/`onOpenChange` are exposed so a keybinding can open a picker — `a` has to
 * reach the assignee menu without a mouse, and a component with private open state
 * could not be driven from the registry.
 */

export interface Option {
  id: string;
  label: string;
  icon?: React.ReactNode;
  swatch?: string;
  /** A muted mono identifier *before* the label — an issue key ahead of its
   *  title, the way every row that names an issue leads with one. */
  kicker?: string;
  /** Secondary text, muted and right-aligned — a key prefix under a petname. */
  hint?: string;
  /** A glyph on the row's trailing edge — a priority mark, a state. The `hint`
   *  is words about the option; this is data the row carries. */
  trailing?: React.ReactNode;
  /** Matched by search but not shown. A member's full key, a label's id. */
  keywords?: string[];
}

type Mode =
  | { multi?: false; value: Option | null; onPick: (id: string) => void }
  | { multi: true; selected: readonly string[]; onToggle: (id: string) => void };

type Props = {
  /** Accessible name, and the trigger's text when nothing is chosen. */
  label: string;
  options: Option[];
  /** Trigger content. Defaults to the single-select face. */
  face?: React.ReactNode;
  disabled?: boolean;
  placeholder?: string;
  className?: string;
  /** Controlled, so a keybinding can open this. Uncontrolled if omitted. */
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
  emptyText?: string;
  /**
   * Make the picker *creatable*: typing a name no option carries offers a
   * "Create" row (Linear's on-the-fly labels). The daemon is the one that
   * actually mints — this only forwards the typed name — so the row appears
   * exactly when the query matches no existing label, not on every keystroke.
   */
  onCreate?: (text: string) => void;
  /**
   * A section label over the options — Linear's menus name what the rows *are*
   * ("Navigation") when the rows are objects rather than values. Omitted, the
   * list stays a bare run, which is right for four statuses.
   */
  heading?: string;
  /**
   * The wider shell. A row carrying a kicker, a title and a trailing glyph is
   * a sentence, and the value-picker's 240px cut every one of them short.
   */
  wide?: boolean;
  /**
   * The shape a colour swatch takes. Round is the default — a label, a status,
   * a member colour. Projects are square everywhere else they are identified
   * (sidebar, project cards, the header crumb), so their pickers say square too:
   * one object, one glyph, whichever surface you meet it on.
   */
  swatchShape?: "dot" | "square";
} & Mode & {
  tone?: ControlTone;
  size?: ControlSize;
  /**
   * Put the swatch in a fixed-width glyph slot, and say how wide.
   *
   * A column of rows needs every label to start at the same offset whatever
   * mark precedes it — that is a layout contract, not a look, which is why it
   * survives as its own prop rather than being inferred from a tone. A bare
   * swatch is `mark-sm` (8px) against neighbours carrying `icon-sm` (14px)
   * glyphs, so without a slot its label sits 6px left of the column and reads
   * as a hanging indent.
   *
   * The WIDTH is the caller's because the slot has to match the glyphs it is
   * lining up with, and those differ by surface: the breadcrumb sets 16px
   * (`md`, what `crumbGlyph` draws), the issue rail 14px (`sm`). Passing the
   * wrong one is a 2px error nobody sees, which is exactly why it should be
   * stated at the call site rather than guessed here.
   */
  swatchSlot?: "sm" | "md";
};

/** The options, under a section label when one is asked for. cmdk's group
 *  hides itself when the query filters every row out, so the heading never
 *  stands over an empty list. */
function MaybeGroup({ heading, children }: { heading?: string | undefined; children: React.ReactNode }) {
  if (!heading) return <>{children}</>;
  return (
    <Command.Group
      heading={heading}
      className="[&_[cmdk-group-heading]]:text-mute [&_[cmdk-group-heading]]:px-2 [&_[cmdk-group-heading]]:pt-1.5 [&_[cmdk-group-heading]]:pb-1 [&_[cmdk-group-heading]]:text-xs"
    >
      {children}
    </Command.Group>
  );
}

export function Combobox(props: Props) {
  const {
    label,
    options,
    face,
    disabled,
    placeholder,
    className,
    open,
    onOpenChange,
    emptyText,
    tone,
    size,
    swatchSlot,
    onCreate,
    heading,
    wide,
    swatchShape,
  } = props;
  const swatch = cn("size-mark-sm shrink-0", swatchShape === "square" ? "rounded-mark" : "rounded-full");

  // Open state is internal *and* overridable. A keybinding needs to force it open;
  // a single-select pick needs to close it. Both have to work, so the component owns
  // a copy and mirrors any controlled value over the top.
  const [internal, setInternal] = useState(false);
  // The live query, held only so the create row can offer what was typed.
  const [query, setQuery] = useState("");
  const isOpen = open ?? internal;
  const setOpen = (o: boolean) => {
    setInternal(o);
    onOpenChange?.(o);
    if (!o) setQuery("");
  };

  /**
   * Which edge the menu hangs from, decided by where the trigger actually is.
   *
   * Astryx's Popover does not flip. Given `alignment="start"` it will place the
   * panel from the trigger's inline start whatever is in the way, and its
   * `max-width: 100%` then squeezes the panel into whatever room is left — so
   * against the window's right edge you get a narrowed sheet flush to the glass
   * with no border on that side.
   *
   * That is the issue rail's every picker, and the last board column's, and any
   * row chip far enough right. It is not a property of those surfaces, so it is
   * not something their call sites should each have to remember: it is a
   * property of *this trigger, right now*, which is what makes it the
   * component's job.
   *
   * Measured on open rather than on render — a rail can be resized and a row can
   * scroll under the pointer between one open and the next — and on `isOpen`
   * rather than inside `setOpen`, because a keybinding can open a picker
   * without going through it.
   */
  const panelWidth = wide ? 320 : 240;
  const triggerRef = useRef<HTMLButtonElement>(null);
  const [alignEnd, setAlignEnd] = useState(false);
  useEffect(() => {
    if (!isOpen) return;
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) return;
    // `8` is a breathing gap, not a magic number: a panel that lands exactly on
    // the viewport edge has no room left to paint the hairline that separates it
    // from the page, which is the visible half of this bug.
    setAlignEnd(rect.left + panelWidth > window.innerWidth - 8);
  }, [isOpen, panelWidth]);

  const single = props.multi !== true ? props.value : null;
  // In a breadcrumb the swatch sits in the trail's shared glyph slot, so a
  // project that is a picker starts its name at the same offset as a project
  // that is a plain crumb. Everywhere else the swatch is just a swatch.
  const triggerSwatch = single?.swatch ? (
    <span className={swatch} style={{ background: single.swatch }} />
  ) : null;
  const content = face ?? (
    <>
      {single?.icon}
      {triggerSwatch &&
        (swatchSlot ? (
          <span className={swatchSlot === "sm" ? railGlyph : crumbGlyph}>{triggerSwatch}</span>
        ) : (
          triggerSwatch
        ))}
      <span className={cn("min-w-0 truncate", !single && "text-mute")}>{single?.label ?? placeholder ?? label}</span>
    </>
  );

  // A read-only space still shows its values — it just cannot open a menu over
  // them. Rendering nothing would hide the data; rendering a dead button would
  // promise something the engine refuses.
  if (disabled) {
    return (
      <span
        className={cn(
          controlTrigger({ tone, size, open: isOpen }),
          "text-dim",
          className,
        )}
      >
        {content}
      </span>
    );
  }

  const isSelected = (id: string) =>
    props.multi === true ? props.selected.includes(id) : props.value?.id === id;

  return (
    <Popover
      isOpen={isOpen}
      onOpenChange={setOpen}
      alignment={alignEnd ? "end" : "start"}
      /**
       * The width belongs to the POPOVER, not to the content inside it.
       *
       * Astryx sizes the panel before our content exists and defaults it to
       * `auto` — which, against a viewport edge, resolves to "whatever room is
       * left" under a `max-width: 100%`. A fixed width stated on the child then
       * has nothing to push back against: it simply overflows the shell and
       * gets clipped.
       *
       * That is why the issue rail's pickers lost their right border. The
       * trigger sits ~200px from the window edge, Astryx sized the panel to
       * 197px, and a 240px menu was drawn inside it — no rounding, no edge, and
       * the panel unable to flip because the number it would need to flip
       * *around* was never given to it.
       *
       * Told the width up front, the positioner can do its job. Every Popover
       * in the app had this the wrong way round; the rail is just where it
       * showed first.
       */
      width={panelWidth}
      // Gated on `isOpen`, unlike every other popover in the app. Astryx renders
      // popover content into the DOM whether or not it is showing — it reveals
      // it through the native popover API rather than by mounting — and a
      // Combobox is the one control an issue row carries five of. Ungated, a
      // list of 200 rows would mount a thousand cmdk instances nobody opened.
      content={
        !isOpen ? null : (
        <div className="overflow-hidden p-0">
          <Command filter={cmdkFilter} loop>
            <Command.Input
              autoFocus
              value={query}
              onValueChange={setQuery}
              placeholder={`${label}…`}
              className="border-line placeholder:text-mute w-full border-b bg-transparent px-3 py-2 text-base outline-none"
            />
            <Command.List className="max-h-overlay-md overflow-y-auto p-1">
              {/* The create row replaces "no matches" when creating is possible:
                  an empty result with a dead end and an empty result with a way
                  forward are different answers. */}
              {!(onCreate && query.trim()) && (
                <Command.Empty className="text-mute px-2 py-3 text-center text-sm">
                  {emptyText ?? "No matches"}
                </Command.Empty>
              )}
              <MaybeGroup heading={heading}>
              {options.map((o) => (
                <Command.Item
                  key={o.id}
                  // Identity is the **id**, not the label: cmdk keys items by
                  // `value`, so two members sharing a petname — or two labels named
                  // the same in different cases — would collapse into one row that
                  // highlights twice. Search still reaches the label through
                  // `keywords`, which `cmdkFilter` scores identically.
                  value={o.id}
                  keywords={[o.label, ...(o.keywords ?? [])]}
                  onSelect={() => {
                    if (props.multi === true) {
                      // Multi stays open: choosing three labels should cost one trip
                      // to the menu, not three.
                      props.onToggle(o.id);
                    } else {
                      props.onPick(o.id);
                      setOpen(false);
                    }
                  }}
                  // `menuRow` — the same row the verb menus draw. See its note
                  // in `primitives.tsx` for why the highlight is a fill rather
                  // than the colour-lift this used to be.
                  className={menuRow}
                >
                  {o.icon}
                  {o.swatch && <span className={swatch} style={{ background: o.swatch }} />}
                  {o.kicker && (
                    <span className="text-mute shrink-0 font-mono text-xs tabular-nums">
                      {o.kicker}
                    </span>
                  )}
                  {/* The check rides directly after the name — Linear's row: the
                      mark belongs to the label it affirms, and the far edge stays
                      the hint's. Nothing needs a reserved slot; the hint is
                      right-aligned, so a check popping in moves nothing else. */}
                  <span className="flex min-w-0 flex-1 items-center gap-2">
                    <span className="min-w-0 truncate">{o.label}</span>
                    {isSelected(o.id) && <Check className="size-icon-xs shrink-0" />}
                  </span>
                  {o.hint && <span className="text-mute shrink-0 font-mono text-2xs">{o.hint}</span>}
                  {o.trailing && <span className="text-mute shrink-0">{o.trailing}</span>}
                </Command.Item>
              ))}
              </MaybeGroup>
              {onCreate &&
                query.trim() &&
                !options.some((o) => o.label.toLowerCase() === query.trim().toLowerCase()) && (
                  <Command.Item
                    // forceMount: this row must survive cmdk's filter — its whole
                    // point is to show when nothing else matches the query.
                    forceMount
                    value={`create:${query.trim()}`}
                    onSelect={() => {
                      onCreate(query.trim());
                      setQuery("");
                      if (props.multi !== true) setOpen(false);
                    }}
                    className={menuRow}
                  >
                    <Plus className="size-icon-xs shrink-0" />
                    <span className="min-w-0 flex-1 truncate">
                      Create “{query.trim()}”
                    </span>
                  </Command.Item>
                )}
            </Command.List>
          </Command>
        </div>
        )
      }
    >
      <button
        ref={triggerRef}
        type="button"
        aria-label={label}
        className={cn(controlTrigger({ tone, size, open: isOpen }), className)}
      >
        {content}
      </button>
    </Popover>
  );
}
