import { describe, expect, it } from "vitest";

import { parseMarkdown } from "./markdown";
import { sourceOffsetAt } from "./sourceMap";

/**
 * The caret has to land where the eye did.
 *
 * Every case here is written the same way: take a source, say where in the
 * RENDERED text the click was, and assert what the source says at the offset
 * that comes back. Asserting the character rather than the number is
 * deliberate — a number tells you the test passed, a character tells you the
 * caret is in front of the right word.
 */
const at = (source: string, rendered: number) => source.slice(sourceOffsetAt(source, rendered));

describe("mapping a rendered offset back to source", () => {
  it("is the identity for plain prose", () => {
    const source = "the supervisor hands the descriptor";
    expect(sourceOffsetAt(source, 0)).toBe(0);
    expect(at(source, 4)).toBe("supervisor hands the descriptor");
    expect(at(source, 15)).toBe("hands the descriptor");
  });

  /** The whole reason this exists: a paragraph the author hard-wrapped renders
   *  as one flowed run, so every offset past the first line is shifted by the
   *  newlines that became spaces. */
  it("counts a joined newline as the one space it renders as", () => {
    const source = "one two\nthree four\nfive";
    // "one two three four five"
    //           ^ 8 — the first character after the newline-as-space
    expect(at(source, 8)).toBe("three four\nfive");
    expect(at(source, 19)).toBe("five");
  });

  it("steps over a heading's hashes", () => {
    const source = "## This is a bridge";
    expect(sourceOffsetAt(source, 0)).toBe(3);
    expect(at(source, 8)).toBe("a bridge");
  });

  it("steps over emphasis marks without losing the words inside", () => {
    const source = "the format **already specifies** attachment";
    // renders: "the format already specifies attachment"
    expect(at(source, 11)).toBe("already specifies** attachment");
    expect(at(source, 19)).toBe("specifies** attachment");
    expect(at(source, 29)).toBe("attachment");
  });

  it("steps over inline code fences", () => {
    const source = "so `Virtualized` uses the values-only path";
    expect(at(source, 3)).toBe("Virtualized` uses the values-only path");
    expect(at(source, 15)).toBe("uses the values-only path");
  });

  /** A link renders its label and hides its target, so an offset inside the
   *  label maps into the label and everything after it clears the whole
   *  `](href)` tail in one step. */
  it("maps inside a link's label, and past its target", () => {
    const source = "see [the spec](https://example.com/x) for more";
    expect(at(source, 4)).toBe("the spec](https://example.com/x) for more");
    expect(at(source, 8)).toBe("spec](https://example.com/x) for more");
    expect(at(source, 13)).toBe("for more");
  });

  it("steps over a bullet and its checkbox", () => {
    expect(at("- a second, purpose-built path", 0)).toBe("a second, purpose-built path");
    expect(at("- [ ] ship the bridge", 0)).toBe("ship the bridge");
    expect(at("3. ship the bridge", 5)).toBe("the bridge");
  });

  it("steps over a quote marker", () => {
    expect(at("> the carrier already specifies it", 0)).toBe("the carrier already specifies it");
  });

  /** Inside a fence nothing is markup, so the walk must not eat a `*` or a `#`
   *  that happens to be code. */
  it("copies a fenced block through verbatim", () => {
    const source = "```rust\nlet x = *ptr; // ## not a heading\n```";
    const body = source.indexOf("let x");
    expect(sourceOffsetAt(source, 8)).toBeGreaterThanOrEqual(body);
  });

  it("clamps past the end rather than running off it", () => {
    const source = "short";
    expect(sourceOffsetAt(source, 999)).toBe(source.length);
    expect(sourceOffsetAt(source, -5)).toBe(0);
  });
});

/**
 * The spans are what carry a block's slice to the mapper, so they have to
 * describe the bytes the block was actually built from.
 */
describe("blocks know where they came from", () => {
  it("spans the lines it consumed, and only those", () => {
    const source = "first para\nwrapped on\n\n## A heading\n\nlast";
    const blocks = parseMarkdown(source);
    const slices = blocks.map((b) => source.slice(b.span!.from, b.span!.to));
    expect(slices).toEqual(["first para\nwrapped on", "## A heading", "last"]);
  });

  it("keeps a fenced block's own lines, fences included", () => {
    const source = "before\n\n```js\nconst a = 1;\n```\n\nafter";
    const [, code] = parseMarkdown(source);
    expect(source.slice(code!.span!.from, code!.span!.to)).toBe("```js\nconst a = 1;\n```");
  });

  /** Round trip: an offset taken from a block's own slice has to be an offset
   *  into the whole document once its `from` is added back. */
  it("composes with the mapper to give a document offset", () => {
    const source = "intro line\n\n## Scope: same-host only\n\ntail";
    const heading = parseMarkdown(source)[1]!;
    const slice = source.slice(heading.span!.from, heading.span!.to);
    const offset = heading.span!.from + sourceOffsetAt(slice, 7);
    expect(source.slice(offset)).toBe("same-host only\n\ntail");
  });
});
