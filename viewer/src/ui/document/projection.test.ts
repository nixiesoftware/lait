import { describe, expect, it } from "vitest";

import { serializeDocument, upgradeMarkdown } from "../../core/document";
import {
  blocksFromDocumentNode,
  documentNodeFromSource,
  editorPosition,
  projectDocument,
  projectSource,
  projectionSplice,
  sourcePosition,
} from "./projection";
import { laitDocumentSchema, safeDocumentHref } from "./schema";

describe("Lait document projection", () => {
  it("round-trips the controlled Typst vocabulary without a second stored format", () => {
    const source = upgradeMarkdown([
      "## Details",
      "",
      "Ship **carefully** with [the guide](https://example.com) and ENG-7.",
      "",
      "> [!WARNING]",
      "> Preserve cursors.",
      "",
      "- [x] migrate issues",
      "- keep Typst hidden",
      "",
      "```rust",
      "fn main() {}",
      "```",
      "",
      "| Name | State |",
      "|:--|--:|",
      "| editor | ready |",
    ].join("\n")).source;

    const projection = projectSource(source);
    expect(projection.canonical).toBe(true);
    expect(projection.source).toBe(source);
    expect(blocksFromDocumentNode(projection.doc)).toMatchObject([
      { kind: "heading", level: 2 },
      { kind: "paragraph" },
      { kind: "callout", tone: "warning" },
      { kind: "list" },
      { kind: "code", lang: "rust" },
      { kind: "table" },
    ]);
  });

  it("keeps Typst and editor coordinates connected across hidden syntax and Unicode", () => {
    const source = upgradeMarkdown("# Plan\n\nA 🛰️ **bold** step.").source;
    const projection = projectSource(source);
    const sourceBold = Array.from(source.slice(0, source.indexOf("bold"))).length;
    const editorBold = editorPosition(projection, sourceBold);

    expect(projection.doc.textBetween(0, projection.doc.content.size, "\n")).toContain("bold");
    expect(sourcePosition(projection, editorBold)).toBe(sourceBold);
    expect(editorPosition(projection, sourcePosition(projection, editorBold + 2))).toBe(editorBold + 2);
  });

  it("serializes editor changes back to a minimal scalar splice", () => {
    const before = upgradeMarkdown("A🙂C").source;
    const doc = documentNodeFromSource(before);
    const paragraph = doc.firstChild!;
    const changed = doc.copy(
      doc.content.replaceChild(0, paragraph.copy(paragraph.content.append(
        documentNodeFromSource(upgradeMarkdown("B").source).firstChild!.content,
      ))),
    );
    const after = projectDocument(changed).source;
    const splice = projectionSplice(before, after);

    expect(splice).not.toBeNull();
    const scalars = Array.from(before);
    scalars.splice(splice!.index, splice!.delete, ...Array.from(splice!.insert));
    expect(scalars.join("")).toBe(after);
  });

  it("keeps a mark open across semantic inline components", () => {
    const strong = laitDocumentSchema.marks.strong!.create();
    const paragraph = laitDocumentSchema.nodes.paragraph!.create(null, [
      laitDocumentSchema.text("See ", [strong]),
      laitDocumentSchema.nodes.issue_ref!.create({ ref: "ENG-7" }, null, [strong]),
      laitDocumentSchema.text(" now", [strong]),
    ]);
    const doc = laitDocumentSchema.nodes.doc!.create(null, paragraph);

    expect(projectDocument(doc).source).toContain("*See ENG-7 now*");
    expect(projectDocument(doc).source).not.toContain("**ENG-7**");
  });

  it("round-trips raw code whose contents cannot use a Typst fence", () => {
    const source = serializeDocument([{
      kind: "code",
      lang: "text",
      text: "before\n```nested\nafter \\\"quoted\\\"",
    }]);
    const projection = projectSource(source);

    expect(source).toContain("#raw(block: true");
    expect(projection.canonical).toBe(true);
    expect(projection.source).toBe(source);
  });

  it("keeps interactive document links on web protocols", () => {
    expect(safeDocumentHref("https://example.com/guide")).toBe("https://example.com/guide");
    expect(safeDocumentHref("javascript:alert(1)")).toBeNull();
    expect(safeDocumentHref("file:///secret")).toBeNull();
  });

});
