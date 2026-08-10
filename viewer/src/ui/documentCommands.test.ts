import { EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import {
  DOCUMENT_HEADING,
  insertDocumentLink,
  toggleDocumentBlock,
  toggleDocumentQuote,
  toggleDocumentTask,
} from "./documentCommands";

function apply(text: string, select: { anchor: number; head: number }, command: (state: EditorState) => object) {
  const state = EditorState.create({ doc: text, selection: select });
  return state.update(command(state)).state.doc.toString();
}

describe("hidden document formatting commands", () => {
  it("writes Typst heading markers without exposing a language choice", () => {
    expect(apply("Plan", { anchor: 0, head: 4 }, (state) =>
      toggleDocumentBlock(state, DOCUMENT_HEADING(2))))
      .toBe("== Plan");
  });

  it("wraps quotes and tasks in the trusted Lait vocabulary", () => {
    expect(apply("Careful", { anchor: 0, head: 7 }, toggleDocumentQuote))
      .toBe("#quote(block: true)[Careful]");
    expect(apply("Ship it", { anchor: 0, head: 7 }, toggleDocumentTask))
      .toBe("#lait-task(false)[Ship it]");
  });

  it("keeps the link target selected inside canonical source", () => {
    const state = EditorState.create({ doc: "guide", selection: { anchor: 0, head: 5 } });
    const transaction = state.update(insertDocumentLink(state));
    expect(transaction.state.doc.toString()).toBe('#link("url")[guide]');
    expect(transaction.state.sliceDoc(
      transaction.state.selection.main.from,
      transaction.state.selection.main.to,
    )).toBe("url");
  });
});
