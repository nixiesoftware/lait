import { createContext, useCallback, useContext, useMemo, type ReactNode } from "react";
import { rpc as defaultRpc, spaceRpc as defaultSpaceRpc } from "./api";
import {
  applyOverlay,
  Overlay,
  overlayRow,
  type Field,
  type PredictionValue,
} from "./core/overlay";
import { useWorldResource } from "./core/worldViewReact";
import { type ResourceSnapshot, WorldViewStore } from "./core/worldViewStore";
import type {
  ActivityEvent,
  AssignmentDto,
  BoardView,
  DirtyPlane,
  DirtyScope,
  GraphView,
  IssueView,
  LabelDto,
  MemberDto,
  MilestoneDto,
  ProjectDto,
  ProjectUpdateDto,
  Response,
  Row,
  SpaceDoorbell,
  BaselineRef,
  BaselineRevisionDto,
  BaselineView,
  SpaceRequest,
  SpecBody,
  SpecKind,
  SpecRef,
  SpecReference,
  SpecRevision,
  SpecState,
  SpecView,
  StatusInfo,
  WorldRequest,
} from "./types";

type Rpc = (space: string, request: WorldRequest) => Promise<Response>;
/** Generic Space control, injected like `Rpc` so both transports stay substitutable. */
type SpaceRpc = (space: string, request: SpaceRequest) => Promise<Response>;

const part = (value: string | null | undefined) => encodeURIComponent(value ?? "_");
const prefix = (space: string) => `space:${part(space)}/`;
export const projectKeys = {
  board: (space: string, project: string | null) => `${prefix(space)}board:${part(project)}`,
  row: (space: string, reff: string) => `${prefix(space)}row:${part(reff)}`,
  issue: (space: string, reff: string) => `${prefix(space)}issue:${part(reff)}`,
  graph: (space: string, reff: string) => `${prefix(space)}graph:${part(reff)}`,
  history: (space: string, reff: string) => `${prefix(space)}history:${part(reff)}`,
  milestones: (space: string, project: string) => `${prefix(space)}milestones:${part(project)}`,
  specs: (space: string, project: string | null) => `${prefix(space)}specs:${part(project)}`,
  spec: (space: string, spec: string) => `${prefix(space)}spec:${part(spec)}`,
  specHistory: (space: string, spec: string) => `${prefix(space)}spec-history:${part(spec)}`,
  specReferences: (space: string, project: string | null) =>
    `${prefix(space)}spec-references:${part(project)}`,
  baselines: (space: string, project: string | null) => `${prefix(space)}baselines:${part(project)}`,
  baseline: (space: string, baseline: string) => `${prefix(space)}baseline:${part(baseline)}`,
  baselineHistory: (space: string, baseline: string) =>
    `${prefix(space)}baseline-history:${part(baseline)}`,
  grants: (space: string) => `${prefix(space)}grants`,
  updates: (space: string, project: string) => `${prefix(space)}updates:${part(project)}`,
  labels: (space: string) => `${prefix(space)}labels`,
  members: (space: string) => `${prefix(space)}members`,
  projects: (space: string) => `${prefix(space)}projects`,
  status: (space: string) => `${prefix(space)}status`,
};

/**
 * The catalog planes *this product* projects. Closed on purpose: the wire type
 * is a World-declared `string`, so a plane the engine does not emit can no
 * longer fail to compile there — it fails here instead, beside the loaders that
 * declare it.
 */
export type IssuesPlane =
  | "space"
  | "projects"
  | "labels"
  | "workflow"
  | "boards"
  | "milestones"
  | "cycles"
  | "updates"
  | "initiatives"
  | "teams"
  | "triage"
  | "roles"
  | "specs"
  | "docs"
  | "relations";

/**
 * A catalog plane this resource projects — optionally only that plane's slice
 * for one scope, matched by the **stable id** rather than the display label.
 */
export type CatalogDependency = IssuesPlane | { plane: IssuesPlane; scopeId: string };

/**
 * Which issue bodies this resource is computed from.
 *
 * Separate from `catalog` because "the milestone records of project X moved" and
 * "an issue in project X moved" are different questions with different answers,
 * and a single `project` field could only ever express one of them. Milestone
 * progress needs both; a board needs its own project's issues; a graph needs
 * neighbours it cannot enumerate in advance, so it takes `"any"`.
 */
export type IssueDependency = "any" | { scopeId: string } | { docs: readonly string[] };

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
  readonly dirty: readonly DirtyScope[];
  /** Every dirty doc, flattened — the `{ docs }` dependency asks about these. */
  readonly docs: ReadonlySet<string>;
  readonly planes: readonly DirtyPlane[];
  readonly authority: boolean;
  readonly activity: boolean;
}

const ISSUES_WORLD = "com.lait.issues";

/**
 * Does this ring's catalog dirt reach that resource?
 *
 * A plane carrying a scope is one container's slice of a plane, and it reaches
 * only a dependency naming that same container — by **id**, because `label` is a
 * display alias a rename moves. A bare plane dependency takes the plane whole.
 */
function planeIsStale(d: Derivation, ring: Ring): boolean {
  if (!d.catalog?.length) return false;
  return ring.planes.some((dirty) =>
    d.catalog!.some((dep) =>
      typeof dep === "string"
        ? dep === dirty.plane
        : dep.plane === dirty.plane &&
          (dirty.scope == null || dep.scopeId === dirty.scope.id)));
}

/** Does this ring's issue dirt reach that resource? */
function issuesAreStale(d: Derivation, ring: Ring): boolean {
  if (d.issues === undefined) return false;
  if (d.issues === "any") return ring.dirty.length > 0;
  if ("scopeId" in d.issues) {
    const wanted = d.issues.scopeId;
    return ring.dirty.some((s) => s.id === wanted);
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
    private readonly spaceRpc: SpaceRpc = defaultSpaceRpc,
    resources = new WorldViewStore(),
  ) {
    this.resources = resources;
  }

  selectBoard(space: string, project: string | null): BoardView | null {
    const key = projectKeys.board(space, project);
    const source = this.resources.read<BoardView>(key);
    const overlay = source.data
      ? source.data.columns.flatMap((column) => column.rows)
        .map((row) => `${row.doc_id}:${this.overlay.signature(row.doc_id)}`)
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
    const overlay = this.overlay.signature(row.doc_id);
    const cached = this.rowSelectors.get(key);
    if (cached?.source === source && cached.overlay === overlay) return cached.value;
    const value = overlayRow(row, this.overlay);
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
    // Every predictable field reads through the overlaid row, so an optimistic
    // write shows in the detail rail exactly as it does on the row — the body
    // keeps what only it carries (description, comments, label ids, …).
    const issue: IssueView | null = base && row
      ? {
          ...base,
          title: row.title,
          status: row.status,
          priority: row.priority,
          assignees: row.assignees,
          label_names: row.label_names ?? [],
          // Explicit null, not omission: a predicted *clear* has to beat the
          // body's stale value, and a spread that skips the key cannot.
          due_date: row.due_date ?? null,
          estimate: row.estimate ?? null,
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
          ...(id ? [{ plane: "boards" as const, scopeId: id }] : ["boards" as const]),
        ],
        issues: id ? { scopeId: id } : "any",
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
      catalog: [{ plane: "milestones", scopeId: project }, "workflow"],
      issues: { scopeId: project },
    }, force);
  }

  ensureUpdates(space: string, project: string, force = false): Promise<ProjectUpdateDto[]> {
    return this.load(projectKeys.updates(space, project), async () => {
      const result = await this.rpc(space, { cmd: "project_updates", project });
      if (result.kind !== "updates") throw new Error("Expected updates response");
      return result.updates;
      // One dependency, where the milestones above need three: an update is
      // authored once and never edited, so no issue moving and no workflow
      // change can alter what a past post said. Only the feed's own plane can.
      //
      // `project` is the `prj_` id the ring matches on, so posting in ENG does
      // not refetch DSN's feed.
    }, { catalog: [{ plane: "updates", scopeId: project }] }, force);
  }

  ensureSpecs(space: string, project: string | null, force = false): Promise<SpecView[]> {
    return this.load(projectKeys.specs(space, project), async () => {
      const result = await this.rpc(space, { cmd: "spec_list", project });
      if (result.kind !== "specs") throw new Error("Expected specs response");
      for (const spec of result.specs) this.resources.set(projectKeys.spec(space, spec.spec), spec);
      return result.specs;
      // Specs and Baselines are Bodies of their own rather than a region of the
      // catalog, so their plane is digested from Body version stamps. Coarse in
      // one direction only — space-wide rather than per-project — because naming
      // the project would mean reading every Spec on every doorbell, and the
      // cost of that outweighs one register refetch in a quiet project.
    }, { catalog: ["specs"] }, force);
  }

  ensureSpec(space: string, spec: string, force = false): Promise<SpecView> {
    return this.load(projectKeys.spec(space, spec), async () => {
      const result = await this.rpc(space, { cmd: "spec_show", spec });
      if (result.kind !== "spec") throw new Error("Expected spec response");
      return result.spec;
      // Same plane as the register above — and the reader needs its own resource
      // rather than picking its row out of the list: a deep link opens on a Spec
      // before any register has loaded.
    }, { catalog: ["specs"] }, force);
  }

  /**
   * Create a Spec, and land its view where the reader will look for it.
   *
   * The reply is the new Spec's own view, so the reader does not have to fetch
   * what it was just handed. The register is refreshed rather than patched: it
   * is a server-ordered list, and inserting locally would guess an order the
   * next doorbell overwrites.
   */
  async createSpec(
    space: string,
    project: string,
    kind: SpecKind,
    title: string,
  ): Promise<SpecView> {
    const result = await this.rpc(space, { cmd: "spec_new", project, kind, title });
    if (result.kind !== "spec") throw new Error("Expected spec response");
    this.resources.set(projectKeys.spec(space, result.spec.spec), result.spec);
    await this.refreshSpecRegisters(space);
    return result.spec;
  }

  /**
   * Revise a Spec — the only way its content changes.
   *
   * `expected` is the head the edit was written against, and the engine rejects
   * anything else as a conflict rather than merging. Storing the reply matters
   * for more than freshness: the *next* edit needs the revision this one just
   * created, and a reader still holding the old head would write against a
   * predecessor and be refused.
   */
  async reviseSpec(
    space: string,
    spec: string,
    expected: string,
    patch: { title?: string; text?: string },
  ): Promise<SpecView> {
    const result = await this.rpc(space, { cmd: "spec_revise", spec, expected, ...patch });
    if (result.kind !== "spec") throw new Error("Expected spec response");
    this.resources.set(projectKeys.spec(space, spec), result.spec);
    await this.refreshSpecRegisters(space);
    return result.spec;
  }

  /**
   * Move a Spec through its lifecycle.
   *
   * `expected` is the head the transition was composed against, exactly as a
   * revision is — the engine refuses a state change aimed at a head that has
   * moved, and refuses one at all while several heads exist.
   */
  async setSpecState(
    space: string,
    spec: string,
    expected: string,
    state: SpecState,
  ): Promise<SpecView> {
    const result = await this.rpc(space, { cmd: "spec_state", spec, expected, state });
    if (result.kind !== "spec") throw new Error("Expected spec response");
    this.resources.set(projectKeys.spec(space, spec), result.spec);
    await this.refreshSpecRegisters(space);
    return result.spec;
  }

  /**
   * Resolve concurrent heads into one successor.
   *
   * `expectedHeads` must be the complete current head set — the engine compares
   * it as a sorted set and refuses anything else, which is what stops a
   * resolution composed against two heads from landing after a third appears.
   * The result is a new draft whose predecessors are all of them; no branch is
   * deleted and none is declared the winner.
   */
  async resolveSpec(
    space: string,
    spec: string,
    expectedHeads: string[],
    body: SpecBody,
  ): Promise<SpecView> {
    const result = await this.rpc(space, {
      cmd: "spec_resolve",
      spec,
      expected_heads: expectedHeads,
      body_json: JSON.stringify(body),
    });
    if (result.kind !== "spec") throw new Error("Expected spec response");
    this.resources.set(projectKeys.spec(space, spec), result.spec);
    await Promise.all([
      this.ensureSpecHistory(space, spec, true).catch(() => undefined),
      this.refreshSpecRegisters(space),
    ]);
    return result.spec;
  }

  ensureSpecHistory(space: string, spec: string, force = false): Promise<SpecRevision[]> {
    return this.load(projectKeys.specHistory(space, spec), async () => {
      const result = await this.rpc(space, { cmd: "spec_history", spec });
      if (result.kind !== "spec_revisions") throw new Error("Expected spec revisions response");
      return result.revisions;
    }, { catalog: ["specs"] }, force);
  }

  ensureBaselines(space: string, project: string | null, force = false): Promise<BaselineView[]> {
    return this.load(projectKeys.baselines(space, project), async () => {
      const result = await this.rpc(space, { cmd: "baseline_list", project });
      if (result.kind !== "baselines") throw new Error("Expected baselines response");
      for (const baseline of result.baselines) {
        this.resources.set(projectKeys.baseline(space, baseline.baseline), baseline);
      }
      return result.baselines;
    }, { catalog: ["specs"] }, force);
  }

  ensureBaseline(space: string, baseline: string, force = false): Promise<BaselineView> {
    return this.load(projectKeys.baseline(space, baseline), async () => {
      const result = await this.rpc(space, { cmd: "baseline_show", baseline });
      if (result.kind !== "baseline") throw new Error("Expected baseline response");
      return result;
    }, { catalog: ["specs"] }, force);
  }

  ensureBaselineHistory(
    space: string,
    baseline: string,
    force = false,
  ): Promise<BaselineRevisionDto[]> {
    return this.load(projectKeys.baselineHistory(space, baseline), async () => {
      const result = await this.rpc(space, { cmd: "baseline_history", baseline });
      if (result.kind !== "baseline_revisions") {
        throw new Error("Expected baseline revisions response");
      }
      return result.revisions;
    }, { catalog: ["specs"] }, force);
  }

  async createBaseline(
    space: string,
    project: string,
    name: string,
    members: SpecRef[],
  ): Promise<BaselineView> {
    const result = await this.rpc(space, { cmd: "baseline_new", project, name, members });
    if (result.kind !== "baseline") throw new Error("Expected baseline response");
    this.resources.set(projectKeys.baseline(space, result.baseline), result);
    await this.refreshBaselineRegisters(space);
    return result;
  }

  /**
   * Revise a Baseline — the only way its member set changes.
   *
   * Removing or replacing a member does not edit the issued set; it writes a
   * successor draft, exactly as revising a Spec does. An issued set that could
   * be edited in place would not be a set anyone could have agreed to.
   */
  async reviseBaseline(
    space: string,
    baseline: string,
    expected: string,
    patch: { name?: string; members?: SpecRef[] },
  ): Promise<BaselineView> {
    const result = await this.rpc(space, { cmd: "baseline_revise", baseline, expected, ...patch });
    if (result.kind !== "baseline") throw new Error("Expected baseline response");
    this.resources.set(projectKeys.baseline(space, baseline), result);
    await Promise.all([
      this.ensureBaselineHistory(space, baseline, true).catch(() => undefined),
      this.refreshBaselineRegisters(space),
    ]);
    return result;
  }

  async setBaselineState(
    space: string,
    baseline: string,
    expected: string,
    state: SpecState,
  ): Promise<BaselineView> {
    const result = await this.rpc(space, { cmd: "baseline_state", baseline, expected, state });
    if (result.kind !== "baseline") throw new Error("Expected baseline response");
    this.resources.set(projectKeys.baseline(space, baseline), result);
    await Promise.all([
      this.ensureBaselineHistory(space, baseline, true).catch(() => undefined),
      this.refreshBaselineRegisters(space),
    ]);
    return result;
  }

  /** Pin an exact issued Baseline revision to an Issue, or clear the pin. */
  async bindBaseline(
    space: string,
    reff: string,
    baseline: BaselineRef | null,
  ): Promise<void> {
    await this.rpc(space, { cmd: "issue_baseline", reff, baseline });
  }

  private refreshBaselineRegisters(space: string): Promise<void> {
    return this.refreshActive(
      [...this.loaders.keys()].filter((key) => key.startsWith(`${prefix(space)}baselines:`)),
    );
  }

  /**
   * Every typed link in scope, and who asserts it.
   *
   * One resource for two questions that look different and are not: "what
   * verifies this document" and "which issued requirements have nothing behind
   * them" are both reads of the same edge set. Asking either one document at a
   * time would be a query per row.
   */
  ensureSpecReferences(
    space: string,
    project: string | null,
    force = false,
  ): Promise<SpecReference[]> {
    return this.load(projectKeys.specReferences(space, project), async () => {
      const result = await this.rpc(space, { cmd: "spec_references", project });
      if (result.kind !== "spec_references") {
        throw new Error("Expected spec references response");
      }
      return result.references;
    }, { catalog: ["specs"] }, force);
  }

  ensureGrants(space: string, force = false): Promise<AssignmentDto[]> {
    return this.load(projectKeys.grants(space), async () => {
      const result = await this.rpc(space, { cmd: "access_list" });
      if (result.kind !== "assignments") throw new Error("Expected assignments response");
      return result.rows;
      // Scoped capability assignments converge as signed authority records, not
      // as catalog structure — the same plane membership rides, and the same one
      // a revocation moves.
    }, { authority: true }, force);
  }

  /**
   * Re-read whichever registers are on screen.
   *
   * Not `ensureSpecs(space, project)` — a register is keyed by the project
   * handle the *caller* had (a KEY, or `null` for the whole space), and a Spec
   * view reports the resolved `prj_` id, which is a third string. Refreshing by
   * what is registered sidesteps having to reconcile them.
   */
  private refreshSpecRegisters(space: string): Promise<void> {
    return this.refreshActive(
      [...this.loaders.keys()].filter((key) => key.startsWith(`${prefix(space)}specs:`)),
    );
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
      const result = await this.spaceRpc(space, { cmd: "members" });
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
      const result = await this.spaceRpc(space, { cmd: "status" });
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

  // ---- field writes ---------------------------------------------------------
  //
  // Every surface that edits an issue field — a list row's chip, a board card,
  // the detail rail, a swimlane drop — calls one of these. The prediction and
  // the wire format live together here, so a second caller cannot invent a
  // second spelling of "reassign" (the app shell and the detail rail once held
  // two, and they disagreed).

  async editTitle(space: string, reff: string, title: string): Promise<boolean> {
    return this.predict(space, reff, "title", title, { cmd: "issue_edit", reff, title });
  }

  async setStatus(space: string, reff: string, status: string): Promise<boolean> {
    return this.predict(space, reff, "status", status, { cmd: "issue_edit", reff, status });
  }

  async setPriority(space: string, reff: string, priority: string): Promise<boolean> {
    return this.predict(space, reff, "priority", priority, { cmd: "issue_edit", reff, priority });
  }

  /** `due` is the engine's `YYYY-MM-DD` (UTC), or null to clear. */
  async setDue(space: string, reff: string, due: string | null): Promise<boolean> {
    const predicted = due === null ? null : Math.floor(Date.parse(`${due}T00:00:00Z`) / 1000);
    return this.predict(space, reff, "due", predicted, {
      cmd: "issue_edit",
      reff,
      due: due ?? "none",
    });
  }

  /** `estimate` is a numeric string or `"none"` — the wire's own shape. */
  async setEstimate(space: string, reff: string, estimate: string): Promise<boolean> {
    const predicted = estimate === "none" ? null : Number(estimate);
    return this.predict(space, reff, "estimate", predicted, {
      cmd: "issue_edit",
      reff,
      estimate,
    });
  }

  /** Add or remove one assignee. `key` must be a full 64-hex device key —
   *  `index::resolve_device` does not consult the member directory. */
  async toggleAssignee(space: string, reff: string, key: string, add: boolean): Promise<boolean> {
    return this.predictFromRow(
      space,
      reff,
      "assignees",
      (row) =>
        add
          ? [...row.assignees.filter((k) => k !== key), key]
          : row.assignees.filter((k) => k !== key),
      () => this.rpc(space, { cmd: "assign", reff, who: [key], add }),
    );
  }

  /**
   * Make `keys` the exact assignee set. `assign` is add/remove per key, so this
   * is a small batch — removals first, then the additions.
   */
  async setAssignees(space: string, reff: string, keys: readonly string[]): Promise<boolean> {
    return this.predictFromRow(space, reff, "assignees", () => [...keys], async () => {
      const row = this.resources.read<Row>(projectKeys.row(space, reff)).data;
      const current = row?.assignees ?? [];
      for (const k of current) {
        if (!keys.includes(k)) await this.rpc(space, { cmd: "assign", reff, who: [k], add: false });
      }
      for (const k of keys) {
        if (!current.includes(k)) await this.rpc(space, { cmd: "assign", reff, who: [k], add: true });
      }
    });
  }

  /** Attach or detach one label, by name — `Request::Label` resolves names. */
  async toggleLabel(space: string, reff: string, name: string, add: boolean): Promise<boolean> {
    return this.predictFromRow(space, reff, "labels", (row) => {
      const names = row.label_names ?? [];
      return add ? [...names.filter((n) => n !== name), name] : names.filter((n) => n !== name);
    }, () => this.rpc(space, { cmd: "label", reff, ...(add ? { add: [name] } : { remove: [name] }) }));
  }

  /**
   * Swap one label for another (or for nothing). One swap, two requests, in
   * this order: the engine's label op is add-or-remove on a name set, so a
   * rename is a detach and an attach — removing first keeps the set from
   * briefly holding both.
   */
  async swapLabel(space: string, reff: string, from: string, to: string | null): Promise<boolean> {
    return this.predictFromRow(space, reff, "labels", (row) => {
      const names = (row.label_names ?? []).filter((n) => n !== from);
      return to !== null && !names.includes(to) ? [...names, to] : names;
    }, async () => {
      await this.rpc(space, { cmd: "label", reff, remove: [from] });
      if (to !== null) await this.rpc(space, { cmd: "label", reff, add: [to] });
    });
  }

  async predictValue(
    space: string,
    doc: string,
    field: Field,
    value: PredictionValue,
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

    const invalidations = doorbell.invalidations.filter((entry) => entry.world === ISSUES_WORLD);
    const scopes = invalidations.flatMap((entry) => entry.dirty);
    const planes = invalidations.flatMap((entry) => entry.planes);
    const dirty = scopes.flatMap((scope) => scope.docs);
    const ring: Ring = {
      dirty: scopes,
      docs: new Set(dirty),
      planes,
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
    value: PredictionValue,
    request: WorldRequest,
  ): Promise<boolean> {
    return this.predictFromRow(space, reff, field, () => value, () => this.rpc(space, request));
  }

  /**
   * Predict a value computed from the current row, then send. The row read and
   * the overlay write happen together so an array field (assignees, labels)
   * derives its replacement from the same row it patches — and a reff with no
   * row yet has nothing to predict against, so the write is simply not sent.
   */
  private async predictFromRow(
    space: string,
    reff: string,
    field: Field,
    value: (row: Row) => PredictionValue,
    send: () => Promise<unknown>,
  ): Promise<boolean> {
    const row = this.resources.read<Row>(projectKeys.row(space, reff)).data;
    if (!row) return false;
    // Compute from the *overlaid* row: a second toggle while the first is still
    // unconfirmed must stack on the prediction, not on the server's stale set.
    this.overlay.set(row.doc_id, field, value(overlayRow(row, this.overlay)));
    this.notifyRows(space, [row.doc_id]);
    try {
      await send();
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

/**
 * A project's milestones, live.
 *
 * Keyed on the `prj_` **id**, never the display KEY: the doorbell rings a plane
 * by stable id, so a resource registered under a key would quietly stop
 * refreshing the first time someone renamed the project. Callers holding a
 * `ProjectDto` want `project.id`.
 *
 * `null` — the project is not known yet — parks on a shared empty key instead of
 * asking the daemon about a project that isn't there.
 */
export function useProjectMilestones(
  space: string,
  projectId: string | null | undefined,
): ResourceSnapshot<MilestoneDto[]> {
  const store = useProjectViewerStore();
  return useWorldResource<MilestoneDto[]>(
    projectKeys.milestones(space, projectId ?? "_unknown"),
    useCallback(
      () => (projectId ? store.ensureMilestones(space, projectId) : Promise.resolve([])),
      [projectId, space, store],
    ),
  );
}

/**
 * A project's Specs, live.
 *
 * Keyed on the project **KEY**, unlike milestones — `spec_list` resolves a
 * project reference the way every other command does, and the register knows the
 * key it is drawing. `null` is the whole space, which is what the request means
 * by an absent project rather than a placeholder for one we could not find.
 */
export function useProjectSpecs(
  space: string,
  project: string | null,
): ResourceSnapshot<SpecView[]> {
  const store = useProjectViewerStore();
  return useWorldResource<SpecView[]>(
    projectKeys.specs(space, project),
    useCallback(() => store.ensureSpecs(space, project), [project, space, store]),
  );
}

/** This node's effective scoped grants, live. One resource for the whole Space:
 *  the reply is already every assignment, so a per-project key would be four
 *  fetches of the same rows. */
export function useGrants(space: string): ResourceSnapshot<AssignmentDto[]> {
  const store = useProjectViewerStore();
  return useWorldResource<AssignmentDto[]>(
    projectKeys.grants(space),
    useCallback(() => store.ensureGrants(space), [space, store]),
  );
}

/** A project's Baselines, live. Same handle rule as the Spec register. */
export function useProjectBaselines(
  space: string,
  project: string | null,
): ResourceSnapshot<BaselineView[]> {
  const store = useProjectViewerStore();
  return useWorldResource<BaselineView[]>(
    projectKeys.baselines(space, project),
    useCallback(() => store.ensureBaselines(space, project), [project, space, store]),
  );
}

export function useBaseline(
  space: string,
  baseline: string | null,
): ResourceSnapshot<BaselineView> {
  const store = useProjectViewerStore();
  const load = useCallback(
    () => store.ensureBaseline(space, baseline ?? ""),
    [space, baseline, store],
  );
  return useWorldResource<BaselineView>(
    projectKeys.baseline(space, baseline ?? "_none"),
    baseline ? load : undefined,
  );
}

export function useBaselineHistory(
  space: string,
  baseline: string | null,
): ResourceSnapshot<BaselineRevisionDto[]> {
  const store = useProjectViewerStore();
  const load = useCallback(
    () => store.ensureBaselineHistory(space, baseline ?? ""),
    [space, baseline, store],
  );
  return useWorldResource<BaselineRevisionDto[]>(
    projectKeys.baselineHistory(space, baseline ?? "_none"),
    baseline ? load : undefined,
  );
}

/** Every typed link in scope, live — the incoming half of the graph, and what
 *  coverage is computed from. */
export function useSpecReferences(
  space: string,
  project: string | null,
): ResourceSnapshot<SpecReference[]> {
  const store = useProjectViewerStore();
  return useWorldResource<SpecReference[]>(
    projectKeys.specReferences(space, project),
    useCallback(() => store.ensureSpecReferences(space, project), [project, space, store]),
  );
}

/** One Spec's whole revision DAG, live. */
export function useSpecHistory(
  space: string,
  spec: string | null,
): ResourceSnapshot<SpecRevision[]> {
  const store = useProjectViewerStore();
  const load = useCallback(
    () => store.ensureSpecHistory(space, spec ?? ""),
    [space, spec, store],
  );
  return useWorldResource<SpecRevision[]>(
    projectKeys.specHistory(space, spec ?? "_none"),
    spec ? load : undefined,
  );
}

/** One Spec, live — the reader's own resource, so a deep link does not wait on
 *  a register it may never draw. */
export function useSpec(space: string, spec: string | null): ResourceSnapshot<SpecView> {
  const store = useProjectViewerStore();
  const load = useCallback(
    () => store.ensureSpec(space, spec ?? ""),
    [space, spec, store],
  );
  // No spec open parks on a key nothing fetches, rather than asking the daemon
  // about the empty string.
  return useWorldResource<SpecView>(projectKeys.spec(space, spec ?? "_none"), spec ? load : undefined);
}

/** A project's status-update feed, live. Same id-not-key rule as above. */
export function useProjectUpdates(
  space: string,
  projectId: string | null | undefined,
): ResourceSnapshot<ProjectUpdateDto[]> {
  const store = useProjectViewerStore();
  return useWorldResource<ProjectUpdateDto[]>(
    projectKeys.updates(space, projectId ?? "_unknown"),
    useCallback(
      () => (projectId ? store.ensureUpdates(space, projectId) : Promise.resolve([])),
      [projectId, space, store],
    ),
  );
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
  // The detail rail's milestone picker and the project overview's milestone list
  // are the same resource under the same key — one fetch, one invalidation, and
  // the two surfaces can never disagree about a milestone's progress.
  useProjectMilestones(space, projectId);
  return useMemo(
    () => store.selectIssueDetail(space, reff),
    // The resource objects are immutable change tokens.
    [body, graph, history, reff, row, space, store, projectId],
  );
}
