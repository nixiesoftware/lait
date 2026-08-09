import type { EditorState, TransactionSpec } from "@codemirror/state";
import { useLayoutEffect, useRef, useState } from "react";
import {
  Bold,
  Code,
  Heading1,
  Heading2,
  Heading3,
  Italic,
  Link2,
  List,
  ListOrdered,
  ListTodo,
  Quote,
  SquareCode,
  Strikethrough,
  Underline,
} from "lucide-react";

import type { BarAnchor } from "./CodeMirrorEditor";
import {
  BULLET,
  HEADING,
  ORDERED,
  QUOTE,
  TASK,
  blocked,
  insertLink,
  toggleBlock,
  toggleFence,
  toggleWrap,
  wrapped,
} from "./markdownCommands";
import { cn } from "./primitives";

/**
 * The bar that appears over a selection.
 *
 * Every button writes Markdown source — see `markdownCommands.ts` for why that
 * is the only representation available and for the toggles' edge cases.
 *
 * **Nothing here may take focus.** The editor is a live collaborative surface:
 * its selection is the thing the whole bar is about, and it is also broadcast
 * as awareness to everyone else on the issue. A button that focused itself
 * would collapse the range before its own handler ran, and would publish a
 * retracted caret to every other reader. So every control cancels its own
 * `mousedown` and acts from there — no click, no focus, no blur, and the
 * editor's own `focusChanged` commit never fires while you are formatting.
 *
 * That constraint is also why the heading and list groups are spelled out
 * rather than folded into the two dropdowns the design shows: a menu is a
 * focus trap by construction, and three buttons that cannot lose the selection
 * beat one button that can.
 */
/** Breathing room between the bar and the window's edges, and between the bar
 *  and the text it is about. */
const MARGIN = 8;

/**
 * Where the bar actually fits, given how wide it turned out to be.
 *
 * Its width is not knowable in advance — it depends on the icon font and the
 * user's zoom — so the anchor arrives as the selection's box and the bar
 * resolves the rest once it can measure itself. Two corrections:
 *
 * - **Slide, don't overflow.** A selection near either margin would centre the
 *   bar half off-screen. The centre is clamped so both ends stay inside the
 *   window, which detaches the bar from the exact middle of the range — the
 *   right trade, because a control you cannot reach is worse than one that is
 *   not quite centred.
 * - **Flip below when there is no room above.** Selecting the first line of a
 *   description scrolled to the top of the pane leaves nothing overhead, and a
 *   bar rendered there is clipped by the window rather than merely ugly.
 */
export function fit(
  at: Omit<BarAnchor, "state">,
  size: { width: number; height: number } | null,
  viewportWidth: number,
): { left: number; top: number; above: boolean } {
  if (!size) return { left: at.left, top: at.top, above: true };

  const half = size.width / 2;
  // `Math.max` on the upper bound too: in a window narrower than the bar the
  // two limits cross, and the left edge is the one to keep.
  const clamped = Math.min(
    Math.max(at.left + at.hostLeft, MARGIN + half),
    Math.max(MARGIN + half, viewportWidth - MARGIN - half),
  );

  const above = at.top + at.hostTop - size.height - MARGIN >= MARGIN;
  return { left: clamped - at.hostLeft, top: above ? at.top : at.bottom, above };
}

export function SelectionToolbar({
  at,
  state,
  run,
}: {
  at: BarAnchor;
  state: EditorState;
  run: (spec: TransactionSpec) => void;
}) {
  const self = useRef<HTMLDivElement | null>(null);
  const [size, setSize] = useState<{ width: number; height: number } | null>(null);

  // Measured, not assumed, and re-measured whenever the anchor moves: the same
  // bar is a different width once a toggle turns on and an icon's label changes
  // the pressed state's box.
  useLayoutEffect(() => {
    const node = self.current;
    if (!node) return;
    const box = node.getBoundingClientRect();
    setSize((held) =>
      held && held.width === box.width && held.height === box.height
        ? held
        : { width: box.width, height: box.height },
    );
  });

  const placed = fit(at, size, window.innerWidth);
  return (
    <div
      ref={self}
      role="toolbar"
      aria-label="Format selection"
      // `mousedown` is cancelled on the container as well as on each button, so
      // the gaps between them are dead too — a click that lands on 3px of
      // padding must not blur the editor either.
      onMouseDown={(event) => event.preventDefault()}
      className={cn(
        "border-line bg-raised shadow-overlay rounded-surface absolute z-20 flex items-center",
        "gap-0.5 border p-1",
        // Hidden for exactly one frame, while it is being measured at a
        // position that has not been corrected yet.
        size ? "visible" : "invisible",
      )}
      style={{
        top: placed.top,
        left: placed.left,
        transform: placed.above
          ? `translate(-50%, calc(-100% - ${MARGIN}px))`
          : `translate(-50%, ${MARGIN}px)`,
      }}
    >
      <Button
        label="Heading 1"
        icon={<Heading1 className="size-icon-sm" />}
        on={blocked(state, HEADING(1))}
        act={() => run(toggleBlock(state, HEADING(1)))}
      />
      <Button
        label="Heading 2"
        icon={<Heading2 className="size-icon-sm" />}
        on={blocked(state, HEADING(2))}
        act={() => run(toggleBlock(state, HEADING(2)))}
      />
      <Button
        label="Heading 3"
        icon={<Heading3 className="size-icon-sm" />}
        on={blocked(state, HEADING(3))}
        act={() => run(toggleBlock(state, HEADING(3)))}
      />

      <Rule />

      <Button
        label="Bold"
        hint="⌘B"
        icon={<Bold className="size-icon-sm" />}
        on={wrapped(state, "**")}
        act={() => run(toggleWrap(state, "**"))}
      />
      <Button
        label="Italic"
        hint="⌘I"
        icon={<Italic className="size-icon-sm" />}
        on={wrapped(state, "*")}
        act={() => run(toggleWrap(state, "*"))}
      />
      <Button
        label="Strikethrough"
        icon={<Strikethrough className="size-icon-sm" />}
        on={wrapped(state, "~~")}
        act={() => run(toggleWrap(state, "~~"))}
      />
      <Button
        label="Underline"
        hint="⌘U"
        icon={<Underline className="size-icon-sm" />}
        on={wrapped(state, "<u>", "</u>")}
        act={() => run(toggleWrap(state, "<u>", "</u>"))}
      />

      <Rule />

      <Button
        label="Link"
        hint="⌘K"
        icon={<Link2 className="size-icon-sm" />}
        act={() => run(insertLink(state))}
      />
      <Button
        label="Quote"
        icon={<Quote className="size-icon-sm" />}
        on={blocked(state, QUOTE)}
        act={() => run(toggleBlock(state, QUOTE))}
      />
      <Button
        label="Code"
        icon={<Code className="size-icon-sm" />}
        on={wrapped(state, "`")}
        act={() => run(toggleWrap(state, "`"))}
      />
      <Button
        label="Code block"
        icon={<SquareCode className="size-icon-sm" />}
        act={() => run(toggleFence(state))}
      />

      <Rule />

      <Button
        label="Bulleted list"
        icon={<List className="size-icon-sm" />}
        on={blocked(state, BULLET)}
        act={() => run(toggleBlock(state, BULLET))}
      />
      <Button
        label="Numbered list"
        icon={<ListOrdered className="size-icon-sm" />}
        on={blocked(state, ORDERED)}
        act={() => run(toggleBlock(state, ORDERED))}
      />
      <Button
        label="Task list"
        icon={<ListTodo className="size-icon-sm" />}
        on={blocked(state, TASK)}
        act={() => run(toggleBlock(state, TASK))}
      />
    </div>
  );
}

function Rule() {
  return <span className="bg-line mx-1 h-4 w-px shrink-0" aria-hidden="true" />;
}

function Button({
  label,
  hint,
  icon,
  on = false,
  act,
}: {
  label: string;
  hint?: string;
  icon: React.ReactNode;
  on?: boolean;
  act: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={on}
      title={hint ? `${label} (${hint})` : label}
      // Not `onClick`: a click is a mousedown the editor has already seen the
      // blur from. The whole interaction happens before focus can move.
      onMouseDown={(event) => {
        event.preventDefault();
        event.stopPropagation();
        act();
      }}
      className={cn(
        "rounded-mark inline-flex size-ctl-sm shrink-0 items-center justify-center",
        on ? "bg-active text-fg" : "text-dim hover:bg-hover hover:text-fg",
      )}
    >
      {icon}
    </button>
  );
}
