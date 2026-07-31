/**
 * Turning an engine span into one a JavaScript string can be sliced by.
 *
 * The engine counts in **Unicode scalars**, because that is what the
 * collaborative text algebra counts in and what an agent reading the issue as
 * JSON counts in. A JavaScript string indexes **UTF-16 code units**, so every
 * character outside the Basic Multilingual Plane — an emoji, most of CJK
 * Extension B, a musical symbol — is two units and one scalar.
 *
 * Slicing a scalar offset directly is therefore correct until somebody puts an
 * emoji in a description, and then silently wrong for every comment after it in
 * the same field: the highlight slides left by one unit per astral character
 * that precedes it, landing on the wrong words rather than failing.
 *
 * There is no cheap conversion. Both directions walk the string, which is fine
 * for a description and is why this is a function rather than an inline
 * expression somebody copies.
 */

/** A span in UTF-16 code units, ready to slice a JS string with. */
export interface CodeUnitSpan {
  start: number;
  end: number;
}

/**
 * Convert a scalar offset to a code-unit offset.
 *
 * An offset past the end of the string clamps to its length rather than
 * throwing. The engine resolves against the text as *it* holds it, and a client
 * can be a beat behind — a highlight that stops at the end of what is on screen
 * is the honest rendering of that, and an exception in a render path is not.
 */
export function codeUnitOffset(text: string, scalars: number): number {
  if (scalars <= 0) return 0;
  let seen = 0;
  let units = 0;
  for (const character of text) {
    if (seen >= scalars) break;
    units += character.length;
    seen += 1;
  }
  return Math.min(units, text.length);
}

/**
 * Convert a resolved span to one that can slice `text`.
 *
 * Walks once rather than calling `codeUnitOffset` twice, because the two offsets
 * share a prefix and a description is not short.
 */
export function codeUnitSpan(text: string, start: number, end: number): CodeUnitSpan {
  const from = Math.max(0, Math.min(start, end));
  const to = Math.max(start, end);
  let seen = 0;
  let units = 0;
  let startUnits: number | null = from === 0 ? 0 : null;
  let endUnits: number | null = to === 0 ? 0 : null;
  for (const character of text) {
    if (startUnits !== null && endUnits !== null) break;
    units += character.length;
    seen += 1;
    if (startUnits === null && seen >= from) startUnits = units;
    if (endUnits === null && seen >= to) endUnits = units;
  }
  return {
    start: Math.min(startUnits ?? text.length, text.length),
    end: Math.min(endUnits ?? text.length, text.length),
  };
}
