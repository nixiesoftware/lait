/**
 * Generates `src/theme/laitTheme.ts` — the palette projected onto Astryx's
 * token vocabulary.
 *
 * WHY A GENERATOR AND NOT A HAND-WRITTEN THEME. Astryx's own themes are hand
 * -authored TypeScript: `neutralTheme.ts` is 637 lines of literal hex with the
 * reasoning in comments beside it. That is the thing `tool/palette.mjs` exists
 * to not be. A hand-written theme would be a second palette that starts equal
 * to ours and drifts, so this file *derives* the theme from the same seeds and
 * curves `generate-tokens.mjs` reads, and the drift becomes impossible rather
 * than merely discouraged.
 *
 * WHAT ASTRYX WANTS. `defineTheme({ tokens })` takes a flat map of CSS custom
 * property names to either a string or a `[light, dark]` tuple, which it turns
 * into `light-dark()`. That is the shape we already emit, so `oklch()` passes
 * through untouched — nothing here converts to hex, and the browser keeps
 * doing the per-display gamut mapping that made us choose OKLCH.
 *
 * WHAT WE DELIBERATELY DO NOT USE. `defineTheme` also accepts
 * `color: { accent: '#RRGGBB' }`, which derives a whole palette from one seed
 * using Google's HCT model. It is the same *idea* as our generator and a
 * different *curve*, and it takes hex — feeding it our blue would hand the
 * palette to someone else's perceptual model and quietly discard ours. So we
 * pass explicit `tokens` instead, which `defineTheme` documents as taking
 * precedence over anything generated.
 *
 * Run with `npm run tokens:astryx`. Output is committed and checked the same
 * way `tokens.generated.css` is.
 */

import { densityDelta, geometryTokens } from "./geometry.mjs";
import {
  ACCENT,
  ACCENT_FG,
  HUE,
  LIGHTNESS_OFFSET,
  familyPairs,
  neutralPairs,
  oklch,
  priorityPairs,
} from "./palette.mjs";

const n = neutralPairs();
const f = familyPairs();
const p = priorityPairs();

/**
 * Astryx's nine categorical hues, at the angles its own docs name.
 *
 * These are the one place the two systems genuinely overlap in intent: Astryx
 * wants nine mutually distinguishable hues for labels and chips, and we already
 * have a curve whose entire job is to make hues read at equal weight. So the
 * hues are Astryx's and the lightness and chroma are ours — the nine come off
 * the same ramp as `accent`, `danger`, `ok` and `warn`, which is what stops a
 * label chip from out-shouting a priority glyph two columns away.
 *
 * The warm end takes the same lift yellow takes everywhere else: hold orange
 * and yellow at the shared lightness and they turn to olive and mud.
 */
const CATEGORICAL = {
  red: 25, orange: 65, yellow: 90, green: 145, teal: 180,
  cyan: 215, blue: 250, purple: 320, pink: 355,
};
const WARM = new Set(["orange", "yellow"]);

/** A family at an arbitrary hue, on the shared accent curve. */
function family(hue, warm) {
  const off = warm ? LIGHTNESS_OFFSET.warn : { dark: 0, light: 0 };
  return [
    oklch(ACCENT.light.l + off.light, ACCENT.light.c, hue),
    oklch(ACCENT.dark.l + off.dark, ACCENT.dark.c, hue),
  ];
}

/**
 * A tint of `token` over the page.
 *
 * Astryx wants a `-muted` slot for every status and categorical family — the
 * banner background behind the text, the chip fill behind the label. We have no
 * such rung: our palette stops at the solid. Rather than invent nine pastels by
 * hand (which is exactly the twenty-hand-picked-hex problem the generator was
 * written to end), each muted slot is declared as a mix of its own solid into
 * the page, so it is derived from the token it belongs to and moves when that
 * token moves.
 *
 * `in oklch` on purpose: mixing in sRGB darkens and desaturates through the
 * middle, which is the whole reason the palette is in OKLCH to begin with.
 */
const tint = (token, pct) =>
  `color-mix(in oklch, var(${token}) ${pct}%, var(--color-background-body))`;

const tokens = {};
/**
 * A comment, kept in token order.
 *
 * `tokens` is one flat map because that is what `defineTheme` takes, but the
 * emitted file wants prose between the groups. A key no CSS property could ever
 * have carries it, and NUL is the one character guaranteed never to collide
 * with a custom-property name.
 *
 * WRITE IT AS AN ESCAPE, NEVER AS THE BYTE. `\0` here and in the two
 * `startsWith` guards below is two characters of source; typing the actual NUL
 * makes this file *contain* a NUL, and then git classifies it as binary. It
 * did, for a while: `git diff` reported `Bin 28104 -> 29521 bytes` for the file
 * that decides every colour in the app, `git log -p` showed nothing, and grep
 * silently matched nothing without `-a`. The runtime value is identical either
 * way — `--check` proves it — so there is no cost to the escape and a real one
 * to the byte.
 */
const section = (title) => {
  tokens[`\0${Object.keys(tokens).length}`] = title; // ordered comment marker
};
const set = (name, value) => {
  tokens[name] = value;
};

// ── Surfaces ─────────────────────────────────────────────────────────────
// Our ladder is sunken < bg < raised and it climbs the same way in both
// themes. Astryx splits the top rung by *role* rather than by height — card,
// popover and surface are three names for "above the canvas" — so all three
// take `raised` and the elevation difference is carried by its shadows, which
// is how its own neutral theme does it in dark mode.
section("Surfaces — our sunken < bg < raised ladder, mapped onto Astryx's roles");
set("--color-background-muted", n.sunken);
set("--color-background-body", n.bg);
set("--color-background-surface", n.raised);
set("--color-background-card", n.raised);
set("--color-background-popover", n.raised);
set("--color-background-inverted", n.fg);

// ── Interaction ──────────────────────────────────────────────────────────
// `--color-tint-hover` takes our solid `hover`. The `--color-overlay-*` pair
// is deliberately NOT mapped: those are alpha tints Astryx composites over
// whatever sits behind them, and our hover/active are opaque surface rungs.
// Putting an opaque colour in an overlay slot paints over the content instead
// of tinting it. Astryx's alphas are left in place; `active` reaches the UI
// through component overrides rather than through a token that means something
// else.
section("Interaction — see note: the overlay-* alphas stay Astryx's on purpose");
set("--color-tint-hover", n.hover);
set("--color-skeleton", n.hover);
set("--color-track", n.line);

// ── Lines ────────────────────────────────────────────────────────────────
section("Lines");
set("--color-border", n.line);
set("--color-border-emphasized", n.lineStrong);

// ── Text and icons ───────────────────────────────────────────────────────
// `dim` outranks `mute` in both themes — 0.709 vs 0.620 in dark, 0.478 vs
// 0.556 in light — so `dim` is the secondary voice and `mute` the tertiary
// one. Astryx has only two rungs below primary, so `mute` lands on `disabled`.
section("Text and icons — dim is the secondary voice, mute the tertiary");
set("--color-text-primary", n.fg);
set("--color-text-secondary", n.dim);
set("--color-text-disabled", n.mute);
set("--color-text-accent", f.accent);
set("--color-icon-primary", n.fg);
set("--color-icon-secondary", n.dim);
set("--color-icon-disabled", n.mute);
set("--color-icon-accent", f.accent);

// ── Accent and status ────────────────────────────────────────────────────
// `--color-accent-fg` is the one colour in the palette that is a decision
// rather than a position on a ramp, and it is the one Astryx already has a
// name for: `--color-on-accent`.
section("Accent and status");
// THE ONE NAME BOTH VOCABULARIES USE. Tailwind's `@theme` in
// `tokens.generated.css` declares `--color-accent` for our own utilities
// (`bg-accent`, `ring-accent/50`), and Astryx declares it for its components.
// Both take it from `familyPairs().accent`, so they agree by construction
// rather than by anyone remembering — and `designSystem.test.ts` asserts it,
// because "agree by construction" is only true while both keep reading the
// same function.
set("--color-accent", f.accent);
set("--color-accent-muted", tint("--color-accent", 12));
set("--color-on-accent", ACCENT_FG);
set("--color-success", f.ok);
set("--color-success-muted", tint("--color-success", 12));
set("--color-on-success", ACCENT_FG);
set("--color-error", f.danger);
set("--color-error-muted", tint("--color-error", 12));
set("--color-on-error", ACCENT_FG);
set("--color-warning", f.warn);
set("--color-warning-muted", tint("--color-warning", 12));
// Not ACCENT_FG: `warn` is the lifted yellow, and white on it fails at every
// size. It is the one status that reads against the dark end of the ramp.
set("--color-on-warning", n.fg[0]);

// ── Categorical ──────────────────────────────────────────────────────────
section("Categorical — Astryx's nine hues, on our curve");
for (const [name, hue] of Object.entries(CATEGORICAL)) {
  const pair = family(hue, WARM.has(name));
  set(`--color-text-${name}`, pair);
  set(`--color-icon-${name}`, pair);
  set(`--color-border-${name}`, tint(`--color-text-${name}`, 40));
  set(`--color-background-${name}`, tint(`--color-text-${name}`, 12));
}
// Grey is the tenth chip and it is not a hue — it is the neutral ramp,
// which we already have.
set("--color-text-gray", n.dim);
set("--color-icon-gray", n.dim);
set("--color-border-gray", n.line);
set("--color-background-gray", n.sunken);

// ── Radius ───────────────────────────────────────────────────────────────
// Our three semantic radii are mark 4 / control 8 / surface 12, which line up
// with Astryx's inner / element / container exactly. `page` is Astryx's own
// 28px, which is a marketing-page radius; a tracker's outermost surface is
// still a surface, so it takes 16.
section("Radius — mark/control/surface land on inner/element/container");
set("--radius-inner", "4px");
set("--radius-element", "8px");
set("--radius-container", "12px");
set("--radius-page", "16px");

// ── Elevation ────────────────────────────────────────────────────────────
// `low` is the ONLY elevation this app raises anything with, and every one of
// its callers is a chip: the project tabs, the display and filter pills, a
// dialog's Cancel, a calendar's today. Astryx's default is built for a card —
// `0 1px 1px` over `0 2px 8px` at 10% — and on a 24px pill that second layer is
// a soft halo twice the height of the thing casting it, which reads as blur
// rather than as lift.
//
// So it is retuned rather than dropped: the edge still has to say the chip is
// off the bar, it just has to say it in the space a chip occupies. Half the
// offset, a quarter of the blur, roughly half the light-mode alpha. Dark keeps
// more alpha because a shadow on a near-black surface has less to work with —
// the same asymmetry the popover hairline note explains.
//
// One token, because one token is what all of them read. There is no chip in
// the app that wants the old halo, and a per-call-site override would be the
// drift this ladder exists to prevent.
section("Elevation — `low` is a chip's lift, not a card's");
set(
  "--shadow-low",
  "0px 1px 1px light-dark(rgba(0, 0, 0, 0.04), rgba(0, 0, 0, 0.18)), " +
    "0px 1px 2px light-dark(rgba(0, 0, 0, 0.05), rgba(0, 0, 0, 0.14))",
);

// ── Type sizes ───────────────────────────────────────────────────────────
// The five rungs the tracker uses, at COMPACT. NOT `typography.scale`: Astryx
// generates a geometric ladder from `{ base, ratio }` and ours is deliberately
// bent — see `tool/geometry.mjs`.
//
// Only the SIZES live here. The spacing scale and the leading ratios are in
// `geometry.generated.css` instead, because `astryx theme build` silently
// discards them from a theme — see the note above that emit.
section("Type sizes — explicit, not a generated ratio: our ladder is bent");
for (const [name, value] of Object.entries(geometryTokens("compact"))) {
  if (name.startsWith("--font-size-")) set(name, value);
}

const SANS =
  '"Inter Variable", ui-sans-serif, system-ui, -apple-system, "Segoe UI", Roboto, sans-serif';
const MONO =
  '"Roboto Mono Variable", ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace';

// ── Priority ─────────────────────────────────────────────────────────────
/**
 * The four priority glyphs, as new `StatusDot` and `Badge` variants.
 *
 * `defineTheme`'s `tokens` map is typed against a closed vocabulary of 79
 * colour tokens, and "urgent / high / medium / low" is not in it — Astryx has
 * no opinion about priority because priority is lait's domain, not a design
 * system's. That looked at first like the place the migration stops.
 *
 * It is not. `StatusDotVariantMap` and `BadgeVariantMap` are declared as open
 * interfaces precisely so a consumer can add to them, and `astryx theme build`
 * detects any `variant:*` key here that is not in the base type and writes the
 * module augmentation into the emitted `.d.ts`. So the four priorities enter
 * the system as first-class variants — `<StatusDot variant="urgent" />` type
 * -checks — and they are still drawn from our ramp rather than picked by hand.
 *
 * This is the difference between a theme and a skin: the vocabulary extends.
 */
/**
 * Buttons: what survives from our ten variants, and why it is only two.
 *
 * `primitives.tsx` grew ten button variants — ghost, outline, primary, danger,
 * destructive, active, toolbar, inline, pill — each with a paragraph of
 * reasoning beside it. Astryx ships four. The first read of that gap is that we
 * need six generated variants. The second read, after `astryx component
 * Button|IconButton|Link|SegmentedControl`, is that most of ours existed
 * because we did not have the right COMPONENT:
 *
 *   outline   -> variant="secondary" elevation="low"   (elevation IS shadow-control)
 *   toolbar   -> variant="secondary" size="sm"
 *   active    -> <SegmentedControl>   — the variant was emulating one
 *   inline    -> <Link>               — it is a text action in prose
 *   pill      -> <IconButton>         — as is our `size="icon"`
 *   ghost, destructive                — native, one for one
 *
 * That leaves exactly two things Astryx does not already say:
 */
const buttons = {
  /**
   * Pills, not boxes — at every size, and this is a base rule rather than a
   * per-variant one for the reason `primitives.tsx` gave: "a button carries no
   * border of its own in the common variants, so its shape is whatever the fill
   * describes — and a row of buttons has to agree: a ghost Cancel beside a
   * primary Save cannot be a pill next to a box."
   *
   * Astryx's default is `--radius-element` (8px), which is a box. It is a
   * defensible default and it is not ours, so it moves here rather than at 200
   * call sites.
   */
  base: { borderRadius: "var(--radius-full)" },
  /**
   * Our `outline` — which is what `secondary` now carries — is the page
   * background with a whisper of lift, not a tint. Astryx fills it with a
   * blue-tinted alpha (`rgba(5, 54, 89, 0.1)` in light), which reads as a
   * faintly coloured chip in a toolbar of grey ones.
   *
   * The original note is still the argument: the edge is "a half-pixel ring and
   * a whisper of lift, not a border — measured off Linear's toolbar, where a
   * 1px border proved to be the thing making ours read as outlines drawn on the
   * bar rather than buttons resting on it." A tinted fill does the same damage
   * from the other direction.
   */
  /**
   * `secondary` is a RAISED surface, not a page-coloured one.
   *
   * It used to take `--color-background-body`, which is the page — so a resting
   * chip was exactly the colour of what it sat on and the only thing proving it
   * was a control was a drop shadow. In light mode that shadow reads as a
   * hairline and the illusion holds. In dark it is black on near-black and the
   * chip disappears, which is the same failure the popover had.
   *
   * `--color-background-surface` is the `raised` rung, and the ladder's own rule
   * is that raised means lighter-than-the-page in BOTH themes. So the fill now
   * does the work and the shadow only has to add the lift.
   */
  "variant:secondary": {
    backgroundColor: "var(--color-background-surface)",
    color: "var(--color-text-primary)",
    ":hover": { backgroundColor: "var(--color-tint-hover)" },
  },
  /**
   * The pressed state in a segmented group: a deeper fill and a ring, and
   * deliberately NO lift. From `primitives.tsx`: "a pressed control is set INTO
   * the bar, and a shadow claiming it had risen off is the one thing that would
   * make the pair read as two kinds of button."
   *
   * It has to be a real variant. Collapsing it onto `secondary` and carrying
   * the difference in `elevation` alone was the first attempt, and it lost the
   * selected state entirely — the current project tab rendered identically to
   * the other three.
   *
   * The fill is the neutral ramp's `active` rung, which is the one rung with no
   * home in Astryx's token vocabulary: its `--color-overlay-pressed` is an
   * alpha tint composited over content, and ours is an opaque surface. So the
   * value is stated here rather than mapped.
   */
  "variant:active": {
    backgroundColor: `light-dark(${n.active[0]}, ${n.active[1]})`,
    color: "var(--color-text-primary)",
    boxShadow: "0 0 0 0.5px light-dark(rgb(0 0 0 / 0.09), rgb(255 255 255 / 0.1))",
  },
  /**
   * Our `primary` is a NEUTRAL INVERSE, not an accent fill. The reasoning is
   * in `primitives.tsx` and it still holds: "a neutral inverse commit keeps
   * blue available for focus and state instead of making every save look like
   * a Jira call-to-action." Astryx's `primary` is accent-filled, so this is a
   * real disagreement about meaning rather than about colour, and it is worth
   * keeping — a tracker is mostly blue-for-state.
   *
   * Not a new variant: `primary` already means "the one commit on this
   * screen". We are changing what it looks like, not what it is.
   */
  "variant:primary": {
    backgroundColor: "var(--color-background-inverted)",
    color: "var(--color-background-body)",
    ":hover": {
      backgroundColor:
        "color-mix(in oklch, var(--color-background-inverted) 85%, var(--color-background-body))",
    },
  },
  /**
   * `danger` is the quiet destructive — the inline "X" that only reddens under
   * the pointer. Astryx's `destructive` is the filled commit in a delete
   * dialog, which is our `destructive` and a different thing: one asks, the
   * other confirms. Having both is the point, so this one is new vocabulary
   * and `astryx theme build` writes the augmentation for it.
   */
  "variant:danger": {
    backgroundColor: "transparent",
    color: "var(--color-text-disabled)",
    ":hover": {
      backgroundColor: "var(--color-error-muted)",
      color: "var(--color-error)",
    },
  },
};

const components = {
  button: buttons,
  iconbutton: buttons,
  /**
   * The popover panel owns no padding; its content does.
   *
   * `.astryx-popover` ships `padding: 12px`, which is a good default for a
   * panel of prose. Ours are menus: `PopoverContent` had none, and every call
   * site states its own (`p-2` on the pickers, `w-72 p-2` on saved views) so a
   * list can run edge to edge and a row's hover fill can reach the panel wall.
   * Stacked, the two became a 20px inset and the menus grew a margin nobody
   * asked for.
   *
   * Zeroing it here rather than fighting it at ten call sites keeps the rule
   * where the rule belongs: the panel is a surface, the content decides its
   * own inset.
   */
  /**
   * The popover panel owns no padding — its content does — but it DOES own an
   * edge.
   *
   * Astryx ships the panel with `border: 0` and separates it from the page with
   * a drop shadow alone. In light mode that works. In dark it does not: the
   * shadow is `rgba(0, 0, 0, 0.2)` painted on a near-black page, so the only
   * thing left holding the panel off the background is a 0.04 step in
   * lightness, and a menu over a dark surface reads as a hole rather than a
   * sheet.
   *
   * The hairline that fixes it is NOT here, and cannot be: `.astryx-popover` is
   * two boxes in from the sheet. The element that paints the fill, the radius
   * and the shadow carries no stable class at all, so a border set through this
   * key draws a rectangle *inside* the panel. It lives in `styles.css` instead,
   * against a `:has()` selector that can name the sheet — see the note there.
   *
   * Padding still belongs here: an inset applied to the inner box is exactly
   * what an inset should do.
   */
  popover: { base: { padding: "0" } },
  /**
   * The dialog sheet owns no padding either, and for a sharper reason.
   *
   * `.astryx-dialog` insets its content box by 16px. Every dialog we ship —
   * the composer, New project, New spec, the prompt, Governance — is built as
   * *regions*: a header, a body, a footer, each stating its own `p-4` and
   * several separated by a `border-t`/`border-b`. Stacked on Astryx's inset
   * that reads as a 32px margin, which is the popover bug again and merely
   * ugly.
   *
   * The rule is the part that is actually broken. A divider between two
   * regions of a dialog is structural: it says "the footer is a different kind
   * of thing from the body", and it can only say that by running wall to wall.
   * Inside a padded content box it stops 16px short at both ends and becomes a
   * decorative line floating in the sheet — which is what every dialog in the
   * app was drawing.
   *
   * Five of the seven call sites lose nothing: their headers are ours and
   * already state their own insets. The two that use Astryx's `DialogHeader`
   * were living on the sheet's padding — it has none of its own and no theme
   * key to give it any — so their inset is restored in `styles.css` against
   * `.astryx-layout-header`, for the same reason the popover's hairline lives
   * there: the rule is writable, the theme just has nowhere to write it.
   */
  dialog: { base: { padding: "0" } },
  /**
   * The menu row's highlight, in our neutral instead of Astryx's blue.
   *
   * Astryx paints both hover and keyboard focus with `rgba(5, 54, 89, 0.047)`
   * — a blue-cast wash. At 4.7% it is nearly invisible on its own, but it is
   * the same cast we already took off `secondary` buttons, and a menu row and
   * a list row sitting a few pixels apart under the same pointer should not
   * disagree about what "under the pointer" looks like.
   *
   * `--color-tint-hover` and Tailwind's `--color-hover` are the SAME derived
   * value out of `palette.mjs`, so pointing the menu at the token is what puts
   * every hover surface in the app on one number. `designSystem.test.ts`
   * holds them equal.
   *
   * BOTH STATES, or the fix is half a fix. Astryx drives keyboard navigation
   * with roving `tabindex` and styles the highlight through `:focus-visible`,
   * not a `data-highlighted` attribute — so overriding `:hover` alone leaves
   * arrowing through a menu painted blue while pointing at it paints grey.
   */
  /**
   * The menu panel's wall, one step out from Astryx's.
   *
   * 4px put a row's hover fill 4px from the panel edge, which on a 12px-radius
   * sheet is close enough that the fill's own corner fights the panel's — the
   * row reads as pressed against the wall rather than set inside it. 6px is the
   * same inset the rail and the pickers now use, so a menu, a combobox list and
   * the sidebar all hold their rows off the edge by the same amount.
   *
   * It is a theme key rather than a call-site class because it has to reach
   * every menu, including the ones Astryx composes internally (a submenu's
   * panel is not something we render).
   */
  "dropdown-menu": { base: { padding: "6px" } },
  "dropdown-menu-item": {
    base: {
      ":hover": { backgroundColor: "var(--color-tint-hover)" },
      ":focus-visible": { backgroundColor: "var(--color-tint-hover)" },
    },
  },
  statusdot: Object.fromEntries(
    Object.entries(p).map(([name, [light, dark]]) => [
      `variant:${name}`,
      { backgroundColor: `light-dark(${light}, ${dark})` },
    ]),
  ),
  badge: Object.fromEntries(
    Object.entries(p).map(([name, [light, dark]]) => [
      `variant:${name}`,
      {
        color: `light-dark(${light}, ${dark})`,
        backgroundColor: `color-mix(in oklch, light-dark(${light}, ${dark}) 12%, var(--color-background-body))`,
        borderColor: `color-mix(in oklch, light-dark(${light}, ${dark}) 40%, var(--color-background-body))`,
      },
    ]),
  ),
};

// ── Emit ─────────────────────────────────────────────────────────────────

const lit = (v) =>
  Array.isArray(v) ? `[${JSON.stringify(v[0])}, ${JSON.stringify(v[1])}]` : JSON.stringify(v);

const body = Object.entries(tokens)
  .map(([k, v]) =>
    k.startsWith("\0") ? `\n    // ${v}` : `    ${JSON.stringify(k)}: ${lit(v)},`,
  )
  .join("\n")
  .replace(/^\n/, "");

const out = `// GENERATED by tool/generate-astryx-theme.mjs — do not edit.
//
// The palette in \`tool/palette.mjs\`, projected onto Astryx's token vocabulary.
// Colour is derived, not authored: a seed gives a hue, and lightness and chroma
// come from shared curves. Edit the seeds or the curves and re-run
// \`npm run tokens:astryx\`.
//
// Values stay in \`oklch()\` rather than hex so the browser keeps gamut-mapping
// per display. \`[light, dark]\` tuples become \`light-dark()\` inside defineTheme.

import {defineTheme} from "@astryxdesign/core/theme";

export const laitTheme = defineTheme({
  name: "lait",

  typography: {
    body: {family: "Inter Variable", fallbacks: ${JSON.stringify(SANS.split(", ").slice(1).join(", "))}},
    heading: {family: "Inter Variable", fallbacks: ${JSON.stringify(SANS.split(", ").slice(1).join(", "))}},
    code: {family: "Roboto Mono Variable", fallbacks: ${JSON.stringify(MONO.split(", ").slice(1).join(", "))}},
  },

  // Snappier than Astryx's 175/410/975 default. A tracker is a keyboard
  // surface: the animation has to be finished before the next keystroke lands,
  // not still easing through it.
  motion: {fast: 110, medium: 260, slow: 600, ratio: 0.75},

  tokens: {
${body}
  },

  // Priority is lait's vocabulary, not Astryx's. These four keys are not in
  // the base variant types; \`astryx theme build\` detects that and emits the
  // module augmentations that make them type-check.
  components: ${JSON.stringify(components, null, 2).replace(/\n/g, "\n  ")},
});
`;

/**
 * Comfortable, as a cascade layer rather than a second theme.
 *
 * WHY NOT `defineTheme({ extends: laitTheme })`. Measured: a theme that extends
 * ours and overrides two tokens still compiles to 93 token overrides and
 * 9.4 KB — `extends` merges and re-emits the whole set, so every density would
 * carry a duplicate of all 92 colour tokens for the twenty that differ.
 *
 * WHY NOT A SECOND `<Theme>`. Swapping the theme prop re-renders the provider
 * and re-registers the theme before the browser does the same style recalc a
 * bare attribute flip would have done alone. Density is a CSS concern and
 * paying React for it is a straight loss.
 *
 * THE REAL REASON IS ORTHOGONALITY. Density is not a theme, and modelling it as
 * one multiplies: N themes x M densities is N*M artefacts to keep in agreement.
 * As a layer it is N + M. One theme and two densities today; the argument only
 * gets stronger the first time there is a second theme.
 *
 * This works because Astryx components read tokens through `var()` — 426
 * `--spacing-*` references, 50 `--radius-*`, 25 `--font-size-*` — so overriding
 * at the root reaches every component without touching any of them.
 */
const compact = geometryTokens("compact");
const delta = densityDelta();

/**
 * WHY GEOMETRY IS NOT IN THE THEME.
 *
 * It was, at first. `defineTheme`'s `tokens` map is typed
 * `Partial<Record<TokenName, TokenValue>>`, and `TokenName` is built from
 * `keyof typeof spacingDefaults | keyof typeof typeScaleDefaults` among
 * others — so `--spacing-3` and `--text-body-leading` type-check.
 *
 * `astryx theme build` then drops them. Given 106 tokens it emitted 92 and
 * reported "92 token overrides" without naming the 26 it discarded: all 15
 * `--spacing-*` and all 11 `--text-*-leading`. The builder's emit list is
 * narrower than the type that gates it, and it fails silently.
 *
 * That is a 0.3.0 bug and it will presumably be fixed. Waiting for it is not
 * necessary, because the same tokens applied perfectly well from a plain
 * cascade layer — which is where geometry belonged anyway. Colour goes through
 * the theme, geometry goes through this file, and neither has to know about
 * the other.
 *
 * Both densities are emitted here so there is one place to read the axis.
 */
const geometryCss = `/* GENERATED by tool/generate-astryx-theme.mjs — do not edit.
 *
 * The geometry axis: the spacing scale and the type ladder, both densities.
 * Colour is absent on purpose — density has no colour, and colour has no
 * density. Edit \`tool/geometry.mjs\` and run \`npm run tokens:astryx\`.
 *
 * NOT part of the Astryx theme: \`astryx theme build\` type-checks these token
 * names and then silently discards them (0.3.0). They apply correctly from a
 * layer, and geometry is a better fit for one regardless.
 *
 * This layer must come after \`astryx-theme\`; the order is declared in
 * \`styles.css\` so it does not depend on import order. */
@layer astryx-density {
  /* Compact — the tracker's default. */
  :root {
${Object.entries(compact)
  .map(([k, v]) => `    ${k}: ${v};`)
  .join("\n")}
  }

  /* Comfortable — the ${Object.keys(delta).length} tokens that differ. Rows get more room; the
     pinned axes (icons, marks) read none of this and do not move. */
  :root[data-density="comfortable"] {
${Object.entries(delta)
  .map(([k, v]) => `    ${k}: ${v};`)
  .join("\n")}
  }
}
`;

const { writeFileSync, readFileSync, existsSync, mkdirSync } = await import("node:fs");
const dir = new URL("../src/theme/", import.meta.url);
const path = new URL("laitTheme.ts", dir);
const densityPath = new URL("geometry.generated.css", dir);

if (process.argv.includes("--check")) {
  const currentDensity = existsSync(densityPath) ? readFileSync(densityPath, "utf8") : "";
  if (currentDensity.replace(/\r\n/g, "\n") !== geometryCss) {
    console.error("geometry.generated.css is stale — run `npm run tokens:astryx`");
    process.exit(1);
  }
  const current = existsSync(path) ? readFileSync(path, "utf8") : "";
  if (current.replace(/\r\n/g, "\n") !== out) {
    console.error("laitTheme.ts is stale — run `npm run tokens:astryx`");
    process.exit(1);
  }
  console.log("astryx theme sources are up to date");
} else {
  mkdirSync(dir, { recursive: true });
  writeFileSync(path, out);
  writeFileSync(densityPath, geometryCss);
  console.log(`wrote ${Object.keys(delta).length} density overrides to src/theme/geometry.generated.css`);
  const count = Object.keys(tokens).filter((k) => !k.startsWith("\0")).length;
  console.log(`wrote ${count} tokens to src/theme/laitTheme.ts`);
}
