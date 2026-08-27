import { describe, expect, it } from "vitest";

import { mergeBoards, projectsOf, teamAsProject, ungrouped } from "./teams";
import type { BoardView, ProjectDto, Row, TeamDto, WorkflowState } from "../types";

const STATES: WorkflowState[] = [
  { id: "backlog", name: "Backlog", category: "backlog", color: "gray" },
  { id: "doing", name: "In Progress", category: "active", color: "blue" },
  { id: "done", name: "Done", category: "done", color: "green" },
];

const project = (key: string, over: Partial<ProjectDto> = {}): ProjectDto => ({
  id: `prj_${key}`,
  name: key,
  key,
  color: "blue",
  ...over,
});

const team = (key: string, over: Partial<TeamDto> = {}): TeamDto => ({
  id: `tm_${key}`,
  name: key,
  key,
  members: [],
  projects: [],
  ...over,
});

const row = (doc: string, status: string): Row => ({
  reff: `iss_${doc}`,
  doc_id: doc,
  project_id: "prj_x",
  key_alias: doc,
  title: doc,
  status,
  priority: "none",
  assignee_summary: "",
  assignees: [],
  tombstone: false,
  provisional: false,
});

const board = (rows: Row[], states = STATES): BoardView => ({
  schema_version: 1,
  project: project("X"),
  columns: states.map((state) => ({ state, rows: rows.filter((r) => r.status === state.id) })),
  total: rows.length,
  complete: true,
});

describe("which projects a team owns", () => {
  const teams = [team("PLAT"), team("DES")];
  const projects = [
    project("INF", { team: "tm_PLAT" }),
    project("GW", { team: "tm_PLAT" }),
    project("UI", { team: "tm_DES" }),
    project("SCRATCH"),
  ];

  it("reads the field on the project, not the team's back-reference", () => {
    // The back-reference here is deliberately empty and wrong; the authority is
    // `ProjectDto.team`, and if the two ever drift the field on the thing being
    // grouped is what decides which group it is in.
    expect(projectsOf(teams[0]!, projects).map((p) => p.key)).toEqual(["INF", "GW"]);
    expect(projectsOf(teams[1]!, projects).map((p) => p.key)).toEqual(["UI"]);
  });

  it("puts a project with no team in the ungrouped bucket", () => {
    expect(ungrouped(teams, projects).map((p) => p.key)).toEqual(["SCRATCH"]);
  });

  /** A team deleted out from under its projects leaves them addressable. */
  it("puts a project whose team no longer exists in the ungrouped bucket too", () => {
    expect(ungrouped([teams[1]!], projects).map((p) => p.key)).toEqual(["INF", "GW", "SCRATCH"]);
  });

  it("makes every project ungrouped when there are no teams", () => {
    expect(ungrouped([], projects)).toHaveLength(4);
  });
});

describe("one board across several projects", () => {
  it("concatenates rows column by column", () => {
    const merged = mergeBoards(
      [board([row("a", "backlog"), row("b", "doing")]), board([row("c", "backlog")])],
      project("PLAT"),
    );
    expect(merged!.columns.map((c) => [c.state.id, c.rows.map((r) => r.doc_id)])).toEqual([
      ["backlog", ["a", "c"]],
      ["doing", ["b"]],
      ["done", []],
    ]);
  });

  it("reports itself as the scope it was given", () => {
    const merged = mergeBoards([board([])], project("PLAT"));
    expect(merged!.project.key).toBe("PLAT");
  });

  /**
   * A project still loading contributes nothing rather than dropping a column
   * every other project has — the columns are the *space's* workflow, so a
   * partial board must not narrow them.
   */
  it("keeps a column a later board has not got to yet", () => {
    const partial = board([row("a", "backlog")], [STATES[0]!]);
    const merged = mergeBoards([partial, board([row("b", "done")])], project("PLAT"));
    expect(merged!.columns.map((c) => c.state.id)).toEqual(["backlog", "doing", "done"]);
    expect(merged!.columns[2]!.rows.map((r) => r.doc_id)).toEqual(["b"]);
  });

  it("is null when nothing has loaded, the same shape as a board that has not arrived", () => {
    expect(mergeBoards([], project("PLAT"))).toBeNull();
  });

  it("does not mutate the boards it merges", () => {
    const one = board([row("a", "backlog")]);
    mergeBoards([one, board([row("b", "backlog")])], project("PLAT"));
    expect(one.columns[0]!.rows.map((r) => r.doc_id)).toEqual(["a"]);
  });
});

describe("a team standing in for a project", () => {
  it("carries the team's own identity, so nothing keyed on it can collide", () => {
    const stand = teamAsProject(team("PLAT"));
    expect([stand.id, stand.key, stand.name]).toEqual(["tm_PLAT", "PLAT", "PLAT"]);
  });
});
