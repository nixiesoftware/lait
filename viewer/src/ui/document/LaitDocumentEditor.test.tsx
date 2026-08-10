import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { EditorState } from "prosemirror-state";
import { EditorView as CodeMirror } from "@codemirror/view";

import { upgradeMarkdown } from "../../core/document";
import LaitDocumentEditor, { issueReferencePlugin } from "./LaitDocumentEditor";
import { laitDocumentSchema } from "./schema";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(async () => {
  if (root) await act(() => root!.unmount());
  container?.remove();
  root = null;
  container = null;
});

function renderEditor(source: string, extra: Partial<React.ComponentProps<typeof LaitDocumentEditor>> = {}) {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  const props: React.ComponentProps<typeof LaitDocumentEditor> = {
    value: source,
    onChange: vi.fn(),
    onCommit: vi.fn(),
    ...extra,
  };
  act(() => root!.render(<LaitDocumentEditor {...props} />));
  return { container, props };
}

describe("Lait document editor", () => {
  it("renders the app document instead of exposing canonical Typst", () => {
    const source = upgradeMarkdown([
      "# Plan",
      "",
      "Edit ENG-7 in place.",
      "",
      "```rust",
      "fn main() {}",
      "```",
    ].join("\n")).source;
    const shown = renderEditor(source).container;

    expect(shown.querySelector(".ProseMirror")).not.toBeNull();
    expect(shown.querySelector("h1")?.textContent).toBe("Plan");
    expect(shown.querySelector("[data-ref='ENG-7']")?.textContent).toBe("ENG-7");
    expect(shown.querySelector(".lait-doc-code-shell .cm-editor")).not.toBeNull();
    expect((shown.querySelector("[aria-label='Code language']") as HTMLInputElement).value)
      .toBe("rust");
    expect(shown.textContent).not.toContain("lait-document");
    expect(shown.textContent).not.toContain("#raw");
    expect(shown.textContent).not.toContain("= Plan");
  });

  it("projects durable collaborator positions into the rich document", () => {
    const source = upgradeMarkdown("A collaborative paragraph.").source;
    const anchor = Array.from(source.slice(0, source.indexOf("paragraph"))).length;
    const shown = renderEditor(source, {
      remoteCursors: [{ actor: "alice", name: "Alice", color: "red", anchor }],
    }).container;

    expect(shown.querySelector("[data-remote-actor='alice']")?.textContent).toBe("Alice");
  });

  it("keeps collaborator positions visible inside embedded code editors", () => {
    const source = upgradeMarkdown("```rust\nfn main() {}\n```").source;
    const anchor = Array.from(source.slice(0, source.indexOf("main"))).length;
    const shown = renderEditor(source, {
      remoteCursors: [{ actor: "alice", name: "Alice", color: "red", anchor }],
    }).container;

    expect(shown.querySelector(".lait-doc-code-shell [data-remote-actor='alice']")?.textContent)
      .toBe("Alice");
  });

  it("routes code edits and their cursor through canonical Typst coordinates", () => {
    const source = upgradeMarkdown("```rust\nfn main() {}\n```").source;
    const onChange = vi.fn();
    const onAwareness = vi.fn();
    const shown = renderEditor(source, { onChange, onAwareness }).container;
    const code = CodeMirror.findFromDOM(shown.querySelector(".cm-editor") as HTMLElement)!;

    act(() => {
      code.focus();
      code.dispatch({
        changes: { from: 3, to: 3, insert: " safe" },
        selection: { anchor: 8 },
      });
    });

    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange.mock.calls[0]![0]).toContain("fn  safemain() {}");
    expect(onChange.mock.calls[0]![0]).toMatch(/^\/\/ lait-document:1/);
    expect(onAwareness.mock.calls.at(-1)?.[0]).toBeGreaterThan(0);
  });

  it("promotes typed issue aliases to semantic inline components", () => {
    const paragraph = laitDocumentSchema.nodes.paragraph!.create(
      null,
      laitDocumentSchema.text("See ENG-7"),
    );
    const state = EditorState.create({
      schema: laitDocumentSchema,
      doc: laitDocumentSchema.nodes.doc!.create(null, paragraph),
      plugins: [issueReferencePlugin],
    });
    const result = state.applyTransaction(state.tr.insertText(".", state.doc.content.size - 1));
    const types: string[] = [];
    result.state.doc.descendants((node) => {
      types.push(node.type.name);
    });

    expect(types).toContain("issue_ref");
    expect(result.state.doc.textBetween(0, result.state.doc.content.size)).toContain("ENG-7");
  });
});
