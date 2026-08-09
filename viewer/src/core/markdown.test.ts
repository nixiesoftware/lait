import { describe, expect, it } from "vitest";

import {
  CALLOUT_TONES,
  looksLikeMarkdown,
  parseInline,
  parseMarkdown,
  parseRefs,
  type Block,
  type Inline,
} from "./markdown";

describe("looksLikeMarkdown", () => {
  it("stays out of plain prose", () => {
    expect(looksLikeMarkdown("just a sentence.")).toBe(false);
    expect(looksLikeMarkdown("two lines\nof plain text")).toBe(false);
    // A lone asterisk mid-sentence is arithmetic, not emphasis.
    expect(looksLikeMarkdown("2 * 3 = 6")).toBe(false);
  });

  it("wakes up for the marks people actually type", () => {
    expect(looksLikeMarkdown("# heading")).toBe(true);
    expect(looksLikeMarkdown("- a list")).toBe(true);
    expect(looksLikeMarkdown("with `code` inline")).toBe(true);
    expect(looksLikeMarkdown("**bold** claim")).toBe(true);
    expect(looksLikeMarkdown("see [docs](https://example.com)")).toBe(true);
  });
});

describe("blocks", () => {
  it("parses headings h1–h4 and leaves ##### as prose", () => {
    expect(parseMarkdown("## Title")[0]).toMatchObject({ kind: "heading", level: 2 });
    expect(parseMarkdown("##### deep")[0]!.kind).toBe("paragraph");
  });

  it("keeps a fence's body verbatim, and EOF closes an unclosed fence", () => {
    const [block] = parseMarkdown("```rust\nlet x = **not bold**;\n```");
    expect(block).toMatchObject({ kind: "code", lang: "rust", text: "let x = **not bold**;" });
    const [open] = parseMarkdown("```\ndangling");
    expect(open).toMatchObject({ kind: "code", text: "dangling" });
  });

  it("groups consecutive bullets into one list, and reads checklists", () => {
    const [list] = parseMarkdown("- [x] done\n- [ ] not yet\n- plain") as [
      Extract<Block, { kind: "list" }>,
    ];
    expect(list.kind).toBe("list");
    expect(list.items.map((i) => i.checked)).toEqual([true, false, null]);
  });

  it("distinguishes ordered from unordered lists", () => {
    expect(parseMarkdown("1. a\n2. b")[0]).toMatchObject({ kind: "list", ordered: true });
    expect(parseMarkdown("* a")[0]).toMatchObject({ kind: "list", ordered: false });
  });

  it("merges consecutive quote lines into one quote", () => {
    const blocks = parseMarkdown("> first\n> second");
    expect(blocks).toHaveLength(1);
    expect(blocks[0]!.kind).toBe("quote");
  });

  it("collapses a soft line break inside a paragraph", () => {
    // Reversed with the document treatment. Keeping every source newline made
    // sense when the body was a narrow pane echoing the CLI; at a 35rem measure
    // it broke sentences wherever the author's editor happened to wrap.
    const [p] = parseMarkdown("line one\nline two") as [Extract<Block, { kind: "paragraph" }>];
    expect(p.children).toEqual([{ kind: "text", text: "line one line two" }]);
  });

  it("reads a rule, and blank lines split paragraphs", () => {
    const kinds = parseMarkdown("a\n\n---\n\nb").map((b) => b.kind);
    expect(kinds).toEqual(["paragraph", "hr", "paragraph"]);
  });
});

describe("inlines", () => {
  it("gives ** precedence over *", () => {
    expect(parseInline("**bold**")[0]!.kind).toBe("strong");
  });

  it("keeps code content literal", () => {
    expect(parseInline("`**x**`")).toEqual([{ kind: "code", text: "**x**" }]);
  });

  it("does not read snake_case as emphasis", () => {
    expect(parseInline("a snake_case_name here")).toEqual([
      { kind: "text", text: "a snake_case_name here" },
    ]);
  });

  it("links only http(s); other schemes stay text", () => {
    expect(parseInline("[x](https://a.b)")[0]).toMatchObject({ kind: "link", href: "https://a.b" });
    // `javascript:` must never become an href — the whole safety argument.
    const hostile = parseInline("[x](javascript:alert(1))");
    expect(hostile.every((i) => i.kind !== "link")).toBe(true);
  });

  it("autolinks bare URLs without eating trailing punctuation", () => {
    const parts = parseInline("see https://example.com/a, ok");
    expect(parts[1]).toMatchObject({ kind: "link", href: "https://example.com/a" });
    expect(parts[2]).toEqual({ kind: "text", text: ", ok" });
  });
});

describe("tables", () => {
  it("parses a pipe table with a header and alignment", () => {
    const [block] = parseMarkdown("| a | b | c |\n|:--|:-:|--:|\n| 1 | 2 | 3 |");
    expect(block).toMatchObject({ kind: "table", align: ["left", "center", "right"] });
    const table = block as Extract<Block, { kind: "table" }>;
    expect(table.head.map((c) => (c[0] as { text: string }).text)).toEqual(["a", "b", "c"]);
    expect(table.rows).toHaveLength(1);
    expect(table.rows[0]!.map((c) => (c[0] as { text: string }).text)).toEqual(["1", "2", "3"]);
  });

  it("accepts rows without outer pipes", () => {
    const [block] = parseMarkdown("a | b\n--- | ---\n1 | 2");
    const table = block as Extract<Block, { kind: "table" }>;
    expect(table.kind).toBe("table");
    expect(table.head).toHaveLength(2);
    expect(table.rows[0]).toHaveLength(2);
  });

  it("formats inside cells", () => {
    const [block] = parseMarkdown("| a |\n| --- |\n| `x` |");
    const table = block as Extract<Block, { kind: "table" }>;
    expect(table.rows[0]![0]![0]).toEqual({ kind: "code", text: "x" });
  });

  it("does not eat a sentence that merely contains a pipe", () => {
    // The delimiter row is required. Without it `a | b` is prose, and turning
    // it into a table would silently restructure someone's paragraph.
    const [block] = parseMarkdown("run foo | grep bar to check");
    expect(block!.kind).toBe("paragraph");
  });

  it("ends the paragraph above it rather than being absorbed", () => {
    const blocks = parseMarkdown("Here are the counts:\n| a |\n| --- |\n| 1 |");
    expect(blocks.map((b) => b.kind)).toEqual(["paragraph", "table"]);
  });
});

describe("callouts", () => {
  it("reads GitHub alert syntax as a callout", () => {
    const [block] = parseMarkdown("> [!WARNING]\n> Do not do this.");
    expect(block).toMatchObject({ kind: "callout", tone: "warning" });
    const callout = block as Extract<Block, { kind: "callout" }>;
    expect((callout.children[0] as { text: string }).text).toBe("Do not do this.");
  });

  it("accepts every tone, case-insensitively", () => {
    for (const tone of CALLOUT_TONES) {
      const [block] = parseMarkdown(`> [!${tone.toUpperCase()}]\n> body`);
      expect(block).toMatchObject({ kind: "callout", tone });
    }
  });

  it("leaves an ordinary quote alone", () => {
    const [block] = parseMarkdown("> just a quotation");
    expect(block!.kind).toBe("quote");
  });

  it("does not fire on a quote that merely starts with a bracket", () => {
    const [block] = parseMarkdown("> [!not-a-tone] still a quote");
    expect(block!.kind).toBe("quote");
  });
});

describe("heading anchors", () => {
  it("slugs the heading text", () => {
    const [block] = parseMarkdown("## Fix options, part 2");
    expect(block).toMatchObject({ kind: "heading", id: "fix-options-part-2" });
  });

  it("de-duplicates repeated headings so an anchor is unambiguous", () => {
    const blocks = parseMarkdown("## Notes\n\n## Notes\n\n## Notes");
    expect(blocks.map((b) => (b as { id: string }).id)).toEqual(["notes", "notes-2", "notes-3"]);
  });

  it("always produces a usable anchor", () => {
    const [block] = parseMarkdown("### ???");
    expect((block as { id: string }).id).toBe("section");
  });
});

describe("soft line breaks", () => {
  it("joins a hard-wrapped paragraph into one run", () => {
    // The body is a document at a 35rem measure now. Text wrapped at 78 columns
    // by someone's editor used to break mid-sentence wherever that editor
    // happened to stop; the rendered width is the browser's business.
    const [block] = parseMarkdown("one two\nthree four\nfive");
    expect(block).toMatchObject({ kind: "paragraph" });
    const text = (block as Extract<Block, { kind: "paragraph" }>).children
      .map((i) => (i as { text: string }).text)
      .join("");
    expect(text).toBe("one two three four five");
  });

  it("keeps a break the author actually asked for", () => {
    // Two trailing spaces, or a trailing backslash — the two spellings every
    // other Markdown tool accepts for "break here on purpose".
    for (const source of ["one  \ntwo", "one\\\ntwo"]) {
      const [block] = parseMarkdown(source);
      const text = (block as Extract<Block, { kind: "paragraph" }>).children
        .map((i) => (i as { text: string }).text)
        .join("");
      expect(text).toBe("one\ntwo");
    }
  });

  it("still ends a paragraph at a blank line", () => {
    expect(parseMarkdown("one\ntwo\n\nthree").map((b) => b.kind)).toEqual([
      "paragraph",
      "paragraph",
    ]);
  });

  it("applies the same rule inside quotes and callouts", () => {
    const [quote] = parseMarkdown("> one\n> two");
    expect(
      (quote as Extract<Block, { kind: "quote" }>).children
        .map((i) => (i as { text: string }).text)
        .join(""),
    ).toBe("one two");

    const [callout] = parseMarkdown("> [!NOTE]\n> one\n> two");
    expect(
      (callout as Extract<Block, { kind: "callout" }>).children
        .map((i) => (i as { text: string }).text)
        .join(""),
    ).toBe("one two");
  });
});

describe("issue refs", () => {
  const refs = (parts: Inline[]) =>
    parts.filter((p) => p.kind === "ref").map((p) => (p as { ref: string }).ref);

  it("reads a bare alias out of a sentence", () => {
    expect(refs(parseInline("blocked on ENG-142 until the gate lands"))).toEqual(["ENG-142"]);
  });

  it("keeps the collision suffix the engine mints", () => {
    // Two issues created in the same second get `ENG-7` and `ENG-7b`.
    expect(refs(parseInline("see ENG-7b"))).toEqual(["ENG-7b"]);
  });

  it("will not half-match a longer token", () => {
    // Bounded on both sides by a non-word: a key is at most 8 letters, and the
    // boundary is what stops `SUPERSENG-1` reading as `SENG-1`.
    expect(refs(parseInline("SUPERSENG-1"))).toEqual([]);
    expect(refs(parseInline("ENG-142-b"))).toEqual([]);
    expect(refs(parseInline("release-ENG-1"))).toEqual([]);
  });

  it("finds one after a path separator, which is a boundary", () => {
    expect(refs(parseInline("docs/ENG-1"))).toEqual(["ENG-1"]);
  });

  it("stays out of code, links and URLs", () => {
    expect(refs(parseInline("`ENG-1`"))).toEqual([]);
    expect(refs(parseInline("[text](https://x.test/ENG-1)"))).toEqual([]);
    expect(refs(parseInline("https://x.test/ENG-1"))).toEqual([]);
  });

  it("resolves inside a link's own text, where prose still applies", () => {
    const [link] = parseInline("[see ENG-1](https://x.test)");
    expect(link!.kind).toBe("link");
    expect(refs((link as Extract<Inline, { kind: "link" }>).children)).toEqual(["ENG-1"]);
  });

  it("is uppercase only, because that is the form the product shows", () => {
    expect(refs(parseInline("eng-142"))).toEqual([]);
    expect(refs(parseInline("step-1 then part-2"))).toEqual([]);
  });

  it("reports acronyms too, and lets the resolver settle them", () => {
    // The parser has no catalog. These are candidates; `UTF` and `SHA` name no
    // project, so the resolver rejects them without a round trip and they
    // render as the words they are.
    expect(refs(parseInline("UTF-8 and SHA-256"))).toEqual(["UTF-8", "SHA-256"]);
  });

  it("does not drag plain prose into the block grammar", () => {
    // The cost of the line above: `looksLikeMarkdown` must NOT fire on a ref,
    // or a plain paragraph mentioning SHA-256 would start folding its own line
    // breaks the way a Markdown paragraph does.
    expect(looksLikeMarkdown("hashed with SHA-256\nand stored")).toBe(false);
  });

  it("parseRefs leaves every other character exactly where it was", () => {
    const parts = parseRefs("a\n\n  b ENG-1 c  \n");
    const text = parts
      .map((p) => (p.kind === "ref" ? p.ref : (p as { text: string }).text))
      .join("");
    expect(text).toBe("a\n\n  b ENG-1 c  \n");
    expect(refs(parts)).toEqual(["ENG-1"]);
  });
});

describe("underline", () => {
  it("reads the one HTML tag the grammar admits", () => {
    const [u] = parseInline("<u>held</u>");
    expect(u!.kind).toBe("underline");
    expect(looksLikeMarkdown("a <u>b</u> c")).toBe(true);
  });

  it("parses its content like any other emphasis", () => {
    const [u] = parseInline("<u>**held**</u>");
    const inner = (u as Extract<Inline, { kind: "underline" }>).children;
    expect(inner[0]!.kind).toBe("strong");
  });

  it("leaves every other tag as the text it is", () => {
    expect(parseInline("<script>x</script>").every((p) => p.kind === "text")).toBe(true);
    expect(parseInline("<b>x</b>").every((p) => p.kind === "text")).toBe(true);
  });
});
