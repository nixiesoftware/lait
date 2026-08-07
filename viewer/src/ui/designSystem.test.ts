import { spawnSync } from "node:child_process";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

import { cn } from "./primitives";

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
          if (/^\s*(\/\*|\*|\/\/)/.test(line)) return;
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
 * Radius is the one PROJECTED axis: a corner is chosen by role
 * (`rounded-mark|control|surface`) and each role points at a rung on the private
 * value-named ladder. Two things this catches.
 *
 * Bare `rounded` is the reason the axis needed doing at all — it compiled to a
 * hardcoded `.25rem` that read no token, and at 71 sites it was the app's most
 * used corner. Tailwind has no `radius-DEFAULT` namespace entry, so there is no
 * token-side fix; it can only be kept out.
 *
 * A rung name in a class (`rounded-8`) renders nothing, because the ladder lives
 * outside `@theme` and so generates no utilities. That is the point — you cannot
 * spend a raw value — but it fails silently, so it is worth naming.
 */
const RAW_RADIUS = /\brounded(?![-\w[:])|\brounded-(sm|md|lg|xl|none|4|6|8|12|16)\b|\brounded-\[/;

describe("radius is chosen by role, not by value", () => {
  it("no bare, legacy, arbitrary or rung-named radius in class strings", () => {
    const offenders: string[] = [];

    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      if (name.endsWith(".test.tsx")) continue;

      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          // Prose may say "fully rounded" or quote the old utility while
          // explaining why it was wrong; only class strings are policed.
          if (/^\s*(\/\*|\*|\/\/)/.test(line)) return;
          if (RAW_RADIUS.test(line)) offenders.push(`${name}:${i + 1}  ${line.trim().slice(0, 90)}`);
        });
    }

    expect(
      offenders,
      `Corners come from a role: rounded-mark (tight shapes), rounded-control\n` +
        `(anything interactive), rounded-surface (panels and floating things), or\n` +
        `rounded-full. Bare \`rounded\` is a hardcoded 4px that no token can reach,\n` +
        `and a rung name like \`rounded-8\` silently renders nothing.\n\n` +
        offenders.join("\n"),
    ).toEqual([]);
  });
});

/**
 * `tailwind-merge` only resolves a conflict between classes it knows share a
 * group, and its vocabulary is fixed — `rounded-control` is not in it. Left
 * unregistered, a base and a variant that both set a corner BOTH survive, and
 * CSS source order decides which wins. That is a silent failure, so the
 * registration in `primitives.tsx` is worth asserting rather than assuming:
 * the config shape is easy to get subtly wrong and nothing would complain.
 */
describe("cn understands our ladders", () => {
  const cases: Array<[string, string, string]> = [
    ["rounded-control", "rounded-full", "rounded-full"],
    ["rounded-full", "rounded-control", "rounded-control"],
    ["rounded-surface", "rounded-mark", "rounded-mark"],
    ["rounded-control", "rounded-lg", "rounded-lg"],
    ["h-ctl-md", "h-ctl-lg", "h-ctl-lg"],
    ["h-ctl-md", "h-bar-lg", "h-bar-lg"],
    ["min-h-ctl-sm", "min-h-ctl-xl", "min-h-ctl-xl"],
    ["size-icon-sm", "size-icon-lg", "size-icon-lg"],
    ["size-ctl-sm", "size-mark-xs", "size-mark-xs"],
    ["h-ctl-md", "h-8", "h-8"],
  ];

  it.each(cases)("cn(%s, %s) -> %s", (a, b, want) => {
    expect(cn(a, b)).toBe(want);
  });

  it("leaves non-conflicting classes alone", () => {
    expect(cn("h-ctl-md", "rounded-control", "px-2")).toBe("h-ctl-md rounded-control px-2");
  });

  // Not a quirk: `size-*` sets width and height, so it genuinely supersedes a
  // bare height. Worth pinning — it is the one case where a ladder class
  // disappearing from the output is correct rather than a merge bug.
  it("size supersedes height, as it should", () => {
    expect(cn("h-ctl-md", "size-icon-sm")).toBe("size-icon-sm");
  });
});

/**
 * `controlTrigger` used to encode two things on one axis — `property` and
 * `crumb` were the same face at two heights — so height could not be asked for
 * separately and a taller property row needed an eighth variant. Tone and size
 * now compose. These guard the seam that replaced it.
 */
describe("control triggers speak tone x size", () => {
  const TONES = ["quiet", "outline", "pill", "bare"];
  const SIZES = ["none", "xs", "sm", "md", "lg", "xl"];

  it("no call site still passes the retired `variant` prop", () => {
    const offenders: string[] = [];
    const elem = /<(Combobox|DatePicker)\b((?:[^<>]|=\{[^}]*\})*?)\/?>/gs;

    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      const src = readFileSync(file, "utf8");
      for (const m of src.matchAll(elem)) {
        if (/\bvariant=/.test(m[2]!)) offenders.push(`${name}: <${m[1]} variant=…>`);
      }
    }
    expect(
      offenders,
      "Triggers take `tone` and `size`, not `variant`. The seven variants were " +
        "five live ones (two were dead) encoding look and height on one axis.\n\n" +
        offenders.join("\n"),
    ).toEqual([]);
  });

  it("only declared tones and sizes are used", () => {
    const badTone = new Set<string>();
    const badSize = new Set<string>();
    for (const file of tsxFiles(SRC_DIR)) {
      const src = readFileSync(file, "utf8");
      const elem = /<(Combobox|DatePicker)\b((?:[^<>]|=\{[^}]*\})*?)\/?>/gs;
      for (const m of src.matchAll(elem)) {
        const t = /\btone="([a-z]+)"/.exec(m[2]!)?.[1];
        const z = /\bsize="([a-z]+)"/.exec(m[2]!)?.[1];
        if (t && !TONES.includes(t)) badTone.add(t);
        if (z && !SIZES.includes(z)) badSize.add(z);
      }
    }
    expect([...badTone]).toEqual([]);
    expect([...badSize]).toEqual([]);
  });

  /**
   * The one pairing that fails quietly. `bare` exists for a child that already
   * has a shape — a label chip carries its own 20px height — so inheriting the
   * default `md` would silently inflate it to 28px and nothing would error.
   */
  it("`bare` always opts out of the height ladder", () => {
    const offenders: string[] = [];
    const elem = /<(Combobox|DatePicker)\b((?:[^<>]|=\{[^}]*\})*?)\/?>/gs;
    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      for (const m of readFileSync(file, "utf8").matchAll(elem)) {
        const body = m[2]!;
        if (/\btone="bare"/.test(body) && !/\bsize="none"/.test(body)) {
          offenders.push(`${name}: tone="bare" without size="none"`);
        }
      }
    }
    expect(offenders).toEqual([]);
  });
});

/**
 * Floating surfaces have two magnitudes that are easy to leave loose because
 * neither looks like geometry at the call site: how far the surface sits from
 * its trigger (a Radix `sideOffset` number, not a class) and how tall its list
 * may grow before scrolling. They were five literals across six files, and two
 * popovers had quietly drifted 2px off the shared default.
 */
describe("overlay magnitudes are named", () => {
  it("no literal sideOffset and no numeric max-h", () => {
    const offenders: string[] = [];
    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      if (name.endsWith(".test.tsx")) continue;
      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          if (/^\s*(\/\*|\*|\/\/)/.test(line)) return;
          if (/sideOffset=\{\d+\}/.test(line) || /\bmax-h-\d/.test(line)) {
            offenders.push(`${name}:${i + 1}  ${line.trim().slice(0, 80)}`);
          }
        });
    }
    expect(
      offenders,
      "Anchor distance comes from `OverlayGap` (panel|menu|tip) and a scroll cap " +
        "from `max-h-overlay-sm|md|lg`. A loose number here is invisible until a " +
        "whole category of surface needs retuning and there is no one place to do it.\n\n" +
        offenders.join("\n"),
    ).toEqual([]);
  });
});

/**
 * Colour is generated, and a generated file that is also editable stops being
 * generated the first time someone edits it. This re-runs the generator and
 * fails if the committed output has drifted, so the seeds and curves stay the
 * source of truth rather than becoming a comment above the real one.
 */
describe("colour tokens are generated, not authored", () => {
  it("the committed output matches the generator", () => {
    const viewer = join(__dirname, "..", "..");
    const run = spawnSync(
      process.execPath,
      [join(viewer, "tool", "generate-tokens.mjs"), "--check"],
      { encoding: "utf8" },
    );
    expect(
      run.status,
      `tokens.generated.css has drifted from tool/generate-tokens.mjs.\n` +
        `Edit the seeds or curves in the generator and run \`npm run tokens\`.\n\n` +
        (run.stderr || "") + (run.stdout || ""),
    ).toBe(0);
  });

  it("no hand-authored colour survives in styles.css", () => {
    const css = readFileSync(join(__dirname, "..", "styles.css"), "utf8");
    const declarations = [...css.matchAll(/^\s*--color-[\w-]+:/gm)].map((m) => m[0].trim());
    expect(
      declarations,
      `Colour lives in the generator. A --color-* declaration here is a value that\n` +
        `escaped the curves and will not move when a seed does.\n\n` +
        declarations.join("\n"),
    ).toEqual([]);
  });
});

/**
 * `--color-accent` is the one token name BOTH vocabularies declare.
 *
 * Tailwind's `@theme` block in `tokens.generated.css` defines it so our own
 * utilities exist (`bg-accent`, `ring-accent/50`, `border-accent/30`), and the
 * Astryx theme defines it so Astryx's components resolve it. Both read
 * `familyPairs().accent` from `tool/palette.mjs`, so they agree by
 * construction — but only while both keep doing that, and nothing else in the
 * codebase says so out loud.
 *
 * A divergence here is invisible: the app would render two slightly different
 * blues, ours on our markup and theirs on theirs, and every screenshot would
 * look fine on its own.
 */
describe("the accent both systems name", () => {
  it("is the same colour in the Tailwind theme and the Astryx theme", () => {
    const tailwind = readFileSync(join(SRC_DIR, "tokens.generated.css"), "utf8");
    const astryx = readFileSync(join(SRC_DIR, "theme/laitTheme.ts"), "utf8");

    // Both sides hold two `oklch(...)` calls — nested parens, so pull the calls
    // out rather than trying to split the pair.
    const line = /--color-accent:[^;]+;/.exec(tailwind)?.[0] ?? "";
    const fromTailwind = [...line.matchAll(/oklch\([^)]*\)/g)].map((m) => m[0]);
    const astryxLine = /"--color-accent":[^\]]+\]/.exec(astryx)?.[0] ?? "";
    const fromAstryx = [...astryxLine.matchAll(/oklch\([^)]*\)/g)].map((m) => m[0]);

    expect(fromTailwind, "no --color-accent pair in tokens.generated.css").toHaveLength(2);
    expect(fromAstryx, "no --color-accent pair in laitTheme.ts").toHaveLength(2);

    expect(
      fromTailwind,
      "The two vocabularies disagree about the accent. Both should come from " +
        "familyPairs().accent in tool/palette.mjs — re-run `npm run tokens` " +
        "and `npm run tokens:astryx`.",
    ).toEqual(fromAstryx);
  });
});

/**
 * `menuRow` copies a measurement off `.astryx-dropdown-menu-item`, and a copied
 * measurement is a fact with no owner.
 *
 * Two thirds of the app's menu rows are Astryx's (`DropdownMenuItem`); the rest
 * are ours — `cmdk` in the pickers, hand-built rows in the small panels. They
 * have to look identical, and the only thing making them identical is that
 * `menuRow` restates Astryx's numbers in Tailwind's vocabulary. Nothing in the
 * type system connects the two, and an Astryx minor that re-rungs its menu row
 * would leave ours behind without a single test going red.
 *
 * This cannot compare against Astryx's compiled CSS — StyleX emits atomic
 * classes with generated names, so there is nothing stable to read. What it CAN
 * do is hold `menuRow` to the values recorded when it was written, so the drift
 * shows up as a deliberate edit here rather than as two menus that quietly stop
 * matching. If Astryx moves, this test is the checklist of what to re-measure.
 */
describe("our menu row and Astryx's", () => {
  it("still states the measurements it was built from", () => {
    const primitives = readFileSync(join(SRC_DIR, "ui/primitives.tsx"), "utf8");
    const recipe = /export const menuRow =\s*"([^"]+)"/.exec(primitives)?.[1] ?? "";

    expect(recipe, "menuRow is no longer a single string literal").not.toBe("");

    // Measured off a live `.astryx-dropdown-menu-item`: 13px, 6px/8px padding,
    // 8px gap, 8px radius, primary text, tint-hover highlight.
    for (const cls of [
      "text-base", // --font-size-base: 13px
      "py-1.5", // 6px
      "px-2", // 8px
      "gap-2", // 8px
      "rounded-control", // 8px
      "text-fg", // --color-text-primary
      "hover:bg-hover", // --color-tint-hover
      "data-[selected=true]:bg-hover", // cmdk's highlight, same token
    ]) {
      expect(
        recipe,
        `menuRow lost \`${cls}\`. It mirrors .astryx-dropdown-menu-item — if ` +
          "Astryx changed, re-measure and update BOTH this list and the recipe; " +
          "if it did not, this is the drift the test exists to catch.",
      ).toContain(cls);
    }
  });

  it("points the Astryx side at the same hover token", () => {
    const astryx = readFileSync(join(SRC_DIR, "theme/laitTheme.ts"), "utf8");
    const block = /"dropdown-menu-item":\s*\{[\s\S]*?\n {4}\}/.exec(astryx)?.[0] ?? "";

    expect(
      block,
      "The dropdown-menu-item override is gone from laitTheme.ts, so Astryx's " +
        "menu rows are back on their built-in blue-cast overlay while ours use " +
        "--color-tint-hover. Re-run `npm run tokens:astryx`.",
    ).not.toBe("");

    // Both states, or arrowing and pointing disagree — see the note in the
    // generator: Astryx drives keyboard nav with roving tabindex + :focus-visible.
    expect(block).toContain(":hover");
    expect(block).toContain(":focus-visible");
    expect([...block.matchAll(/var\(--color-tint-hover\)/g)]).toHaveLength(2);
  });
});

/**
 * Guards the CASCADE LAYER ORDER, which is load-bearing and not enforced by
 * the thing that declares it.
 *
 * `styles.css` opens with an explicit
 * `@layer reset, theme, base, astryx-base, astryx-theme, astryx-density,
 * components, utilities;` — and that statement does NOT survive into the
 * built stylesheet. Tailwind rewrites it away, so at runtime precedence falls
 * back to FIRST APPEARANCE order, which currently happens to match what we
 * asked for.
 *
 * That is luck, and it is the fragile kind: an `@import` added in the wrong
 * place silently reorders the layers, and the failure is a design system
 * quietly losing to a reset — no error, no stack, just the wrong colour
 * somewhere nobody is looking. This asserts the order the browser will
 * actually use.
 *
 * It reads the committed build output rather than the source, for the same
 * reason `tokens.generated.css` is checked in: the artefact is the thing that
 * ships. A `styles.css` edit without a rebuild is what this catches second.
 */
describe("cascade layers land in the intended order", () => {
  it("orders reset below Astryx below our theme below Tailwind's utilities", () => {
    const css = readFileSync(join(SRC_DIR, "../../src/serve/assets/index.css"), "utf8");
    const seen: string[] = [];
    for (const m of css.matchAll(/@layer\s+([a-z-]+)\s*\{/g)) {
      const name = m[1]!;
      if (!seen.includes(name)) seen.push(name);
    }
    // `properties` is Tailwind's own @property shim and always leads.
    const ours = seen.filter((n) => n !== "properties");
    expect(
      ours,
      "Layer order is by first appearance — the explicit @layer statement in " +
        "styles.css is stripped by Tailwind. If this fails, an import moved.",
    ).toEqual([
      "reset",
      "theme",
      "base",
      "astryx-base",
      "astryx-theme",
      "astryx-density",
      "utilities",
    ]);
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
