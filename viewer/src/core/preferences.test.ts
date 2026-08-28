import { beforeEach, describe, expect, it } from "vitest";

import {
  DEFAULT_PREFERENCES,
  loadPreferences,
  resetPreferencesCache,
  savePreference,
  weekColumn,
  weekdayLabels,
} from "./preferences";

describe("personal preferences", () => {
  beforeEach(() => {
    localStorage.clear();
    resetPreferencesCache();
  });

  it("defaults every field when nothing is stored", () => {
    expect(loadPreferences()).toEqual(DEFAULT_PREFERENCES);
  });

  it("round-trips a change and keeps the other fields", () => {
    savePreference("weekStart", "sunday");
    savePreference("homeView", "inbox");
    expect(loadPreferences()).toEqual({
      homeView: "inbox",
      weekStart: "sunday",
      commentSubmit: "mod-enter",
    });
  });

  it("refuses a value this build does not know", () => {
    localStorage.setItem(
      "lait.prefs",
      JSON.stringify({ homeView: "dashboard", weekStart: "tuesday", commentSubmit: 3 }),
    );
    expect(loadPreferences()).toEqual(DEFAULT_PREFERENCES);
  });

  it("survives a corrupt store", () => {
    localStorage.setItem("lait.prefs", "{not json");
    expect(loadPreferences()).toEqual(DEFAULT_PREFERENCES);
  });

  it("announces a change so live readers can redraw", () => {
    let heard = 0;
    window.addEventListener("lait:prefs", () => heard++);
    savePreference("commentSubmit", "enter");
    expect(heard).toBe(1);
  });
});

describe("week geometry", () => {
  it("puts Monday in the first column of a Monday-first week", () => {
    expect(weekColumn(1, "monday")).toBe(0);
    expect(weekColumn(0, "monday")).toBe(6);
  });

  it("puts Sunday in the first column of a Sunday-first week", () => {
    expect(weekColumn(0, "sunday")).toBe(0);
    expect(weekColumn(6, "sunday")).toBe(6);
  });

  it("labels the columns in the same order it counts them", () => {
    expect(weekdayLabels("monday")).toEqual(["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]);
    expect(weekdayLabels("sunday", "tiny")).toEqual(["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"]);
  });
});
