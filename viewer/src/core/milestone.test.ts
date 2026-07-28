import { describe, expect, it } from "vitest";

import { milestonePercent, milestoneProgress } from "./milestone";

describe("milestone progress", () => {
  it("reads the three states off the counts", () => {
    expect(milestoneProgress({ done: 0, total: 4 })).toBe("not-started");
    expect(milestoneProgress({ done: 1, total: 4 })).toBe("in-progress");
    expect(milestoneProgress({ done: 4, total: 4 })).toBe("complete");
  });

  it("calls an empty milestone not-started, never complete", () => {
    // `done >= total` is true for 0/0, and a milestone nobody has scoped any
    // work into has not been achieved — it has not been started. Getting this
    // backwards fills every new milestone's glyph the moment it is created.
    expect(milestoneProgress({ done: 0, total: 0 })).toBe("not-started");
    expect(milestonePercent({ done: 0, total: 0 })).toBe(0);
  });

  it("stays complete if the counts ever overshoot", () => {
    expect(milestoneProgress({ done: 5, total: 4 })).toBe("complete");
  });

  it("rounds the percentage to whole numbers", () => {
    expect(milestonePercent({ done: 1, total: 3 })).toBe(33);
    expect(milestonePercent({ done: 2, total: 3 })).toBe(67);
    expect(milestonePercent({ done: 1, total: 2 })).toBe(50);
  });
});
