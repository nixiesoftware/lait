import { describe, expect, it } from "vitest";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";

import {
  applyTextSplice,
  codeMirrorChange,
  collaborationDecorations,
  mapOffsetThroughSplice,
  previewPhase,
  scalarOffset,
  textRevision,
  textSplice,
} from "./CodeMirrorEditor";
import { codeUnitOffset } from "../core/anchor";

describe("source-native collaborative Markdown coordinates", () => {
  it("emits the smallest Unicode-scalar splice without serializing the document", () => {
    expect(textSplice("A🙂C", "A🟢BC")).toEqual({
      index: 1,
      delete: 1,
      insert: "🟢B",
    });
    expect(textSplice("same", "same")).toBeNull();
  });

  it("round-trips CodeMirror code-unit positions through Lait scalar positions", () => {
    const text = "A🙂BC";
    expect(scalarOffset(text, 3)).toBe(2);
    expect(codeUnitOffset(text, scalarOffset(text, 3))).toBe(3);
  });

  it("maps a focused selection through insertions and replacements", () => {
    const insertion = { index: 2, delete: 0, insert: "🙂" };
    expect(mapOffsetThroughSplice(1, insertion)).toBe(1);
    expect(mapOffsetThroughSplice(2, insertion)).toBe(2);
    expect(mapOffsetThroughSplice(5, insertion)).toBe(6);

    const replacement = { index: 2, delete: 3, insert: "x" };
    expect(mapOffsetThroughSplice(3, replacement)).toBe(3);
    expect(mapOffsetThroughSplice(7, replacement)).toBe(5);
  });

  it("applies a durable merge as its minimal CodeMirror transaction", () => {
    const before = "A🙂oldZ";
    const splice = { index: 2, delete: 3, insert: "new" };
    const change = codeMirrorChange(before, splice);
    expect(change).toEqual({ from: 3, to: 6, insert: "new" });
    const transaction = EditorState.create({ doc: before }).update({ changes: change });
    expect(transaction.state.doc.toString()).toBe("A🙂newZ");
  });

  it("applies preview splices without touching invalid ranges", () => {
    expect(applyTextSplice("A🙂C", { index: 1, delete: 1, insert: "🟢B" })).toBe("A🟢BC");
    expect(applyTextSplice("short", { index: 9, delete: 0, insert: "x" })).toBeNull();
  });

  it("holds the preview caret across the optimistic-to-durable handoff", () => {
    const preview = {
      actor: "alice",
      name: "Alice",
      color: "red",
      base: textRevision("ac"),
      result: textRevision("abc"),
      index: 1,
      delete: 0,
      insert: "b",
      anchor: 2,
    };
    expect(previewPhase("ac", textRevision("ac"), preview)).toBe("optimistic");
    expect(previewPhase("abc", textRevision("abc"), preview)).toBe("settled");
    expect(previewPhase("axc", textRevision("axc"), preview)).toBeNull();
  });

  it("does not draw an unversioned cursor while its preview is between revisions", () => {
    const state = EditorState.create({ doc: "merged elsewhere" });
    const preview = {
      actor: "alice",
      name: "Alice",
      color: "red",
      base: textRevision("old"),
      result: textRevision("new"),
      index: 0,
      delete: 3,
      insert: "new",
      anchor: 3,
    };
    const decorations = collaborationDecorations(
      state,
      [{ actor: "alice", name: "Alice", color: "red", anchor: 9 }],
      [preview],
    );
    let count = 0;
    decorations.between(0, state.doc.length, () => { count += 1; });
    expect(count).toBe(0);
  });

  it("keeps a caret before the line break at the end of a paragraph", () => {
    const doc = "paragraph\n\n";
    const initial = EditorState.create({ doc });
    const decorations = collaborationDecorations(
      initial,
      [{ actor: "alice", name: "Alice", color: "red", anchor: 9 }],
      [],
    );
    const parent = document.createElement("div");
    document.body.append(parent);
    const editor = new EditorView({
      parent,
      state: EditorState.create({
        doc,
        extensions: [EditorView.decorations.of(decorations)],
      }),
    });
    const caretLine = parent.querySelector(".remote-caret")?.closest(".cm-line");
    expect(caretLine?.textContent).toContain("paragraph");
    editor.destroy();
    parent.remove();
  });

  it("keeps the preview caret on its paragraph through intermediate acknowledgements", () => {
    const base = "paragraph\n\n";
    const result = "paragraphxyz\n\n";
    const preview = {
      actor: "alice",
      name: "Alice",
      color: "red",
      base: textRevision(base),
      result: textRevision(result),
      index: 9,
      delete: 0,
      insert: "xyz",
      anchor: 12,
    };
    const memory = new Map();
    collaborationDecorations(EditorState.create({ doc: base }), [], [preview], memory);

    const intermediate = "paragraphx\n\n";
    const state = EditorState.create({ doc: intermediate });
    const decorations = collaborationDecorations(state, [], [preview], memory);
    const parent = document.createElement("div");
    document.body.append(parent);
    const editor = new EditorView({
      parent,
      state: EditorState.create({
        doc: intermediate,
        extensions: [EditorView.decorations.of(decorations)],
      }),
    });
    const caretLine = parent.querySelector(".remote-caret")?.closest(".cm-line");
    expect(caretLine?.textContent).toContain("paragraphxyz");
    expect(parent.querySelectorAll(".remote-caret")).toHaveLength(1);
    editor.destroy();
    parent.remove();
  });

  it("preserves exact Markdown source—including syntax and character references", () => {
    const source = "## Reproduction\n\n- `code` and &#x20; stay byte-identical";
    const state = EditorState.create({ doc: source });
    expect(state.doc.toString()).toBe(source);
  });
});
