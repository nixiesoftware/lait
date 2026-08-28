import { describe, expect, it } from "vitest";

import { SETTINGS_GROUPS, SETTINGS_PAGES, isSettingsTab, searchSettings } from "./pages";

describe("settings taxonomy", () => {
  it("files every page under a group the rail draws", () => {
    for (const page of SETTINGS_PAGES) expect(SETTINGS_GROUPS).toContain(page.group);
  });

  it("names each page once", () => {
    const tabs = SETTINGS_PAGES.map((page) => page.tab);
    expect(new Set(tabs).size).toBe(tabs.length);
  });

  it("narrows a route value to a known page", () => {
    expect(isSettingsTab("labels")).toBe(true);
    expect(isSettingsTab("billing")).toBe(false);
    expect(isSettingsTab(null)).toBe(false);
  });
});

describe("settings search", () => {
  it("returns every page in rail order for an empty query", () => {
    expect(searchSettings("  ").map((m) => m.page.tab)).toEqual(SETTINGS_PAGES.map((p) => p.tab));
  });

  it("finds a page by a field on it, and says which field", () => {
    const [first] = searchSettings("theme");
    expect(first?.page.tab).toBe("preferences");
    expect(first?.via).toBe("theme");
  });

  it("puts a label match ahead of a keyword match", () => {
    const tabs = searchSettings("team").map((m) => m.page.tab);
    expect(tabs[0]).toBe("teams");
  });

  it("is case-insensitive", () => {
    expect(searchSettings("LABELS")[0]?.page.tab).toBe("labels");
  });

  it("returns nothing for a word no page answers to", () => {
    expect(searchSettings("billing")).toEqual([]);
  });
});
