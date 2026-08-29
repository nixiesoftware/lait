import { describe, expect, it, vi } from "vitest";
import { LaitError } from "./api";
import { loadBoardScroll, saveBoardScroll } from "./core/boardState";
import { projectKeys, ProjectViewerStore } from "./projectStore";
import type {
  BoardView,
  Page,
  DirtyPlane,
  DirtyScope,
  Response,
  SpecSummary,
  Row,
  SpaceDoorbell,
  SpecLink,
  WorkflowState,
  WorldRequest,
} from "./types";

const publication = {
  publication: {
    manifest_root: Array(32).fill(1),
    implementation_digest: Array(32).fill(2),
    extractor_schema_digest: Array(32).fill(3),
  },
  materialization: 1,
};
const acceptedPublication = {
  publication: {
    manifest_root: Array(32).fill(9),
    implementation_digest: Array(32).fill(2),
    extractor_schema_digest: Array(32).fill(3),
  },
  materialization: 2,
};
const page = <T,>(items: T[]): Page<T> => ({ publication, items });

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
  total: 1,
  complete: true,
};

const boardResponse = (view: BoardView): Response => ({
  kind: "board",
  schema_version: view.schema_version,
  project: view.project,
  workflow: view.columns.map((column) => column.state),
  rows: page(view.columns.flatMap((column) => column.rows)),
});

const changeSetResponse = (request: { cmd: string }): Response => ({
  kind: "change_set",
  results: [],
  receipt: {
    operation: (request as { operation?: string }).operation ?? "00".repeat(16),
    phase: "accepted",
    publication: acceptedPublication,
  },
});

describe("ProjectViewerStore", () => {
  it("keeps one operation continuous through arbitrary Board latency and typed rollback", async () => {
    const operation = "11".repeat(16);
    const secondOperation = "22".repeat(16);
    const thirdOperation = "33".repeat(16);
    const committedPublication = {
      publication: {
        manifest_root: Array(32).fill(4),
        implementation_digest: Array(32).fill(2),
        extractor_schema_digest: Array(32).fill(3),
      },
      materialization: 2,
    };
    const done: WorkflowState = {
      id: "done",
      name: "Done",
      category: "done",
      color: "green",
    };
    const other: Row = { ...row, reff: "iss_2", doc_id: "doc_2", key_alias: "ONE-2", title: "Second" };
    let authoritative = false;
    let refreshFirst!: (response: Response) => void;
    let finishMutation!: (response: Response) => void;
    let failMutation!: (error: unknown) => void;
    let mutation = new Promise<Response>((resolve, reject) => {
      finishMutation = resolve;
      failMutation = reject;
    });
    const firstBoardPage = (nextPublication = publication): Response => ({
      kind: "board",
      schema_version: 3,
      project: board.project,
      workflow: [board.columns[0]!.state, done],
      rows: { publication: nextPublication, items: [authoritative ? other : row], next_cursor: "next" },
    });
    const secondBoardPage = (nextPublication = publication): Response => ({
      kind: "board",
      schema_version: 3,
      project: board.project,
      workflow: [board.columns[0]!.state, done],
      rows: {
        publication: nextPublication,
        items: [authoritative ? { ...row, status: "done" } : other],
      },
    });
    const rpc = vi.fn(async (_space: string, request: WorldRequest) => {
      if (request.cmd === "change_set") return mutation;
      if (request.cmd !== "board") throw new Error("unexpected request");
      if (request.page?.cursor) {
        return secondBoardPage(authoritative ? committedPublication : publication);
      }
      if (!authoritative) return firstBoardPage();
      return new Promise<Response>((resolve) => { refreshFirst = resolve; });
    });
    const ids = [operation, operation, secondOperation, thirdOperation];
    const store = new ProjectViewerStore(rpc, undefined, undefined, () => ids.shift()!);
    await store.ensureBoard("local", "ONE");
    await store.loadMoreBoard("local", "ONE");
    const boardKey = projectKeys.board("local", "ONE");
    const rawBefore = store.resources.read<BoardView>(boardKey).data;
    const selected = other.reff;
    saveBoardScroll("prj_1", 347);
    const unsubscribe = store.resources.subscribe(boardKey, () => undefined);

    const pending = store.beginBoardChange("local", "ONE", row.doc_id, row.reff, "done", null);
    expect(pending.operation).toBe(operation);
    expect(store.operation("local", operation)?.phase).toBe("sending");
    expect(store.selectBoard("local", "ONE")?.columns[1]?.rows[0]?.doc_id).toBe(row.doc_id);
    expect(store.resources.read<BoardView>(boardKey).data).toBe(rawBefore);
    await Promise.resolve();
    expect(store.operation("local", operation)?.phase).toBe("sending");
    expect(selected).toBe(other.reff);
    expect(loadBoardScroll("prj_1")).toBe(347);

    finishMutation({
      kind: "change_set",
      results: [{ operation: 0, kind: "issue", id: row.doc_id }],
      receipt: { operation, phase: "accepted", publication: committedPublication },
    });
    await pending.completion;
    expect(store.operation("local", operation)?.phase).toBe("accepted");
    expect(store.overlay.has(row.doc_id)).toBe(true);

    authoritative = true;
    const ringing = store.handleDoorbell({
      space: "local",
      epoch: 1,
      seq: 1,
      reset: false,
      invalidations: [{
        world: "com.lait.issues",
        dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: [row.doc_id] }],
        planes: [],
      }],
      publications: [{ world: "com.lait.issues", publication: committedPublication }],
      change: {
        attribution: { operation: Array(16).fill(0x11), actor: "actor", device: "device" },
        bodies: [],
      },
      authority_advanced: false,
      activity_advanced: false,
      presence_advanced: false,
    });
    await Promise.resolve();
    expect(store.resources.read<BoardView>(boardKey)).toMatchObject({
      state: "refreshing",
      data: rawBefore,
    });
    expect(store.operation("local", operation)?.phase).toBe("accepted");
    expect(store.overlay.has(row.doc_id)).toBe(true);
    expect(selected).toBe(other.reff);
    expect(loadBoardScroll("prj_1")).toBe(347);
    refreshFirst(firstBoardPage(committedPublication));
    await ringing;
    expect(store.operation("local", operation)).toMatchObject({
      phase: "committed",
      publication: committedPublication,
    });
    expect(store.overlay.has(row.doc_id)).toBe(false);
    expect(store.resources.read<BoardView>(boardKey).data?.columns.flatMap((column) => column.rows))
      .toEqual([other, { ...row, status: "done" }]);
    expect(selected).toBe(other.reff);
    expect(loadBoardScroll("prj_1")).toBe(347);

    mutation = Promise.resolve({
      kind: "change_set",
      results: [{ operation: 0, kind: "issue", id: row.doc_id }],
      receipt: { operation, phase: "accepted", publication: committedPublication },
    });
    const replay = store.beginBoardChange("local", "ONE", row.doc_id, row.reff, "done", null);
    expect(replay.operation).toBe(operation);
    expect(store.operation("local", operation)?.phase).toBe("sending");
    await replay.completion;
    expect(store.operation("local", operation)?.phase).toBe("committed");

    mutation = new Promise<Response>((resolve, reject) => {
      finishMutation = resolve;
      failMutation = reject;
    });
    const refused = store.beginBoardChange("local", "ONE", row.doc_id, row.reff, "todo", null);
    expect(store.operation("local", secondOperation)?.phase).toBe("sending");
    failMutation(new LaitError("workflow transition denied", 403, "denied"));
    const rollback = await refused.completion;
    expect(rollback).toMatchObject({
      operation: secondOperation,
      phase: "rolled_back",
      error: { kind: "denied", message: "workflow transition denied" },
    });
    expect(store.overlay.has(row.doc_id)).toBe(false);
    expect(store.resources.read<BoardView>(boardKey).data?.columns.flatMap((column) => column.rows))
      .toEqual([other, { ...row, status: "done" }]);
    expect(selected).toBe(other.reff);
    expect(loadBoardScroll("prj_1")).toBe(347);

    mutation = new Promise<Response>((resolve, reject) => {
      finishMutation = resolve;
      failMutation = reject;
    });
    const changeSetCallsBefore = rpc.mock.calls.filter(([, request]) => request.cmd === "change_set").length;
    const uncertain = store.beginBoardChange("local", "ONE", row.doc_id, row.reff, "todo", null);
    expect(uncertain.operation).toBe(thirdOperation);
    expect(store.operation("local", thirdOperation)?.phase).toBe("sending");
    expect(store.selectBoard("local", "ONE")?.columns[0]?.rows.some((item) =>
      item.doc_id === row.doc_id)).toBe(true);
    failMutation(new LaitError("durable outcome unknown", 500, "indeterminate"));
    const indeterminate = await uncertain.completion;
    expect(indeterminate).toMatchObject({
      operation: thirdOperation,
      phase: "indeterminate",
      error: { kind: "indeterminate", message: "durable outcome unknown" },
    });
    expect(store.overlay.has(row.doc_id)).toBe(true);
    expect(store.selectBoard("local", "ONE")?.columns[0]?.rows.some((item) =>
      item.doc_id === row.doc_id)).toBe(true);
    expect(store.resources.read<BoardView>(boardKey).data?.columns.flatMap((column) => column.rows))
      .toEqual([other, { ...row, status: "done" }]);
    expect(selected).toBe(other.reff);
    expect(loadBoardScroll("prj_1")).toBe(347);
    expect(rpc.mock.calls.filter(([, request]) => request.cmd === "change_set")).toHaveLength(
      changeSetCallsBefore + 1,
    );
    unsubscribe();
  });

  it("normalizes board rows and composes partial detail immediately", async () => {
    const rpc = vi.fn(async () => boardResponse(board));
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

  it("commits a scalar operation only after its own target renders at the exact WPI", async () => {
    const operation = "55".repeat(16);
    const issue = {
      schema_version: 3,
      reff: row.reff,
      doc_id: row.doc_id,
      space_id: "local",
      project_id: row.project_id,
      project_key: "ONE",
      key_alias: row.key_alias,
      title: "Instant",
      description: "",
      status: row.status,
      priority: row.priority,
      assignees: [],
      labels: [],
      label_names: [],
      comments: [],
      created_by: "actor",
      created_at: 1,
      provisional: false,
    };
    const exactPage = <T,>(items: T[]): Page<T> => ({
      publication: acceptedPublication,
      items,
    });
    const rpc = vi.fn(async (_space: string, request: WorldRequest): Promise<Response> => {
      if (request.cmd === "board") return boardResponse(board);
      if (request.cmd === "change_set") return changeSetResponse(request);
      if (request.cmd !== "issue_detail") throw new Error("unexpected request");
      expect(request.publication).toEqual(acceptedPublication);
      return {
        kind: "issue_detail",
        publication: acceptedPublication,
        issue,
        comments: exactPage([]),
        reactions: exactPage([]),
        attachments: exactPage([]),
        checks: exactPage([]),
        outgoing_relations: exactPage([]),
        incoming_relations: exactPage([]),
      };
    });
    const store = new ProjectViewerStore(rpc, undefined, undefined, () => operation);
    // Normalize the row without subscribing to the Board resource. Terminal
    // reconciliation must therefore render this target, not rely on a Board.
    await store.ensureBoard("local", "ONE");
    await store.editTitle("local", row.reff, "Instant");
    expect(store.operation("local", operation)?.phase).toBe("accepted");
    expect(store.overlay.has(row.doc_id)).toBe(true);

    await store.handleDoorbell({
      space: "local",
      epoch: 1,
      seq: 1,
      reset: false,
      invalidations: [{
        world: "com.lait.issues",
        dirty: [{ kind: "project", id: row.project_id, label: "ONE", docs: [row.doc_id] }],
        planes: [],
      }],
      publications: [{ world: "com.lait.issues", publication: acceptedPublication }],
      change: {
        attribution: { operation: Array(16).fill(0x55), actor: "actor", device: "device" },
        bodies: [],
      },
      authority_advanced: false,
      activity_advanced: false,
      presence_advanced: false,
    });
    expect(store.operation("local", operation)).toMatchObject({
      phase: "committed",
      publication: acceptedPublication,
    });
    expect(store.overlay.has(row.doc_id)).toBe(false);
    expect(store.selectIssueDetail("local", row.reff).issue?.title).toBe("Instant");
  });

  it("reconciles an indeterminate operation read-only without blind resubmission", async () => {
    const operation = "66".repeat(16);
    let readiness: "absent" | "building" | "ready" = "building";
    const issue = {
      schema_version: 3,
      reff: row.reff,
      doc_id: row.doc_id,
      space_id: "local",
      project_id: row.project_id,
      project_key: "ONE",
      key_alias: row.key_alias,
      title: "Recovered",
      description: "",
      status: row.status,
      priority: row.priority,
      assignees: [],
      labels: [],
      label_names: [],
      comments: [],
      created_by: "actor",
      created_at: 1,
      provisional: false,
    };
    const exactPage = <T,>(items: T[]): Page<T> => ({ publication: acceptedPublication, items });
    const rpc = vi.fn(async (_space: string, request: WorldRequest): Promise<Response> => {
      if (request.cmd === "board") return boardResponse(board);
      if (request.cmd === "change_set") {
        throw new LaitError("durable outcome unknown", 500, "indeterminate");
      }
      if (request.cmd === "operation_status") {
        return {
          kind: "operation_status",
          operation,
          readiness,
          ...(readiness === "ready" ? { publication: acceptedPublication } : {}),
          results: [{ operation: 0, kind: "issue", id: row.doc_id }],
        };
      }
      if (request.cmd === "issue_detail") {
        expect(request.publication).toEqual(acceptedPublication);
        return {
          kind: "issue_detail",
          publication: acceptedPublication,
          issue,
          comments: exactPage([]),
          reactions: exactPage([]),
          attachments: exactPage([]),
          checks: exactPage([]),
          outgoing_relations: exactPage([]),
          incoming_relations: exactPage([]),
        };
      }
      throw new Error("unexpected request");
    });
    const store = new ProjectViewerStore(rpc, undefined, undefined, () => operation);
    await store.ensureBoard("local", "ONE");
    await store.editTitle("local", row.reff, "Recovered");
    expect(store.operation("local", operation)?.phase).toBe("indeterminate");
    expect(store.overlay.has(row.doc_id)).toBe(true);

    const building = await store.refreshOperationStatus("local", operation);
    expect(building).toMatchObject({ phase: "indeterminate", error: { kind: "building" } });
    expect(store.overlay.has(row.doc_id)).toBe(true);
    expect(rpc.mock.calls.filter(([, request]) => request.cmd === "change_set")).toHaveLength(1);

    readiness = "ready";
    const committed = await store.refreshOperationStatus("local", operation);
    expect(committed).toMatchObject({ phase: "committed", publication: acceptedPublication });
    expect(store.overlay.has(row.doc_id)).toBe(false);
    expect(store.selectIssueDetail("local", row.reff).issue?.title).toBe("Recovered");
    expect(rpc.mock.calls.filter(([, request]) => request.cmd === "change_set")).toHaveLength(1);
  });

  it("rolls optimism back only after status proves the operation absent", async () => {
    const operation = "77".repeat(16);
    const rpc = vi.fn(async (_space: string, request: WorldRequest): Promise<Response> => {
      if (request.cmd === "board") return boardResponse(board);
      if (request.cmd === "change_set") {
        throw new LaitError("durable outcome unknown", 500, "indeterminate");
      }
      if (request.cmd === "operation_status") {
        return { kind: "operation_status", operation, readiness: "absent", results: [] };
      }
      throw new Error("unexpected request");
    });
    const store = new ProjectViewerStore(rpc, undefined, undefined, () => operation);
    await store.ensureBoard("local", "ONE");
    await store.editTitle("local", row.reff, "Maybe");
    expect(store.overlay.has(row.doc_id)).toBe(true);
    const absent = await store.refreshOperationStatus("local", operation);
    expect(absent).toMatchObject({ phase: "rolled_back", error: { kind: "operation_absent" } });
    expect(store.overlay.has(row.doc_id)).toBe(false);
    expect(store.selectRow("local", row.reff)?.title).toBe(row.title);
    expect(rpc.mock.calls.filter(([, request]) => request.cmd === "change_set")).toHaveLength(1);
  });

  it("returns the created issue id from the same accepted ChangeSet", async () => {
    const operation = "88".repeat(16);
    const created = "iss_created";
    const rpc = vi.fn(async (_space: string, request: WorldRequest): Promise<Response> => {
      if (request.cmd !== "change_set") throw new Error("unexpected request");
      return {
        kind: "change_set",
        results: [{ operation: 0, kind: "issue", id: created }],
        receipt: { operation, phase: "accepted", publication: acceptedPublication },
      };
    });
    const store = new ProjectViewerStore(rpc, undefined, undefined, () => operation);
    await expect(store.createIssue("local", {
      project: "prj_1",
      title: "One durable create",
      status: "todo",
    })).resolves.toBe(created);
    const requests = rpc.mock.calls.map(([, request]) => request);
    expect(requests).toHaveLength(1);
    expect(requests[0]).toMatchObject({
      cmd: "change_set",
      operation,
      operations: [{ op: "issue_create", title: "One durable create", status: "todo" }],
    });
    expect(store.operation("local", operation)).toMatchObject({
      phase: "accepted",
      doc: created,
      results: [{ kind: "issue", id: created }],
    });
  });

  it("renders an exact label target before retiring label optimism", async () => {
    const operation = "99".repeat(16);
    const label = { id: "lbl_exact", name: "Exact", color: "violet" };
    const rpc = vi.fn(async (_space: string, request: WorldRequest): Promise<Response> => {
      if (request.cmd === "label_list") {
        return { kind: "labels", page: { publication: acceptedPublication, items: [] } };
      }
      if (request.cmd === "change_set") {
        return {
          kind: "change_set",
          results: [{ operation: 0, kind: "label", id: label.id }],
          receipt: { operation, phase: "accepted", publication: acceptedPublication },
        };
      }
      if (request.cmd === "label_show") {
        expect(request).toMatchObject({ label: label.id, publication: acceptedPublication });
        return { kind: "label", publication: acceptedPublication, label };
      }
      throw new Error("unexpected request");
    });
    const store = new ProjectViewerStore(rpc, undefined, undefined, () => operation);
    await store.ensureLabels("local");
    await store.createLabel("local", label.name, label.color);
    expect(store.operation("local", operation)?.phase).toBe("accepted");
    expect(await store.ensureLabels("local")).toEqual([
      { id: `local:${operation}`, name: label.name, color: label.color },
    ]);

    await store.handleDoorbell({
      space: "local",
      epoch: 1,
      seq: 1,
      reset: false,
      invalidations: [],
      publications: [{ world: "com.lait.issues", publication: acceptedPublication }],
      change: {
        attribution: { operation: Array(16).fill(0x99), actor: "actor", device: "device" },
        bodies: [],
      },
      authority_advanced: false,
      activity_advanced: false,
      presence_advanced: false,
    });
    expect(store.operation("local", operation)?.phase).toBe("committed");
    expect(await store.ensureLabels("local")).toEqual([label]);
    expect(rpc.mock.calls.filter(([, request]) => request.cmd === "label_show")).toHaveLength(1);
  });

  it("shares optimistic values across board and issue detail", async () => {
    let finish!: () => void;
    const write = new Promise<void>((resolve) => { finish = resolve; });
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return boardResponse(board);
      await write;
      return changeSetResponse(request);
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
    const operation = "44".repeat(16);
    let authoritative = board;
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") {
        const response = boardResponse(authoritative) as Extract<Response, { kind: "board" }>;
        return authoritative === board
          ? response
          : { ...response, rows: { ...response.rows, publication: acceptedPublication } };
      }
      return changeSetResponse(request);
    });
    const store = new ProjectViewerStore(rpc, undefined, undefined, () => operation);
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
      invalidations: [{ world: "com.lait.issues", dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: [row.doc_id] }], planes: [] }],
      publications: [{ world: "com.lait.issues", publication: acceptedPublication }],
      change: {
        attribution: { operation: Array(16).fill(0x44), actor: "actor", device: "device" },
        bodies: [],
      },
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
    const rpc = vi.fn(async (_space: string, request: WorldRequest) => {
      if (request.cmd === "board") return boardResponse(board);
      if (request.cmd === "milestone_list") return { kind: "milestones", page: page(milestones) } as Response;
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
      invalidations: [{ world: "com.lait.issues", dirty: [], planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "ONE" } }] }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    expect(store.resources.read(key).data).toHaveLength(2);
    unsubscribe();
  });

  it("refetches a project's Spec register only for that project's Spec writes", async () => {
    let specs: SpecSummary[] = [
      { spec: "spc_1", project: "prj_1", kind: "requirement", heads: ["r1"], issued: [], conflicted: false },
    ];
    const rpc = vi.fn(async (_space: string, request: { cmd: string; project?: string | null }) => {
      if (request.cmd === "spec_list") return { kind: "specs", page: page(specs) } as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureSpecs("local", "ONE");
    await store.ensureSpecs("local", null);
    const mine = projectKeys.specs("local", "ONE");
    const everywhere = projectKeys.specs("local", null);
    const stop = [mine, everywhere].map((key) => store.resources.subscribe(key, () => undefined));
    const lists = () => rpc.mock.calls.filter((call) => call[1].cmd === "spec_list").length;
    const ring = (id: string | null) => store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      invalidations: [{
        world: "com.lait.issues", dirty: [],
        planes: [{ plane: "specs", scope: id ? { kind: "project", id, label: null } : null }],
      }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    // Another project's Spec: the whole-Space register refetches, ONE's does not.
    let before = lists();
    await ring("prj_2");
    expect(lists() - before).toBe(1);

    // ONE's own Spec: both.
    specs = [...specs, { spec: "spc_2", project: "prj_1", kind: "guide", heads: ["r2"], issued: [], conflicted: false }];
    before = lists();
    await ring("prj_1");
    expect(lists() - before).toBe(2);
    expect(store.resources.read(mine).data).toHaveLength(2);

    // A ring that names no project reaches every register.
    before = lists();
    await ring(null);
    expect(lists() - before).toBe(2);
    for (const unsubscribe of stop) unsubscribe();
  });

  it("re-reads an Issue's packet for a Spec anywhere and for the Issue's own doc", async () => {
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "packet") {
        return {
          kind: "packet", issue: row.doc_id, governing: [], guidance: [], proof: [], record: [], conflicts: [],
        } as Response;
      }
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    store.resources.set(projectKeys.row("local", row.reff), row);
    await store.ensurePacket("local", row.reff);
    const key = projectKeys.packet("local", row.reff);
    const unsubscribe = store.resources.subscribe(key, () => undefined);
    const packets = () => rpc.mock.calls.filter((call) => call[1].cmd === "packet").length;
    const ring = (frame: { dirty: DirtyScope[]; planes: DirtyPlane[] }) => store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      invalidations: [{ world: "com.lait.issues", ...frame }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    // A Spec in another project may govern this Issue: the plane is whole.
    let before = packets();
    await ring({ dirty: [], planes: [{ plane: "specs", scope: { kind: "project", id: "prj_2", label: null } }] });
    expect(packets() - before).toBe(1);

    // Another Issue moving is not this one.
    before = packets();
    await ring({ dirty: [{ kind: "project", id: row.project_id, label: "ONE", docs: ["doc_9"] }], planes: [] });
    expect(packets() - before).toBe(0);

    // This Issue -- its binding is a relation on it.
    before = packets();
    await ring({ dirty: [{ kind: "project", id: row.project_id, label: "ONE", docs: [row.doc_id] }], planes: [] });
    expect(packets() - before).toBe(1);
    unsubscribe();
  });

  it("ignores an identical invalidation owned by another World", async () => {
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "milestone_list") return { kind: "milestones", page: page([]) } as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureMilestones("local", "prj_1");
    const key = projectKeys.milestones("local", "prj_1");
    const unsubscribe = store.resources.subscribe(key, () => undefined);
    const before = rpc.mock.calls.filter((call) => call[1].cmd === "milestone_list").length;

    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      invalidations: [{ world: "com.example.calendar", dirty: [], planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "ONE" } }] }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    expect(rpc.mock.calls.filter((call) => call[1].cmd === "milestone_list")).toHaveLength(before);
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
      if (request.cmd === "board") return boardResponse(board);
      if (request.cmd === "milestone_list") return { kind: "milestones", page: page(milestones) } as Response;
      return { kind: "ok", message: null } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureMilestones("local", "prj_1");
    const key = projectKeys.milestones("local", "prj_1");
    const unsubscribe = store.resources.subscribe(key, () => undefined);

    milestones = [{ id: "ms_1", project_id: "prj_1", name: "v1", tombstone: false, total: 2, done: 1 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      invalidations: [{ world: "com.lait.issues", dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: ["doc_1"] }], planes: [] }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });

    expect(store.resources.read<typeof milestones>(key).data?.[0]?.done).toBe(1);

    // ...and another project's issues leave this project's bars alone.
    milestones = [{ id: "ms_1", project_id: "prj_1", name: "v1", tombstone: false, total: 2, done: 2 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 2, reset: false,
      invalidations: [{ world: "com.lait.issues", dirty: [{ kind: "project", id: "prj_2", label: "TWO", docs: ["doc_9"] }], planes: [] }],
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
      if (request.cmd === "project_updates") return { kind: "updates", page: page(updates) } as Response;
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
      invalidations: [{ world: "com.lait.issues", dirty: [], planes: [{ plane: "updates", scope: { kind: "project", id: "prj_1", label: "ONE" } }] }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(key).data).toHaveLength(2);

    const before = rpc.mock.calls.filter((c) => c[1].cmd === "project_updates").length;
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 2, reset: false,
      invalidations: [{ world: "com.lait.issues", dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: ["doc_1"] }], planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "ONE" } }] }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(rpc.mock.calls.filter((c) => c[1].cmd === "project_updates").length).toBe(before);
    unsubscribe();
  });

  it("leaves resources alone that the dirty plane does not reach", async () => {
    // The other half of precision, and the half a coarse ring cannot give you:
    // a milestone edit must not drag the label registry along behind it.
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return boardResponse(board);
      if (request.cmd === "label_list") return { kind: "labels", page: page([]) } as Response;
      return { kind: "milestones", page: page([]) } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    await store.ensureLabels("local");
    const labels = projectKeys.labels("local");
    const unsubscribe = store.resources.subscribe(labels, () => undefined);
    const before = rpc.mock.calls.filter((c) => c[1].cmd === "label_list").length;

    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      invalidations: [{ world: "com.lait.issues", dirty: [], planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "ONE" } }] }],
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
      if (request.cmd === "board") return boardResponse(board);
      return { kind: "milestones", page: page(milestones) } as Response;
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureMilestones("local", "prj_1");
    const key = projectKeys.milestones("local", "prj_1");
    const unsubscribe = store.resources.subscribe(key, () => undefined);

    milestones = [...milestones, { id: "ms_2", project_id: "prj_1", name: "v2", tombstone: false, total: 0, done: 0 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      // Same project, renamed since this resource was registered.
      invalidations: [{ world: "com.lait.issues", dirty: [], planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_1", label: "RENAMED" } }] }],
      authority_advanced: false, activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(key).data).toHaveLength(2);

    // ...and a genuinely different project still does not reach it.
    milestones = [...milestones, { id: "ms_3", project_id: "prj_1", name: "v3", tombstone: false, total: 0, done: 0 }];
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 2, reset: false,
      invalidations: [{ world: "com.lait.issues", dirty: [], planes: [{ plane: "milestones", scope: { kind: "project", id: "prj_2", label: "ONE" } }] }],
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
      invalidations: [],
      authority_advanced: true, activity_advanced: false, presence_advanced: true,
    });
    expect(store.resources.read(key).data).toHaveLength(2);
    unsubscribe();
  });

  it("does not invalidate an unrelated selected issue", async () => {
    const rpc = vi.fn(async () => boardResponse(board));
    const store = new ProjectViewerStore(rpc);
    store.resources.set(projectKeys.issue("local", "iss_other"), {
      schema_version: 3, reff: "iss_other", doc_id: "doc_other", space_id: "local",
      project_id: "prj_1", project_key: "ONE", key_alias: "ONE-2", title: "Other",
      description: "", status: "todo", priority: "none", assignees: [], labels: [],
      label_names: [], comments: [], created_by: "", created_at: 0, provisional: false,
    });
    await store.handleDoorbell({
      space: "local", epoch: 1, seq: 1, reset: false,
      invalidations: [{ world: "com.lait.issues", dirty: [{ kind: "project", id: "prj_1", label: "ONE", docs: [row.doc_id] }], planes: [] }], authority_advanced: false,
      activity_advanced: false, presence_advanced: false,
    });
    expect(store.resources.read(projectKeys.issue("local", "iss_other")).state).toBe("ready");
  });

  it("predicts an assignee toggle and stacks a second on the first", async () => {
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return boardResponse(board);
      return changeSetResponse(request);
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
    const changes = rpc.mock.calls
      .map(([, request]) => request as WorldRequest)
      .filter((request): request is Extract<WorldRequest, { cmd: "change_set" }> =>
        request.cmd === "change_set");
    expect(changes.map((change) => change.operations)).toEqual([
      [{ op: "issue_patch", issue: row.reff, assignees: ["a".repeat(64)] }],
      [{
        op: "issue_patch",
        issue: row.reff,
        assignees: ["a".repeat(64), "b".repeat(64)],
      }],
    ]);
  });

  it("predicts a due date, and its clearing, in the row's units", async () => {
    const rpc = vi.fn(async (_space: string, request: WorldRequest) => {
      if (request.cmd === "board") return boardResponse(board);
      return changeSetResponse(request);
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    await store.setDue("local", row.reff, "2026-07-30");
    expect(store.selectRow("local", row.reff)?.due_date).toBe(
      Date.UTC(2026, 6, 30) / 1000,
    );
    expect(rpc.mock.calls.some(([, request]) =>
      request.cmd === "change_set"
      && request.operations.some((operation) =>
        operation.op === "issue_patch"
        && operation.issue === row.reff
        && operation.due === Date.UTC(2026, 6, 30) / 1000)))
      .toBe(true);
    await store.setDue("local", row.reff, null);
    expect(store.selectRow("local", row.reff)?.due_date).toBeNull();
    expect(rpc.mock.calls.some(([, request]) =>
      request.cmd === "change_set"
      && request.operations.some((operation) =>
        operation.op === "issue_patch"
        && operation.issue === row.reff
        && operation.clear_due === true)))
      .toBe(true);
  });

  it("rolls a refused label toggle back immediately", async () => {
    const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
      if (request.cmd === "board") return boardResponse(board);
      throw new Error("refused");
    });
    const store = new ProjectViewerStore(rpc);
    await store.ensureBoard("local", "ONE");
    await expect(store.toggleLabel("local", row.reff, "infra", true)).rejects.toThrow("refused");
    expect(store.selectRow("local", row.reff)?.label_names ?? []).toEqual([]);
  });

  describe("relating specs", () => {
    const link = (spec: string): SpecLink => ({
      rel: "verifies",
      target: { kind: "spec", spec, revision: "rev_x" },
    });
    /** A `spec_show`/`spec_revise` reply carrying an exact head and link set. */
    const reply = (revision: string, links: SpecLink[]) => ({
      kind: "spec",
      spec: {
        spec: "spc_1",
        project: "prj_1",
        kind: "requirement",
        title: "Login is race-free",
        state: "draft",
        revision,
        heads: [revision],
        issued: [],
        body: {
          spec: "spc_1", project: "prj_1", kind: "requirement",
          title: "Login is race-free", text: "", state: "draft",
          links, author: "act_1", ts: 0,
        },
      },
    }) as unknown as Response;

    it("writes nothing when the staged set matches the committed one", async () => {
      const rpc = vi.fn(async (_space: string, _request: { cmd: string }) =>
        reply("rev_1", [link("spc_a")]),
      );
      const store = new ProjectViewerStore(rpc as never);
      await store.relateSpec("local", "spc_1", "rev_1", { added: [], removed: [] });
      const revises = rpc.mock.calls
        .map(([, request]) => request)
        .filter((request) => request.cmd === "spec_revise");
      expect(revises).toHaveLength(0);
    });

    it("replays the delta onto a head that moved, keeping the other author's link", async () => {
      let head = "rev_1";
      const rpc = vi.fn(async (_space: string, request: { cmd: string; links?: SpecLink[] }) => {
        if (request.cmd === "spec_show") {
          // The head has already moved to rev_2, where somebody else added d.
          return reply(head, head === "rev_1" ? [link("spc_a")] : [link("spc_a"), link("spc_d")]);
        }
        if (request.cmd === "spec_revise") {
          if (head === "rev_1") { head = "rev_2"; throw new Error("that change conflicts with the current state"); }
          return reply("rev_3", request.links ?? []);
        }
        return { kind: "ok", message: null } as Response;
      });
      const store = new ProjectViewerStore(rpc as never);
      await store.relateSpec("local", "spc_1", "rev_1", { added: [link("spc_c")], removed: [] });
      const revises = rpc.mock.calls
        .map(([, request]) => request as { cmd: string; expected?: string; links?: SpecLink[] })
        .filter((request) => request.cmd === "spec_revise");
      expect(revises).toHaveLength(2);
      expect(revises[1]?.expected).toBe("rev_2");
      // The rebase kept d — which this author never saw — and still added c.
      expect(revises[1]?.links).toEqual([link("spc_a"), link("spc_d"), link("spc_c")]);
    });

    it("rethrows a refusal the head did not move under", async () => {
      const rpc = vi.fn(async (_space: string, request: { cmd: string }) => {
        if (request.cmd === "spec_show") return reply("rev_1", []);
        throw new Error("no such target");
      });
      const store = new ProjectViewerStore(rpc as never);
      await expect(
        store.relateSpec("local", "spc_1", "rev_1", { added: [link("spc_c")], removed: [] }),
      ).rejects.toThrow("no such target");
      const revises = rpc.mock.calls
        .map(([, request]) => request as { cmd: string })
        .filter((request) => request.cmd === "spec_revise");
      expect(revises).toHaveLength(1);
    });
  });
});
