import { describe, expect, it } from "vitest";

import {
  applySourceSplices,
  DOCUMENT_PREFIX,
  documentPlainText,
  escapeText,
  parseDocument,
  sourceSplices,
  upgradeMarkdown,
} from "./document";

describe("hidden Lait document model", () => {
  it("converts the supported legacy grammar into canonical Typst", () => {
    const upgraded = upgradeMarkdown([
      "# Plan",
      "",
      "Ship **carefully** with [the guide](https://example.com).",
      "",
      "- [x] preserve cursor anchors",
      "- keep arbitrary # and $ prose",
    ].join("\n"));

    expect(upgraded.source).toContain("= Plan");
    expect(upgraded.source).toContain("*carefully*");
    expect(upgraded.source).toContain('#link("https://example.com")[the guide]');
    expect(upgraded.source).toContain("#lait-task(true)[preserve cursor anchors]");
    expect(upgraded.source).toContain("keep arbitrary \\# and \\$ prose");
    expect(applySourceSplices("# Plan\n\nShip **carefully** with [the guide](https://example.com).\n\n- [x] preserve cursor anchors\n- keep arbitrary # and $ prose", upgraded.splices))
      .toBe(upgraded.source);
  });

  it("round-trips canonical blocks through the safe typed AST", () => {
    const { source } = upgradeMarkdown([
      "## Details",
      "",
      "> [!WARNING]",
      "> Keep **this** visible.",
      "",
      "| Name | State |",
      "|:--|--:|",
      "| one | `ready` |",
    ].join("\n"));

    expect(parseDocument(source)).toMatchObject([
      { kind: "heading", level: 2 },
      { kind: "callout", tone: "warning" },
      { kind: "table", align: ["left", "right"] },
    ]);
  });

  it("projects compiler-valid callout and table spellings into semantic blocks", () => {
    const source = `${DOCUMENT_PREFIX}#lait-callout("warning", [Keep *this* visible.])

#lait-table(
  header: (
    [Name],
    [State],
  ),
  rows: (
    (
      [editor],
      [ready],
    ),
  ),
  align: (
    "left",
    "right",
  ),
)`;

    expect(parseDocument(source)).toMatchObject([
      {
        kind: "callout",
        tone: "warning",
        children: [
          { kind: "text", text: "Keep " },
          { kind: "strong", children: [{ kind: "text", text: "this" }] },
          { kind: "text", text: " visible." },
        ],
      },
      {
        kind: "table",
        align: ["left", "right"],
        head: [
          [{ kind: "text", text: "Name" }],
          [{ kind: "text", text: "State" }],
        ],
        rows: [[
          [{ kind: "text", text: "editor" }],
          [{ kind: "text", text: "ready" }],
        ]],
      },
    ]);
  });

  it("computes ordered scalar splices without splitting Unicode", () => {
    const before = "A 🛰️ cursor and bold text";
    const after = "= A 🛰️ cursor and *bold* text";
    const splices = sourceSplices(before, after);

    expect(applySourceSplices(before, splices)).toBe(after);
    expect(splices).toEqual([...splices].sort((a, b) => b.index - a.index));
  });

  it("escapes every Typst introducer in ordinary user text", () => {
    expect(escapeText("# [x] * _ ` $ <tag> @ref \\"))
      .toBe("\\# \\[x\\] \\* \\_ \\` \\$ \\<tag\\> \\@ref \\\\");
  });

  it("copies document meaning without exposing its serialization", () => {
    const { source } = upgradeMarkdown("# Plan\n\nShip **carefully**.");
    expect(documentPlainText(source)).toBe("Plan\n\nShip carefully.");
    expect(documentPlainText(source)).not.toContain("lait-document");
  });
});
