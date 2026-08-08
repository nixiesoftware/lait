/**
 * Where a click on the rendered document lands in the source that made it.
 *
 * The description is read as prose and written as Markdown, and those are two
 * different strings: `## Scope` renders as `Scope`, `**bold**` as `bold`, and a
 * lone newline inside a paragraph renders as a space (see `joinSoft`). So a
 * caret placed by eye in the rendered text has no direct index into the source
 * — which is the one thing click-to-edit has to get right, because a caret that
 * jumps when you click is worse than not being able to click at all.
 *
 * The approach is a single left-to-right walk of one block's source, counting
 * the characters that survive into the render. When the count reaches the
 * rendered offset, the source cursor is the answer. It is exact for prose —
 * which is nearly all of it — and lands inside the right word wherever markup
 * is involved, because the skipped runs are short and the error can never
 * exceed the length of the punctuation being skipped.
 *
 * Deliberately NOT a second parser. Reusing `parseMarkdown` here would mean
 * threading offsets through every inline rule and changing the AST that four
 * other surfaces read; this needs to know only *how many characters vanish*,
 * which is a much smaller question than *what they meant*.
 */

/** Fence, so a code block's body is copied through verbatim. */
const FENCE = /^\s*(?:```|~~~)/;

/** Leading block punctuation: heading hashes, quote markers, list bullets and
 *  their checkbox, table pipes. Everything here is dropped before the render. */
const LEADER = /^(\s*)(#{1,4}\s+|>\s?|(?:[-*+]|\d+[.)])\s+(?:\[[ xX]\]\s+)?)/;

/** Inline punctuation that renders as nothing. Ordered longest-first so `**`
 *  is consumed before `*` — the same precedence the parser gives them. */
const MARKS = ["**", "__", "~~", "*", "_", "`"];

/**
 * The source offset for `rendered` characters into `source`.
 *
 * `source` is one block's slice, `rendered` an offset into that block's visible
 * text. The result is an offset into `source`, clamped to its bounds.
 */
export function sourceOffsetAt(source: string, rendered: number): number {
  const want = Math.max(0, rendered);
  let seen = 0;
  let at = 0;
  let lineStart = true;

  /**
   * Advance past everything at `at` that renders as nothing.
   *
   * A loop, because these stack: `- **bold` is a bullet then a mark, and the
   * caret belongs after both. It runs BEFORE the offset is checked, which is
   * the whole trick — asking "have we counted enough?" while still standing on
   * a `*` would put the caret in front of the punctuation the reader cannot
   * see, and clicking the first letter of a bold word would land outside it.
   */
  const skipInvisible = () => {
    for (;;) {
      if (lineStart) {
        const leader = LEADER.exec(source.slice(at));
        lineStart = false;
        if (leader && leader[0].length > 0) {
          at += leader[0].length;
          continue;
        }
      }
      // `](href)` — a link's target, which renders as nothing. Its label is
      // ordinary text, so only the brackets and the tail are stepped over.
      if (source.startsWith("](", at)) {
        const close = source.indexOf(")", at);
        if (close !== -1) {
          at = close + 1;
          continue;
        }
      }
      if (source[at] === "[" && source.indexOf("](", at) !== -1) {
        at += 1;
        continue;
      }
      const mark = MARKS.find((m) => source.startsWith(m, at));
      if (mark) {
        at += mark.length;
        continue;
      }
      return;
    }
  };

  while (at < source.length) {
    // A fenced block renders verbatim: nothing inside it is markup, so the
    // walk hands back offsets one for one and a `*` or a `#` in the code is
    // just a character.
    if (lineStart && FENCE.test(source.slice(at))) {
      const close = source.indexOf("\n", at);
      const line = (close === -1 ? source.length : close) - at;
      if (seen + line >= want) return at + (want - seen);
      seen += line;
      at += line;
      lineStart = false;
      continue;
    }

    skipInvisible();
    if (at >= source.length) break;
    if (seen >= want) return at;

    if (source[at] === "\n") {
      // `joinSoft` turns this into one space, so it costs one rendered
      // character. A hard break (two trailing spaces, or a backslash) also
      // renders as exactly one character — same arithmetic, different glyph.
      seen += 1;
      at += 1;
      lineStart = true;
      continue;
    }

    seen += 1;
    at += 1;
  }
  return at;
}

/**
 * The source offset for a point in the rendered DOM, or `null` if the point is
 * not inside a block that knows where it came from.
 *
 * `root` is the rendered container; blocks carry `data-md-from` (see
 * `Markdown.tsx`). The rendered offset is measured with a Range rather than by
 * summing text nodes, because a Range counts exactly what the browser shows —
 * including text inside nested `<strong>`/`<code>` — and is what the caret
 * APIs already speak.
 */
export function sourceOffsetFromPoint(
  root: HTMLElement,
  source: string,
  x: number,
  y: number,
): number | null {
  const caret = caretAt(x, y);
  if (!caret || !root.contains(caret.node)) return null;
  const block = (caret.node.nodeType === 1 ? (caret.node as Element) : caret.node.parentElement)
    ?.closest<HTMLElement>("[data-md-from]");
  if (!block) return null;
  const from = Number(block.dataset.mdFrom);
  const to = Number(block.dataset.mdTo);
  if (!Number.isFinite(from) || !Number.isFinite(to)) return null;

  const range = document.createRange();
  range.setStart(block, 0);
  try {
    range.setEnd(caret.node, caret.offset);
  } catch {
    return from;
  }
  return from + sourceOffsetAt(source.slice(from, to), range.toString().length);
}

/** `caretPositionFromPoint` is the standard; WebKit only ships the older
 *  `caretRangeFromPoint`. Both answer the same question. */
function caretAt(x: number, y: number): { node: Node; offset: number } | null {
  const modern = (
    document as Document & {
      caretPositionFromPoint?: (x: number, y: number) => { offsetNode: Node; offset: number } | null;
    }
  ).caretPositionFromPoint?.(x, y);
  if (modern) return { node: modern.offsetNode, offset: modern.offset };
  const legacy = (
    document as Document & { caretRangeFromPoint?: (x: number, y: number) => Range | null }
  ).caretRangeFromPoint?.(x, y);
  return legacy ? { node: legacy.startContainer, offset: legacy.startOffset } : null;
}
