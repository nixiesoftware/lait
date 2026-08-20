import { describe, expect, it } from "vitest";

import { shortcut, shortcutModifier } from "./platform";

describe("platform shortcuts", () => {
  it("uses Command on macOS", () => {
    expect(shortcutModifier("macos")).toBe("⌘");
    expect(shortcut("macos", "K")).toBe("⌘K");
  });

  it("uses Control everywhere else", () => {
    expect(shortcut("windows", "K")).toBe("CtrlK");
    expect(shortcut("kiosk", "K")).toBe("CtrlK");
  });
});
