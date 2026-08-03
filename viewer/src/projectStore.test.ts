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
      dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: [row.doc_id] }],
      planes: [],
      authority_advanced: false,
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
    // `dirty` — and milestones used to be invalidated only when some
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
      dirty: [],
      planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "ONE" } }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    expect(store.resources.read(key).data).toHaveLength(2);
    unsubscribe();
  });

  it("refreshes milestone progress when an issue moves, not just the records", async () => {
    // The half of the milestone derivation that has nothing to do with the
    // catalog: `total`/`done` are counted from ISSUE bodies, so dragging a card
    // to Done changes a percentage on a project overview nobody touched. The
    // ring here names a dirty doc and no milestone plane at all — the shape a
    // private `useState` copy in the overview could never hear.
    let milestones = [{ id: "ms_1", project_id: "prj_1", name: "v1", tombstone: false, total: 2, done: 0 }];
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      if (request.cmd === "milestone_list") return { kind: "milestones", milestones } as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureMilestones("local", "prj_1");
    const key = projectKeys.milestones("local", "prj_1");
    const unsubscribe = store.resources.subscribe(key, () => undefined);

    milestones = [{ id: "ms_1", project_id: "prj_1", name: "v1", tombstone: false, total: 2, done: 1 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: ["doc_1"] }],
      planes: [],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    expect(store.resources.read<typeof milestones>(key).data?.[0]?.done).toBe(1);

    // ...and another project's issues leave this project's bars alone.
    milestones = [{ id: "ms_1", project_id: "prj_1", name: "v1", tombstone: false, total: 2, done: 2 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 2, reset: false,
      dirty: [{ kind: "project", id: "prj_2", label: "TWO", docs: ["doc_9"] }],
      planes: [],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read<typeof milestones>(key).data?.[0]?.done).toBe(1);
    unsubscribe();
  });

  it("rings the update feed on its own plane and nothing else's", async () => {
    // An update is authored once and never edited, so unlike the milestone bars
    // the feed depends on its own plane alone: no issue moving can change what a
    // past post said, and a milestone edit must not refetch it.
    let updates = [{ id: "upd_1", author: "act_1", ts: 1, body: "first" }];
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "project_updates") return { kind: "updates", updates } as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureUpdates("local", "prj_1");
    const key = projectKeys.updates("local", "prj_1");
    const unsubscribe = store.resources.subscribe(key, () => undefined);

    // A teammate's post reaches us without a reload of our own.
    updates = [...updates, { id: "upd_2", author: "act_2", ts: 2, body: "second" }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      dirty: [],
      planes: [{ plane: "updates", scope: { kind: "project", id: "prj_1", label: "ONE" } }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(key).data).toHaveLength(2);

    const before = rpc.mock.calls.filter((c) => c[1].cmd === "project_updates").length;
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 2, reset: false,
      dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: ["doc_1"] }],
      planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "ONE" } }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(rpc.mock.calls.filter((c) => c[1].cmd === "project_updates").length).toBe(before);
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
      dirty: [],
      planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "ONE" } }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    const after = rpc.mock.calls.filter((c) => c[1].cmd === "label_list").length;
    expect(after).toBe(before);
    unsubscribe();
  });

  it("matches a project dependency by id, not by its renameable key", async () => {
    // The reason the ring carries both. A project KEY is a display alias that
    // `project edit --key` moves; the `prj_` id is identity. If dependencies
    // matched on the key, the first rename would silently detach every resource
    // scoped to that project — a panel that stops refreshing and says nothing.
    let milestones = [{ id: "ms_1", project_id: "prj_1", name: "v1", tombstone: false, total: 0, done: 0 }];
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      return { kind: "milestones", milestones } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureMilestones("local", "prj_1");
    const key = projectKeys.milestones("local", "prj_1");
    const unsubscribe = store.resources.subscribe(key, () => undefined);

    milestones = [...milestones, { id: "ms_2", project_id: "prj_1", name: "v2", tombstone: false, total: 0, done: 0 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      dirty: [],
      // Same project, renamed since this resource was registered.
      planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "RENAMED" } }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(key).data).toHaveLength(2);

    // ...and a genuinely different project still does not reach it.
    milestones = [...milestones, { id: "ms_3", project_id: "prj_1", name: "v3", tombstone: false, total: 0, done: 0 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 2, reset: false,
      dirty: [],
      planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_2", label: "ONE" } }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(key).data).toHaveLength(2);
    unsubscribe();
  });

  it("refreshes members on authority, which is not a catalog plane", async () => {
    let members = [
      { actor: "act_1", key: "act_1", nick: "a", alias: "", role: "admin", admin: true, me: true, devices: [] },
    ];
    // Membership is generic Space control, so it rides the space transport —
    // injected, like the World one, rather than reaching for the module import.
    const spaceRpc = vi.fn(async () => ({ kind: "members", members }) as Response);
    const store = new ProjectViewerStore(undefined, spaceRpc);
    await store.ensureMembers("local");
    const key = projectKeys.members("local");
    const unsubscribe = store.resources.subscribe(key, () => undefined);

    members = [...members, {
      actor: "act_2", key: "act_2", nick: "b", alias: "", role: "member",
      admin: false, me: false, devices: [],
    }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      dirty: [], planes: [],
      authority_advanced: true, activity_advanced: false, presence_advanced: true,
    });
    expect(store.resources.read(key).data).toHaveLength(2);
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
      dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: [row.doc_id] }],
      planes: [], authority_advanced: false,
      activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(projectKeys.issue("local", "iss_other")).state).toBe("ready");
  });

  it("predicts an assignee toggle and stacks a second on the first", async () => {
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    const first = store.toggleAssignee("local", row.reff, "a".repeat(64), true);
    // Before the doorbell retires the first guess, a second toggle must build
    // on the predicted set — not the server's stale empty one.
    const second = store.toggleAssignee("local", row.reff, "b".repeat(64), true);
    expect(store.selectRow("local", row.reff)?.assignees).toEqual([
      "a".repeat(64),
      "b".repeat(64),
    ]);
    expect(store.selectBoard("local", "ONE")?.columns[0]?.rows[0]?.assignees).toHaveLength(2);
    await Promise.all([first, second]);
    expect(rpc).toHaveBeenCalledWith("local", {
      cmd: "assign", reff: row.reff, who: ["a".repeat(64)], add: true,
    });
  });

  it("predicts a due date, and its clearing, in the row's units", async () => {
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    await store.setDue("local", row.reff, "2026-07-30");
    expect(store.selectRow("local", row.reff)?.due_date).toBe(
      Date.UTC(2026, 6, 30) / 1000,
    );
    expect(rpc).toHaveBeenCalledWith("local", {
      cmd: "issue_edit", reff: row.reff, due: "2026-07-30",
    });
    await store.setDue("local", row.reff, null);
    expect(store.selectRow("local", row.reff)?.due_date).toBeNull();
    expect(rpc).toHaveBeenCalledWith("local", {
      cmd: "issue_edit", reff: row.reff, due: "none",
    });
  });

  it("rolls a refused label toggle back immediately", async () => {
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return board as Response;
      throw new Error("refused");
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    await expect(store.toggleLabel("local", row.reff, "infra", true)).rejects.toThrow("refused");
    expect(store.selectRow("local", row.reff)?.label_names ?? []).toEqual([]);
  });
});
