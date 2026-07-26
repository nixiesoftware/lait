import { describe, expect, it, vi } from "vitest";
import { projectKeys, ProjectViewerStore } from "./projectStore";
import type { BoardView, Response, Row, SpaceDoorbell } from "./types";

const row: Row = {
  reff: "iss_1",
  doc_id: "doc_1",
  project_id: "prj_1",
  key_alias: "ONE-1",
  title: "Catalog title",
  status: "todo",
  priority: "none",
  assignee_summary: "",
  assignees: [],
  tombstone: false,
  provisional: false,
};

const board: BoardView & { kind: "board" } = {
  kind: "board",
  schema_version: 3,
  project: { id: "prj_1", key: "ONE", name: "One", color: "blue" },
  columns: [{
    state: { id: "todo", name: "Todo", category: "backlog", color: "gray" },
    rows: [row],
  }],
};

describe("ProjectViewerStore", () => {
  it("normalizes board rows and composes partial detail immediately", async () => {
    const rpc = vi.fn(async () => board as Response);
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    expect(store.selectRow("local", row.reff)).toEqual(row);
    const detail = store.selectIssueDetail("local", row.reff);
    expect(store.selectRow("local", row.reff)).toBe(store.selectRow("local", row.reff));
    expect(store.selectBoard("local", "ONE")).toBe(store.selectBoard("local", "ONE"));
    expect(store.selectIssueDetail("local", row.reff)).toBe(detail);
    expect(detail).toMatchObject({
      partial: true,
      issue: { title: "Catalog title", description: "", comments: [] },
    });
  });

  it("shares optimistic values across board and issue detail", async () => {
    let finish!: () => void;
    const write = new Promise<void>((resolve) => { finish = resolve; });
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      await write;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    const pending = store.editTitle("local", row.reff, "Instant");
    expect(store.selectBoard("local", "ONE")?.columns[0]?.rows[0]?.title).toBe("Instant");
    expect(store.selectIssueDetail("local", row.reff).issue?.title).toBe("Instant");
    finish();
    await pending;
  });

  it("refreshes an affected board before retiring its prediction", async () => {
    let authoritative = board;
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return authoritative as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    const unsubscribe = store.resources.subscribe(projectKeys.board("local", "ONE"), () => undefined);
    await store.editTitle("local", row.reff, "Instant");
    authoritative = {
      ...board,
      columns: [{ ...board.columns[0]!, rows: [{ ...row, title: "Authoritative" }] }],
    };
    const doorbell: SpaceDoorbell = {
      space: "local",
      epoch: 1,
      seq: 1,
      reset: false,
      dirty_by_project: { ONE: [row.doc_id] },
      dirty_catalog: [],
      activity_advanced: false,
      presence_advanced: false,
    };
    await store.handleDoorbell(doorbell);
    expect(store.selectIssueDetail("local", row.reff).issue?.title).toBe("Authoritative");
    expect(store.overlay.has(row.doc_id)).toBe(false);
    unsubscribe();
  });

  it("refreshes catalog-derived resources on a ring that names no doc", async () => {
    // The case the old switch missed. A milestone write touches the catalog and
    // no issue body, so the frame carries catalog planes with an EMPTY
    // `dirty_by_project` — and milestones used to be invalidated only when some
    // issue doc was dirty, so the list it belongs to never refreshed.
    let milestones = [{ id: "ms_1", project_id: "prj_1", name: "v1", tombstone: false, total: 0, done: 0 }];
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      if (request.cmd === "milestone_list") return { kind: "milestones", milestones } as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    await store.ensureMilestones("local", "prj_1");
    const key = projectKeys.milestones("local", "prj_1");
    const unsubscribe = store.resources.subscribe(key, () => undefined);

    milestones = [...milestones, { id: "ms_2", project_id: "prj_1", name: "v2", tombstone: false, total: 0, done: 0 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      dirty_by_project: {}, dirty_catalog: [{ scope: "milestones", project: "ONE" }],
      activity_advanced: false, presence_advanced: false,
    });

    expect(store.resources.read(key).data).toHaveLength(2);
    unsubscribe();
  });

  it("leaves resources alone that the dirty plane does not reach", async () => {
    // The other half of precision, and the half a coarse ring cannot give you:
    // a milestone edit must not drag the label registry along behind it.
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      if (request.cmd === "label_list") return { kind: "labels", labels: [] } as Response;
      return { kind: "milestones", milestones: [] } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    await store.ensureLabels("local");
    const labels = projectKeys.labels("local");
    const unsubscribe = store.resources.subscribe(labels, () => undefined);
    const before = rpc.mock.calls.filter((c) => c[1].cmd === "label_list").length;

    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      dirty_by_project: {}, dirty_catalog: [{ scope: "milestones", project: "ONE" }],
      activity_advanced: false, presence_advanced: false,
    });

    const after = rpc.mock.calls.filter((c) => c[1].cmd === "label_list").length;
    expect(after).toBe(before);
    unsubscribe();
  });

  it("does not invalidate an unrelated selected issue", async () => {
    const rpc = vi.fn(async () => board as Response);
    const store = new ProjectViewerStore(rpc);
    store.resources.set(projectKeys.issue("local", "iss_other"), {
      schema_version: 3, reff: "iss_other", doc_id: "doc_other", space_id: "local",
      project_id: "prj_1", project_key: "ONE", key_alias: "ONE-2", title: "Other",
      description: "", status: "todo", priority: "none", assignees: [], labels: [],
      label_names: [], comments: [], created_by: "", created_at: 0, provisional: false,
    });
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      dirty_by_project: { ONE: [row.doc_id] }, dirty_catalog: [],
      activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(projectKeys.issue("local", "iss_other")).state).toBe("ready");
  });
});
