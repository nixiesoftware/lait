import { describe, expect, it } from "vitest";

import { highlight, isHighlightable } from "./highlight";

/**
 * These drive the real Shiki, not a stub. The module's whole job is a
 * negotiation with that library — alias resolution, lazy grammar loading, the
 * dual-theme token shape — and a mock would only assert that the negotiation is
 * what I already believed it to be.
 */
describe("highlight", () => {
  it("colours a known language with both themes on every token", async () => {
    const lines = await highlight("$ lait serve --json", "bash");
    expect(lines).not.toBeNull();
    const tokens = lines!.flat();
    expect(tokens.map((t) => t.content).join("")).toBe("$ lait serve --json");
    // `defaultColor: false` is what keeps a theme flip from needing a
    // re-tokenise: no inline `color`, both variables present, CSS decides.
    for (const token of tokens) {
      expect(token.style["--shiki-light"]).toMatch(/^#/);
      expect(token.style["--shiki-dark"]).toMatch(/^#/);
      expect(token.style["color"]).toBeUndefined();
    }
  });

  it("resolves an alias on a cold highlighter", async () => {
    // The regression this guards: aliases used to be checked against
    // `getLoadedLanguages()`, which on a cold page has not yet heard of the
    // grammar — so `sh` declined once and then worked forever after, which is
    // exactly the shape of bug that never reproduces while you are looking.
    const lines = await highlight("echo hi", "sh");
    expect(lines).not.toBeNull();
    expect(lines!.flat().map((t) => t.content).join("")).toBe("echo hi");
  });

  it("keeps line structure so the renderer can emit newlines itself", async () => {
    const lines = await highlight("fn main() {}\nfn other() {}", "rust");
    expect(lines).not.toBeNull();
    expect(lines!.length).toBe(2);
    expect(lines![0]!.map((t) => t.content).join("")).toBe("fn main() {}");
  });

  it("declines rather than throws for a language we do not carry", async () => {
    expect(await highlight("10 PRINT", "basic")).toBeNull();
    expect(await highlight("plain", null)).toBeNull();
    expect(await highlight("plain", "")).toBeNull();
  });

  it("agrees with itself about what is highlightable", () => {
    expect(isHighlightable("rust")).toBe(true);
    expect(isHighlightable("SH")).toBe(true);
    expect(isHighlightable("ts")).toBe(true);
    expect(isHighlightable("basic")).toBe(false);
    expect(isHighlightable(null)).toBe(false);
    // The renderer skips loading Shiki entirely when this says no, so a
    // disagreement here would mean a fence that could be coloured never is.
    expect(isHighlightable("zsh")).toBe(true);
  });
});
