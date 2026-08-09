import { EditorSelection, type EditorState, type TransactionSpec } from "@codemirror/state";

/**
 * What the selection toolbar does to the document.
 *
 * Kept apart from the toolbar that calls them, and from the editor they run in,
 * because these are the part with the edge cases: what happens when you bold
 * something already bold, when a selection spans a heading and a list, when it
 * ends on a line's newline. A button is a click; this is the behaviour, and it
 * is tested without a DOM.
 *
 * Everything here returns a `TransactionSpec` rather than dispatching, so the
 * caller decides when it happens — and so a test can assert the resulting
 * document without an `EditorView`.
 *
 * THE INVARIANT. Every command writes Markdown *source*. The stored bytes are
 * the source of truth, the CRDT splices over them, and agents read and write
 * the same string over MCP — so a toolbar that produced any other
 * representation would be inventing a second document nobody else can read.
 */

/**
 * The block markers that are mutually exclusive: a line is a heading or a quote
 * or a list item, and pressing one while another holds replaces it.
 *
 * The checkbox is part of the bullet, not a thing after it. Matching `- ` alone
 * on `- [ ] a` reports the line as a plain bullet, so the task button sees no
 * marker of its own and adds a second one — `- [ ] [ ] a`.
 */
const BLOCK_PREFIX = /^\s*(#{1,6} +|> ?|[-*+] +(\[[ xX]\] +)?|\d+[.)] +)/;

/**
 * Where the selection's wrapper is, if it has one.
 *
 * Two places to look, and the second is the one every naive implementation
 * misses: the markers can be *outside* the selection. Double-clicking a word
 * inside `**bold**` selects `bold`, not `**bold**`, so un-bolding has to look
 * at the characters either side of the range — otherwise the button only ever
 * adds, and a second press gives `****bold****`.
 *
 * THE RUN CHECK. A marker only counts if the run of marker characters is
 * exactly this marker and no longer. Emphasis and strong emphasis are the same
 * character repeated, so without this every bold span reads as italic too —
 * both buttons light up on `**bold**`, and pressing the lit one strips a single
 * star and leaves `*bold*`. The neighbour on the far side of each marker
 * settles it: another `*` there means the run is longer than `*`, so this is
 * somebody else's marker.
 */
function located(state: EditorState, open: string, close: string): "inside" | "outside" | null {
  const { from, to } = state.selection.main;
  const text = state.sliceDoc(from, to);
  const lead = open[0]!;
  const trail = close[close.length - 1]!;

  if (text.length >= open.length + close.length && text.startsWith(open) && text.endsWith(close)) {
    const afterOpen = text[open.length];
    const beforeClose = text[text.length - close.length - 1];
    if (afterOpen !== lead && beforeClose !== trail) return "inside";
  }

  if (
    from >= open.length &&
    state.sliceDoc(from - open.length, from) === open &&
    state.sliceDoc(to, Math.min(state.doc.length, to + close.length)) === close
  ) {
    const outerBefore = from - open.length - 1;
    const outerAfter = to + close.length;
    const before = outerBefore >= 0 ? state.sliceDoc(outerBefore, outerBefore + 1) : "";
    const after =
      outerAfter < state.doc.length ? state.sliceDoc(outerAfter, outerAfter + 1) : "";
    if (before !== lead && after !== trail) return "outside";
  }

  return null;
}

/** Toggle an inline wrapper around the selection. See `located` for the cases. */
export function toggleWrap(state: EditorState, open: string, close = open): TransactionSpec {
  const { from, to } = state.selection.main;
  const text = state.sliceDoc(from, to);

  switch (located(state, open, close)) {
    case "inside": {
      const inner = text.slice(open.length, text.length - close.length);
      return {
        changes: { from, to, insert: inner },
        selection: EditorSelection.range(from, from + inner.length),
      };
    }
    case "outside":
      return {
        changes: [
          { from: from - open.length, to, insert: text },
          { from: to, to: to + close.length, insert: "" },
        ],
        selection: EditorSelection.range(from - open.length, to - open.length),
      };
    default:
      return {
        changes: { from, to, insert: open + text + close },
        selection: EditorSelection.range(from + open.length, to + open.length),
      };
  }
}

/**
 * Toggle a per-line block marker across every line the selection touches.
 *
 * `marker` takes the line's index within the selection so an ordered list can
 * number itself. Pressing a marker every line already carries removes it;
 * otherwise every line gets it, and whatever *other* block marker was there is
 * replaced rather than stacked — `> # a` is not a thing anyone means to write.
 */
export function toggleBlock(
  state: EditorState,
  marker: (index: number) => string,
): TransactionSpec {
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from).number;
  // A selection that ends exactly at a line start has not touched that line;
  // dragging down through three lines otherwise marks a fourth.
  const endLine = state.doc.lineAt(range.to);
  const last = endLine.from === range.to && endLine.number > first ? endLine.number - 1 : endLine.number;

  let held = true;
  for (let n = first; n <= last; n += 1) {
    const line = state.doc.line(n);
    if ((BLOCK_PREFIX.exec(line.text)?.[0] ?? "") !== marker(n - first)) held = false;
  }

  const changes = [];
  for (let n = first; n <= last; n += 1) {
    const line = state.doc.line(n);
    const existing = BLOCK_PREFIX.exec(line.text)?.[0] ?? "";
    changes.push({
      from: line.from,
      to: line.from + existing.length,
      insert: held ? "" : marker(n - first),
    });
  }
  return { changes };
}

export const HEADING = (level: number) => (): string => `${"#".repeat(level)} `;
export const QUOTE = (): string => "> ";
export const BULLET = (): string => "- ";
export const ORDERED = (index: number): string => `${index + 1}. `;
export const TASK = (): string => "- [ ] ";

/**
 * Fence the selection as a code block.
 *
 * Whole lines, always. A fence is a block construct and half a line inside one
 * is not valid Markdown — so the range is widened to the lines it touches
 * before the fences go on, which is also what makes the un-fence check simple.
 */
export function toggleFence(state: EditorState): TransactionSpec {
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from);
  const last = state.doc.lineAt(range.to);

  const above = first.number > 1 ? state.doc.line(first.number - 1) : null;
  const below = last.number < state.doc.lines ? state.doc.line(last.number + 1) : null;
  if (above?.text.startsWith("```") && below?.text.startsWith("```")) {
    return {
      changes: [
        { from: above.from, to: first.from, insert: "" },
        { from: last.to, to: below.to, insert: "" },
      ],
    };
  }

  return {
    changes: [
      { from: first.from, to: first.from, insert: "```\n" },
      { from: last.to, to: last.to, insert: "\n```" },
    ],
    selection: EditorSelection.cursor(first.from + 3),
  };
}

/**
 * Link the selection, with the target left selected.
 *
 * No prompt. A modal over a live collaborative editor has to take focus, and
 * taking focus collapses the selection the link is *about* — so the URL goes in
 * as a selected placeholder and the next keystroke replaces it, which is one
 * fewer surface and one fewer way to lose the range.
 */
export function insertLink(state: EditorState, placeholder = "url"): TransactionSpec {
  const { from, to } = state.selection.main;
  const text = state.sliceDoc(from, to);
  const insert = `[${text}](${placeholder})`;
  const target = from + text.length + 3;
  return {
    changes: { from, to, insert },
    selection: EditorSelection.range(target, target + placeholder.length),
  };
}

/** Whether a mark is already on the selection, for the pressed state. Exactly
 *  the test `toggleWrap` acts on, so a lit button always un-marks. */
export function wrapped(state: EditorState, open: string, close = open): boolean {
  return located(state, open, close) !== null;
}

/** Whether every line the selection touches already carries this marker. */
export function blocked(state: EditorState, marker: (index: number) => string): boolean {
  const range = state.selection.main;
  const first = state.doc.lineAt(range.from).number;
  const endLine = state.doc.lineAt(range.to);
  const last = endLine.from === range.to && endLine.number > first ? endLine.number - 1 : endLine.number;
  for (let n = first; n <= last; n += 1) {
    const line = state.doc.line(n);
    if ((BLOCK_PREFIX.exec(line.text)?.[0] ?? "") !== marker(n - first)) return false;
  }
  return true;
}
