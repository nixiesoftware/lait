import { beforeEach, describe, expect, it } from "vitest";
import { loadBoardScroll, loadHiddenColumns, saveBoardScroll, saveHiddenColumns } from "./boardState";

describe("board window state", () => {
  beforeEach(() => localStorage.clear());

  it("persists horizontal position per project and recovers from bad storage", () => {
    saveBoardScroll("a", 241.7);
    saveBoardScroll("b", 19);
    expect(loadBoardScroll("a")).toBe(242);
    expect(loadBoardScroll("b")).toBe(19);
    localStorage.setItem("lait.board-scroll.bad", "not-a-number");
    expect(loadBoardScroll("bad")).toBe(0);
  });
});

describe("hidden board columns", () => {
  beforeEach(() => localStorage.clear());

  it("round-trips the hidden set per project", () => {
    saveHiddenColumns("prj_1", ["done", "canceled"]);
    expect(loadHiddenColumns("prj_1")).toEqual(["done", "canceled"]);
    expect(loadHiddenColumns("prj_2")).toEqual([]);
  });

  // The encoding is load-bearing: storing what is SHOWN would hide every status
  // a workflow gains from everyone who ever used this control.
  it("shows a status the workflow gains later", () => {
    saveHiddenColumns("prj_1", ["done"]);
    const workflow = ["backlog", "started", "done", "blocked"];
    const hidden = loadHiddenColumns("prj_1");
    expect(workflow.filter((s) => !hidden.includes(s))).toEqual(["backlog", "started", "blocked"]);
  });

  it("survives corrupt storage rather than throwing", () => {
    localStorage.setItem("lait.board-hidden.prj_1", "{not json");
    expect(loadHiddenColumns("prj_1")).toEqual([]);
    localStorage.setItem("lait.board-hidden.prj_1", '["ok",7,null]');
    expect(loadHiddenColumns("prj_1")).toEqual(["ok"]);
  });
});
