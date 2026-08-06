/**
 * The density axis: the spacing quantum and the type ladder, per density.
 *
 * WHY THIS IS SEPARATE FROM `palette.mjs`. Colour has no density and density
 * has no colour. Keeping them in one file would invite a token that reads both,
 * which is the first step toward "comfortable mode is slightly bluer".
 *
 * WHY THE LADDER IS A TABLE AND NOT A RATIO. Astryx's `typography.scale` takes
 * `{ base, ratio }` and generates a geometric ladder. Ours is not geometric —
 * 10/11/12/13/15 tightens as it falls, because a 1px step is a larger
 * perceptual move at 11px than at 15px. A ratio would smooth out a curve we
 * bent on purpose, so the rungs are stated and the bend is preserved.
 *
 * WHAT DOES NOT APPEAR HERE. Icons and marks. They are PINNED against the type
 * scale rather than derived from the spacing unit, and the whole reason that
 * distinction exists is so density can move rows without fattening glyphs —
 * see the tripwire in `designSystem.test.ts`. If a glyph size ever shows up in
 * this file, that guard has been defeated.
 */

/**
 * The spacing quantum. Astryx's `--spacing-N` scale is `unit × N` exactly as
 * ours is, which is the single luckiest alignment in the migration: the whole
 * 15-rung scale is derivable, so density moves it by moving one number.
 */
export const UNIT = { compact: 4, comfortable: 4.5 };

/**
 * Astryx's spacing rungs, as multiples of the unit. Read off its own defaults
 * (`--spacing-2` is 8px at a 4px unit, `--spacing-0-5` is 2px), so this is a
 * description of their scale rather than a new opinion about ours.
 */
export const SPACING_STEPS = {
  "0": 0, "0-5": 0.5, "1": 1, "1-5": 1.5, "2": 2, "3": 3, "4": 4,
  "5": 5, "6": 6, "7": 7, "8": 8, "9": 9, "10": 10, "11": 11, "12": 12,
};

/**
 * The type ladder: `[size, lineHeight]` in px, per density.
 *
 * Only the five rungs the tracker actually uses. Astryx ships twelve
 * (`4xs`…`5xl`); the seven above `lg` are display sizes for marketing surfaces
 * and they stay at Astryx's defaults, unmoved by density — a page heading has
 * no business inflating because someone loosened their issue rows.
 */
export const TYPE = {
  compact: {
    "2xs": [10, 14],
    xs: [11, 16],
    sm: [12, 16],
    base: [13, 20],
    lg: [15, 22],
  },
  comfortable: {
    "2xs": [11, 15],
    xs: [12, 17],
    sm: [13, 18],
    base: [14, 21],
    lg: [16, 23],
  },
};

/**
 * Astryx's type ROLES, and the rung each one sits on.
 *
 * Astryx does not let a component name a font size directly — it names a role
 * (`body`, `supporting`, `heading-4`) and the role resolves to a rung. That is
 * a better model than ours and we adopt it wholesale: this table is the only
 * place a role meets a size, so re-runging a role is a one-line edit rather
 * than a sweep.
 *
 * `-leading` is a UNITLESS RATIO in Astryx, not a px line-height. That is what
 * makes density cheap: the ratio is computed from our own (size, lineHeight)
 * pair per density, so the bent ladder survives and nothing has to restate a
 * pixel height downstream.
 */
export const ROLE_RUNG = {
  body: "base",
  label: "base",
  code: "base",
  large: "lg",
  supporting: "sm",
  "heading-1": "lg",
  "heading-2": "lg",
  "heading-3": "lg",
  "heading-4": "base",
  "heading-5": "sm",
  "heading-6": "xs",
};

const round4 = (n) => +n.toFixed(4);

/** Every geometry token for one density, as `{ name: value }`. */
export function geometryTokens(density) {
  const unit = UNIT[density];
  const type = TYPE[density];
  const out = {};

  for (const [step, n] of Object.entries(SPACING_STEPS)) {
    out[`--spacing-${step}`] = `${round4(unit * n)}px`;
  }

  for (const [rung, [size]] of Object.entries(type)) {
    out[`--font-size-${rung}`] = `${size}px`;
  }

  for (const [role, rung] of Object.entries(ROLE_RUNG)) {
    const [size, lineHeight] = type[rung];
    out[`--text-${role}-leading`] = String(round4(lineHeight / size));
  }

  return out;
}

/** The tokens that actually differ between the two densities. */
export function densityDelta() {
  const compact = geometryTokens("compact");
  const comfortable = geometryTokens("comfortable");
  return Object.fromEntries(
    Object.entries(comfortable).filter(([k, v]) => compact[k] !== v),
  );
}
