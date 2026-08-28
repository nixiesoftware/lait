import { describe, expect, it } from "vitest";

import { artSize, type EmptyStateArt } from "./emptyArt";

const ARTS: EmptyStateArt[] = [
  "activity",
  "archive",
  "filtered",
  "inbox",
  "issues",
  "people",
  "projects",
  "space",
  "specs",
  "unavailable",
];

/** The authoring lattice: 72 × 52 cells at the 2.5px pitch. */
const CELL = 2.5;
const LATTICE = { width: 72 * CELL, height: 52 * CELL };

describe("the empty-state plates", () => {
  /**
   * A plate that carries the lattice's blank carries its own spacing with it,
   * and every drawing pads differently — 22.5px down the left of `projects`
   * against 37.5px for `unavailable`. Cropped to the ink, the margin under a
   * plate and the edge it starts on belong to the layout, which has one of
   * each for the whole set.
   */
  it("is the size of its drawing, not of the lattice it was drawn on", () => {
    for (const art of ARTS) {
      const { width, height } = artSize(art);
      expect(width, art).toBeGreaterThan(0);
      expect(width, art).toBeLessThan(LATTICE.width);
      expect(height, art).toBeLessThan(LATTICE.height);
      // The ruling is fixed, so a plate is always a whole number of cells: a
      // size off the pitch would mean something resampled the screen.
      expect((width / CELL) % 1, art).toBe(0);
      expect((height / CELL) % 1, art).toBe(0);
    }
  });

  /** Objects differ, so their plates do. One shared size would mean the crop
   *  had quietly become a box again. */
  it("gives each drawing its own measure", () => {
    const sizes = new Set(ARTS.map((art) => `${artSize(art).width}x${artSize(art).height}`));
    expect(sizes.size).toBeGreaterThan(1);
  });
});
