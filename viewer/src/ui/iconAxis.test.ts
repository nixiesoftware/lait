import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * Guards the icon axis's *derivation*, not its values.
 *
 * A glyph is sized against the type scale, not against the 4px spacing unit —
 * so icon sizes are PINNED (`--spacing-icon-*`, a literal) rather than
 * SCALAR-DERIVED (`size-4`, which compiles to `calc(var(--spacing) * 4)`).
 * Get the kind wrong and nothing looks broken until someone toggles
 * comfortable density, at which point every glyph in the app silently fattens
 * by 12.5% relative to the text beside it. That is how the bug arrived the
 * first time; this test is what stops it arriving again.
 *
 * A tripwire is the weaker form of enforcement — the intent is an analyzer
 * rule, which lands with the wider closure work (docs/plans/11, phase 4) once
 * the project has a lint tier. Until then this is what we have, so it is
 * deliberately narrow: it only polices glyphs, and it names the exemptions
 * rather than pattern-matching around them.
 */

const UI_DIR = join(__dirname);
const SRC_DIR = join(__dirname, "..");

/** A numeric `size-*` utility on a capitalised JSX element — captures the tag
 *  so non-glyph components can be excused by name rather than by file. */
const CAPITALISED_WITH_SCALAR_SIZE = /<([A-Z]\w*)[^>]*?\bclassName=[^>]*?\bsize-\d/;

/** `[&>svg]:size-N` — a scalar size aimed through a wrapper at a glyph. */
const SVG_SELECTOR_WITH_SCALAR_SIZE = /\[&>svg\]:size-\d/;

/**
 * Capitalised elements that are *not* glyphs. These take a numeric size
 * legitimately: an avatar is a component dimension measured against row
 * rhythm, not a glyph measured against text. It moves to the heights axis
 * when that lands — not by being quietly retyped as an icon.
 *
 * Named rather than file-exempted on purpose: exempting `Members.tsx` because
 * it renders one avatar would also blind the guard to every real glyph in
 * that file.
 */
const NOT_A_GLYPH = new Set(["Avatar"]);

function tsxFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) return tsxFiles(full);
    return entry.endsWith(".tsx") ? [full] : [];
  });
}

describe("icon axis is pinned, not scalar-derived", () => {
  it("no glyph carries a numeric size-* utility", () => {
    const offenders: string[] = [];

    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";

      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          const tag = CAPITALISED_WITH_SCALAR_SIZE.exec(line)?.[1];
          const scalarGlyph = tag !== undefined && !NOT_A_GLYPH.has(tag);
          if (scalarGlyph || SVG_SELECTOR_WITH_SCALAR_SIZE.test(line)) {
            offenders.push(`${name}:${i + 1}  ${line.trim().slice(0, 90)}`);
          }
        });
    }

    expect(
      offenders,
      `Glyphs must use the pinned icon ladder (size-icon-2xs|xs|sm|md|lg), not a\n` +
        `numeric utility — the numeric form scales with --spacing and will grow\n` +
        `the glyph when density changes. See docs/plans/11-geometry-token-ladder.md.\n\n` +
        offenders.join("\n"),
    ).toEqual([]);
  });

  it("the ladder is the only icon size vocabulary in use", () => {
    expect(rungsUsed("icon")).toEqual(["2xs", "lg", "md", "sm", "xs"]);
  });
});

/**
 * A mark is a coloured shape that carries state or identity — a status dot, a
 * pending pulse, a project or label swatch. It has no glyph inside it, so its
 * size is chosen for how heavily the colour reads beside the label it
 * annotates. That is a different claim from how roomy a row is, so marks are
 * pinned too (`--spacing-mark-*`).
 */
const MARK_SHAPE = /\bsize-\d[\d.]*\b/;
const IS_ROUND = /\brounded-(full|sm|\[)/;
/*
 * Deliberately NOT also testing for colour. The obvious reading of "a mark is
 * a coloured shape" is `bg-*` or an inline `style={{ background }}`, but the
 * daemon-status dot in Sidebar takes its fill from a variable
 * (`cn("size-… rounded-full", cls)`), so a colour test silently skipped it —
 * the guard passed while the dot sat back on a scalar utility. Shape alone
 * turns out to be both stronger and quieter here: every real mark is round,
 * and the non-marks are excluded below for reasons that are about behaviour
 * rather than paint.
 */
/** A control part, not a mark: marks do not move. The switch thumb is
 *  `bg-bg size-3 rounded-full transition-transform` — coloured and round, but
 *  it slides, which makes it part of the control and heights-axis material. */
const MOVES = /transition-transform|translate-x-/;
/** Nor do marks hold anything. A mark is an empty shape; something that
 *  centres a child is a container sized to fit its content, and is sized
 *  against row rhythm. Calendar's "today" badge is the case in point —
 *  coloured and round, but it holds the date number. */
const HOLDS_CONTENT = /items-center|justify-center/;

describe("mark axis is pinned, not scalar-derived", () => {
  it("no coloured mark carries a numeric size-* utility", () => {
    const offenders: string[] = [];

    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          if (MOVES.test(line) || HOLDS_CONTENT.test(line)) return;
          if (MARK_SHAPE.test(line) && IS_ROUND.test(line)) {
            offenders.push(`${name}:${i + 1}  ${line.trim().slice(0, 90)}`);
          }
        });
    }

    expect(
      offenders,
      `Marks must use the pinned mark ladder (size-mark-xs|sm|md|lg|xl), not a\n` +
        `numeric utility — the numeric form scales with --spacing, so loosening\n` +
        `rows would also make every status dot louder.\n\n` +
        offenders.join("\n"),
    ).toEqual([]);
  });

  it("the ladder is the only mark size vocabulary in use", () => {
    expect(rungsUsed("mark")).toEqual(["lg", "md", "sm", "xl", "xs"]);
  });
});

/**
 * Control heights are the opposite case from icons and marks: they are SUPPOSED
 * to move with density, so they stay derived — just from `--scale` and under
 * one name (`h-ctl-*`) rather than arriving from five recipes under three
 * vocabularies. What this guards is the vocabulary, not the derivation: a raw
 * `h-7` still renders 28px today, so nothing looks wrong, and the ladder quietly
 * stops being the single place a height is decided.
 */
const CONTROL_RANGE = /\b(min-)?h-(5|6|7|8|9|10|11)\b/;
/**
 * Geometry that is not a control height. The timeline is a chart — its bars and
 * rulers are plotted, not laid out on a control ladder — and the switch's track
 * and thumb are sub-control parts below the smallest rung.
 */
const NOT_A_CONTROL = new Set(["Timeline.tsx"]);

describe("control heights speak one vocabulary", () => {
  it("no numeric h-* in the control range outside charts", () => {
    const offenders: string[] = [];

    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      if (NOT_A_CONTROL.has(name) || name.endsWith(".test.tsx")) continue;

      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          // Prose in a doc comment may legitimately name an old utility while
          // explaining history; only policed in real class strings.
          if (/^\s*(\*|\/\/)/.test(line)) return;
          if (CONTROL_RANGE.test(line)) {
            offenders.push(`${name}:${i + 1}  ${line.trim().slice(0, 90)}`);
          }
        });
    }

    expect(
      offenders,
      `Control heights come from the ladder (h-ctl-xs|sm|md|lg|xl) or, for a bar\n` +
        `sized by what it holds, h-bar-sm|md|lg. A raw h-7 renders the same 28px\n` +
        `today, which is exactly why this is worth catching: it works, and the\n` +
        `ladder silently stops being the one place a height is decided.\n\n` +
        offenders.join("\n"),
    ).toEqual([]);
  });

  it("the ladder is the only control-height vocabulary in use", () => {
    expect(rungsUsed("ctl")).toEqual(["lg", "md", "sm", "xl", "xs"]);
    expect(rungsUsed("bar")).toEqual(["lg", "md", "sm"]);
  });
});

/**
 * Rungs actually referenced in source for a given axis. A rung used here but
 * missing from the token block compiles to nothing at all — Tailwind emits no
 * utility for an undefined named entry — so the element silently falls back to
 * its intrinsic size rather than failing loudly.
 */
function rungsUsed(axis: "icon" | "mark" | "ctl" | "bar"): string[] {
  const seen = new Set<string>();
  // Any sizing utility, not just `size-*`: the control ladder is reached mostly
  // through `h-` and `min-h-`, so a `size-`-only pattern reported three rungs
  // where five are in use.
  const pattern = new RegExp(`\\b(?:size|min-h|max-h|h|w)-${axis}-([a-z0-9]+)\\b`, "g");
  for (const file of tsxFiles(UI_DIR)) {
    for (const m of readFileSync(file, "utf8").matchAll(pattern)) seen.add(m[1]!);
  }
  return [...seen].sort();
}
