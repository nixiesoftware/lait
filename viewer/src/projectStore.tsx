import { createContext, useCallback, useContext, useMemo, type ReactNode } from "react";
import { rpc as defaultRpc } from "./api";
import { applyOverlay, Overlay, type Field } from "./core/overlay";
import { useWorldResource } from "./core/worldViewReact";
import { type ResourceSnapshot, WorldViewStore } from "./core/worldViewStore";
import type {
  ActivityEvent,
  BoardView,
  CatalogPlane,
  CatalogScope,
  DirtyProject,
  GraphView,
  IssueView,
  LabelDto,
  MemberDto,
  MilestoneDto,
  Priority,
  ProjectDto,
  Request,
  Response,
  Row,
  SpaceDoorbell,
  StatusInfo,
} from "./types";

type Rpc = (space: string, request: Request) => Promise<Response>;

const part = (value: string | null | undefined) => encodeURIComponent(value ?? "_");
const prefix = (space: string) => `space:${part(space)}/`;
export const projectKeys = {
  board: (space: string, project: string | null) => `${prefix(space)}board:${part(project)}`,
  row: (space: string, reff: string) => `${prefix(space)}row:${part(reff)}`,
  issue: (space: string, reff: string) => `${prefix(space)}issue:${part(reff)}`,
  graph: (space: string, reff: string) => `${prefix(space)}graph:${part(reff)}`,
  history: (space: string, reff: string) => `${prefix(space)}history:${part(reff)}`,
  milestones: (space: string, project: string) => `${prefix(space)}milestones:${part(project)}`,
  labels: (space: string) => `${prefix(space)}labels`,
  members: (space: string) => `${prefix(space)}members`,
  projects: (space: string) => `${prefix(space)}projects`,
  status: (space: string) => `${prefix(space)}status`,
};

/**
 * A catalog plane this resource projects — optionally only that plane's slice
 * for one project, matched by the **stable id** rather than the display key.
 */
export type CatalogDependency = CatalogPlane | { plane: CatalogPlane; projectId: string };

/**
 * Which issue bodies this resource is computed from.
 *
 * Separate from `catalog` because "the milestone records of project X moved" and
 * "an issue in project X moved" are different questions with different answers,
 * and a single `project` field could only ever express one of them. Milestone
 * progress needs both; a board needs its own project's issues; a graph needs
 * neighbours it cannot enumerate in advance, so it takes `"any"`.
 */
export type IssueDependency = "any" | { projectId: string } | { docs: readonly string[] };

/**
 * What a resource is derived from — the planes that can make it stale.
 *
 * Declared beside the loader, not in a switch somewhere else: the one place you
 * write "here is how to fetch this" is the same place you write "and here is
 * what invalidates it", so a new feature cannot quietly acquire a panel that
 * never refreshes. Everything omitted means "this ring does not touch me"; an
 * empty `Derivation` is a resource nothing invalidates but a `reset`.
 */
export interface Derivation {
  /** Catalog planes this resource projects. */
  readonly catalog?: readonly CatalogDependency[];
  /** Issue bodies this resource is computed from. */
  readonly issues?: IssueDependency;
  /** Stale when membership, roles, devices or keys advance. */
  readonly authority?: boolean;
  /** Stale when the activity feed advances. */
  readonly activity?: boolean;
}

/** One doorbell, reduced to the things a `Derivation` asks about. */
interface Ring {
  readonly dirty: readonly DirtyProject[];
  /** Every dirty doc, flattened — the `{ docs }` dependency asks about these. */
  readonly docs: ReadonlySet<string>;
  readonly planes: readonly CatalogScope[];
  readonly authority: boolean;
  readonly activity: boolean;
}

/**
 * Does this ring's catalog dirt reach that resource?
 *
 * A scope carrying a project is one project's slice of a plane, and it reaches
 * only a dependency naming that same project — by **id**, because the key is a
 * display alias a rename moves. A bare plane dependency takes the plane whole.
 */
function planeIsStale(d: Derivation, ring: Ring): boolean {
  if (!d.catalog?.length) return false;
  return ring.planes.some((scope) =>
    d.catalog!.some((dep) =>
      typeof dep === "string"
        ? dep === scope.scope
        : dep.plane === scope.scope &&
          (scope.project_id == null || dep.projectId === scope.project_id)));
}

/** Does this ring's issue dirt reach that resource? */
function issuesAreStale(d: Derivation, ring: Ring): boolean {
  if (d.issues === undefined) return false;
  if (d.issues === "any") return ring.dirty.length > 0;
  if ("projectId" in d.issues) {
    const wanted = d.issues.projectId;
    return ring.dirty.some((p) => p.project_id === wanted);
  }
  return d.issues.docs.some((doc) => ring.docs.has(doc));
}

/**
 * Does this ring make that resource stale?
 *
 * Any one clause is enough — a `Derivation` is a set of independent reasons, not
 * a conjunction.
 */
function isStale(d: Derivation, ring: Ring): boolean {
  if (planeIsStale(d, ring)) return true;
  if (issuesAreStale(d, ring)) return true;
  if (d.authority && ring.authority) return true;
  return !!d.activity && ring.activity;
}

export interface IssueDetailSnapshot {
  readonly issue: IssueView | null;
  readonly row: Row | null;
  readonly body: ResourceSnapshot<IssueView>;
  readonly graph: ResourceSnapshot<GraphView>;
  readonly history: ResourceSnapshot<ActivityEvent[]>;
  readonly milestones: ResourceSnapshot<MilestoneDto[]>;
  readonly partial: boolean;
  readonly secondaryError: unknown | null;
}

function issueFromRow(space: string, row: Row): IssueView {
  return {
    schema_version: 3,
    reff: row.reff,
    doc_id: row.doc_id,
    space_id: space,
    project_id: row.project_id,
    project_key: row.key_alias?.split("-")[0] ?? null,
    key_alias: row.key_alias,
    title: row.title,
    description: "",
    status: row.status,
    priority: row.priority,
    assignees: row.assignees,
    labels: [],
    label_names: row.label_names ?? [],
    comments: [],
    created_by: "",
    created_at: 0,
    ...(row.due_date !== undefined ? { due_date: row.due_date } : {}),
    ...(row.estimate !== undefined ? { estimate: row.estimate } : {}),
    provisional: row.provisional,
  };
}

export class ProjectViewerStore {
  readonly resources: WorldViewStore;
  readonly overlay = new Overlay();
  /**
   * A derivation may be a thunk, because some of what a resource depends on is
   * only knowable once it has loaded: a board is registered under a project KEY
   * but must match on the project's stable id, which arrives with the board
   * itself. Evaluating at ring time rather than registration time means the
   * dependency sharpens as soon as the data exists, instead of being frozen at
   * whatever was known the first time.
   */
  private loaders = new Map<
    string,
    { load: () => Promise<unknown>; derivation: Derivation | (() => Derivation) }
  >();
  private rowsByDoc = new Map<string, Map<string, Row>>();
  private boardSelectors = new Map<string, {
    source: ResourceSnapshot<BoardView>;
    overlay: string;
    value: BoardView | null;
  }>();
  private rowSelectors = new Map<string, {
    source: ResourceSnapshot<Row>;
    overlay: string;
    value: Row | null;
  }>();
  private detailSelectors = new Map<string, {
    dependencies: readonly unknown[];
    value: IssueDetailSnapshot;
  }>();

  constructor(
    private readonly rpc: Rpc = defaultRpc,
    resources = new WorldViewStore(),
  ) {
    this.resources = resources;
  }

  selectBoard(space: string, project: string | null): BoardView | null {
    const key = projectKeys.board(space, project);
    const source = this.resources.read<BoardView>(key);
    const overlay = source.data
      ? source.data.columns.flatMap((column) => column.rows)
        .map((row) => `${row.doc_id}:${this.overlay.get(row.doc_id, "title") ?? ""}:${this.overlay.get(row.doc_id, "status") ?? ""}:${this.overlay.get(row.doc_id, "priority") ?? ""}`)
        .join("|")
      : "";
    const cached = this.boardSelectors.get(key);
    if (cached?.source === source && cached.overlay === overlay) return cached.value;
    const value = source.data ? applyOverlay(source.data, this.overlay).board : null;
    this.boardSelectors.set(key, { source, overlay, value });
    return value;
  }

  selectRow(space: string, reff: string): Row | null {
    const key = projectKeys.row(space, reff);
    const source = this.resources.read<Row>(key);
    const row = source.data;
    if (!row) return null;
    const overlay = `${this.overlay.get(row.doc_id, "title") ?? ""}:${this.overlay.get(row.doc_id, "status") ?? ""}:${this.overlay.get(row.doc_id, "priority") ?? ""}`;
    const cached = this.rowSelectors.get(key);
    if (cached?.source === source && cached.overlay === overlay) return cached.value;
    const value = !this.overlay.has(row.doc_id) ? row : {
        ...row,
        title: this.overlay.get(row.doc_id, "title") ?? row.title,
        status: this.overlay.get(row.doc_id, "status") ?? row.status,
        priority: (this.overlay.get(row.doc_id, "priority") as Priority | undefined) ?? row.priority,
      };
    this.rowSelectors.set(key, { source, overlay, value });
    return value;
  }

  selectIssueDetail(space: string, reff: string): IssueDetailSnapshot {
    const row = this.selectRow(space, reff);
    const body = this.resources.read<IssueView>(projectKeys.issue(space, reff));
    const graph = this.resources.read<GraphView>(projectKeys.graph(space, reff));
    const history = this.resources.read<ActivityEvent[]>(projectKeys.history(space, reff));
    const projectId = body.data?.project_id ?? row?.project_id;
    const milestones = projectId
      ? this.resources.read<MilestoneDto[]>(projectKeys.milestones(space, projectId))
      : this.resources.read<MilestoneDto[]>(projectKeys.milestones(space, "_unknown"));
    const selectorKey = `${space}/${reff}`;
    const dependencies = [row, body, graph, history, milestones] as const;
    const cached = this.detailSelectors.get(selectorKey);
    if (cached && cached.dependencies.every((value, index) => value === dependencies[index])) {
      return cached.value;
    }
    const base = body.data ?? (row ? issueFromRow(space, row) : null);
    const issue: IssueView | null = base && row
      ? {
          ...base,
          title: row.title,
          status: row.status,
          priority: row.priority,
          assignees: body.data?.assignees ?? row.assignees,
          label_names: body.data?.label_names ?? row.label_names ?? [],
          ...(body.data?.due_date !== undefined
            ? { due_date: body.data.due_date }
            : row.due_date !== undefined ? { due_date: row.due_date } : {}),
          ...(body.data?.estimate !== undefined
            ? { estimate: body.data.estimate }
            : row.estimate !== undefined ? { estimate: row.estimate } : {}),
        }
      : base;
    const value = {
      issue,
      row,
      body,
      graph,
      history,
      milestones,
      partial: body.data === undefined,
      secondaryError: graph.error ?? history.error ?? milestones.error,
    };
    this.detailSelectors.set(selectorKey, { dependencies, value });
    return value;
  }

  ensureBoard(space: string, project: string | null, force = false): Promise<BoardView> {
    const key = projectKeys.board(space, project);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "board", project });
      if (result.kind !== "board") throw new Error("Expected board response");
      this.ingestBoard(space, result);
      return result;
      // A board is its project's rows in its project's order, columned by the
      // workflow, labelled from the row index. Registered under a project KEY
      // but matched on the project's stable id, which only arrives with the
      // board — hence a thunk, and "any project" until the first load lands.
    }, () => {
      const id = this.resources.read<BoardView>(key).data?.project.id;
      return {
        catalog: [
          "workflow",
          "docs",
          ...(id ? [{ plane: "boards" as const, projectId: id }] : ["boards" as const]),
        ],
        issues: id ? { projectId: id } : "any",
      };
    }, force);
  }

  ensureIssue(space: string, reff: string, force = false): Promise<IssueView> {
    const key = projectKeys.issue(space, reff);
    const promise = this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "issue_view", reff });
      if (result.kind !== "issue") throw new Error("Expected issue response");
      this.ingestIssue(space, result);
      return result;
      // The ring names docs, this resource is keyed by `reff` — so resolve the
      // doc through the row we hold. Before any row exists (a deep link opened
      // cold) there is nothing to resolve against, and "any issue" is the honest
      // answer rather than a dependency that silently matches nothing.
    }, () => {
      const doc = this.resources.read<Row>(projectKeys.row(space, reff)).data?.doc_id;
      return { issues: doc ? { docs: [doc] } : "any" };
    }, force);
    this.resources.evict(`${prefix(space)}issue:`, 200, new Set([key]));
    return promise;
  }

  ensureGraph(space: string, reff: string, force = false): Promise<GraphView> {
    const key = projectKeys.graph(space, reff);
    const promise = this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "issue_graph", reff });
      if (result.kind !== "graph") throw new Error("Expected graph response");
      for (const row of [
        ...(result.parent ? [result.parent] : []),
        ...result.children,
        ...result.links.map((link) => link.row),
        ...result.blocked_by,
      ]) this.ingestRow(space, row);
      return result;
      // The neighbourhood is this issue plus its parent, children, links and
      // transitive blockers — a set this resource cannot enumerate before
      // fetching, and which changes as the graph does. So `"any"`: a blocker
      // three hops away closing is real news here, and narrowing to the docs we
      // happen to know about today would miss exactly that.
    }, { issues: "any", catalog: ["relations"] }, force);
    this.resources.evict(`${prefix(space)}graph:`, 50, new Set([key]));
    return promise;
  }

  ensureHistory(space: string, reff: string, force = false): Promise<ActivityEvent[]> {
    const key = projectKeys.history(space, reff);
    const promise = this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "history", reff });
      if (result.kind !== "activity") throw new Error("Expected history response");
      return result.events;
      // The feed is a cursor, not a scope: the doorbell can only say it advanced.
    }, { activity: true }, force);
    this.resources.evict(`${prefix(space)}history:`, 50, new Set([key]));
    return promise;
  }

  ensureMilestones(space: string, project: string, force = false): Promise<MilestoneDto[]> {
    return this.load(projectKeys.milestones(space, project), async () => {
      const result = await this.rpc(space, { cmd: "milestone_list", project });
      if (result.kind !== "milestones") throw new Error("Expected milestones response");
      return result.milestones;
      // Milestones are catalog structure, but their `total`/`done` progress is
      // computed from ISSUE bodies — live issues of the project targeting each
      // milestone, done by status category. Three dependencies, and the split
      // between them is exactly why `catalog` and `issues` are separate fields:
      // the milestone RECORDS of this project, the ISSUES of this project, and
      // the workflow that decides which statuses count as done.
      //
      // `project` here is the `prj_` id, which is what the ring matches on — so
      // unlike the boards this is precise: ENG's issues do not refresh DSN's
      // milestone bars.
    }, {
      catalog: [{ plane: "milestones", projectId: project }, "workflow"],
      issues: { projectId: project },
    }, force);
  }

  ensureLabels(space: string, force = false): Promise<LabelDto[]> {
    return this.load(projectKeys.labels(space), async () => {
      const result = await this.rpc(space, { cmd: "label_list" });
      if (result.kind !== "labels") throw new Error("Expected labels response");
      return result.labels;
    }, { catalog: ["labels"] }, force);
  }

  ensureMembers(space: string, force = false): Promise<MemberDto[]> {
    return this.load(projectKeys.members(space), async () => {
      const result = await this.rpc(space, { cmd: "members" });
      if (result.kind !== "members") throw new Error("Expected members response");
      return result.members;
      // Membership is not catalog structure at all — it converges as signed
      // authority records. Its own dependency, matching its own plane.
    }, { authority: true }, force);
  }

  ensureProjects(space: string, force = false): Promise<ProjectDto[]> {
    return this.load(projectKeys.projects(space), async () => {
      const result = await this.rpc(space, { cmd: "project_list" });
      if (result.kind !== "projects") throw new Error("Expected projects response");
      return result.projects;
    }, { catalog: ["projects"] }, force);
  }

  ensureStatus(space: string, force = false): Promise<StatusInfo> {
    return this.load(projectKeys.status(space), async () => {
      const result = await this.rpc(space, { cmd: "status" });
      if (result.kind !== "status") throw new Error("Expected status response");
      return result;
      // Status names the space and counts its projects, members and issues —
      // the issue count from the row index, the member count from authority.
    }, { catalog: ["space", "projects", "docs"], authority: true }, force);
  }

  ensureIssueDetail(space: string, reff: string): void {
    void this.ensureIssue(space, reff).then((issue) => {
      void this.ensureMilestones(space, issue.project_id).catch(() => undefined);
    }).catch(() => undefined);
    void this.ensureGraph(space, reff).catch(() => undefined);
    void this.ensureHistory(space, reff).catch(() => undefined);
  }

  prefetchIssue(space: string, reff: string): void {
    void this.ensureIssue(space, reff).catch(() => undefined);
  }

  async editTitle(space: string, reff: string, title: string): Promise<boolean> {
    return this.predict(space, reff, "title", title, { cmd: "issue_edit", reff, title });
  }

  async setStatus(space: string, reff: string, status: string): Promise<boolean> {
    return this.predict(space, reff, "status", status, { cmd: "issue_edit", reff, status });
  }

  async setPriority(space: string, reff: string, priority: string): Promise<boolean> {
    return this.predict(space, reff, "priority", priority, { cmd: "issue_edit", reff, priority });
  }

  async predictValue(
    space: string,
    doc: string,
    field: Field,
    value: string,
    send: () => Promise<unknown>,
  ): Promise<boolean> {
    this.overlay.set(doc, field, value);
    this.notifyRows(space, [doc]);
    try {
      await send();
      return true;
    } catch (error) {
      this.overlay.clearDoc(doc);
      this.notifyRows(space, [doc]);
      throw error;
    }
  }

  async handleDoorbell(doorbell: SpaceDoorbell): Promise<void> {
    const space = doorbell.space;
    const scope = prefix(space);
    if (doorbell.reset) {
      this.overlay.clear();
      const keys = this.resources.reset((key) => key.startsWith(scope));
      await this.refreshActive(keys);
      return;
    }

    const dirty = doorbell.dirty_by_project.flatMap((p) => p.docs);
    const ring: Ring = {
      dirty: doorbell.dirty_by_project,
      docs: new Set(dirty),
      planes: doorbell.dirty_catalog,
      authority: doorbell.authority_advanced,
      activity: doorbell.activity_advanced,
    };

    // Every registered resource answers for itself. Nothing here knows what a
    // milestone or a label *is* — that lives with the loader that fetches it.
    const stale: string[] = [];
    for (const [key, entry] of this.loaders) {
      if (!key.startsWith(scope)) continue;
      const derivation =
        typeof entry.derivation === "function" ? entry.derivation() : entry.derivation;
      if (isStale(derivation, ring)) stale.push(key);
    }
    for (const key of stale) this.resources.invalidate(key);

    // Boards first, then retire the guesses. Clearing a prediction before the
    // authoritative rows land flashes the stale server value for a frame, which
    // is the one thing the optimism exists to prevent.
    const boards = stale.filter((key) => key.startsWith(`${scope}board:`));
    await this.refreshActive(boards);
    for (const doc of dirty) this.overlay.clearDoc(doc);
    this.notifyRows(space, dirty);
    for (const key of boards) this.resources.notify(key);
    await this.refreshActive(stale.filter((key) => !boards.includes(key)));
  }

  expirePredictions(space: string): boolean {
    if (!this.overlay.sweep()) return false;
    for (const row of this.rowsByDoc.get(space)?.values() ?? []) {
      this.resources.notify(projectKeys.row(space, row.reff));
    }
    for (const key of this.loaders.keys()) {
      if (key.startsWith(`${prefix(space)}board:`)) this.resources.notify(key);
    }
    return true;
  }

  private ingestBoard(space: string, board: BoardView): void {
    for (const row of board.columns.flatMap((column) => column.rows)) this.ingestRow(space, row);
  }

  private ingestIssue(space: string, issue: IssueView): void {
    const existing = this.resources.read<Row>(projectKeys.row(space, issue.reff)).data;
    if (existing) {
      this.ingestRow(space, {
        ...existing,
        title: issue.title,
        status: issue.status,
        priority: issue.priority,
        assignees: issue.assignees,
        ...(issue.due_date !== undefined ? { due_date: issue.due_date } : {}),
        ...(issue.estimate !== undefined ? { estimate: issue.estimate } : {}),
        label_names: issue.label_names,
        provisional: issue.provisional,
      });
    }
  }

  private ingestRow(space: string, row: Row): void {
    const rows = this.rowsByDoc.get(space) ?? new Map<string, Row>();
    rows.set(row.doc_id, row);
    this.rowsByDoc.set(space, rows);
    this.resources.set(projectKeys.row(space, row.reff), row);
  }

  private async predict(
    space: string,
    reff: string,
    field: Field,
    value: string,
    request: Request,
  ): Promise<boolean> {
    const row = this.resources.read<Row>(projectKeys.row(space, reff)).data;
    if (!row) return false;
    this.overlay.set(row.doc_id, field, value);
    this.notifyRows(space, [row.doc_id]);
    try {
      await this.rpc(space, request);
      return true;
    } catch (error) {
      this.overlay.clearDoc(row.doc_id);
      this.notifyRows(space, [row.doc_id]);
      throw error;
    }
  }

  private notifyRows(space: string, docs: readonly string[]): void {
    for (const doc of docs) {
      const row = this.rowsByDoc.get(space)?.get(doc);
      if (row) this.resources.notify(projectKeys.row(space, row.reff));
    }
    for (const key of this.loaders.keys()) {
      if (key.startsWith(`${prefix(space)}board:`)) this.resources.notify(key);
    }
  }

  /**
   * Register a resource: how to fetch it, and what makes it stale. The
   * `derivation` is not optional on purpose — a resource that declares nothing
   * has to say so, which is a decision rather than an omission.
   */
  private load<T>(
    key: string,
    loader: () => Promise<T>,
    derivation: Derivation | (() => Derivation),
    force: boolean,
  ): Promise<T> {
    this.loaders.set(key, { load: loader, derivation });
    return this.resources.ensure(key, loader, { force });
  }

  private async refreshActive(keys: readonly string[]): Promise<void> {
    await Promise.all(keys.map(async (key) => {
      const entry = this.loaders.get(key);
      if (!entry || !this.resources.isActive(key)) return;
      await this.resources.ensure(key, entry.load, { force: true }).catch(() => undefined);
    }));
  }
}

const ProjectStoreContext = createContext<ProjectViewerStore | null>(null);

export function ProjectViewerStoreProvider({
  store,
  children,
}: {
  store: ProjectViewerStore;
  children: ReactNode;
}) {
  return <ProjectStoreContext.Provider value={store}>{children}</ProjectStoreContext.Provider>;
}

export function useProjectViewerStore(): ProjectViewerStore {
  const store = useContext(ProjectStoreContext);
  if (!store) throw new Error("ProjectViewerStoreProvider is missing");
  return store;
}

export function useProjectBoard(space: string | null, project: string | null) {
  const store = useProjectViewerStore();
  const key = space ? projectKeys.board(space, project) : "project:none/board";
  const loader = useCallback(
    () => space ? store.ensureBoard(space, project) : Promise.reject(new Error("No space")),
    [project, space, store],
  );
  const resource = useWorldResource<BoardView>(key, space ? loader : undefined);
  return useMemo(
    () => ({ resource, board: space ? store.selectBoard(space, project) : null }),
    [project, resource, space, store],
  );
}

export function useProjectRegistry<T>(
  key: string,
  loader: (() => Promise<T>) | undefined,
): ResourceSnapshot<T> {
  return useWorldResource(key, loader);
}

export function useIssueDetail(space: string, reff: string): IssueDetailSnapshot {
  const store = useProjectViewerStore();
  const row = useWorldResource<Row>(projectKeys.row(space, reff));
  const body = useWorldResource<IssueView>(
    projectKeys.issue(space, reff),
    useCallback(() => store.ensureIssue(space, reff), [reff, space, store]),
  );
  const graph = useWorldResource<GraphView>(
    projectKeys.graph(space, reff),
    useCallback(() => store.ensureGraph(space, reff), [reff, space, store]),
  );
  const history = useWorldResource<ActivityEvent[]>(
    projectKeys.history(space, reff),
    useCallback(() => store.ensureHistory(space, reff), [reff, space, store]),
  );
  const projectId = body.data?.project_id ?? row.data?.project_id;
  useWorldResource<MilestoneDto[]>(
    projectId ? projectKeys.milestones(space, projectId) : projectKeys.milestones(space, "_unknown"),
    useCallback(
      () => projectId ? store.ensureMilestones(space, projectId) : Promise.resolve([]),
      [projectId, space, store],
    ),
  );
  return useMemo(
    () => store.selectIssueDetail(space, reff),
    // The resource objects are immutable change tokens.
    [body, graph, history, reff, row, space, store, projectId],
  );
}
