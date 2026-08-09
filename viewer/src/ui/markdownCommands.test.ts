import { EditorSelection, EditorState } from "@codemirror/state";
import { describe, expect, it } from "vitest";

import {
  BULLET,
  HEADING,
  ORDERED,
  QUOTE,
  TASK,
  blocked,
  insertLink,
  toggleBlock,
  toggleFence,
  toggleWrap,
  wrapped,
} from "./markdownCommands";

/** A document with `|` marking the selection's two ends (or one, for a caret). */
function at(source: string): EditorState {
  const first = source.indexOf("|");
  const second = source.indexOf("|", first + 1);
  const doc = source.replace(/\|/g, "");
  const anchor = first;
  const head = second === -1 ? first : second - 1;
  return EditorState.create({ doc, selection: EditorSelection.range(anchor, head) });
}

/** Apply a spec and read back the document with the selection marked. */
function after(state: EditorState, spec: ReturnType<typeof toggleWrap>): string {
  const next = state.update(spec).state;
  const { from, to } = next.selection.main;
  const doc = next.doc.toString();
  return from === to
    ? `${doc.slice(0, from)}|${doc.slice(from)}`
    : `${doc.slice(0, from)}|${doc.slice(from, to)}|${doc.slice(to)}`;
}

describe("toggleWrap", () => {
  it("wraps the selection and keeps it selected", () => {
    expect(after(at("a |bold| c"), toggleWrap(at("a |bold| c"), "**"))).toBe("a **|bold|** c");
  });

  it("unwraps when the markers are inside the selection", () => {
    const state = at("a |**bold**| c");
    expect(after(state, toggleWrap(state, "**"))).toBe("a |bold| c");
  });

  it("unwraps when the markers are OUTSIDE the selection", () => {
    // The case a naive toggle misses. Double-clicking a word inside `**bold**`
    // selects `bold`, so un-bolding has to look either side of the range — or
    // the button only ever adds and a second press gives `****bold****`.
    const state = at("a **|bold|** c");
    expect(after(state, toggleWrap(state, "**"))).toBe("a |bold| c");
  });

  it("round-trips: two presses leave the document as it was", () => {
    const first = at("a |bold| c");
    const once = first.update(toggleWrap(first, "**")).state;
    const twice = once.update(toggleWrap(once, "**")).state;
    expect(twice.doc.toString()).toBe("a bold c");
  });

  it("handles the asymmetric pair underline needs", () => {
    const state = at("a |held| c");
    expect(after(state, toggleWrap(state, "<u>", "</u>"))).toBe("a <u>|held|</u> c");
    const on = at("a <u>|held|</u> c");
    expect(after(on, toggleWrap(on, "<u>", "</u>"))).toBe("a |held| c");
  });

  it("reports what is already on", () => {
    expect(wrapped(at("a |**b**| c"), "**")).toBe(true);
    expect(wrapped(at("a **|b|** c"), "**")).toBe(true);
    expect(wrapped(at("a |b| c"), "**")).toBe(false);
  });

  it("does not read bold as italic", () => {
    // `**` and `*` are the same character, so a bold span starts and ends the
    // way an italic one does. Both buttons lighting up is the visible half of
    // the bug; the other half is that pressing the lit italic button would
    // strip one star and leave `*bold*`.
    expect(wrapped(at("a |**b**| c"), "*")).toBe(false);
    expect(wrapped(at("a **|b|** c"), "*")).toBe(false);
    expect(wrapped(at("a |*b*| c"), "*")).toBe(true);
    expect(wrapped(at("a *|b|* c"), "*")).toBe(true);
  });

  it("italicises a bold span instead of unwrapping it", () => {
    const state = at("a **|b|** c");
    expect(state.update(toggleWrap(state, "*")).state.doc.toString()).toBe("a ***b*** c");
  });

  it("does not read a marker at the document edge as a wrapper", () => {
    // `from - open.length` goes negative here; slicing from a clamped 0 used to
    // compare an empty string against an empty marker and report a match.
    expect(wrapped(at("|b|old"), "**")).toBe(false);
  });
});

describe("toggleBlock", () => {
  it("marks every line the selection touches", () => {
    const state = at("|one\ntwo\nthree|");
    expect(state.update(toggleBlock(state, BULLET)).state.doc.toString()).toBe(
      "- one\n- two\n- three",
    );
  });

  it("numbers an ordered list from the selection's first line", () => {
    const state = at("|one\ntwo|");
    expect(state.update(toggleBlock(state, ORDERED)).state.doc.toString()).toBe("1. one\n2. two");
  });

  it("removes the marker when every line already carries it", () => {
    const state = at("|- one\n- two|");
    expect(state.update(toggleBlock(state, BULLET)).state.doc.toString()).toBe("one\ntwo");
  });

  it("replaces a different block marker rather than stacking on it", () => {
    // `> # a` is not a thing anyone means to write.
    const state = at("|# one|");
    expect(state.update(toggleBlock(state, QUOTE)).state.doc.toString()).toBe("> one");
  });

  it("does not mark the line a selection merely ends at", () => {
    // Dragging down through two lines stops exactly at the third's start; the
    // third has no selected character in it and must not be marked.
    const state = at("|one\ntwo\n|three");
    expect(state.update(toggleBlock(state, BULLET)).state.doc.toString()).toBe(
      "- one\n- two\nthree",
    );
  });

  it("reports what is already on", () => {
    expect(blocked(at("|# a|"), HEADING(1))).toBe(true);
    expect(blocked(at("|# a|"), HEADING(2))).toBe(false);
    expect(blocked(at("|- [ ] a|"), TASK)).toBe(true);
  });
});

describe("toggleFence", () => {
  it("fences whole lines, never half of one", () => {
    const state = at("a\nb|cd|e\nf");
    expect(state.update(toggleFence(state)).state.doc.toString()).toBe("a\n```\nbcde\n```\nf");
  });

  it("unfences a block it already put fences around", () => {
    const state = at("```\n|x|\n```");
    expect(state.update(toggleFence(state)).state.doc.toString()).toBe("x");
  });
});

describe("insertLink", () => {
  it("leaves the target selected so the next keystroke is the URL", () => {
    const state = at("see |docs| here");
    expect(after(state, insertLink(state))).toBe("see [docs](|url|) here");
  });
});
