import { describe, expect, it } from "vitest";

import type { LibraryWorld } from "./client";
import { channelSelection, isModified } from "./settings";

function world(over: Partial<LibraryWorld> = {}): LibraryWorld {
  return {
    key: "issues",
    worldMount: "issues",
    installed: true,
    displayName: "Issues",
    opensAt: "/",
    version: 3,
    tagline: null,
    accent: null,
    people: null,
    update: null,
    install: null,
    channel: null,
    sourceDir: null,
    sourceStanding: null,
    ...over,
  };
}

describe("isModified", () => {
  it("is quiet on a World nobody has touched", () => {
    expect(isModified(world())).toBe(false);
  });

  it("is on for a channel of its own", () => {
    expect(isModified(world({ channel: "test" }))).toBe(true);
  });

  it("says nothing about a World the Library is not showing", () => {
    expect(isModified(null)).toBe(false);
  });
});

describe("channelSelection", () => {
  // The distinction the whole control exists for. Following the device is
  // following whatever the device becomes; being set to test is a decision.
  // Drawing the first as the second would report a default as a choice.
  it("separates following the device from being set to what the device is on", () => {
    expect(channelSelection(world({ channel: null }))).toBe("device");
    expect(channelSelection(world({ channel: "test" }))).toBe("test");
    expect(channelSelection(world({ channel: "stable" }))).toBe("stable");
  });

  // Not silently "device": the World is following *something*, and answering
  // with the default would be a confident wrong answer about a record that
  // exists.
  it("refuses to read a channel it does not know as the default", () => {
    expect(channelSelection(world({ channel: "nightly" }))).toBe("unknown");
  });
});
