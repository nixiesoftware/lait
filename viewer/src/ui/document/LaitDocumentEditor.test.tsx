import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { EditorState } from "prosemirror-state";
import { EditorView as CodeMirror } from "@codemirror/view";

import { DOCUMENT_PREFIX, upgradeMarkdown } from "../../core/document";
import { escapeSelection } from "./CodeBlockView";
import LaitDocumentEditor, { issueReferencePlugin } from "./LaitDocumentEditor";
import { projectSource } from "./projection";
import { laitDocumentSchema } from "./schema";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

// ProseMirror positions its selection toolbar from DOM Range geometry. jsdom
// deliberately does not lay text out, so give that positioning seam one stable
// rectangle without changing the editor code or weakening the assertion.
if (!(Range.prototype as Range & { getClientRects?: () => DOMRectList }).getClientRects) {
  Object.defineProperty(Range.prototype, "getClientRects", {
    configurable: true,
    value: () => [{
      bottom: 24,
      height: 16,
      left: 8,
      right: 48,
      top: 8,
      width: 40,
      x: 8,
      y: 8,
      toJSON: () => ({}),
    }],
  });
}

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(async () => {
  if (root) await act(async () => {
    root!.unmount();
    await Promise.resolve();
  });
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

/** Put `words[from..to)` of the first paragraph under the browser selection and
 *  let ProseMirror read it, the way a real gesture eventually would. */
function selectText(editor: HTMLElement, from: number, to: number) {
  const text = editor.querySelector("p")!.firstChild!;
  const range = document.createRange();
  range.setStart(text, from);
  range.setEnd(text, to);
  act(() => {
    window.getSelection()!.removeAllRanges();
    window.getSelection()!.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
  });
}

const bar = (shown: HTMLElement) => shown.querySelector("[aria-label='Text formatting']");

describe("Lait document editor", () => {
  it("shows controls only for a real text selection, never from focus alone", () => {
    vi.useFakeTimers();
    try {
      const shown = renderEditor(upgradeMarkdown("Select these words.").source).container;
      const editor = shown.querySelector(".ProseMirror") as HTMLElement;

      act(() => editor.focus());
      expect(shown.querySelector("[aria-label='Document blocks']")).toBeNull();
      expect(bar(shown)).toBeNull();

      selectText(editor, 0, 6);
      act(() => void vi.advanceTimersByTime(400));
      expect(bar(shown)).not.toBeNull();

      act(() => editor.dispatchEvent(new KeyboardEvent("keydown", {
        bubbles: true,
        key: "Escape",
      })));
      expect(bar(shown)).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  // The bar's whole complaint was that it arrived mid-gesture and then moved.
  // Both halves are asserted: nothing while the range is still being chosen, and
  // nothing on the frame the gesture ends either — the entrance is deferred.
  it("withholds the formatting bar until the selection gesture is finished", () => {
    vi.useFakeTimers();
    try {
      const shown = renderEditor(upgradeMarkdown("Select these words.").source).container;
      const editor = shown.querySelector(".ProseMirror") as HTMLElement;
      const mount = editor.parentElement!;

      act(() => editor.focus());
      act(() => void mount.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true })));

      // Mid-drag: the range exists and grows, and the bar stays away from it.
      selectText(editor, 0, 6);
      act(() => void vi.advanceTimersByTime(1_000));
      expect(bar(shown)).toBeNull();
      selectText(editor, 0, 12);
      act(() => void vi.advanceTimersByTime(1_000));
      expect(bar(shown)).toBeNull();

      // Released, but not yet settled.
      act(() => void window.dispatchEvent(new MouseEvent("pointerup")));
      expect(bar(shown)).toBeNull();

      act(() => void vi.advanceTimersByTime(400));
      expect(bar(shown)).not.toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("names every formatting control and its shortcut", () => {
    vi.useFakeTimers();
    try {
      const shown = renderEditor(upgradeMarkdown("Select these words.").source).container;
      const editor = shown.querySelector(".ProseMirror") as HTMLElement;
      act(() => editor.focus());
      selectText(editor, 0, 6);
      act(() => void vi.advanceTimersByTime(400));

      const labels = Array.from(bar(shown)!.querySelectorAll("button"))
        .map((button) => button.getAttribute("aria-label"));
      // Not "strong"/"em"/"strike" — those are schema names, and they were what
      // a screen reader was reading out.
      expect(labels).toEqual([
        "Bold",
        "Italic",
        "Underline",
        "Strikethrough",
        "Code",
        "Link",
      ]);
      expect(bar(shown)!.querySelectorAll("svg").length).toBe(labels.length);
    } finally {
      vi.useRealTimers();
    }
  });

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

  it("visualizes compiler-valid callouts and tables in an issue body", () => {
    const source = `${DOCUMENT_PREFIX}#lait-callout("warning", [Keep this visible.])

#lait-table(
  header: ([Name], [State]),
  rows: (
    ([editor], [ready]),
  ),
  align: ("left", "right"),
)`;
    const shown = renderEditor(source).container;

    expect(shown.querySelector(".lait-doc-callout-warning")?.textContent)
      .toContain("Keep this visible.");
    expect([...shown.querySelectorAll("table th")].map((cell) => cell.textContent))
      .toEqual(["Name", "State"]);
    expect([...shown.querySelectorAll("table td")].map((cell) => cell.textContent))
      .toEqual(["editor", "ready"]);
    expect(shown.textContent).not.toContain("#lait-");
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

  it("commits prose without a hidden Plan position", () => {
    const source = upgradeMarkdown("Before.\n\nAfter.").source;
    const onCommit = vi.fn();
    const shown = renderEditor(source, { onCommit }).container;

    act(() => shown.querySelector<HTMLElement>(".ProseMirror")!.dispatchEvent(
      new FocusEvent("blur", { bubbles: true }),
    ));
    expect(onCommit).toHaveBeenCalledWith();
  });

  /**
   * Arrowing off the edge of a code block that has nothing on the other side.
   *
   * `Selection.near` promises a *valid* position, not one outside the block, so
   * with no block on the far side it answers with the code block's own content.
   * Acting on that used to dispatch an outer selection into a node view
   * ProseMirror cannot draw a caret in and scroll the page to it — from the
   * first line of a leading code block, that is position 1, the top of the
   * document — while returning `true`, so the keypress was swallowed too.
   *
   * Asserted on the rule itself rather than through a keypress. Every DOM-level
   * channel is distorted here: jsdom will not focus a `contenteditable` div, so
   * `activeElement` never moves; the bad escape lands *inside* the block, so the
   * resulting selection is indistinguishable from having stayed; and CodeMirror's
   * own motion consumes arrow keys on a guess without a laid-out document, so
   * `defaultPrevented` measures jsdom rather than the branch under test.
   */
  describe("arrow-key escape from an embedded code block", () => {
    const destination = (markdown: string, direction: -1 | 1) => {
      const doc = projectSource(upgradeMarkdown(markdown).source).doc;
      let at = -1;
      let size = 0;
      doc.descendants((node, pos) => {
        if (node.type.name === "code_block" && at === -1) {
          at = pos;
          size = node.nodeSize;
        }
        return true;
      });
      const next = escapeSelection(doc, at, size, direction);
      return next === null ? null : doc.resolve(next.from).parent.type.name;
    };

    it("declines to leave a code block that opens the document", () => {
      expect(destination("```rust\nfn main() {}\n```\n\nTrailing prose.", -1)).toBeNull();
    });

    it("declines to leave a code block that closes the document", () => {
      expect(destination("Intro prose.\n\n```rust\nfn main() {}\n```", 1)).toBeNull();
    });

    it("declines in both directions when the code block is the whole document", () => {
      expect(destination("```rust\nfn main() {}\n```", -1)).toBeNull();
      expect(destination("```rust\nfn main() {}\n```", 1)).toBeNull();
    });

    /** The other half of the contract, and what keeps the refusals above from
     *  passing for the wrong reason: a real destination is still reached. */
    it("leaves a code block that has prose on the far side", () => {
      const surrounded = "Intro prose.\n\n```rust\nfn main() {}\n```\n\nTrailing prose.";
      expect(destination(surrounded, 1)).toBe("paragraph");
      expect(destination(surrounded, -1)).toBe("paragraph");
    });
  });

  /**
   * The open code block gets the colours the closed one already had, from the
   * same tokeniser — so a block does not change appearance just because someone
   * put a caret in it. Asserted on both theme variables being present with no
   * inline `color`, which is what lets a theme flip re-paint without a
   * re-tokenise (see `core/highlight.ts`).
   */
  it("syntax-colours an embedded code block from the shared highlighter", async () => {
    const shown = renderEditor(upgradeMarkdown("```rust\nfn main() {}\n```").source).container;
    const code = shown.querySelector(".cm-editor") as HTMLElement;

    for (let attempt = 0; attempt < 100 && !code.querySelector(".cm-shiki"); attempt += 1) {
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
    }

    const coloured = [...code.querySelectorAll(".cm-shiki")];
    expect(coloured.length).toBeGreaterThan(0);
    expect(coloured.some((span) => span.getAttribute("style")?.includes("--shiki-dark"))).toBe(true);
    expect(coloured.some((span) => span.getAttribute("style")?.includes("--shiki-light"))).toBe(true);
    expect(coloured.every((span) => !/(^|;)\s*color:/.test(span.getAttribute("style") ?? ""))).toBe(true);
    // Tokenising must not disturb the text it colours.
    expect(code.textContent).toContain("fn main() {}");
  });

  /** A language the app does not carry is a plain block, not a broken one. */
  it("leaves an unknown language uncoloured rather than failing", async () => {
    const shown = renderEditor(upgradeMarkdown("```brainfuck\n+[----->+++<]>+.\n```").source)
      .container;
    const code = shown.querySelector(".cm-editor") as HTMLElement;

    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 200));
    });

    expect(code.querySelectorAll(".cm-shiki").length).toBe(0);
    expect(code.textContent).toContain("+[----->+++<]>+.");
  });
});
