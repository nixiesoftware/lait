import { describe, expect, it } from "vitest";

import { fit } from "./SelectionToolbar";

/** A selection box, host-relative, with the host at a given viewport origin. */
const anchor = (over: Partial<Parameters<typeof fit>[0]> = {}) => ({
  top: 100,
  bottom: 120,
  left: 300,
  hostTop: 200,
  hostLeft: 40,
  ...over,
});

const BAR = { width: 420, height: 36 };

describe("fit", () => {
  it("centres on the selection when there is room", () => {
    const placed = fit(anchor(), BAR, 1200);
    expect(placed.left).toBe(300);
    expect(placed.above).toBe(true);
    expect(placed.top).toBe(100);
  });

  it("slides right rather than hanging off the left edge", () => {
    // Centre would be at viewport x=50, so the bar's left end would sit at
    // -160. It slides until that end clears the margin.
    const placed = fit(anchor({ left: 10 }), BAR, 1200);
    expect(placed.left + 40 - BAR.width / 2).toBe(8);
  });

  it("slides left rather than hanging off the right edge", () => {
    const placed = fit(anchor({ left: 1150 }), BAR, 1200);
    expect(placed.left + 40 + BAR.width / 2).toBe(1200 - 8);
  });

  it("keeps the left edge visible in a window narrower than itself", () => {
    // The two clamps cross. Being cut off on the right beats being cut off on
    // both, because the first button is the one you reach for.
    const placed = fit(anchor(), BAR, 200);
    expect(placed.left + 40 - BAR.width / 2).toBe(8);
  });

  it("flips below when the selection is too near the top of the window", () => {
    // Viewport top of the range is 200 + 4 = 204... with the host at 200 and
    // the range 4px into it there is no room for a 36px bar plus margins.
    const placed = fit(anchor({ top: 4, hostTop: 8 }), BAR, 1200);
    expect(placed.above).toBe(false);
    expect(placed.top).toBe(120);
  });

  it("stays above when the range clears the bar's height and both margins", () => {
    const placed = fit(anchor({ top: 0, hostTop: 52 }), BAR, 1200);
    expect(placed.above).toBe(true);
  });

  it("reports the raw anchor until it has been measured", () => {
    const placed = fit(anchor(), null, 1200);
    expect(placed).toEqual({ left: 300, top: 100, above: true });
  });
});
