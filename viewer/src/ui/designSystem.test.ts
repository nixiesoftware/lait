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
    expect(rungsUsed("mark")).toEqual(["lg", "sm", "xs"]);
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
 * Geometry that is not a control height: a chart's bars and rulers are plotted
 * rather than laid out on a control ladder, and the switch's track and thumb are
 * sub-control parts below the smallest rung.
 *
 * Empty since the dependency morphology was withdrawn — it was the only chart
 * here. Kept because the exemption is a standing rule about charts, not about
 * that one file.
 */
const NOT_A_CONTROL = new Set<string>([]);

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
    // `lg` (44) is the header and sidebar; `md` (36) the toolbar. `bar-sm`
    // (32) is declared in `styles.css` and read by nothing today.
    //
    // That is a normal state for a rung, not a defect, but it has a consequence
    // worth writing down once: Tailwind emits an `@theme` variable only where
    // something reads it, so an unread rung is not merely unused — it is absent
    // from the built stylesheet, and `var(--spacing-bar-sm)` would resolve to
    // nothing. The declaration survives as the place "why 32" is recorded, and
    // a surface that later wants a band shorter than a toolbar should reach for
    // the rung rather than invent an `h-8`.
    //
    // This list will move as bands come and go. Update it; that is the job. It
    // exists so the movement is deliberate rather than discovered later.
    expect(rungsUsed("bar")).toEqual(["lg", "md"]);
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

describe("bordered controls share one hover border", () => {
  it("recolors the existing border without adding an outline", () => {
    const css = readFileSync(join(__dirname, "..", "styles.css"), "utf8");
    const token = /--control-border-hover:\s*([^;]+);/.exec(css)?.[1] ?? "";

    expect(token).toContain("var(--color-border-emphasized) 75%");
    expect(token).toContain("var(--color-text-secondary)");
    expect(css).toContain("border-color: var(--control-border-hover)");
    expect(css).not.toContain("--shadow-control-hover");
    expect(css).toContain("transition-duration: var(--duration-fast)");
    expect(css).toContain("transition-timing-function: var(--ease-standard)");

    for (const selector of [
      ".astryx-text-input",
      ".astryx-textarea",
      ".astryx-number-input",
      ".astryx-date-input",
      ".astryx-time-input",
      ".astryx-selector",
      ".astryx-typeahead",
      ".astryx-tokenizer",
      'button[data-tone="outline"]',
      ".control-hover-outline",
    ]) {
      expect(css, `${selector} has drifted off the canonical hover border`).toContain(selector);
    }

    expect(css).toContain(":hover:not(:focus-within)");
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

  /**
   * `laitTheme.ts` is not what the browser reads. `src/theme/lait.css` is, and
   * it is a SEPARATE build step (`astryx theme build`) whose output is
   * committed.
   *
   * That gap silently ate a day's worth of theme work: the generator was re-run,
   * `laitTheme.ts` held the new overrides, every test that read it passed, and
   * the running app kept serving a `lait.css` from hours earlier. Nothing was
   * wrong except that the artifact the page loads had never been rebuilt.
   *
   * So the test reads the CSS, not the source. If an override is in the theme
   * and not in the stylesheet, the stylesheet is stale — run `npm run
   * tokens:astryx`, which now chains both steps.
   */
  it("the built stylesheet carries the overrides, not just the theme source", () => {
    const css = readFileSync(join(SRC_DIR, "theme/lait.css"), "utf8");

    for (const [what, needle] of [
      ["the menu row's hover token", ".astryx-dropdown-menu-item:hover"],
      ["the menu row's focus token", ".astryx-dropdown-menu-item:focus-visible"],
      ["the popover's hairline", ".astryx-popover"],
      ["the pill radius", ".astryx-button"],
    ] as const) {
      expect(
        css,
        `${what} is missing from src/theme/lait.css. The theme source and the ` +
          "stylesheet have diverged — run `npm run tokens:astryx`.",
      ).toContain(needle);
    }

    // The one that actually rotted: a token whose value must track palette.mjs.
    const body = /--color-background-body:\s*light-dark\(([^)]*\)),\s*([^)]*\))\)/.exec(css);
    expect(body, "no --color-background-body in the built stylesheet").not.toBeNull();

    const theme = readFileSync(join(SRC_DIR, "theme/laitTheme.ts"), "utf8");
    const fromTheme = /"--color-background-body":\s*\[([^\]]*)\]/.exec(theme)?.[1] ?? "";
    const darkFromTheme = [...fromTheme.matchAll(/oklch\([^)]*\)/g)].map((m) => m[0])[1];

    expect(
      body?.[2],
      "The stylesheet's page background disagrees with the theme's. This is the " +
        "stale-artifact failure: `npm run tokens:astryx`.",
    ).toBe(darkFromTheme);
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
    const css = readFileSync(
      join(SRC_DIR, "../../products/issues-app/assets/web/index.css"),
      "utf8",
    );
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

/**
 * `interactiveRow` brings NO layout — "content layout remains the caller's
 * concern" is the first line of its own doc — so a caller that wants its
 * children in a row has to say `flex` itself.
 *
 * Three call sites did not, and the failure is silent: the element still
 * renders, still hovers, still focuses. It just lays its children out as inline
 * content, because a `<button>` is inline-block. A sub-issue row put its status
 * glyph on one line and ran the key straight into the title on the next —
 * "EXEC-28exec::Package composition" — and nothing threw.
 *
 * They arrived that way in the Astryx migration: each used to wrap lait's own
 * `Button`, whose recipe carried `inline-flex … gap-1.5`, and the migration
 * swapped it for a bare `<button>` + this recipe. The `justify-start` left
 * behind in every one of those class strings is the fingerprint — it does
 * nothing at all outside a flex box.
 *
 * Scoped to `interactiveRow` deliberately. The broad version of this test —
 * "any alignment utility needs a flex in the same string" — flags seven call
 * sites that are all correct, because the box legitimately comes from the
 * recipe being composed with (`Toolbar`, `navigationItem`, `Badge`, Astryx's
 * `Button`). `interactiveRow` is the only recipe in the codebase that brings
 * none, which is exactly what makes it the only one worth policing.
 */
/**
 * The seam with Astryx, held open on purpose.
 *
 * Astryx is the system and it is serving us well. What needs guarding is not
 * Astryx — it is the four places where we have taken something over from it,
 * because every one of those is a place where the library still *offers* the
 * thing we replaced and nothing says so at the call site.
 *
 * Our theme is `@scope`d to `[data-astryx-theme="lait"]` and layered after
 * `astryx-base`, so where it speaks it wins outright. That is the whole
 * mechanism, and it is also the whole hazard: winning is silent. A property we
 * set here does not merely add to Astryx's — it takes the property, along with
 * every state Astryx was using it to express.
 *
 * Four guards, one per takeover. None of them polices taste; each one catches a
 * change that would otherwise look completely fine and be wrong.
 */
describe("what we take over from Astryx, we own outright", () => {
  const themeSource = () => readFileSync(join(SRC_DIR, "theme/laitTheme.ts"), "utf8");
  const buttonBlocks = (src: string) =>
    src.slice(src.indexOf('"button": {'), src.indexOf('"popover": {'));

  /** Files that exhibit the surface rather than use it. */
  const SHOWCASE = new Set(["proof.tsx"]);

  /**
   * Only files that actually import Astryx's Button are Astryx's to police.
   * `SelectionToolbar` declares a local `Button` of its own — a formatting
   * control that must never take focus, so it cannot be one of these — and
   * matching on the tag name alone would file every one of its fourteen
   * buttons as a violation. Keying on the import means the guard switches
   * itself back on the day that file starts using the real one.
   */
  const usesAstryxButton = (src: string) =>
    /import \{[^}]*\b(?:Button|IconButton)\b[^}]*\} from "@astryxdesign\/core"/.test(src);

  /**
   * Comments are prose, and this file's prose is full of example markup —
   * `primitives.tsx` explains the variant system by writing `<Button
   * variant="ghost">` in a doc block. Blanked rather than deleted so line
   * numbers in a failure still point at the real line.
   */
  const withoutComments = (src: string) =>
    src.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));

  /**
   * Astryx's own `:active` press feedback is a `backgroundImage` overlay. So a
   * variant we give a resting `backgroundImage` to has quietly taken the press
   * state with it, and pressing that control stops looking like anything at
   * all. Nothing breaks, nothing warns; the button just goes dead under the
   * finger. This is the exact trap the convex/concave surfaces walked into.
   */
  it("a variant with a resting backgroundImage states its own :active", () => {
    const block = buttonBlocks(themeSource());
    const variants = [...block.matchAll(/"(variant:[\w-]+)": \{([\s\S]*?)\n {6}\}/g)];
    expect(variants.length, "no variant blocks parsed out of laitTheme.ts").toBeGreaterThan(0);

    const mute = variants
      .filter((m) => /"backgroundImage":/.test(m[2] ?? "") && !/":active":/.test(m[2] ?? ""))
      .map((m) => m[1]!);

    expect(
      mute,
      "These variants set a resting `backgroundImage`, which is where Astryx " +
        "keeps its press overlay — so they have taken the pressed state over " +
        "without replacing it, and pressing them does nothing visible. Give " +
        "each one a `:active` block in laitTheme.ts.",
    ).toEqual([]);
  });

  /**
   * `laitTheme.ts` is a *source*. The browser reads `lait.css`, which is
   * produced from it by `astryx theme build`, and the two drift the moment
   * somebody edits the theme and skips the build. There is already a guard for
   * this shape on the menu row; the button surface is now the larger override
   * and carries the same risk, so it gets the same guard — every declaration
   * the theme states, present verbatim in the stylesheet.
   */
  it("every button declaration in the theme survives into the stylesheet", () => {
    const declared = [
      ...buttonBlocks(themeSource()).matchAll(
        /"(?:backgroundImage|boxShadow|backgroundColor|color|borderRadius)":\s*"([^"]+)"/g,
      ),
    ].map((m) => m[1]!);
    expect(declared.length, "no button declarations parsed out of laitTheme.ts").toBeGreaterThan(10);

    const css = readFileSync(join(SRC_DIR, "theme/lait.css"), "utf8");
    expect(
      declared.filter((value) => !css.includes(value)),
      "The theme source and the built stylesheet have diverged: these values " +
        "are in laitTheme.ts and not in lait.css. Run `npm run tokens:astryx`.",
    ).toEqual([]);
  });

  /**
   * Astryx ships `destructive`; we added `danger`, and they do not look alike —
   * theirs is a filled call to action, ours is a quiet control that reddens
   * under the pointer, for revoking and retrying inside a surface that is
   * already alarmed. Both type-check, because our variants reach the type
   * system through the generated `lait.variants.d.ts`. So reaching for the
   * name Astryx documents silently gets a button we never designed.
   *
   * The rule is the general one rather than a ban on that single word: the
   * vocabulary is what our theme defines, plus `ghost`, which we deliberately
   * leave to Astryx because it has no surface for us to take over.
   */
  it("call sites use the variants our theme defines, and ghost", () => {
    const themed = new Set(
      [...buttonBlocks(themeSource()).matchAll(/"variant:([\w-]+)":/g)].map((m) => m[1]!),
    );
    expect(themed.size, "no variants parsed out of laitTheme.ts").toBeGreaterThan(2);
    const ours = new Set([...themed, "ghost"]);

    const offenders: string[] = [];
    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      if (name.endsWith(".test.tsx") || SHOWCASE.has(name)) continue;
      const raw = readFileSync(file, "utf8");
      if (!usesAstryxButton(raw)) continue;
      withoutComments(raw)
        .split("\n")
        .forEach((line, i) => {
          const m = /<(?:Button|IconButton)\b[^>]*?\bvariant="([\w-]+)"/.exec(line);
          if (m && !ours.has(m[1]!)) offenders.push(`${name}:${i + 1}  variant="${m[1]}"`);
        });
    }

    expect(
      offenders,
      "These call sites use a Button variant our theme does not define, so they " +
        "render Astryx's treatment rather than ours. `destructive` is the one " +
        "that catches people: we spell it `danger`, and ours is quiet until " +
        "hovered.",
    ).toEqual([]);
  });

  /**
   * Two props Astryx still offers that no longer mean anything here.
   *
   * `elevation` was a lift spelled at the call site — twenty-nine buttons
   * carried it and one forgot. The variant carries its own surface now, so the
   * prop is inert on every filled variant and misleading on the rest.
   *
   * `size` is worse, because its default is wrong for us: Astryx defaults to
   * `md` (32px) and the house height is `sm` (28px), so a button that simply
   * omits the prop comes out a head taller than everything beside it. That is
   * how fifteen of them ended up oversized in Specs and Settings.
   */
  it("no call site spells elevation, or leans on Astryx's size default", () => {
    const elevation: string[] = [];
    const unsized: string[] = [];

    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      if (name.endsWith(".test.tsx") || SHOWCASE.has(name)) continue;
      const raw = readFileSync(file, "utf8");
      if (!usesAstryxButton(raw)) continue;
      const src = withoutComments(raw);

      for (const m of src.matchAll(/<(Button|IconButton)\b/g)) {
        let i = m.index! + m[0].length;
        let depth = 0;
        while (i < src.length) {
          const c = src[i];
          if (c === "{") depth += 1;
          else if (c === "}") depth -= 1;
          else if (c === ">" && depth === 0) break;
          i += 1;
        }
        const attrs = src.slice(m.index! + m[0].length, i);
        const at = `${name}:${src.slice(0, m.index).split("\n").length}`;
        if (/\belevation=/.test(attrs)) elevation.push(at);
        if (!/\bsize=/.test(attrs)) unsized.push(at);
      }
    }

    expect(
      elevation,
      "`elevation` is the variant's job now — a filled variant paints its own " +
        "lift in laitTheme.ts, and this prop is silently inert there.",
    ).toEqual([]);
    expect(
      unsized,
      "These buttons state no size, so they take Astryx's default of `md` " +
        "(32px). The house height is `sm` (28px); `md` is a modal's committing " +
        "action and nothing else.",
    ).toEqual([]);
  });
});

describe("interactiveRow callers declare their own layout", () => {
  it("no alignment utility beside interactiveRow() without a flex box", () => {
    const offenders: string[] = [];
    // Whole CLASS TOKENS, not substrings. `\bflex\b` looks right and is not:
    // it matches inside `flex-1`, which is a flex-GROW utility and says nothing
    // about display — the first cut of this test passed the very bug it was
    // written for because of it.
    const ALIGNS = (t: string) => /^(justify|items|gap)-/.test(t);
    const BOXES = new Set(["flex", "inline-flex", "grid", "inline-grid"]);

    for (const file of tsxFiles(SRC_DIR)) {
      const name = file.split(/[\\/]/).pop() ?? "";
      if (name.endsWith(".test.tsx")) continue;
      const src = readFileSync(file, "utf8");

      for (const m of src.matchAll(/cn\(\s*interactiveRow\([^)]*\)([\s\S]{0,300}?)\)/g)) {
        const rest = m[1] ?? "";
        const tokens = rest.split(/[\s"',]+/).filter(Boolean);
        if (tokens.some(ALIGNS) && !tokens.some((t) => BOXES.has(t))) {
          const line = src.slice(0, m.index).split("\n").length;
          offenders.push(`${name}:${line}  ${rest.replace(/\s+/g, " ").trim().slice(0, 80)}`);
        }
      }
    }

    expect(
      offenders,
      "These compose `interactiveRow()` with alignment utilities but never\n" +
        "declare a flex box. `interactiveRow` brings no layout, and a `<button>`\n" +
        "is inline-block — the children will stack and run together.\n\n" +
        offenders.join("\n"),
    ).toEqual([]);
  });
});
