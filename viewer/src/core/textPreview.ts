/** One operation in the Markdown CRDT's Unicode-scalar coordinate system. */
export interface TextSplice {
  index: number;
  delete: number;
  insert: string;
  /**
   * Scalar length of the document these offsets were measured against.
   *
   * The World refuses the splice when this disagrees with what it holds. A
   * positional write with no such agreement overwrites whatever now sits at
   * that offset — which is how an editor measuring a re-serialized copy of a
   * document silently corrupted it.
   */
  base_len?: number;
}

/** The smallest contiguous splice between two Markdown strings. Array.from is
 * intentional: JavaScript indexes UTF-16 code units, while the CRDT protocol
 * indexes Unicode scalar values. */
export function textSplice(before: string, after: string): TextSplice | null {
  if (before === after) return null;
  const left = Array.from(before);
  const right = Array.from(after);
  let prefix = 0;
  while (prefix < left.length && prefix < right.length && left[prefix] === right[prefix]) {
    prefix += 1;
  }
  let suffix = 0;
  while (
    suffix < left.length - prefix
    && suffix < right.length - prefix
    && left[left.length - 1 - suffix] === right[right.length - 1 - suffix]
  ) {
    suffix += 1;
  }
  return {
    index: prefix,
    delete: left.length - prefix - suffix,
    insert: right.slice(prefix, right.length - suffix).join(""),
  };
}

/** Apply one scalar-coordinate splice only when it is valid for this text. */
export function applyTextSplice(value: string, splice: TextSplice): string | null {
  const scalars = Array.from(value);
  if (
    splice.index < 0
    || splice.delete < 0
    || splice.index > scalars.length
    || splice.index + splice.delete > scalars.length
  ) return null;
  return [
    ...scalars.slice(0, splice.index),
    splice.insert,
    ...scalars.slice(splice.index + splice.delete),
  ].join("");
}

/** Extend a cumulative base-relative preview when the next transaction only
 * touches its replacement text, the common typing/backspace path. */
export function extendTextSplice(cumulative: TextSplice, next: TextSplice): TextSplice | null {
  const inserted = Array.from(cumulative.insert);
  const relative = next.index - cumulative.index;
  if (relative < 0 || relative + next.delete > inserted.length) return null;
  inserted.splice(relative, next.delete, ...Array.from(next.insert));
  // Spell the TextSplice out instead of spreading. Callers may pass a richer
  // preview object structurally; leaking its old `result` revision into this
  // return value lets a later spread overwrite the revision of the new text.
  return {
    index: cumulative.index,
    delete: cumulative.delete,
    insert: inserted.join(""),
  };
}

/** Keep a lossy preview on the same receiver-known base through a typing run.
 *
 * A local splice may become durable before the peer's normal document stream
 * delivers it. Rebasing the very next preview onto that newly acknowledged
 * text makes the preview temporarily inapplicable at the peer, so its text and
 * caret blink out while the durable stream catches up. The previous preview's
 * result identifies the transaction's actual start document; when those match,
 * extending it preserves the older, already drawable base across acknowledgements.
 */
export function continueTextPreview(
  previous: (TextSplice & { base: string; result: string }) | null,
  previousRevision: string,
  settledRevision: string,
  settled: string,
  current: string,
  next: TextSplice,
): (TextSplice & { base: string }) | null {
  if (previous?.result === previousRevision) {
    const extended = extendTextSplice(previous, next);
    if (extended !== null) return { base: previous.base, ...extended };
  }
  const rebased = textSplice(settled, current);
  return rebased === null ? null : { base: settledRevision, ...rebased };
}

/** A fast 128-bit equality token for exact serialized Markdown revisions.
 * This is not an authority digest; a mismatch hides a lossy preview and the
 * durable CRDT remains the only source of truth. */
export function textRevision(value: string): string {
  let a = 0x811c9dc5;
  let b = 0x9e3779b9;
  let c = 0x85ebca6b;
  let d = 0xc2b2ae35;
  let n = 0;
  for (const scalar of value) {
    const point = scalar.codePointAt(0) ?? 0;
    a = Math.imul(a ^ point, 0x01000193);
    b = Math.imul(b ^ (point + n), 0x27d4eb2d);
    c = Math.imul(c ^ (point >>> 16) ^ n, 0x165667b1);
    d = Math.imul(d ^ point ^ Math.imul(n, 0x9e3779b1), 0x85ebca77);
    n += 1;
  }
  const hex = (lane: number) => (lane >>> 0).toString(16).padStart(8, "0");
  return `${hex(a)}${hex(b)}${hex(c)}${hex(d)}`;
}
