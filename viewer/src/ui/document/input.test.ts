import { describe, expect, it } from "vitest";
import { EditorState, TextSelection } from "prosemirror-state";

import { documentNodeFromSource, projectDocument } from "./projection";
import { laitDocumentSchema } from "./schema";
import {
  markdownBlockEnter,
  markdownBlockInput,
  markdownInlineInput,
  matchingSlashCommands,
  runSlashCommand,
} from "./input";
import { DOCUMENT_PREFIX } from "../../core/document";

function state(text: string): EditorState {
  const paragraph = laitDocumentSchema.nodes.paragraph!.create(
    null,
    text ? laitDocumentSchema.text(text) : null,
  );
  const doc = laitDocumentSchema.nodes.doc!.create(null, paragraph);
  return EditorState.create({
    schema: laitDocumentSchema,
    doc,
    selection: TextSelection.create(doc, text.length + 1),
  });
}

describe("semantic document input", () => {
  it("turns Markdown heading and list gestures into canonical Typst blocks", () => {
    const heading = markdownBlockInput(state("##"), 3, 3, " ");
    expect(heading?.doc.firstChild?.type.name).toBe("heading");
    expect(heading?.doc.firstChild?.attrs.level).toBe(2);
    expect(projectDocument(heading!.doc).source).toBe(`${DOCUMENT_PREFIX}== `);

    const bullet = markdownBlockInput(state("-"), 2, 2, " ");
    expect(bullet?.doc.firstChild?.type.name).toBe("bullet_list");
    expect(projectDocument(bullet!.doc).source).toBe(`${DOCUMENT_PREFIX}- `);

    const task = markdownBlockInput(state("- [ ]"), 6, 6, " ");
    expect(task?.doc.firstChild?.firstChild?.attrs.checked).toBe(false);
    expect(projectDocument(task!.doc).source).toBe(`${DOCUMENT_PREFIX}#lait-task(false)[]`);
  });

  it("turns Markdown links and emphasis into Typst marks without storing Markdown", () => {
    const linkState = state("Read [guide](https://example.com");
    const link = markdownInlineInput(linkState, linkState.selection.from, linkState.selection.to, ")");
    expect(link?.doc.textContent).toBe("Read guide");
    expect(projectDocument(link!.doc).source)
      .toBe(`${DOCUMENT_PREFIX}Read #link("https://example.com/")[guide]`);

    const strongState = state("Make **this*");
    const strong = markdownInlineInput(
      strongState,
      strongState.selection.from,
      strongState.selection.to,
      "*",
    );
    expect(projectDocument(strong!.doc).source).toBe(`${DOCUMENT_PREFIX}Make *this*`);
  });

  it("recognizes fenced code and dividers on Enter", () => {
    const code = markdownBlockEnter(state("```rust"));
    expect(code?.doc.firstChild?.type.name).toBe("code_block");
    expect(code?.doc.firstChild?.attrs.language).toBe("rust");

    const divider = markdownBlockEnter(state("---"));
    expect(divider?.doc.firstChild?.type.name).toBe("horizontal_rule");
    expect(divider?.doc.childCount).toBe(2);
  });

  it("filters and executes slash commands as semantic blocks", () => {
    expect(matchingSlashCommands("head").map((command) => command.label))
      .toEqual(["Heading 1", "Heading 2", "Heading 3", "Heading 4"]);
    const commandState = state("/bullet");
    const transaction = runSlashCommand(commandState, "bullet-list");
    expect(transaction?.doc.firstChild?.type.name).toBe("bullet_list");
    const roundTrip = documentNodeFromSource(projectDocument(transaction!.doc).source);
    expect(roundTrip.firstChild?.type.name).toBe("bullet_list");
  });
});
