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
    const seen = new Set<string>();
    for (const file of tsxFiles(UI_DIR)) {
      for (const m of readFileSync(file, "utf8").matchAll(/\bsize-icon-([a-z0-9]+)\b/g)) {
        seen.add(m[1]!);
      }
    }
    // A rung appearing here that is not in the token block would compile to
    // nothing at all — Tailwind emits no utility for an undefined named entry,
    // so the glyph would silently fall back to its intrinsic size.
    expect([...seen].sort()).toEqual(["2xs", "lg", "md", "sm", "xs"]);
  });
});
