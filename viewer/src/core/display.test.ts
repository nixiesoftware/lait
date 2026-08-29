import { describe, expect, it } from "vitest";

import { DEFAULT_DISPLAY, filterNotice, groupRows } from "./display";
import type { BoardView, Row } from "../types";

const row = (over: Partial<Row> & { reff: string }): Row => ({
  doc_id: `doc_${over.reff}`,
  project_id: "prj_1",
  key_alias: null,
  title: "",
  status: "backlog",
  priority: "none",
  assignee_summary: "",
  assignees: [],
  tombstone: false,
  provisional: false,
  ...over,
});

const board = (rows: Row[]): BoardView => ({
  schema_version: 1,
  project: { id: "prj_1", name: "P", key: "P", color: "blue" },
  columns: [
    {
      state: { id: "backlog", name: "Backlog", category: "backlog", color: "gray" },
      rows: rows.filter((r) => r.status === "backlog"),
    },
    {
      state: { id: "done", name: "Done", category: "done", color: "green" },
      rows: rows.filter((r) => r.status === "done"),
    },
  ],
  total: null,
  complete: true,
});

describe("groupRows", () => {
  const rows = [
    row({ reff: "a", title: "zebra", priority: "low", assignees: ["k1"] }),
    row({ reff: "b", title: "apple", priority: "urgent", assignees: ["k2", "k1"] }),
    row({ reff: "c", title: "mango", status: "done" }),
  ];

  it("status grouping is the board's own columns, order untouched", () => {
    const groups = groupRows(board(rows), DEFAULT_DISPLAY);
    expect(groups.map((g) => g.key)).toEqual(["backlog", "done"]);
    expect(groups[0]!.state?.name).toBe("Backlog");
    expect(groups[0]!.rows.map((r) => r.reff)).toEqual(["a", "b"]);
  });

  it("groups by first assignee, one group per issue, unassigned last", () => {
    const groups = groupRows(board(rows), { ...DEFAULT_DISPLAY, group: "assignee" });
    expect(groups.map((g) => g.key)).toEqual(["k1", "k2", "unassigned"]);
    // b has two assignees but appears exactly once (under k2, its first).
    expect(groups.flatMap((g) => g.rows).filter((r) => r.reff === "b")).toHaveLength(1);
    expect(groups[2]!.label).toBe("Unassigned");
  });

  it("groups by priority, highest first, empty tiers dropped", () => {
    const groups = groupRows(board(rows), { ...DEFAULT_DISPLAY, group: "priority" });
    expect(groups.map((g) => g.key)).toEqual(["urgent", "low", "none"]);
  });

  it("orders by priority stably and by title alphabetically", () => {
    const byPriority = groupRows(board(rows), { ...DEFAULT_DISPLAY, group: "none", order: "priority" });
    expect(byPriority[0]!.rows.map((r) => r.reff)).toEqual(["b", "a", "c"]);
    const byTitle = groupRows(board(rows), { ...DEFAULT_DISPLAY, group: "none", order: "title" });
    expect(byTitle[0]!.rows.map((r) => r.reff)).toEqual(["b", "c", "a"]);
  });

  it("keeps display arrangements independent by view and project", async () => {
    localStorage.clear();
    const { loadDisplay, saveDisplay } = await import("./display");
    saveDisplay({ group: "priority", order: "title", deleted: false });
    expect(loadDisplay("missing")).toMatchObject({ group: "priority", order: "title" });
    saveDisplay({ group: "none", order: "board", deleted: true }, "ws/PRJ/list");
    expect(loadDisplay("ws/PRJ/list")).toEqual({ group: "none", order: "board", deleted: true });
    expect(loadDisplay("ws/PRJ/board")).toMatchObject({ group: "priority", order: "title" });
  });
});

describe("filterNotice", () => {
  it("counts what the filter held back", () => {
    expect(filterNotice(12, 9)).toEqual({ hidden: 3, unloaded: null, show: true });
  });

  it("says nothing when the filter hid nothing", () => {
    expect(filterNotice(12, 12)).toEqual({ hidden: 0, unloaded: null, show: false });
  });

  // The empty case belongs to the filtered-empty state, which offers the same
  // "clear" action. Two notices answering one question is worse than either.
  it("defers to the empty state when the filter hid everything", () => {
    expect(filterNotice(12, 0)).toEqual({ hidden: 12, unloaded: null, show: false });
  });

  // A stale total must never render "-2 issues hidden".
  it("never reports a negative count", () => {
    expect(filterNotice(3, 5)).toEqual({ hidden: 0, unloaded: null, show: false });
  });

  // The defect this exists for. A hundred rows loaded from a project the
  // engine counted at five hundred: the filter hid three of the hundred, and
  // four hundred were never fetched. Those are different facts, and the old
  // notice reported only the first as if it were the whole story.
  it("separates what the filter hid from what was never loaded", () => {
    expect(filterNotice(100, 97, 500)).toEqual({ hidden: 3, unloaded: 400, show: true });
  });

  // Nothing hidden by the filter, but a page short of the project: still
  // worth saying, because a count over these rows is a count of a page.
  it("shows for unloaded rows even when the filter hid nothing", () => {
    expect(filterNotice(100, 100, 500)).toEqual({ hidden: 0, unloaded: 400, show: true });
  });

  // Unmeasured is absent, never zero. An engine that declined to count must not
  // be read as having counted nothing.
  it("passes an unmeasured total through as absent", () => {
    expect(filterNotice(100, 97, null)).toEqual({ hidden: 3, unloaded: null, show: true });
  });

  it("does not invent unloaded rows when everything is loaded", () => {
    expect(filterNotice(12, 9, 12)).toEqual({ hidden: 3, unloaded: 0, show: true });
  });
});
