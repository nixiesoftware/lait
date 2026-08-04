import { act, StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";

import { Markdown } from "./Markdown";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

/**
 * The renderer was built for the split issue pane — headings a step off body
 * size, one flat gap between every block — and that pane is gone. These pin the
 * document contract that replaced it.
 *
 * Most of that contract is CSS, which jsdom will not compute, so the assertions
 * are on the hooks the `.prose` layer in `styles.css` selects: the heading
 * *level* (not a size class), the `prose` class itself, and `.prose-figure` on
 * a fenced block. Each of those is a join between two files, which is exactly
 * the kind of thing that silently comes apart.
 */
describe("Markdown", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  const draw = (node: React.ReactNode): HTMLDivElement => {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root!.render(<StrictMode>{node}</StrictMode>));
    return host;
  };

  afterEach(() => {
    act(() => root?.unmount());
    host?.remove();
    host = null;
    root = null;
  });

  it("gives headings their real level rather than a uniform size class", () => {
    const el = draw(<Markdown text={"# One\n\n## Two\n\n### Three\n\n#### Four"} />);
    expect([...el.querySelectorAll("h1,h2,h3,h4")].map((h) => h.tagName)).toEqual([
      "H1",
      "H2",
      "H3",
      "H4",
    ]);
  });

  it("carries the prose class so the document rhythm applies", () => {
    const el = draw(<Markdown text={"# Heading\n\nBody."} />);
    expect(el.querySelector(".prose")).toBeTruthy();
  });

  it("keeps plain text on the same scale as parsed markdown", () => {
    // The short-circuit path renders a bare paragraph. It used to skip every
    // typographic decision with it, so an issue whose body happened to contain
    // no markdown was typeset differently from one that did.
    const el = draw(<Markdown text="Just a sentence." />);
    expect(el.querySelector("p")?.className).toContain("prose");
  });

  it("uses the tighter rhythm for a comment", () => {
    const el = draw(<Markdown text={"# H\n\nBody."} density="tight" />);
    expect(el.querySelector(".prose.prose-tight")).toBeTruthy();
  });

  it("renders a fenced block as a figure, with its language and a copy button", () => {
    const el = draw(<Markdown text={"```sh\nlait --json\n```"} />);
    // `.prose-figure` is what the spacing rule selects. The block is a wrapper
    // now, not a bare <pre>, so `.prose > pre` walks straight past it and the
    // block silently takes the ordinary paragraph gap instead of a figure's.
    const figure = el.querySelector(".prose-figure");
    expect(figure).toBeTruthy();
    expect(figure!.querySelector("pre")?.textContent).toBe("lait --json");
    expect(figure!.textContent).toContain("sh");
    expect(el.querySelector('button[aria-label="Copy code"]')).toBeTruthy();
  });

  it("omits the language strip when the fence carries none", () => {
    const el = draw(<Markdown text={"```\nbare\n```"} />);
    expect(el.querySelector(".prose-figure")).toBeTruthy();
    expect(el.querySelector("pre")?.textContent).toBe("bare");
  });

  it("paints Shiki's tokens onto real elements once highlighting resolves", async () => {
    // The seam between `core/highlight.ts` and this renderer. The module hands
    // back style objects keyed by CSS custom property, and React only forwards
    // those if they arrive as a style object rather than a class — so this is
    // the assertion that the colours reach the DOM at all.
    const host = document.createElement("div");
    document.body.append(host);
    const r = createRoot(host);
    await act(async () => {
      r.render(
        <StrictMode>
          <Markdown text={"```rust\nfn main() {}\n```"} />
        </StrictMode>,
      );
    });
    // Poll rather than sleep a fixed span: the first call builds the
    // highlighter and compiles a grammar, which is fast but not instant, and a
    // hard-coded wait is the kind of test that passes on this machine.
    for (let i = 0; i < 100 && !host.querySelector("pre span[style]"); i++) {
      await act(async () => {
        await new Promise((resolve) => setTimeout(resolve, 20));
      });
    }

    const pre = host.querySelector("pre.shiki-block");
    expect(pre).toBeTruthy();
    // Text survives tokenisation intact — a highlighter that drops or reorders
    // characters is worse than no highlighter.
    expect(pre!.textContent).toBe("fn main() {}");
    const coloured = [...host.querySelectorAll("pre span[style]")];
    expect(coloured.length).toBeGreaterThan(0);
    expect(coloured.some((s) => s.getAttribute("style")?.includes("--shiki-dark"))).toBe(true);
    expect(coloured.some((s) => s.getAttribute("style")?.includes("--shiki-light"))).toBe(true);

    act(() => r.unmount());
    host.remove();
  });

  it("shows the code before the highlighter arrives, never a blank block", () => {
    // Shiki is a dynamic import. Rendering nothing until it lands would put an
    // empty rectangle in the middle of an issue body on first paint.
    const el = draw(<Markdown text={"```rust\nfn main() {}\n```"} />);
    expect(el.querySelector("pre")?.textContent).toBe("fn main() {}");
  });

  it("still refuses to route any string through innerHTML", () => {
    const el = draw(<Markdown text={"<img src=x onerror=alert(1)>"} />);
    expect(el.querySelector("img")).toBeNull();
    expect(el.textContent).toContain("<img src=x onerror=alert(1)>");
  });
});
