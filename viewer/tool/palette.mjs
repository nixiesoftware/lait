/**
 * The palette's source of truth: seeds, curves, and the OKLCH helpers that
 * turn them into colour.
 *
 * WHY THIS IS ITS OWN MODULE. It used to live inside `generate-tokens.mjs`,
 * which was fine while that file was the only consumer. It is not any more:
 * `generate-astryx-theme.mjs` maps the same roles onto Astryx's token
 * vocabulary. Two generators reading two copies of the curves is how a palette
 * splits in half without anyone noticing — so the curves live here and the
 * generators are only ever *projections* of them.
 *
 * Nothing in this file writes anything. Adding an output means adding a
 * generator, not editing this.
 */

// ── Seeds: a hue, and nothing else ───────────────────────────────────────
// Chroma is a curve, not a property of the seed. Only the angle is chosen here.
export const HUE = {
  accent: 264,
  danger: 22,
  ok: 147,
  warn: 78,
  neutral: 286,
};

// ── The neutral ramp ─────────────────────────────────────────────────────
// Read off the palette these replace — except the light surface rungs, which
// were deliberately re-rung: light mode used to put pure white on `bg` and a
// grey on `raised`, so a card rendered *darker* than the canvas it sat on,
// the opposite of dark mode and of every elevation model (Linear's included).
// Now both themes climb the same way — sunken < bg < raised — so "raised"
// means lighter-than-the-page everywhere: a grey canvas, white cards on it.
// Chroma rises toward the middle: a near-black and a near-white cannot carry
// tint, the steps between them can, and a little of it is what stops a grey
// looking dead.
// `bright` is the rung past `fg` — the voice of the one row a menu is pointing
// at. Highlight-by-colour (no fill) needs somewhere brighter than the resting
// text to go: pure white in dark, and the same distance past `fg` toward black
// in light. Both ends shed their chroma, as the extremes always do here.
// DARK'S PAGE SITS WHERE THE BOARD CANVAS USED TO.
//
// The board drew its canvas on `raised` (0.1881) while every other view drew on
// `bg` (0.1505), and the board was the one that looked right — a page that dark
// is not a surface, it is an absence, and every panel on it had to fight to
// prove it was there. So `bg` takes 0.1881 and the rungs above it move up to
// keep their spacing.
//
// The STEPS are preserved, not the values: sunken sits a short step under the
// page as before, `raised` keeps a lift so a card still reads as a card, and
// hover/active/line climb with them. `raised` is 0.040 over the page —
// deliberately a hair MORE than the 0.038 it had before, because the first
// attempt at this lift left only 0.027 and made floating panels harder to see,
// which was the opposite of the point. Text rungs do NOT move — lifting the
// surfaces already costs a little contrast, and lifting the ink with them would
// spend the rest.
//
// The board itself is pinned to `bg` at the call site now (`Board.tsx`), so the
// surface that was pointed at is the surface that shipped, rather than
// something lighter wearing its name.
//
// Light mode is untouched. It never had this problem: a shadow on a white page
// is visible, so its panels never depended on lightness alone.
export const NEUTRAL = {
  dark: {
    sunken: [0.1650, 0.005], bg: [0.1881, 0.006], raised: [0.2280, 0.008],
    hover: [0.2560, 0.010], active: [0.2820, 0.013], line: [0.3100, 0.013],
    lineStrong: [0.3650, 0.016], mute: [0.6203, 0.016], dim: [0.7089, 0.014],
    fg: [0.9349, 0.004], bright: [1, 0],
  },
  light: {
    sunken: [0.9677, 0.003], bg: [0.9851, 0.001], raised: [1, 0],
    hover: [0.9617, 0.003], active: [0.947, 0.004], line: [0.9261, 0.005],
    lineStrong: [0.8658, 0.010], mute: [0.5558, 0.017], dim: [0.4783, 0.016],
    fg: [0.2059, 0.006], bright: [0.13, 0.003],
  },
};

// ── The accent curve ─────────────────────────────────────────────────────
// Anchored on the brand blue's own measured lightness rather than on an average
// of the four. The brand is the fixed point and the semantic colours conform to
// it — averaging would have moved every family including the one nobody asked
// to change.
//
// Chroma is per-theme, not per-family: a dark surface needs less of it to read
// as saturated. The browser reduces it further where a display cannot show it.
export const ACCENT = {
  dark: { l: 0.571, c: 0.17 },
  light: { l: 0.581, c: 0.2 },
};

/**
 * The one place a family is allowed off the shared lightness, and the reason
 * has to be written down.
 *
 * Equal OKLCH lightness does buy equal contrast — relative luminance tracks L
 * closely enough that a red and a green at one L sit the same distance from the
 * surface behind them. What it does not buy is equal *identity*. Yellow's
 * chroma peaks at high lightness, so holding it at 0.571 does not produce a
 * darker yellow, it produces olive: the hue stops being recognisable as the
 * thing it encodes. That is a poor trade for a weight equality nobody perceives
 * as a gain.
 *
 * So yellow is lifted, deliberately and visibly. Any entry here is a claim that
 * the curve is wrong for that hue — which is why the table is one line long and
 * why it names the hue rather than the role.
 */
export const LIGHTNESS_OFFSET = { warn: { dark: 0.1, light: -0.02 } };

/** The four hues a priority glyph can take. Same curve as the semantic
 *  families, slightly desaturated — see `generate-tokens.mjs`. */
export const PRIORITY = { urgent: HUE.danger, high: 40, medium: HUE.warn, low: 250 };

/** The one colour that is a decision rather than a position on a ramp:
 *  white on a solid, chosen for contrast. */
export const ACCENT_FG = "#ffffff";

export const ok3 = (n) => +n.toFixed(3);

export const oklch = (l, c, h) =>
  c === 0 ? `oklch(${ok3(l)} 0 0)` : `oklch(${ok3(l)} ${ok3(c)} ${h})`;

/** The neutral ramp as `{ role: [light, dark] }`, hue already applied. */
export function neutralPairs() {
  const out = {};
  for (const role of Object.keys(NEUTRAL.dark)) {
    const [dl, dc] = NEUTRAL.dark[role];
    const [ll, lc] = NEUTRAL.light[role];
    out[role] = [oklch(ll, lc, HUE.neutral), oklch(dl, dc, HUE.neutral)];
  }
  return out;
}

/** The four semantic families as `{ family: [light, dark] }`. */
export function familyPairs() {
  const out = {};
  for (const family of ["accent", "danger", "ok", "warn"]) {
    const off = LIGHTNESS_OFFSET[family] ?? { dark: 0, light: 0 };
    out[family] = [
      oklch(ACCENT.light.l + off.light, ACCENT.light.c, HUE[family]),
      oklch(ACCENT.dark.l + off.dark, ACCENT.dark.c, HUE[family]),
    ];
  }
  return out;
}

/** The four priority glyphs as `{ name: [light, dark] }`. */
export function priorityPairs() {
  const out = {};
  for (const [name, hue] of Object.entries(PRIORITY)) {
    // Yellow needs the same lift here as it does above, for the same reason: a
    // priority glyph is read at 12px and has to be identifiable by hue alone.
    const off = hue === HUE.warn ? LIGHTNESS_OFFSET.warn : { dark: 0, light: 0 };
    out[name] = [
      oklch(ACCENT.light.l + off.light, ACCENT.light.c * 0.9, hue),
      oklch(ACCENT.dark.l + 0.06 + off.dark, ACCENT.dark.c * 0.85, hue),
    ];
  }
  return out;
}
