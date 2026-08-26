import { describe, expect, it } from "vitest";

import { emptinessOf } from "./emptiness";

describe("why a work area has no rows", () => {
  /**
   * The case that was on screen: a World whose data is its own and starts
   * empty. It loaded perfectly, and the shell drew a warning triangle over it
   * saying the local projection could not be loaded — while the header, two
   * inches away, said "Ready locally".
   */
  it("is a first run when the Space has answered and holds no project", () => {
    expect(
      emptinessOf({ hasRows: false, board: "error", projects: "ready", projectCount: 0 }),
    ).toBe("failed");
    expect(
      emptinessOf({ hasRows: false, board: "cold", projects: "ready", projectCount: 0 }),
    ).toBe("no-projects");
  });

  /**
   * An empty list that has not answered says nothing at all. Reading a first
   * run off it is the same defect one layer down: an absence has to say which
   * kind it is before anything is concluded from it.
   */
  it("is still loading when the project list has not answered", () => {
    for (const projects of ["cold", "refreshing", "partial"] as const) {
      expect(
        emptinessOf({ hasRows: false, board: "cold", projects, projectCount: 0 }),
      ).toBe("loading");
    }
  });

  it("is a failure when either read failed, because that is the only one anybody can act on", () => {
    expect(
      emptinessOf({ hasRows: false, board: "error", projects: "ready", projectCount: 3 }),
    ).toBe("failed");
    expect(
      emptinessOf({ hasRows: false, board: "ready", projects: "error", projectCount: 0 }),
    ).toBe("failed");
  });

  it("is nothing at all when there are rows to draw", () => {
    expect(
      emptinessOf({ hasRows: true, board: "error", projects: "error", projectCount: 0 }),
    ).toBe("none");
  });

  /** A Space with projects whose board is simply still coming. */
  it("is loading when projects exist but the board has not answered", () => {
    expect(
      emptinessOf({ hasRows: false, board: "cold", projects: "ready", projectCount: 4 }),
    ).toBe("loading");
  });
});
