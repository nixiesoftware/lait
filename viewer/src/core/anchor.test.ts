import { describe, expect, it } from "vitest";

import { codeUnitOffset, codeUnitSpan } from "./anchor";

describe("scalar offsets become code-unit offsets", () => {
  it("is the identity while every character is one unit", () => {
    // Which is why the bug this exists for is invisible in every test corpus
    // written in ASCII, and in every description anybody types until the first
    // emoji.
    const text = "this word is wrong";
    expect(codeUnitOffset(text, 5)).toBe(5);
    expect(codeUnitSpan(text, 5, 9)).toEqual({ start: 5, end: 9 });
  });

  it("counts an astral character as one scalar and two units", () => {
    // "🙂 this word" — the engine says the word starts at scalar 7, and slicing
    // a JS string at 7 lands one unit early, on the space.
    const text = "🙂 this word";
    expect([...text].length).toBe(11);
    expect(text.length).toBe(12);

    const span = codeUnitSpan(text, 7, 11);
    expect(span).toEqual({ start: 8, end: 12 });
    expect(text.slice(span.start, span.end)).toBe("word");
    // What slicing the raw scalar offsets would have given.
    expect(text.slice(7, 11)).toBe(" wor");
  });

  it("slides by one unit for every astral character in front of the span", () => {
    // The reason this is worth a module rather than an inline `+ 1`: the error
    // is not constant, it accumulates with the text before the span.
    const text = "🙂🙂🙂 word";
    const scalarStart = 4;
    const span = codeUnitSpan(text, scalarStart, scalarStart + 4);
    expect(span.start - scalarStart).toBe(3);
    expect(text.slice(span.start, span.end)).toBe("word");
  });

  it("clamps past the end rather than throwing", () => {
    // The engine resolves against the text as it holds it and a client can be a
    // beat behind. A highlight stopping at the end of what is on screen is the
    // honest rendering of that; an exception in a render path is not.
    const text = "short";
    expect(codeUnitOffset(text, 99)).toBe(5);
    expect(codeUnitSpan(text, 3, 99)).toEqual({ start: 3, end: 5 });
  });

  it("treats a backwards span as the range between its ends", () => {
    // Not reachable from the engine, which refuses one — so this is about what
    // a renderer does with a value it did not produce, and the answer is
    // something drawable rather than a negative-length slice.
    const text = "this word is wrong";
    expect(codeUnitSpan(text, 9, 5)).toEqual({ start: 5, end: 9 });
  });

  it("a zero-width span is a caret and stays one", () => {
    const text = "🙂 this word";
    expect(codeUnitSpan(text, 7, 7)).toEqual({ start: 8, end: 8 });
    expect(codeUnitOffset(text, 0)).toBe(0);
  });
});
