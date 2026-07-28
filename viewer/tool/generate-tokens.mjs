/**
 * Generates the colour tokens in `src/tokens.generated.css`.
 *
 * WHY THIS EXISTS. The palette used to be twenty independently chosen hex
 * pairs. Nothing structural made two semantic colours read at the same weight —
 * it held only for as long as someone had eyeballed it, and measurement said it
 * had already stopped holding: in dark mode the four accent families sat at
 * OKLCH lightness 0.571 (accent), 0.677 (danger), 0.695 (ok) and 0.720 (warn).
 * A 0.149 spread is not a palette, it is four colours that happen to coexist.
 *
 * So colour is DERIVED now, like every other axis. A seed contributes its hue;
 * lightness and chroma come from shared curves. Two families differ in hue and
 * in nothing else, which is what makes them equal weight by construction rather
 * than by inspection.
 *
 * WHY THE NEUTRALS DID NOT MOVE. Measuring the existing greys showed they were
 * already one hue-consistent ramp — 285.8-286.4 across every step, both themes.
 * The L curve below IS that measurement, so the greys this emits are the greys
 * we had. The generator earns its keep on the accent families, not on them.
 *
 * WHY `oklch()` RATHER THAN HEX. The browser gamut-maps out-of-range values by
 * reducing chroma while holding hue, which is exactly what a hand-rolled clamp
 * would have to reimplement — and it does it per-display rather than for sRGB
 * only. Emitting hex would throw that away.
 *
 * Run with `npm run tokens`. The output is committed; `designSystem.test.ts`
 * re-runs this and fails if the committed file has drifted, so the source of
 * truth cannot silently become the generated file.
 */

// ── Seeds: a hue, and nothing else ───────────────────────────────────────
// Chroma is a curve, not a property of the seed. Only the angle is chosen here.
const HUE = {
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
const NEUTRAL = {
  dark: {
    sunken: [0.1354, 0.005], bg: [0.1505, 0.004], raised: [0.1881, 0.006],
    hover: [0.22, 0.010], active: [0.2466, 0.013], line: [0.2716, 0.013],
    lineStrong: [0.3286, 0.016], mute: [0.6203, 0.016], dim: [0.7089, 0.014],
    fg: [0.9349, 0.004],
  },
  light: {
    sunken: [0.9677, 0.003], bg: [0.9851, 0.001], raised: [1, 0],
    hover: [0.9617, 0.003], active: [0.947, 0.004], line: [0.9261, 0.005],
    lineStrong: [0.8658, 0.010], mute: [0.5558, 0.017], dim: [0.4783, 0.016],
    fg: [0.2059, 0.006],
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
const ACCENT = {
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
const LIGHTNESS_OFFSET = { warn: { dark: 0.1, light: -0.02 } };

const ok3 = (n) => +n.toFixed(3);
const oklch = (l, c, h) => (c === 0 ? `oklch(${ok3(l)} 0 0)` : `oklch(${ok3(l)} ${ok3(c)} ${h})`);

/** `light-dark()` reads `color-scheme`, so one property flips the whole theme
 *  and neither palette has to be restated. */
const pair = (light, dark) => `light-dark(${light}, ${dark})`;

const lines = [];
const emit = (name, light, dark) => lines.push(`  --color-${name}: ${pair(light, dark)};`);

// Neutrals, by role.
for (const role of Object.keys(NEUTRAL.dark)) {
  const [dl, dc] = NEUTRAL.dark[role];
  const [ll, lc] = NEUTRAL.light[role];
  const css = role === "lineStrong" ? "line-strong" : role;
  emit(css, oklch(ll, lc, HUE.neutral), oklch(dl, dc, HUE.neutral));
}

// The four accent families: same curve, different hue. That is the whole point.
for (const family of ["accent", "danger", "ok", "warn"]) {
  const off = LIGHTNESS_OFFSET[family] ?? { dark: 0, light: 0 };
  emit(
    family,
    oklch(ACCENT.light.l + off.light, ACCENT.light.c, HUE[family]),
    oklch(ACCENT.dark.l + off.dark, ACCENT.dark.c, HUE[family]),
  );
}

// Priority glyphs are the clearest case for the shared curve: four hues that
// have to read as one family at a glance, spanning the same range as the
// semantic colours above.
const PRIORITY = { urgent: HUE.danger, high: 40, medium: HUE.warn, low: 250 };
for (const [name, hue] of Object.entries(PRIORITY)) {
  // Yellow needs the same lift here as it does above, for the same reason: a
  // priority glyph is read at 12px and has to be identifiable by hue alone.
  const off = hue === HUE.warn ? LIGHTNESS_OFFSET.warn : { dark: 0, light: 0 };
  emit(
    name,
    oklch(ACCENT.light.l + off.light, ACCENT.light.c * 0.9, hue),
    oklch(ACCENT.dark.l + 0.06 + off.dark, ACCENT.dark.c * 0.85, hue),
  );
}

const out = `/* GENERATED by tool/generate-tokens.mjs — do not edit.
 *
 * Colour is derived, not authored: a seed gives a hue, and lightness and chroma
 * come from shared curves, so two families differ in hue and nothing else. Edit
 * the seeds or the curves in the generator and run \`npm run tokens\`.
 *
 * \`--color-accent-fg\` is not here: it is the one colour that is a decision
 * rather than a position on a ramp — white on a solid, chosen for contrast. */
@theme {
${lines.join("\n")}
  --color-accent-fg: #ffffff;
}
`;

const path = new URL("../src/tokens.generated.css", import.meta.url);
const { writeFileSync, readFileSync, existsSync } = await import("node:fs");

if (process.argv.includes("--check")) {
  const current = existsSync(path) ? readFileSync(path, "utf8") : "";
  if (current.replace(/\r\n/g, "\n") !== out) {
    console.error("tokens.generated.css is stale — run `npm run tokens`");
    process.exit(1);
  }
  console.log("tokens.generated.css is up to date");
} else {
  writeFileSync(path, out);
  console.log(`wrote ${lines.length + 1} colour tokens to src/tokens.generated.css`);
}
