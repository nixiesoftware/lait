import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it } from "vitest";

import { LabelChip, LabelChips } from "./primitives";

(globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT: boolean })
  .IS_REACT_ACT_ENVIRONMENT = true;

/**
 * The chip existed three times — detail rail, list row, board card — and had
 * drifted to two border tokens, two text colours and two heights. These pin the
 * things that drift silently: the colour actually reaching the element, the
 * overflow arithmetic, and the fact that a name never disappears.
 */
describe("LabelChip", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  const draw = (node: React.ReactNode): HTMLDivElement => {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root!.render(node));
    return host;
  };

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  it("takes its edge from the label's own colour and leaves the ground alone", () => {
    const el = draw(<LabelChip name="bug" color="red" />);
    const chip = el.firstElementChild as HTMLElement;
    // `color-mix` rather than an opacity utility, because the palette arrives as
    // `var(--color-*)` tokens that Tailwind's `/30` syntax cannot reach into.
    expect(chip.style.borderColor).toContain("color-mix");
    expect(chip.style.borderColor).toContain("--color-danger");
    // No fill. A washed pill outweighs the date and project it sits beside in a
    // list row, which is not the label's importance.
    expect(chip.style.background).toBe("");
  });

  it("falls back to the muted token for a colour we did not design", () => {
    const el = draw(<LabelChip name="mystery" color="chartreuse" />);
    const chip = el.firstElementChild as HTMLElement;
    expect(chip.style.borderColor).toContain("--color-mute");
  });

  it("speaks quieter in a dense row than in the rail", () => {
    const rail = draw(<LabelChip name="x" color="blue" />).firstElementChild!;
    expect(rail.className).toContain("text-fg");
    act(() => root!.unmount());
    host!.remove();
    const row = draw(<LabelChip name="x" color="blue" size="sm" />).firstElementChild!;
    expect(row.className).toContain("text-dim");
  });

  it("keeps the name reachable when it is too long to show", () => {
    const el = draw(<LabelChip name="a-very-long-label-name" color="blue" />);
    const chip = el.firstElementChild as HTMLElement;
    expect(chip.getAttribute("title")).toBe("a-very-long-label-name");
    expect(chip.textContent).toContain("a-very-long-label-name");
  });

  it("stays shorter than the row it sits in, at both sizes", () => {
    // `ctl-xs` (20px) inside a `ctl-md` (28px) rail row. At 24px it left 2px of
    // air where every bare entry beside it has six, and a wrapped pair ran at a
    // pitch the rail did not share — so height is the one thing the sizes agree
    // on. Now a rung apart on the control ladder rather than two loose numbers.
    const md = draw(<LabelChip name="x" color="blue" />).firstElementChild!;
    expect(md.className).toContain("h-ctl-xs");
    act(() => root!.unmount());
    host!.remove();
    const sm = draw(<LabelChip name="x" color="blue" size="sm" />).firstElementChild!;
    expect(sm.className).toContain("h-ctl-xs");
    expect(sm.className).toContain("rounded-full");
    // What differs is type size, not box size.
    expect(md.className).toContain("text-sm");
    expect(sm.className).toContain("text-xs");
    // Both carry the weight the issue titles beside them do, so a label reads
    // as a peer of the row rather than a caption under it.
    expect(md.className).toContain("font-medium");
    expect(sm.className).toContain("font-medium");
  });
});

describe("LabelChips", () => {
  let host: HTMLDivElement | null = null;
  let root: ReturnType<typeof createRoot> | null = null;

  const draw = (node: React.ReactNode): HTMLDivElement => {
    host = document.createElement("div");
    document.body.append(host);
    root = createRoot(host);
    act(() => root!.render(node));
    return host;
  };

  afterEach(() => {
    if (root) act(() => root?.unmount());
    host?.remove();
    root = null;
    host = null;
  });

  const colorOf = () => "gray";

  it("renders nothing at all when there are no labels", () => {
    const el = draw(<LabelChips names={[]} colorOf={colorOf} />);
    expect(el.textContent).toBe("");
  });

  it("folds the tail into a count and keeps the hidden names in the tooltip", () => {
    const el = draw(<LabelChips names={["a", "b", "c", "d"]} colorOf={colorOf} max={2} />);
    expect(el.textContent).toContain("+2");
    const more = [...el.querySelectorAll("span")].find((s) => s.textContent === "+2");
    // The count is the only place the dropped labels still exist on screen.
    expect(more?.getAttribute("title")).toBe("c, d");
  });

  it("shows every label when no cap is given", () => {
    const el = draw(<LabelChips names={["a", "b", "c"]} colorOf={colorOf} />);
    expect(el.textContent).not.toContain("+");
    expect(el.textContent).toContain("a");
    expect(el.textContent).toContain("c");
  });

  it("does not print a count when the cap is exactly met", () => {
    const el = draw(<LabelChips names={["a", "b"]} colorOf={colorOf} max={2} />);
    expect(el.textContent).not.toContain("+0");
  });
});
