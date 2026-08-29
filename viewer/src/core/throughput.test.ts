import { describe, expect, it } from "vitest";

import { layOutProject, measureThroughput } from "./throughput";
import type { BoardView, MilestoneDto, ProjectDto, Row, WorkflowState } from "../types";

const STATES: WorkflowState[] = [
  { id: "backlog", name: "Backlog", category: "backlog", color: "gray" },
  { id: "doing", name: "In Progress", category: "active", color: "blue" },
  { id: "done", name: "Done", category: "done", color: "green" },
];

const NOW = 1_800_000_000;
const WEEK = 604_800;

function row(doc: string, over: Partial<Row> = {}): Row {
  return {
    reff: `iss_${doc}`,
    doc_id: doc,
    project_id: "prj_1",
    key_alias: `ENG-${doc}`,
    title: `Issue ${doc}`,
    status: "backlog",
    priority: "none",
    assignee_summary: "",
    assignees: [],
    tombstone: false,
    provisional: false,
    ...over,
  };
}

const project = (over: Partial<ProjectDto> = {}): ProjectDto => ({
  id: "prj_1",
  name: "Engine",
  key: "ENG",
  color: "blue",
  ...over,
});

/** A board is a workflow's columns; the layout only reads the rows out of it. */
const board = (rows: Row[]): BoardView => ({
  schema_version: 1,
  project: project(),
  columns: STATES.map((state) => ({ state, rows: rows.filter((r) => r.status === state.id) })),
  total: rows.length,
  complete: true,
});

const milestone = (id: string, over: Partial<MilestoneDto> = {}): MilestoneDto => ({
  id,
  name: id,
  total: 0,
  done: 0,
  ...over,
});

describe("measuring throughput", () => {
  it("is closed work over the window the space has been running", () => {
    const rows = [
      row("a", { status: "done" }),
      row("b", { status: "done" }),
      row("c", { status: "doing" }),
    ];
    const rate = measureThroughput(
      [project({ start_date: NOW - 4 * WEEK })],
      { ENG: board(rows) },
      STATES,
      NOW,
    );
    expect(rate).not.toBeNull();
    expect(rate!.done).toBe(2);
    expect(rate!.sinceWeeks).toBeCloseTo(4, 5);
    expect(rate!.perWeek).toBeCloseTo(0.5, 5);
  });

  /**
   * The three ways there is nothing to measure. Each returns `null` rather than
   * a stand-in: the view's whole claim is that its dates are derived from an
   * observation, so a fabricated rate would be worse than no dates at all.
   */
  it("reports no rate rather than inventing one", () => {
    const finished = [row("a", { status: "done" })];
    // No start date anywhere — no window to measure over.
    expect(
      measureThroughput([project()], { ENG: board(finished) }, STATES, NOW),
    ).toBeNull();
    // Started in the future.
    expect(
      measureThroughput(
        [project({ start_date: NOW + WEEK })],
        { ENG: board(finished) },
        STATES,
        NOW,
      ),
    ).toBeNull();
    // Nothing closed yet.
    expect(
      measureThroughput(
        [project({ start_date: NOW - WEEK })],
        { ENG: board([row("a")]) },
        STATES,
        NOW,
      ),
    ).toBeNull();
  });
});

describe("laying a project out on the work axis", () => {
  // Estimates are deliberately varied and deliberately ignored: the axis counts
  // issues, so each of these is exactly one unit wide.
  const rows = [
    row("d1", { status: "done", estimate: 2, milestone: "m1" }),
    row("d2", { status: "done", estimate: 3, milestone: "m1" }),
    row("a1", { status: "doing", estimate: 5, milestone: "m1" }),
    row("b1", { status: "backlog", estimate: 8, milestone: "m2" }),
  ];
  const lay = (rate: number | null = null) =>
    layOutProject(
      project(),
      board(rows),
      [milestone("m1"), milestone("m2")],
      STATES,
      rate,
      NOW,
    );

  /** The origin is now, so finished work is behind you and the rest is ahead. */
  it("puts finished work left of zero and the rest right of it", () => {
    const work = lay();
    expect(work.done).toBe(2);
    expect(work.remaining).toBe(2);
    expect(work.active).toBe(1);
    expect(work.blocks[0]!.from).toBe(-2);
    expect(work.blocks.at(-1)!.to).toBe(2);
  });

  it("keeps later-milestone completions behind now and earlier unfinished work ahead", () => {
    const work = layOutProject(
      project(),
      board([
        row("todo", { milestone: "m1" }),
        row("done", { milestone: "m2", status: "done" }),
      ]),
      [milestone("m1"), milestone("m2")],
      STATES,
      1,
      NOW,
    );
    expect(work.blocks.map((block) => [block.row.doc_id, block.from, block.to])).toEqual([
      ["done", -1, 0],
      ["todo", 0, 1],
    ]);
    expect(work.stops.find((stop) => stop.milestone.id === "m2")?.projected).toBeNull();
  });

  /** Contiguous same-stage work in the same milestone is one drawn block; the
   *  chart should not paint four rectangles where one says the same thing. */
  it("merges a run of one stage inside one milestone", () => {
    const work = lay();
    const first = work.segments[0]!;
    expect(first.stage).toBe("done");
    expect(first.from).toBe(-2);
    expect(first.to).toBe(0);
    expect(work.segments).toHaveLength(3);
  });

  it("stops a milestone where its last issue finishes", () => {
    const work = lay();
    expect(work.stops.map((s) => [s.milestone.id, s.from, s.at])).toEqual([
      ["m1", -2, 1],
      ["m2", 1, 2],
    ]);
    expect(work.stops[0]!.remaining).toBe(1);
  });

  /**
   * The second reading of the axis. One issue out at one issue a week is one
   * week out — and with no rate there is no date, rather than a date with no
   * basis.
   */
  it("projects a landing date from the rate, and only from the rate", () => {
    expect(lay().stops[0]!.projected).toBeNull();
    const projected = lay(1).stops[0]!.projected;
    expect(projected).toBeCloseTo(NOW + WEEK, 0);
  });

  it("calls a milestone late only when the work lands after its own target", () => {
    const soon = layOutProject(
      project(),
      board(rows),
      [milestone("m1", { target_date: NOW + WEEK / 2 }), milestone("m2")],
      STATES,
      1,
      NOW,
    );
    // One issue at one a week lands in a week; the target is half that.
    expect(soon.stops[0]!.late).toBe(true);

    const roomy = layOutProject(
      project(),
      board(rows),
      [milestone("m1", { target_date: NOW + 10 * WEEK }), milestone("m2")],
      STATES,
      1,
      NOW,
    );
    expect(roomy.stops[0]!.late).toBe(false);
  });

  it("says nothing about lateness with no target or no rate", () => {
    expect(lay(1).stops[0]!.late).toBe(false);
    const noRate = layOutProject(
      project(),
      board(rows),
      [milestone("m1", { target_date: NOW - WEEK })],
      STATES,
      null,
      NOW,
    );
    expect(noRate.stops[0]!.late).toBe(false);
  });

  it("skips a milestone with no issues rather than drawing a stop at zero", () => {
    const work = layOutProject(
      project(),
      board(rows),
      [milestone("m1"), milestone("empty"), milestone("m2")],
      STATES,
      null,
      NOW,
    );
    expect(work.stops.map((s) => s.milestone.id)).toEqual(["m1", "m2"]);
  });

  it("handles a project whose board has not loaded", () => {
    const work = layOutProject(project(), undefined, [], STATES, null, NOW);
    expect(work.done).toBe(0);
    expect(work.remaining).toBe(0);
    expect(work.blocks).toEqual([]);
  });
});
