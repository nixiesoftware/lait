import { describe, expect, it } from "vitest";

import type { PresentedItem, PresentedProgram } from "./client";
import { advance, holdMs, refreshDelayMs, refreshFloorMs, untimedHoldMs } from "./present";

const item = (durationMs: number | null): PresentedItem =>
  ({ id: "itm", durationMs, assessment: "current", spokenSummary: null, scene: { kind: "blank", reason: "program_ended" } });

const program = (overrides: Partial<PresentedProgram>): PresentedProgram =>
  ({ assessment: "current", partialReasons: [], cycle: "hold_last", refreshAfterMs: null, items: [], ...overrides });

describe("the screen's pacing rules", () => {
  it("holds an untimed item for the standing duration", () => {
    expect(holdMs(item(null))).toBe(untimedHoldMs);
    expect(holdMs(item(3000))).toBe(3000);
  });

  it("floors the re-ask, whatever a program declares", () => {
    expect(refreshDelayMs(program({ refreshAfterMs: 1 }))).toBe(refreshFloorMs);
    expect(refreshDelayMs(program({ refreshAfterMs: 30_000 }))).toBe(30_000);
    expect(refreshDelayMs(program({ refreshAfterMs: null }))).toBeNull();
  });

  it("advances through the cycle, and holds on a word it does not know", () => {
    expect(advance("loop", 0, 3)).toEqual({ kind: "show", index: 1 });
    expect(advance("loop", 2, 3)).toEqual({ kind: "show", index: 0 });
    expect(advance("poll_at_end", 2, 3)).toEqual({ kind: "refresh" });
    expect(advance("blank_at_end", 2, 3)).toEqual({ kind: "blank" });
    expect(advance("hold_last", 2, 3)).toEqual({ kind: "hold" });
    // An unknown cycle holding the last frame is the conservative reading.
    expect(advance("something_newer", 2, 3)).toEqual({ kind: "hold" });
  });
});
