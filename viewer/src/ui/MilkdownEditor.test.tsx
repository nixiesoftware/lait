import { afterEach, describe, expect, it } from "vitest";
import { Editor, defaultValueCtx, remarkStringifyOptionsCtx, rootCtx } from "@milkdown/kit/core";
import { commonmark } from "@milkdown/kit/preset/commonmark";
import { gfm } from "@milkdown/kit/preset/gfm";
import { getMarkdown } from "@milkdown/kit/utils";

import { SERIALIZE } from "./MilkdownEditor";

/**
 * The whole reason Milkdown was chosen over Tiptap and Lexical: the document is
 * parsed and serialized by remark, so what the editor writes back is what it was
 * given. lait stores descriptions as plain CRDT text and `lait show` prints them
 * verbatim — an editor that normalises on save rewrites an agent's issue body
 * the moment a human touches one word.
 *
 * These drive the real editor over the real presets. A mock would only assert
 * that remark behaves the way I hoped it did.
 */
describe("Milkdown round-trip", () => {
  let editor: Editor | null = null;

  afterEach(async () => {
    await editor?.destroy();
    editor = null;
  });

  const roundTrip = async (markdown: string): Promise<string> => {
    const root = document.createElement("div");
    document.body.append(root);
    editor = await Editor.make()
      .config((ctx) => {
        ctx.set(rootCtx, root);
        ctx.set(defaultValueCtx, markdown);
        ctx.set(remarkStringifyOptionsCtx, SERIALIZE);
      })
      .use(commonmark)
      .use(gfm)
      .create();
    let out = "";
    editor.action((ctx) => {
      out = getMarkdown()(ctx);
    });
    root.remove();
    return out.trim();
  };

  it("returns prose unchanged", async () => {
    const source = "A paragraph with `code`, **bold**, *em* and a [link](https://a.b).";
    expect(await roundTrip(source)).toBe(source);
  });

  it("keeps heading levels and fenced code with its language", async () => {
    const source = "## Reproduction\n\n```bash\n$ lait serve --json\n```";
    expect(await roundTrip(source)).toBe(source);
  });

  it("keeps `-` bullets and `*` emphasis rather than remark's defaults", async () => {
    // remark would write `*` bullets and `_emphasis_`. Both are valid Markdown
    // and neither is what anyone types, so both would rewrite bodies wholesale.
    expect(await roundTrip("*emphasis* and **strong**")).toBe("*emphasis* and **strong**");
    expect(await roundTrip("- one\n- two")).toContain("- one");
  });

  it("leaves a table's content and pipes alone", async () => {
    // `tablePipeAlign: false`. remark pads every cell to the widest in its
    // column by default, so a table came back with different bytes on every
    // line even when nothing in it changed.
    const out = await roundTrip("| a | b |\n| --- | --- |\n| 1 | 2 |");
    expect(out.split("\n")[0]).toBe("| a | b |");
    expect(out.split("\n")[2]).toBe("| 1 | 2 |");
  });

  describe("known deviations", () => {
    // Two things do not come back byte-identical. Both are semantic no-ops —
    // the rendered document is the same — but they *are* rewrites, so they are
    // pinned here rather than left to be discovered in a diff.

    it("shortens a table's delimiter row to its minimum", async () => {
      const out = await roundTrip("| a | b |\n| --- | --- |\n| 1 | 2 |");
      expect(out.split("\n")[1]).toBe("| - | - |");
    });

    it("writes a tight list back as a loose one", async () => {
      // Upstream: `preset-commonmark` parses mdast `spread` into the *string*
      // `"false"` and hands it straight back to the serializer, where any
      // non-empty string is truthy. So every list is spread on the way out.
      expect(await roundTrip("- one\n- two")).toBe("- one\n\n- two");
    });
  });

  it("keeps task lists", async () => {
    const out = await roundTrip("* [x] done\n* [ ] not yet");
    expect(out).toContain("[x] done");
    expect(out).toContain("[ ] not yet");
  });

  it("does not eat a GitHub alert, even though it has no node for one", async () => {
    // We render these as tinted callouts read-only. Milkdown has no alert node,
    // so it must at least carry the text through as a plain blockquote rather
    // than dropping the marker — losing `[!WARNING]` would silently downgrade
    // every callout in the tracker the first time someone edited the body.
    const out = await roundTrip("> [!WARNING]\n> Do not do this.");
    expect(out).toContain("[!WARNING]");
    expect(out).toContain("Do not do this.");
  });
});
