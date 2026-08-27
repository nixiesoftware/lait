import { createContext, useCallback, useContext, useMemo, type ReactNode } from "react";
import { LaitError, rpc as defaultRpc, spaceRpc as defaultSpaceRpc } from "./api";
import {
  applyOverlay,
  Overlay,
  overlayRow,
  type Field,
  type PredictionValue,
} from "./core/overlay";
import { useWorldResource, useWorldResources } from "./core/worldViewReact";
import {
  type ResourceSnapshot,
  type ResourceState,
  WorldViewStore,
} from "./core/worldViewStore";
import type {
  ActivityEvent,
  AssignmentDto,
  BoardPage,
  BoardPos,
  BoardView,
  DirtyPlane,
  DirtyScope,
  GraphView,
  GeometryView,
  CommentDto,
  IssueView,
  LabelDto,
  MemberDto,
  MilestoneDto,
  Page,
  ProjectDto,
  ProjectUpdateDto,
  PublicationId,
  ReactionRecord,
  TeamDto,
  Response,
  Row,
  SpaceDoorbell,
  BaselineRef,
  Packet,
  BaselineSummary,
  BaselineRevisionDto,
  BaselineView,
  SpaceRequest,
  SpecBody,
  SpecKind,
  SpecLink,
  PlanData,
  SpecObservationRecord,
  SpecRef,
  SpecReferenceFact,
  SpecRel,
  SpecRevision,
  SpecSummary,
  SpecState,
  SpecTarget,
  SpecView,
  StatusInfo,
  WhoamiInfo,
  WorldRequest,
  WorldPublicationId,
  OperationReceipt,
  IssuesChangeOperation,
  ChangePosition,
} from "./types";
import { applyLinkDelta, emptyDelta, type LinkDelta } from "./core/specs";

type Rpc = (space: string, request: WorldRequest) => Promise<Response>;
/** Generic Space control, injected like `Rpc` so both transports stay substitutable. */
type SpaceRpc = (space: string, request: SpaceRequest) => Promise<Response>;

const part = (value: string | null | undefined) => encodeURIComponent(value ?? "_");
const prefix = (space: string) => `space:${part(space)}/`;
const firstPage = () => ({ limit: 100, cursor: null } as const);
const bytesHex = (bytes: ArrayLike<number>) => Array.from(
  bytes,
  (byte) => byte.toString(16).padStart(2, "0"),
)
  .join("");
const publicationKey = (publication?: PublicationId | null) => publication
  ? [
      bytesHex(publication.manifest_root),
      bytesHex(publication.implementation_digest),
      bytesHex(publication.extractor_schema_digest),
    ].join(":")
  : "current";
const publicationCoordinate = (publication: PublicationId) => ({
  manifest_root: bytesHex(publication.manifest_root),
  implementation_digest: bytesHex(publication.implementation_digest),
  extractor_schema_digest: bytesHex(publication.extractor_schema_digest),
});
const worldPublicationKey = (publication: import("./types").WorldPublicationId) =>
  `${publicationKey(publication.publication)}:${publication.materialization}`;

function randomOperationId(): string {
  const bytes = new Uint8Array(16);
  globalThis.crypto.getRandomValues(bytes);
  return bytesHex(bytes);
}

export type OperationPhase = "sending" | "accepted" | "committed" | "rolled_back" | "indeterminate";

export interface OperationFeedback {
  readonly operation: string;
  readonly phase: OperationPhase;
  readonly space: string;
  readonly doc: string;
  readonly resource: string;
  readonly timestamp: number;
  readonly operations: readonly IssuesChangeOperation[];
  readonly publication?: WorldPublicationId;
  readonly results?: readonly { operation: number; kind: string; id: string }[];
  readonly error?: { kind: string; message: string };
}

export interface PendingOperation {
  readonly operation: string;
  readonly completion: Promise<OperationFeedback>;
}

type IssuePatch = Omit<
  Extract<IssuesChangeOperation, { op: "issue_patch" }>,
  "op" | "issue"
>;
type IssueDetailResponse = Extract<Response, { kind: "issue_detail" }>;

function commentsWithReactions(
  comments: readonly CommentDto[],
  records: readonly ReactionRecord[],
): CommentDto[] {
  const active = new Map<string, Map<string, Set<string>>>();
  for (const record of records) {
    const byEmoji = active.get(record.comment) ?? new Map<string, Set<string>>();
    const actors = byEmoji.get(record.emoji) ?? new Set<string>();
    if (record.on) actors.add(record.actor);
    else actors.delete(record.actor);
    if (actors.size > 0) byEmoji.set(record.emoji, actors);
    else byEmoji.delete(record.emoji);
    if (byEmoji.size > 0) active.set(record.comment, byEmoji);
    else active.delete(record.comment);
  }
  return comments.map((comment) => ({
    ...comment,
    reactions: [...(active.get(comment.id ?? "") ?? [])].map(([emoji, actors]) => ({
      emoji,
      actors: [...actors].sort(),
    })),
  }));
}

function appendUnique<T>(current: readonly T[], incoming: readonly T[], key: (value: T) => string): T[] {
  const rows = new Map(current.map((value) => [key(value), value]));
  for (const value of incoming) rows.set(key(value), value);
  return [...rows.values()];
}

export function boardView(page: BoardPage): BoardView {
  return {
    schema_version: page.schema_version,
    project: page.project,
    columns: page.workflow.map((state) => ({
      state,
      rows: page.rows.items.filter((row) => row.status === state.id),
    })),
    // Carried, not recomputed. The engine counted this against the whole
    // posting; counting the rows in hand would answer a different question and
    // call it the same one.
    total: page.rows.exact_total ?? null,
    complete: page.rows.next_cursor == null,
  };
}

function appendBoardPage(current: BoardView, incoming: BoardView): BoardView {
  const currentStates = current.columns.map((column) => column.state.id);
  const incomingStates = incoming.columns.map((column) => column.state.id);
  if (
    current.schema_version !== incoming.schema_version
    || current.project.id !== incoming.project.id
    || currentStates.length !== incomingStates.length
    || currentStates.some((state, index) => state !== incomingStates[index])
  ) {
    throw new Error("Board continuation changed project or workflow");
  }
  return {
    ...current,
    columns: current.columns.map((column, index) => ({
      ...column,
      rows: appendUnique(
        column.rows,
        incoming.columns[index]?.rows ?? [],
        (row) => row.doc_id,
      ),
    })),
    // The newer page's answer wins: it was counted against the same posting
    // and is the more recent measurement. An unmeasured continuation makes the
    // whole thing unmeasured rather than leaving a stale number standing.
    total: incoming.total,
    complete: incoming.complete,
  };
}

function applyBoardMove(board: BoardView, doc: string, pos: BoardPos): BoardView {
  const row = board.columns.flatMap((column) => column.rows).find((candidate) => candidate.doc_id === doc);
  if (!row) return board;
  const columns = board.columns.map((column) => ({
    ...column,
    rows: column.rows.filter((candidate) => candidate.doc_id !== doc),
  }));
  const column = columns.find((candidate) => candidate.state.id === row.status);
  if (!column) return board;
  let index = pos.at === "bottom" ? column.rows.length : 0;
  if (pos.at === "before" || pos.at === "after") {
    const target = column.rows.findIndex((candidate) => candidate.reff === pos.reff);
    if (target >= 0) index = target + (pos.at === "after" ? 1 : 0);
  }
  column.rows.splice(index, 0, row);
  return { ...board, columns };
}

function graphFromDetail(
  reff: string,
  doc: string,
  publication: import("./types").WorldPublicationId,
  outgoing: import("./types").Page<import("./types").IssueRelationDto>,
  incoming: import("./types").Page<import("./types").IssueRelationDto>,
): GraphView {
  const outgoingParents = outgoing.items.filter((item) => item.kind === "parent");
  const children = incoming.items.filter((item) => item.kind === "parent");
  return {
    schema_version: 3,
    reff,
    doc_id: doc,
    publication,
    parent: outgoingParents.length === 1 ? outgoingParents[0]?.row ?? null : null,
    parent_conflicted: outgoingParents.length > 1,
    children: children.map((item) => item.row),
    links: [...outgoing.items, ...incoming.items]
      .filter((item) => item.kind !== "parent")
      .map((item) => ({ kind: item.kind, direction: item.direction, row: item.row })),
    blocked_by: incoming.items
      .filter((item) => item.kind === "blocks")
      .map((item) => item.row),
    next_outgoing: outgoing.next_cursor ?? null,
    next_incoming: incoming.next_cursor ?? null,
  };
}
export const projectKeys = {
  board: (space: string, project: string | null) => `${prefix(space)}board:${part(project)}`,
  row: (space: string, reff: string) => `${prefix(space)}row:${part(reff)}`,
  issue: (space: string, reff: string) => `${prefix(space)}issue:${part(reff)}`,
  graph: (space: string, reff: string) => `${prefix(space)}graph:${part(reff)}`,
  history: (space: string, reff: string) => `${prefix(space)}history:${part(reff)}`,
  milestones: (space: string, project: string) => `${prefix(space)}milestones:${part(project)}`,
  geometry: (space: string, project: string, roots: readonly string[], publication?: PublicationId | null) =>
    `${prefix(space)}geometry:${part(project)}:${part([...roots].sort().join(","))}:${part(publicationKey(publication))}`,
  specs: (space: string, project: string | null) => `${prefix(space)}specs:${part(project)}`,
  spec: (space: string, spec: string) => `${prefix(space)}spec:${part(spec)}`,
  specHistory: (space: string, spec: string) => `${prefix(space)}spec-history:${part(spec)}`,
  specReferences: (space: string, project: string | null) =>
    `${prefix(space)}spec-references:${part(project)}`,
  specObservations: (space: string, project: string | null) =>
    `${prefix(space)}spec-observations:${part(project)}`,
  baselines: (space: string, project: string | null) => `${prefix(space)}baselines:${part(project)}`,
  baseline: (space: string, baseline: string) => `${prefix(space)}baseline:${part(baseline)}`,
  baselineHistory: (space: string, baseline: string) =>
    `${prefix(space)}baseline-history:${part(baseline)}`,
  grants: (space: string) => `${prefix(space)}grants`,
  updates: (space: string, project: string) => `${prefix(space)}updates:${part(project)}`,
  labels: (space: string) => `${prefix(space)}labels`,
  members: (space: string) => `${prefix(space)}members`,
  projects: (space: string) => `${prefix(space)}projects`,
  teams: (space: string) => `${prefix(space)}teams`,
  status: (space: string) => `${prefix(space)}status`,
  standing: (space: string) => `${prefix(space)}standing`,
  packet: (space: string, reff: string) => `${prefix(space)}packet:${part(reff)}`,
  operation: (space: string, operation: string) => `${prefix(space)}operation:${operation}`,
  latestOperation: (space: string) => `${prefix(space)}operation:latest`,
};

/**
 * The `specs` plane, narrowed to one project once its `prj_` id is known.
 *
 * Every Spec resource is keyed by a project KEY (or `null` for the whole
 * Space), and the ring names a project by id -- which only arrives with the
 * data. So each loader declares its derivation as a thunk that reads the id
 * back out of what it loaded, and takes the plane whole until then. A
 * whole-Space resource takes it whole always: a relation crosses projects
 * freely, and the reader that joins them must see every project's writes.
 */
const specsPlane = (id: string | null | undefined): CatalogDependency =>
  id ? { plane: "specs", scopeId: id } : "specs";

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
  readonly history: ResourceSnapshot<Page<ActivityEvent>>;
  readonly milestones: ResourceSnapshot<MilestoneDto[]>;
  readonly partial: boolean;
  readonly secondaryError: unknown | null;
}

export interface PagedResourceSnapshot<T> extends ResourceSnapshot<T[]> {
  readonly nextCursor: string | null;
  readonly loadMore: () => Promise<void>;
}

function pagedResource<T>(
  resource: ResourceSnapshot<T[]>,
  key: string,
  store: ProjectViewerStore,
  loadMore: () => Promise<void>,
): PagedResourceSnapshot<T> {
  return Object.freeze({
    ...resource,
    nextCursor: store.pageContinuation(key)?.cursor ?? null,
    loadMore,
  });
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
  private pageContinuations = new Map<
    string,
    { cursor: string; publication: import("./types").WorldPublicationId }
  >();
  private boardPageDepth = new Map<string, number>();
  private boardPublications = new Map<string, WorldPublicationId>();
  private boardMoves = new Map<string, { resource: string; pos: BoardPos; operation: string }>();
  private operations = new Map<string, OperationFeedback>();
  private operationRollbacks = new Map<string, () => void>();
  private operationPolls = new Map<string, ReturnType<typeof setTimeout>>();
  private observedOperations = new Map<string, WorldPublicationId>();

  constructor(
    private readonly rpc: Rpc = defaultRpc,
    private readonly spaceRpc: SpaceRpc = defaultSpaceRpc,
    resources = new WorldViewStore(),
    private readonly mintOperation: () => string = randomOperationId,
  ) {
    this.resources = resources;
  }

  operation(space: string, operation: string): OperationFeedback | null {
    return this.operations.get(`${space}/${operation}`) ?? null;
  }

  latestOperation(space: string): OperationFeedback | null {
    return this.resources.read<OperationFeedback>(projectKeys.latestOperation(space)).data ?? null;
  }

  private publishOperation(value: OperationFeedback): void {
    this.operations.set(`${value.space}/${value.operation}`, value);
    this.resources.set(projectKeys.operation(value.space, value.operation), value);
    this.resources.set(projectKeys.latestOperation(value.space), value);
  }

  private pageItems<T>(key: string, page: Page<T>): T[] {
    const cursor = page.next_cursor ?? null;
    if (cursor) {
      this.pageContinuations.set(key, { cursor, publication: page.publication });
    } else {
      this.pageContinuations.delete(key);
    }
    return page.items;
  }

  pageContinuation(key: string) {
    return this.pageContinuations.get(key) ?? null;
  }

  private nextPage(key: string) {
    const continuation = this.pageContinuations.get(key);
    return continuation ? { limit: 100, cursor: continuation.cursor } : null;
  }

  private appendPage<T>(
    key: string,
    page: Page<T>,
    identity: (item: T) => string,
  ): void {
    const expected = this.pageContinuations.get(key);
    if (!expected || worldPublicationKey(expected.publication) !== worldPublicationKey(page.publication)) {
      this.resources.invalidate(key);
      throw new Error("Collection cursor crossed publications");
    }
    const current = this.resources.read<T[]>(key).data ?? [];
    const items = appendUnique(current, page.items, identity);
    const next = page.next_cursor ?? null;
    if (next) {
      this.pageContinuations.set(key, { cursor: next, publication: page.publication });
    } else {
      this.pageContinuations.delete(key);
    }
    this.resources.set(key, items, !next);
  }

  selectBoard(space: string, project: string | null): BoardView | null {
    const key = projectKeys.board(space, project);
    const source = this.resources.read<BoardView>(key);
    const overlay = source.data
      ? source.data.columns.flatMap((column) => column.rows)
        .map((row) => `${row.doc_id}:${this.overlay.signature(row.doc_id)}`)
        .concat([...this.boardMoves.entries()].map(([doc, move]) =>
          move.resource === key ? `${doc}:${JSON.stringify(move.pos)}` : ""))
        .join("|")
      : "";
    const cached = this.boardSelectors.get(key);
    if (cached?.source === source && cached.overlay === overlay) return cached.value;
    let value = source.data ? applyOverlay(source.data, this.overlay).board : null;
    if (value) {
      for (const [doc, move] of this.boardMoves) {
        if (move.resource === key) value = applyBoardMove(value, doc, move.pos);
      }
    }
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
    const history = this.resources.read<Page<ActivityEvent>>(projectKeys.history(space, reff));
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
    const loading = this.load(key, async () => {
      const depth = this.boardPageDepth.get(key) ?? 1;
      const result = await this.rpc(space, { cmd: "board", project, page: firstPage() });
      if (result.kind !== "board") throw new Error("Expected board response");
      let view = boardView(result);
      let cursor = result.rows.next_cursor ?? null;
      let loaded = 1;
      while (cursor && loaded < depth) {
        const next = await this.rpc(space, {
          cmd: "board",
          project,
          page: { limit: 100, cursor },
        });
        if (next.kind !== "board") throw new Error("Expected board response");
        if (worldPublicationKey(next.rows.publication) !== worldPublicationKey(result.rows.publication)) {
          throw new Error("Board refresh crossed publications");
        }
        view = appendBoardPage(view, boardView(next));
        cursor = next.rows.next_cursor ?? null;
        loaded += 1;
      }
      if (cursor) {
        this.pageContinuations.set(key, { cursor, publication: result.rows.publication });
      } else {
        this.pageContinuations.delete(key);
      }
      this.boardPageDepth.set(key, loaded);
      this.boardPublications.set(key, result.rows.publication);
      this.ingestBoard(space, view);
      return view;
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
    return loading.then((view) => {
      const publication = this.boardPublications.get(key);
      if (publication) this.reconcileOperations(key, publication);
      return view;
    });
  }

  async loadMoreBoard(space: string, project: string | null): Promise<void> {
    const key = projectKeys.board(space, project);
    const page = this.nextPage(key);
    if (!page) return;
    const expected = this.pageContinuations.get(key);
    const current = this.resources.read<BoardView>(key).data;
    if (!expected || !current) {
      this.resources.invalidate(key);
      throw new Error("Board continuation has no pinned source page");
    }
    const result = await this.rpc(space, { cmd: "board", project, page });
    if (result.kind !== "board") throw new Error("Expected board response");
    if (worldPublicationKey(expected.publication) !== worldPublicationKey(result.rows.publication)) {
      this.resources.invalidate(key);
      throw new Error("Board cursor crossed publications");
    }
    const view = appendBoardPage(current, boardView(result));
    const next = result.rows.next_cursor ?? null;
    if (next) {
      this.pageContinuations.set(key, { cursor: next, publication: result.rows.publication });
    } else {
      this.pageContinuations.delete(key);
    }
    this.ingestBoard(space, view);
    this.boardPageDepth.set(key, (this.boardPageDepth.get(key) ?? 1) + 1);
    this.boardPublications.set(key, result.rows.publication);
    this.resources.set(key, view, !next);
    this.reconcileOperations(key, result.rows.publication);
  }

  private reconcileOperations(
    resource: string,
    publication: WorldPublicationId,
    exactReconciliation = false,
  ): void {
    for (const operation of this.operations.values()) {
      const observed = this.observedOperations.get(operation.operation);
      if (
        operation.resource !== resource
        || operation.phase !== "accepted"
        || !operation.publication
        || worldPublicationKey(operation.publication) !== worldPublicationKey(publication)
        || (!exactReconciliation
          && (!observed
            || worldPublicationKey(observed) !== worldPublicationKey(publication)))
      ) continue;
      this.commitOperation(operation, publication);
    }
  }

  private commitOperation(operation: OperationFeedback, publication: WorldPublicationId): void {
    this.publishOperation({ ...operation, phase: "committed", publication });
    this.overlay.clearOperation(operation.doc, operation.operation);
    if (this.boardMoves.get(operation.doc)?.operation === operation.operation) {
      this.boardMoves.delete(operation.doc);
    }
    this.operationRollbacks.delete(operation.operation);
    const poll = this.operationPolls.get(operation.operation);
    if (poll !== undefined) clearTimeout(poll);
    this.operationPolls.delete(operation.operation);
    this.notifyRows(operation.space, [operation.doc]);
  }

  /**
   * Render the operation's own changed issue at its exact receipt publication.
   * A Detail/List mutation cannot depend on a Board loader being mounted in
   * order to reach `committed`, and an unrelated page at the same publication
   * is not proof that this doc has been rendered.
   */
  private async hydrateOperationTarget(operation: OperationFeedback): Promise<void> {
    if (operation.phase !== "accepted" || !operation.publication) return;
    const observed = this.observedOperations.get(operation.operation);
    if (
      !observed
      || worldPublicationKey(observed) !== worldPublicationKey(operation.publication)
    ) return;
    let rendered = false;
    const labelEffects = operation.results?.filter((result) => result.kind === "label") ?? [];
    if (labelEffects.length > 0) {
      const resource = projectKeys.labels(operation.space);
      let labels = (this.resources.read<LabelDto[]>(resource).data ?? [])
        .filter((label) => label.id !== `local:${operation.operation}`);
      for (const effect of labelEffects) {
        const result = await this.rpc(operation.space, {
          cmd: "label_show",
          label: effect.id,
          publication: operation.publication,
        });
        if (
          result.kind !== "label"
          || worldPublicationKey(result.publication) !== worldPublicationKey(operation.publication)
          || (result.label != null && result.label.id !== effect.id)
        ) return;
        labels = result.label == null
          ? labels.filter((label) => label.id !== effect.id)
          : [...labels.filter((label) => label.id !== effect.id), result.label];
      }
      this.resources.set(resource, labels);
      rendered = true;
    }
    if (operation.doc && !operation.doc.startsWith("local:")) {
      const row = this.rowsByDoc.get(operation.space)?.get(operation.doc);
      const reff = row?.reff ?? operation.doc;
      const result = await this.rpc(operation.space, {
        cmd: "issue_detail",
        reff,
        publication: operation.publication,
      });
      if (
        result.kind !== "issue_detail"
        || worldPublicationKey(result.publication) !== worldPublicationKey(operation.publication)
      ) return;
      this.ingestIssueDetail(operation.space, reff, result);
      rendered = true;
    }
    if (!rendered) return;
    const current = this.operation(operation.space, operation.operation);
    if (current?.phase === "accepted") this.commitOperation(current, operation.publication);
  }

  ensureIssue(space: string, reff: string, force = false): Promise<IssueView> {
    const key = projectKeys.issue(space, reff);
    const promise = this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "issue_detail", reff });
      if (result.kind !== "issue_detail") throw new Error("Expected issue detail response");
      return this.ingestIssueDetail(space, reff, result);
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

  private ingestIssueDetail(
    space: string,
    reff: string,
    result: IssueDetailResponse,
  ): IssueView {
    const issue: IssueView = {
        ...result.issue,
        comments: commentsWithReactions(result.comments.items, result.reactions.items),
        attachments: result.attachments.items,
        checks: result.checks.items,
        enrichment: {
          publication: result.publication,
          reaction_records: result.reactions.items,
          comments: result.comments.next_cursor ?? null,
          reactions: result.reactions.next_cursor ?? null,
          attachments: result.attachments.next_cursor ?? null,
          checks: result.checks.next_cursor ?? null,
          outgoing_relations: result.outgoing_relations.next_cursor ?? null,
          incoming_relations: result.incoming_relations.next_cursor ?? null,
        },
    };
    const graph = graphFromDetail(
        reff,
        issue.doc_id,
        result.publication,
        result.outgoing_relations,
        result.incoming_relations,
      );
    for (const row of [
        ...(graph.parent ? [graph.parent] : []),
        ...graph.children,
        ...graph.links.map((link) => link.row),
    ]) this.ingestRow(space, row);
    this.resources.set(
        projectKeys.graph(space, reff),
        graph,
        !graph.next_outgoing && !graph.next_incoming,
    );
    this.ingestIssue(space, issue);
    this.resources.set(projectKeys.issue(space, reff), issue);
    return issue;
  }

  ensureGraph(space: string, reff: string, force = false): Promise<GraphView> {
    const key = projectKeys.graph(space, reff);
    const promise = this.load(key, async () => {
      await this.ensureIssue(space, reff, force);
      const graph = this.resources.read<GraphView>(key).data;
      if (!graph) throw new Error("Issue detail returned no relation pages");
      return graph;
      // This is a bounded first-page projection. The exact publication and
      // independent direction cursors remain on the value; loading more never
      // drains the opposite direction or silently crosses publications.
    }, { issues: "any", catalog: ["relations"] }, force);
    this.resources.evict(`${prefix(space)}graph:`, 50, new Set([key]));
    return promise;
  }

  /** Continue one issue's comment records at the exact publication returned by
   * the summary. The cursor is never replayed against `current`: a publication
   * mismatch invalidates the detail instead of manufacturing a mixed snapshot. */
  async loadMoreIssueComments(space: string, reff: string): Promise<void> {
    const key = projectKeys.issue(space, reff);
    const issue = this.resources.read<IssueView>(key).data;
    const enrichment = issue?.enrichment;
    if (!issue || !enrichment?.comments) return;
    const result = await this.rpc(space, {
      cmd: "issue_comments",
      reff,
      publication: publicationCoordinate(enrichment.publication.publication),
      page: { limit: 100, cursor: enrichment.comments },
    });
    if (result.kind !== "comments") throw new Error("Expected issue comments response");
    if (worldPublicationKey(result.page.publication) !== worldPublicationKey(enrichment.publication)) {
      this.resources.invalidate(key);
      throw new Error("Issue comment cursor crossed publications");
    }
    const comments = appendUnique(issue.comments, result.page.items, (comment) => comment.id ?? `${comment.ts}:${comment.author}:${comment.body}`);
    this.resources.set(key, {
      ...issue,
      comments: commentsWithReactions(comments, enrichment.reaction_records ?? []),
      enrichment: { ...enrichment, comments: result.page.next_cursor },
    });
  }

  async loadMoreIssueReactions(space: string, reff: string): Promise<void> {
    const key = projectKeys.issue(space, reff);
    const issue = this.resources.read<IssueView>(key).data;
    const enrichment = issue?.enrichment;
    if (!issue || !enrichment?.reactions) return;
    const result = await this.rpc(space, {
      cmd: "issue_reactions",
      reff,
      publication: publicationCoordinate(enrichment.publication.publication),
      page: { limit: 100, cursor: enrichment.reactions },
    });
    if (result.kind !== "reactions") throw new Error("Expected issue reactions response");
    if (worldPublicationKey(result.page.publication) !== worldPublicationKey(enrichment.publication)) {
      this.resources.invalidate(key);
      throw new Error("Issue reaction cursor crossed publications");
    }
    const records = appendUnique(
      enrichment.reaction_records ?? [],
      result.page.items,
      (reaction) => `${reaction.comment}\u0000${reaction.emoji}\u0000${reaction.actor}`,
    );
    this.resources.set(key, {
      ...issue,
      comments: commentsWithReactions(issue.comments, records),
      enrichment: { ...enrichment, reaction_records: records, reactions: result.page.next_cursor },
    });
  }

  async loadMoreIssueAttachments(space: string, reff: string): Promise<void> {
    const key = projectKeys.issue(space, reff);
    const issue = this.resources.read<IssueView>(key).data;
    const enrichment = issue?.enrichment;
    if (!issue || !enrichment?.attachments) return;
    const result = await this.rpc(space, {
      cmd: "issue_attachments",
      reff,
      publication: publicationCoordinate(enrichment.publication.publication),
      page: { limit: 100, cursor: enrichment.attachments },
    });
    if (result.kind !== "attachments") throw new Error("Expected issue attachments response");
    if (worldPublicationKey(result.page.publication) !== worldPublicationKey(enrichment.publication)) {
      this.resources.invalidate(key);
      throw new Error("Issue attachment cursor crossed publications");
    }
    this.resources.set(key, {
      ...issue,
      attachments: appendUnique(issue.attachments ?? [], result.page.items, (attachment) => attachment.id),
      enrichment: { ...enrichment, attachments: result.page.next_cursor },
    });
  }

  async loadMoreIssueChecks(space: string, reff: string): Promise<void> {
    const key = projectKeys.issue(space, reff);
    const issue = this.resources.read<IssueView>(key).data;
    const enrichment = issue?.enrichment;
    if (!issue || !enrichment?.checks) return;
    const result = await this.rpc(space, {
      cmd: "issue_checks",
      reff,
      publication: publicationCoordinate(enrichment.publication.publication),
      page: { limit: 100, cursor: enrichment.checks },
    });
    if (result.kind !== "checks") throw new Error("Expected issue checks response");
    if (worldPublicationKey(result.page.publication) !== worldPublicationKey(enrichment.publication)) {
      this.resources.invalidate(key);
      throw new Error("Issue check cursor crossed publications");
    }
    this.resources.set(key, {
      ...issue,
      checks: appendUnique(issue.checks ?? [], result.page.items, (check) => check.run),
      enrichment: { ...enrichment, checks: result.page.next_cursor },
    });
  }

  async loadMoreIssueRelations(
    space: string,
    reff: string,
    direction: "out" | "in",
  ): Promise<void> {
    const issue = this.resources.read<IssueView>(projectKeys.issue(space, reff)).data;
    const graphKey = projectKeys.graph(space, reff);
    const graph = this.resources.read<GraphView>(graphKey).data;
    const enrichment = issue?.enrichment;
    const cursor = direction === "out" ? enrichment?.outgoing_relations : enrichment?.incoming_relations;
    if (!issue || !graph || !enrichment || !cursor) return;
    const result = await this.rpc(space, {
      cmd: "issue_relations",
      reff,
      direction,
      publication: publicationCoordinate(enrichment.publication.publication),
      page: { limit: 100, cursor },
    });
    if (result.kind !== "relations") throw new Error("Expected issue relations response");
    if (worldPublicationKey(result.page.publication) !== worldPublicationKey(enrichment.publication)) {
      this.resources.invalidate(projectKeys.issue(space, reff));
      this.resources.invalidate(graphKey);
      throw new Error("Issue relation cursor crossed publications");
    }
    const relations = result.page.items;
    const parentRows = direction === "out"
      ? relations.filter((relation) => relation.kind === "parent").map((relation) => relation.row)
      : [];
    const childRows = direction === "in"
      ? relations.filter((relation) => relation.kind === "parent").map((relation) => relation.row)
      : [];
    const links = relations
      .filter((relation) => relation.kind !== "parent")
      .map((relation) => ({ kind: relation.kind, direction: relation.direction, row: relation.row }));
    const next = result.page.next_cursor;
    const mergedGraph: GraphView = {
      ...graph,
      parent: graph.parent ?? parentRows[0] ?? null,
      parent_conflicted: !!graph.parent_conflicted || parentRows.length > (graph.parent ? 0 : 1),
      children: appendUnique(graph.children, childRows, (row) => row.doc_id),
      links: appendUnique(graph.links, links, (link) => `${link.kind}\u0000${link.direction}\u0000${link.row.doc_id}`),
      blocked_by: appendUnique(
        graph.blocked_by,
        direction === "in"
          ? relations.filter((relation) => relation.kind === "blocks").map((relation) => relation.row)
          : [],
        (row) => row.doc_id,
      ),
      ...(direction === "out"
        ? { next_outgoing: next ?? null }
        : { next_incoming: next ?? null }),
    };
    for (const relation of relations) this.ingestRow(space, relation.row);
    this.resources.set(graphKey, mergedGraph, !mergedGraph.next_outgoing && !mergedGraph.next_incoming);
    this.resources.set(projectKeys.issue(space, reff), {
      ...issue,
      enrichment: {
        ...enrichment,
        ...(direction === "out"
          ? { outgoing_relations: next ?? null }
          : { incoming_relations: next ?? null }),
      },
    });
  }

  ensureHistory(space: string, reff: string, force = false): Promise<Page<ActivityEvent>> {
    const key = projectKeys.history(space, reff);
    const promise = this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "history", reff, page: firstPage() });
      if (result.kind !== "activity") throw new Error("Expected history response");
      return result.page;
      // The feed is a cursor, not a scope: the doorbell can only say it advanced.
    }, { activity: true }, force);
    this.resources.evict(`${prefix(space)}history:`, 50, new Set([key]));
    return promise;
  }

  async loadMoreIssueHistory(space: string, reff: string): Promise<void> {
    const key = projectKeys.history(space, reff);
    const current = this.resources.read<Page<ActivityEvent>>(key).data;
    if (!current?.next_cursor) return;
    const result = await this.rpc(space, {
      cmd: "history",
      reff,
      publication: publicationCoordinate(current.publication.publication),
      page: { limit: 100, cursor: current.next_cursor },
    });
    if (result.kind !== "activity") throw new Error("Expected history response");
    if (worldPublicationKey(result.page.publication) !== worldPublicationKey(current.publication)) {
      this.resources.invalidate(key);
      throw new Error("Issue history cursor crossed publications");
    }
    this.resources.set(key, {
      ...result.page,
      items: appendUnique(current.items, result.page.items, (event) => event.cursor ?? `${event.ts}:${event.seq}`),
    });
  }

  ensureGeometry(
    space: string,
    project: string,
    roots: readonly string[],
    publication?: PublicationId | null,
    force = false,
  ): Promise<GeometryView> {
    const canonicalRoots = [...new Set(roots)].sort();
    const key = projectKeys.geometry(space, project, canonicalRoots, publication);
    return this.load(key, async () => {
      const result = await this.rpc(space, {
        cmd: "geometry",
        project,
        roots: canonicalRoots,
        ...(publication ? { publication: publicationCoordinate(publication) } : {}),
      });
      if (result.kind !== "geometry") throw new Error("Expected geometry response");
      if (result.readiness.state === "pending") {
        setTimeout(() => {
          if (this.resources.isActive(key)) {
            void this.ensureGeometry(space, project, canonicalRoots, publication, true)
              .catch(() => undefined);
          }
        }, 25);
      }
      return result;
    }, publication ? {} : {
      catalog: ["projects", "teams", "workflow", "labels", "milestones", "cycles", "docs", "relations"],
      issues: { scopeId: project },
    }, force);
  }

  ensureMilestones(space: string, project: string, force = false): Promise<MilestoneDto[]> {
    const key = projectKeys.milestones(space, project);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "milestone_list", project, page: firstPage() });
      if (result.kind !== "milestones") throw new Error("Expected milestones response");
      return this.pageItems(key, result.page);
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
    const key = projectKeys.updates(space, project);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "project_updates", project, page: firstPage() });
      if (result.kind !== "updates") throw new Error("Expected updates response");
      return this.pageItems(key, result.page);
      // One dependency, where the milestones above need three: an update is
      // authored once and never edited, so no issue moving and no workflow
      // change can alter what a past post said. Only the feed's own plane can.
      //
      // `project` is the `prj_` id the ring matches on, so posting in ENG does
      // not refetch DSN's feed.
    }, { catalog: [{ plane: "updates", scopeId: project }] }, force);
  }

  ensureSpecs(space: string, project: string | null, force = false): Promise<SpecSummary[]> {
    const key = projectKeys.specs(space, project);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "spec_list", project, page: firstPage() });
      if (result.kind !== "specs") throw new Error("Expected specs response");
      return this.pageItems(key, result.page);
      // Specs and Baselines are Bodies of their own rather than a region of the
      // catalog. The ring names the project a Spec write landed in, and the
      // register learns its own project's id from its first row.
    }, () => ({
      catalog: [specsPlane(project ? this.resources.read<SpecSummary[]>(key).data?.[0]?.project : null)],
    }), force);
  }

  ensureSpec(space: string, spec: string, force = false): Promise<SpecView> {
    return this.load(projectKeys.spec(space, spec), async () => {
      const result = await this.rpc(space, { cmd: "spec_show", spec });
      if (result.kind !== "spec") throw new Error("Expected spec response");
      return result.spec;
      // Same plane as the register above — and the reader needs its own resource
      // rather than picking its row out of the list: a deep link opens on a Spec
      // before any register has loaded.
    }, () => ({
      catalog: [specsPlane(this.resources.read<SpecView>(projectKeys.spec(space, spec)).data?.project)],
    }), force);
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
    patch: { title?: string; text?: string; links?: SpecLink[]; plan?: PlanData | null },
  ): Promise<SpecView> {
    const result = await this.rpc(space, { cmd: "spec_revise", spec, expected, ...patch });
    if (result.kind !== "spec") throw new Error("Expected spec response");
    this.resources.set(projectKeys.spec(space, spec), result.spec);
    await this.refreshSpecRegisters(space);
    return result.spec;
  }

  /**
   * Move a legacy Spec body onto the current hidden document schema without
   * changing its lifecycle. The engine writes an immutable successor and uses
   * `expected` as the same compare-and-swap guard as every other revision.
   */
  async upgradeSpecDocument(
    space: string,
    spec: string,
    expected: string,
    text: string,
  ): Promise<SpecView> {
    const result = await this.rpc(space, {
      cmd: "spec_document_upgrade",
      spec,
      expected,
      text,
    });
    if (result.kind !== "spec") throw new Error("Expected spec response");
    this.resources.set(projectKeys.spec(space, spec), result.spec);
    await Promise.all([
      this.ensureSpecHistory(space, spec, true).catch(() => undefined),
      this.refreshSpecRegisters(space),
    ]);
    return result.spec;
  }

  /**
   * Commit a staged change to what a document asserts, rebasing if the head
   * moved under it.
   *
   * `spec_revise` replaces the whole link array against an expected head, so
   * two people adding *different* relations collide even though the two edits
   * do not actually disagree. Replaying the delta onto whatever the head is now
   * is correct without a merge policy, because a link set is a set: the engine
   * canonicalises the Body before it hashes, so nothing here has to reproduce
   * Rust's ordering either.
   *
   * The retry is deliberately not driven by the error. A stale `expected`
   * surfaces as a plain 400 carrying prose (`router.rs`), and matching on that
   * string would make this break the next time somebody rewords it — so the
   * refusal is only ever *evidence*, and the head having actually moved is the
   * thing that decides whether to try again. A refusal with a stationary head
   * is somebody's answer to the request itself, and rethrows untouched.
   */
  async relateSpec(
    space: string,
    spec: string,
    expected: string,
    delta: LinkDelta,
    attempts = 3,
  ): Promise<SpecView> {
    let head = await this.ensureSpec(space, spec);
    // Nothing staged is not a tiny write, it is no write: every revise mints an
    // immutable revision onto the rail, and a revision that changed nothing is
    // noise in the one place a reader goes to find out what changed.
    if (emptyDelta(delta)) return head;
    if (head.revision !== expected) head = await this.ensureSpec(space, spec, true);
    for (let attempt = 0; ; attempt++) {
      const links = applyLinkDelta(head.body.links, delta);
      try {
        return await this.reviseSpec(space, spec, head.revision, { links });
      } catch (reason) {
        if (attempt + 1 >= attempts) throw reason;
        const fresh = await this.ensureSpec(space, spec, true);
        // Same head: the engine refused the request on its merits — the target
        // of a new link has gone, the actor cannot write here. Retrying would
        // only ask the same question again and bury the real answer.
        if (fresh.revision === head.revision) throw reason;
        head = fresh;
      }
    }
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
    const key = projectKeys.specHistory(space, spec);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "spec_history", spec, page: firstPage() });
      if (result.kind !== "spec_revisions") throw new Error("Expected spec revisions response");
      return this.pageItems(key, result.page);
    }, () => ({
      catalog: [specsPlane(this.resources.read<SpecRevision[]>(key).data?.[0]?.body.project)],
    }), force);
  }

  ensureBaselines(space: string, project: string | null, force = false): Promise<BaselineSummary[]> {
    const key = projectKeys.baselines(space, project);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "baseline_list", project, page: firstPage() });
      if (result.kind !== "baselines") throw new Error("Expected baselines response");
      return this.pageItems(key, result.page);
    }, () => ({
      catalog: [specsPlane(project ? this.resources.read<BaselineSummary[]>(key).data?.[0]?.project : null)],
    }), force);
  }

  ensureBaseline(space: string, baseline: string, force = false): Promise<BaselineView> {
    return this.load(projectKeys.baseline(space, baseline), async () => {
      const result = await this.rpc(space, { cmd: "baseline_show", baseline });
      if (result.kind !== "baseline") throw new Error("Expected baseline response");
      return result;
    }, () => ({
      catalog: [specsPlane(
        this.resources.read<BaselineView>(projectKeys.baseline(space, baseline)).data?.project,
      )],
    }), force);
  }

  ensureBaselineHistory(
    space: string,
    baseline: string,
    force = false,
  ): Promise<BaselineRevisionDto[]> {
    const key = projectKeys.baselineHistory(space, baseline);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "baseline_history", baseline, page: firstPage() });
      if (result.kind !== "baseline_revisions") {
        throw new Error("Expected baseline revisions response");
      }
      return this.pageItems(key, result.page);
    }, () => ({
      catalog: [specsPlane(this.resources.read<BaselineRevisionDto[]>(key).data?.[0]?.body.project)],
    }), force);
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

  /**
   * What is in force for one Issue, live.
   *
   * Two things move it: the Issue's own binding, which is a relation on the
   * Issue and so a ring that names its doc; and any Spec or Baseline anywhere
   * -- a Spec in another project may govern this Issue, so the plane is taken
   * whole rather than scoped to the Issue's project. Before this was a
   * resource the panel read the packet once, on mount, and never again: a Spec
   * issued in the next window left the brief showing what governed a minute
   * ago, with nothing on screen to say so.
   */
  ensurePacket(space: string, reff: string, force = false): Promise<Packet> {
    const key = projectKeys.packet(space, reff);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "packet", reff });
      if (result.kind !== "packet") throw new Error("Expected packet response");
      return result;
      // Same resolution as `ensureIssue`: the ring names docs, this resource
      // is keyed by `reff`, and the row we hold joins them.
    }, () => {
      const doc = this.resources.read<Row>(projectKeys.row(space, reff)).data?.doc_id;
      return { catalog: ["specs"], issues: doc ? { docs: [doc] } : "any" };
    }, force);
  }

  /** Pin an exact issued Baseline revision to an Issue, or clear the pin. */
  async bindBaseline(
    space: string,
    reff: string,
    baseline: BaselineRef | null,
  ): Promise<void> {
    await this.rpc(space, { cmd: "issue_baseline", reff, baseline });
    // The doorbell will say so too; this is the panel not waiting for it.
    await this.refreshActive([projectKeys.packet(space, reff)]);
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
  ): Promise<SpecReferenceFact[]> {
    const key = projectKeys.specReferences(space, project);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "spec_references", project, page: firstPage() });
      if (result.kind !== "spec_references") {
        throw new Error("Expected spec references response");
      }
      return this.pageItems(key, result.page);
    }, { catalog: ["specs"] }, force);
  }

  ensureSpecObservations(
    space: string,
    project: string | null,
    force = false,
  ): Promise<SpecObservationRecord[]> {
    const key = projectKeys.specObservations(space, project);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "spec_observations", project, page: firstPage() });
      if (result.kind !== "spec_observations") {
        throw new Error("Expected spec observations response");
      }
      return this.pageItems(key, result.page);
    }, () => ({
      catalog: [specsPlane(
        project ? this.resources.read<SpecObservationRecord[]>(key).data?.[0]?.project : null,
      )],
    }), force);
  }

  /**
   * File a note about the graph.
   *
   * No `expected` and no retry, unlike `relateSpec`: an Observation is not a
   * revision and does not compete for the head, so there is no race here to
   * lose. That is the ergonomic half of why the concept exists — the other half
   * being that it binds nobody's document.
   */
  async observeSpec(
    space: string,
    spec: string,
    rel: SpecRel,
    target: SpecTarget,
    note: string,
  ): Promise<void> {
    const result = await this.rpc(space, { cmd: "spec_observe", spec, rel, target, note });
    if (result.kind !== "spec_observations") throw new Error("Expected spec observations response");
    await this.refreshSpecObservations(space);
  }

  async retractObservation(space: string, spec: string, observation: string): Promise<void> {
    const result = await this.rpc(space, { cmd: "spec_retract", spec, observation });
    if (result.kind !== "spec_observations") throw new Error("Expected spec observations response");
    await this.refreshSpecObservations(space);
  }

  /** Re-read whichever note sets are on screen — same argument as
   *  `refreshSpecRegisters`: keyed by the caller's project handle, not the
   *  resolved id a note's Spec reports. */
  private refreshSpecObservations(space: string): Promise<void> {
    return this.refreshActive(
      [...this.loaders.keys()].filter((key) =>
        key.startsWith(`${prefix(space)}spec-observations:`),
      ),
    );
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
    const key = projectKeys.labels(space);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "label_list", page: firstPage() });
      if (result.kind !== "labels") throw new Error("Expected labels response");
      return this.pageItems(key, result.page);
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
    const key = projectKeys.projects(space);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "project_list", page: firstPage() });
      if (result.kind !== "projects") throw new Error("Expected projects response");
      return this.pageItems(key, result.page);
    }, { catalog: ["projects"] }, force);
  }

  /**
   * The space's teams.
   *
   * Depends on the `teams` plane *and* on `projects`: a team's `projects`
   * back-reference changes when a project is reassigned, and that write lands
   * on the project rather than on the team.
   */
  ensureTeams(space: string, force = false): Promise<TeamDto[]> {
    const key = projectKeys.teams(space);
    return this.load(key, async () => {
      const result = await this.rpc(space, { cmd: "team_list", page: firstPage() });
      if (result.kind !== "teams") throw new Error("Expected teams response");
      return this.pageItems(key, result.page);
    }, { catalog: ["teams", "projects"] }, force);
  }

  async loadMoreMilestones(space: string, project: string): Promise<void> {
    const key = projectKeys.milestones(space, project);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "milestone_list", project, page });
    if (result.kind !== "milestones") throw new Error("Expected milestones response");
    this.appendPage(key, result.page, (item) => item.id);
  }

  async loadMoreUpdates(space: string, project: string): Promise<void> {
    const key = projectKeys.updates(space, project);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "project_updates", project, page });
    if (result.kind !== "updates") throw new Error("Expected updates response");
    this.appendPage(key, result.page, (item) => item.id);
  }

  async loadMoreSpecs(space: string, project: string | null): Promise<void> {
    const key = projectKeys.specs(space, project);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "spec_list", project, page });
    if (result.kind !== "specs") throw new Error("Expected specs response");
    this.appendPage(key, result.page, (item) => item.spec);
  }

  async loadMoreSpecHistory(space: string, spec: string): Promise<void> {
    const key = projectKeys.specHistory(space, spec);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "spec_history", spec, page });
    if (result.kind !== "spec_revisions") throw new Error("Expected spec revisions response");
    this.appendPage(key, result.page, (item) => item.revision);
  }

  async loadMoreBaselines(space: string, project: string | null): Promise<void> {
    const key = projectKeys.baselines(space, project);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "baseline_list", project, page });
    if (result.kind !== "baselines") throw new Error("Expected baselines response");
    this.appendPage(key, result.page, (item) => item.baseline);
  }

  async loadMoreBaselineHistory(space: string, baseline: string): Promise<void> {
    const key = projectKeys.baselineHistory(space, baseline);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "baseline_history", baseline, page });
    if (result.kind !== "baseline_revisions") {
      throw new Error("Expected baseline revisions response");
    }
    this.appendPage(key, result.page, (item) => item.revision);
  }

  async loadMoreSpecReferences(space: string, project: string | null): Promise<void> {
    const key = projectKeys.specReferences(space, project);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "spec_references", project, page });
    if (result.kind !== "spec_references") throw new Error("Expected spec references response");
    this.appendPage(
      key,
      result.page,
      (item) => `${item.spec}\u0000${item.revision}\u0000${JSON.stringify(item.link)}`,
    );
  }

  async loadMoreSpecObservations(space: string, project: string | null): Promise<void> {
    const key = projectKeys.specObservations(space, project);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "spec_observations", project, page });
    if (result.kind !== "spec_observations") {
      throw new Error("Expected spec observations response");
    }
    this.appendPage(
      key,
      result.page,
      (item) => item.kind === "assert" ? item.observation.observation : item.observation,
    );
  }

  async loadMoreLabels(space: string): Promise<void> {
    const key = projectKeys.labels(space);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "label_list", page });
    if (result.kind !== "labels") throw new Error("Expected labels response");
    this.appendPage(key, result.page, (item) => item.id);
  }

  async loadMoreProjects(space: string): Promise<void> {
    const key = projectKeys.projects(space);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "project_list", page });
    if (result.kind !== "projects") throw new Error("Expected projects response");
    this.appendPage(key, result.page, (item) => item.id);
  }

  async loadMoreTeams(space: string): Promise<void> {
    const key = projectKeys.teams(space);
    const page = this.nextPage(key);
    if (!page) return;
    const result = await this.rpc(space, { cmd: "team_list", page });
    if (result.kind !== "teams") throw new Error("Expected teams response");
    this.appendPage(key, result.page, (item) => item.id);
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

  ensureStanding(space: string, force = false): Promise<WhoamiInfo> {
    return this.load(projectKeys.standing(space), async () => {
      const result = await this.spaceRpc(space, { cmd: "whoami" });
      if (result.kind !== "whoami") throw new Error("Expected whoami response");
      return result;
      // Standing is pure authority-plane state: a grant, promotion, or
      // revocation arriving over sync rings `authority_advanced` and this
      // re-resolves — which is how a member gated as view-only unlocks the
      // moment their grant lands, without a reload.
    }, { authority: true }, force);
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
    return this.patchIssue(space, reff, { title }, [["title", title]]);
  }

  async setStatus(space: string, reff: string, status: string): Promise<boolean> {
    return this.patchIssue(space, reff, { status }, [["status", status]]);
  }

  async setPriority(space: string, reff: string, priority: string): Promise<boolean> {
    return this.patchIssue(space, reff, { priority }, [["priority", priority]]);
  }

  /** `due` is the engine's `YYYY-MM-DD` (UTC), or null to clear. */
  async setDue(space: string, reff: string, due: string | null): Promise<boolean> {
    const predicted = due === null ? null : Math.floor(Date.parse(`${due}T00:00:00Z`) / 1000);
    return this.patchIssue(
      space,
      reff,
      due === null ? { clear_due: true } : { due: predicted! },
      [["due", predicted]],
    );
  }

  /** `estimate` is a numeric string or `"none"` — the wire's own shape. */
  async setEstimate(space: string, reff: string, estimate: string): Promise<boolean> {
    const predicted = estimate === "none" ? null : Number(estimate);
    return this.patchIssue(
      space,
      reff,
      estimate === "none" ? { clear_estimate: true } : { estimate: predicted! },
      [["estimate", predicted]],
    );
  }

  /** Add or remove one assignee. `key` must be a full 64-hex device key —
   *  `index::resolve_device` does not consult the member directory. */
  async toggleAssignee(space: string, reff: string, key: string, add: boolean): Promise<boolean> {
    const row = this.selectRow(space, reff);
    if (!row) return false;
    const assignees = add
      ? [...row.assignees.filter((actor) => actor !== key), key]
      : row.assignees.filter((actor) => actor !== key);
    return this.patchIssue(space, reff, { assignees }, [["assignees", assignees]]);
  }

  /**
   * Make `keys` the exact assignee set. `assign` is add/remove per key, so this
   * is a small batch — removals first, then the additions.
   */
  async setAssignees(space: string, reff: string, keys: readonly string[]): Promise<boolean> {
    return this.patchIssue(space, reff, { assignees: [...keys] }, [["assignees", [...keys]]]);
  }

  /** Attach or detach one label, by name — `Request::Label` resolves names. */
  async toggleLabel(space: string, reff: string, name: string, add: boolean): Promise<boolean> {
    const row = this.selectRow(space, reff);
    if (!row) return false;
    const names = row.label_names ?? [];
    const labels = add
      ? [...names.filter((label) => label !== name), name]
      : names.filter((label) => label !== name);
    return this.patchIssue(
      space,
      reff,
      { labels: labels.map((label) => ({ source: "existing", label })) },
      [["labels", labels]],
    );
  }

  /**
   * Swap one label for another (or for nothing). One swap, two requests, in
   * this order: the engine's label op is add-or-remove on a name set, so a
   * rename is a detach and an attach — removing first keeps the set from
   * briefly holding both.
   */
  async swapLabel(space: string, reff: string, from: string, to: string | null): Promise<boolean> {
    const row = this.selectRow(space, reff);
    if (!row) return false;
    const names = (row.label_names ?? []).filter((name) => name !== from);
    const labels = to !== null && !names.includes(to) ? [...names, to] : names;
    return this.patchIssue(
      space,
      reff,
      { labels: labels.map((label) => ({ source: "existing", label })) },
      [["labels", labels]],
    );
  }

  async predictValue(
    space: string,
    doc: string,
    field: Field,
    value: PredictionValue,
  ): Promise<boolean> {
    const row = this.rowsByDoc.get(space)?.get(doc);
    if (!row) return false;
    const patch = field === "title" ? { title: String(value) }
      : field === "status" ? { status: String(value) }
        : field === "priority" ? { priority: String(value) }
          : field === "due" ? (value === null ? { clear_due: true } : { due: Number(value) })
            : field === "estimate" ? (value === null ? { clear_estimate: true } : { estimate: Number(value) })
              : field === "assignees" ? { assignees: [...value as readonly string[]] }
                : { labels: [...value as readonly string[]].map((label) => ({ source: "existing" as const, label })) };
    return this.patchIssue(space, row.reff, patch, [[field, value]]);
  }

  async workIssue(
    space: string,
    doc: string,
    action: "start" | "done" | "stop",
    predictedStatus: string | null,
  ): Promise<boolean> {
    const row = this.rowsByDoc.get(space)?.get(doc);
    if (!row) return false;
    const pending = this.beginChangeSet(
      space,
      doc,
      this.boardResourceFor(space, row),
      [{ op: "issue_work", issue: row.reff, action }],
      (operation) => {
        if (predictedStatus !== null) {
          this.overlay.set(doc, "status", predictedStatus, operation);
          this.notifyRows(space, [doc]);
        }
      },
    );
    const feedback = await pending.completion;
    if (feedback.phase === "rolled_back") {
      throw new LaitError(
        feedback.error?.message ?? "the work-state change was refused",
        400,
        feedback.error?.kind ?? "error",
      );
    }
    return true;
  }

  async tombstoneIssue(space: string, reff: string, on: boolean): Promise<boolean> {
    return this.changeIssueRecord(space, reff, [{ op: "issue_tombstone", issue: reff, on }]);
  }

  async commentIssue(
    space: string,
    reff: string,
    body: string,
    parent: string | null = null,
  ): Promise<boolean> {
    return this.changeIssueRecord(space, reff, [{
      op: "issue_comment",
      issue: reff,
      body,
      ...(parent !== null ? { parent } : {}),
    }]);
  }

  async commentAtIssue(
    space: string,
    reff: string,
    body: string,
    field: string,
    start: number,
    end: number | null,
    source: WorldPublicationId,
    parent: string | null = null,
  ): Promise<boolean> {
    return this.changeIssueRecord(space, reff, [{
      op: "issue_comment_at",
      issue: reff,
      body,
      field,
      start,
      ...(end !== null ? { end } : {}),
      ...(parent !== null ? { parent } : {}),
      source,
    }]);
  }

  async reactIssue(
    space: string,
    reff: string,
    comment: string,
    emoji: string,
    on: boolean,
  ): Promise<boolean> {
    return this.changeIssueRecord(space, reff, [{
      op: "issue_reaction",
      issue: reff,
      comment,
      emoji,
      on,
    }]);
  }

  async linkIssue(
    space: string,
    reff: string,
    kind: string,
    target: string,
    on: boolean,
  ): Promise<boolean> {
    return this.changeIssueRecord(space, reff, [{
      op: "issue_link",
      issue: reff,
      kind,
      target,
      on,
    }]);
  }

  async parentIssue(
    space: string,
    reff: string,
    parent: string | null,
  ): Promise<boolean> {
    return this.changeIssueRecord(space, reff, [{
      op: "issue_parent",
      issue: reff,
      ...(parent !== null ? { parent } : {}),
    }]);
  }

  async createIssue(
    space: string,
    input: {
      project: string;
      title: string;
      status?: string | null;
      parent?: string | null;
      priority?: string | null;
      assignees?: string[];
      labels?: string[];
      body?: string | null;
      due?: number | null;
      estimate?: number | null;
    },
  ): Promise<string> {
    const pending = this.beginChangeSet(
      space,
      "local:issue-create",
      projectKeys.latestOperation(space),
      [{
        op: "issue_create",
        project: { source: "existing", project: input.project },
        title: input.title,
        ...(input.status != null ? { status: input.status } : {}),
        ...(input.parent != null ? { parent: input.parent } : {}),
        ...(input.priority != null ? { priority: input.priority } : {}),
        ...(input.assignees?.length ? { assignees: input.assignees } : {}),
        ...(input.labels?.length
          ? { labels: input.labels.map((label) => ({ source: "existing" as const, label })) }
          : {}),
        ...(input.body != null ? { body: input.body } : {}),
        ...(input.due != null ? { due: input.due } : {}),
        ...(input.estimate != null ? { estimate: input.estimate } : {}),
      }],
      () => undefined,
      (response) => {
        const created = response.results.find((result) => result.kind === "issue");
        return created
          ? { doc: created.id, resource: projectKeys.issue(space, created.id) }
          : null;
      },
    );
    const feedback = await pending.completion;
    if (feedback.phase === "rolled_back" || feedback.phase === "indeterminate") {
      throw new LaitError(
        feedback.error?.message ?? "the issue create outcome is not yet known",
        400,
        feedback.error?.kind ?? (feedback.phase === "indeterminate" ? "indeterminate" : "error"),
      );
    }
    const created = feedback.results?.find((result) => result.kind === "issue");
    if (!created) throw new Error("issue create returned no issue result");
    return created.id;
  }

  async moveIssue(
    space: string,
    reff: string,
    project: string | null,
    position: ChangePosition | null = null,
  ): Promise<boolean> {
    let row = this.selectRow(space, reff);
    if (!row) {
      await this.ensureIssue(space, reff);
      row = this.selectRow(space, reff);
    }
    if (!row) return false;
    const pending = this.beginChangeSet(
      space,
      row.doc_id,
      this.boardResourceFor(space, row),
      [{
        op: "issue_move",
        issue: reff,
        ...(project !== null ? { project: { source: "existing" as const, project } } : {}),
        ...(position !== null ? { position } : {}),
      }],
      (operation) => {
        if (project !== null) this.overlay.set(row!.doc_id, "project", project, operation);
        this.notifyRows(space, [row!.doc_id]);
      },
    );
    return this.acceptIssueFeedback(await pending.completion, "the issue move was refused");
  }

  async setIssueMilestone(
    space: string,
    reff: string,
    milestone: string | null,
  ): Promise<boolean> {
    let row = this.selectRow(space, reff);
    if (!row) {
      await this.ensureIssue(space, reff);
      row = this.selectRow(space, reff);
    }
    if (!row) return false;
    const pending = this.beginChangeSet(
      space,
      row.doc_id,
      projectKeys.issue(space, reff),
      [{ op: "issue_milestone", issue: reff, ...(milestone !== null ? { milestone } : {}) }],
      (operation) => {
        this.overlay.set(row!.doc_id, "milestone", milestone, operation);
        this.notifyRows(space, [row!.doc_id]);
      },
    );
    return this.acceptIssueFeedback(await pending.completion, "the milestone change was refused");
  }

  async createLabel(space: string, name: string, color: string): Promise<boolean> {
    return this.changeLabels(space, [{ op: "label_create", name, color }], (labels, operation) => [
      ...labels,
      { id: `local:${operation}`, name, color },
    ]);
  }

  async editLabel(
    space: string,
    label: string,
    name: string | null,
    color: string | null,
  ): Promise<boolean> {
    return this.changeLabels(
      space,
      [{ op: "label_edit", label, ...(name !== null ? { name } : {}), ...(color !== null ? { color } : {}) }],
      (labels) => labels.map((current) => current.id === label
        ? { ...current, ...(name !== null ? { name } : {}), ...(color !== null ? { color } : {}) }
        : current),
    );
  }

  async deleteLabel(space: string, label: string): Promise<boolean> {
    return this.changeLabels(
      space,
      [{ op: "label_delete", label }],
      (labels) => labels.filter((current) => current.id !== label),
    );
  }

  async createAndAttachLabel(
    space: string,
    reff: string,
    name: string,
    color: string,
  ): Promise<boolean> {
    let row = this.selectRow(space, reff);
    if (!row) {
      await this.ensureIssue(space, reff);
      row = this.selectRow(space, reff);
    }
    if (!row) return false;
    const labelResource = projectKeys.labels(space);
    const priorLabels = this.resources.read<LabelDto[]>(labelResource).data ?? [];
    const nextNames = [...(row.label_names ?? []).filter((label) => label !== name), name];
    const pending = this.beginChangeSet(
      space,
      row.doc_id,
      projectKeys.issue(space, reff),
      [
        { op: "label_create", name, color },
        {
          op: "issue_patch",
          issue: reff,
          labels: [
            ...(row.label_names ?? []).map((label) => ({ source: "existing" as const, label })),
            { source: "created", operation: 0 },
          ],
        },
      ],
      (operation) => {
        this.overlay.set(row!.doc_id, "labels", nextNames, operation);
        this.resources.set(labelResource, [
          ...priorLabels,
          { id: `local:${operation}`, name, color },
        ]);
        this.notifyRows(space, [row!.doc_id]);
      },
      undefined,
      () => this.resources.set(labelResource, priorLabels),
    );
    const feedback = await pending.completion;
    if (feedback.phase === "rolled_back") this.resources.set(labelResource, priorLabels);
    return this.acceptIssueFeedback(feedback, "the label create was refused");
  }

  private async changeLabels(
    space: string,
    operations: IssuesChangeOperation[],
    optimistic: (labels: LabelDto[], operation: string) => LabelDto[],
  ): Promise<boolean> {
    const resource = projectKeys.labels(space);
    const before = this.resources.read<LabelDto[]>(resource).data ?? [];
    const pending = this.beginChangeSet(
      space,
      "local:labels",
      resource,
      operations,
      (operation) => this.resources.set(resource, optimistic(before, operation)),
      undefined,
      () => this.resources.set(resource, before),
    );
    const feedback = await pending.completion;
    if (feedback.phase === "rolled_back") this.resources.set(resource, before);
    return this.acceptIssueFeedback(feedback, "the label change was refused");
  }

  private acceptIssueFeedback(feedback: OperationFeedback, fallback: string): true {
    if (feedback.phase === "rolled_back") {
      throw new LaitError(
        feedback.error?.message ?? fallback,
        400,
        feedback.error?.kind ?? "error",
      );
    }
    return true;
  }

  private async changeIssueRecord(
    space: string,
    reff: string,
    operations: IssuesChangeOperation[],
  ): Promise<boolean> {
    let row = this.selectRow(space, reff);
    if (!row) {
      await this.ensureIssue(space, reff);
      row = this.selectRow(space, reff);
    }
    if (!row) return false;
    const pending = this.beginChangeSet(
      space,
      row.doc_id,
      projectKeys.issue(space, reff),
      operations,
      () => this.resources.notify(projectKeys.issue(space, reff)),
    );
    const feedback = await pending.completion;
    if (feedback.phase === "rolled_back") {
      throw new LaitError(
        feedback.error?.message ?? "the issue change was refused",
        400,
        feedback.error?.kind ?? "error",
      );
    }
    return true;
  }

  private boardResourceFor(space: string, row: Row): string {
    for (const key of this.loaders.keys()) {
      if (!key.startsWith(`${prefix(space)}board:`)) continue;
      if (this.resources.read<BoardView>(key).data?.project.id === row.project_id) return key;
    }
    return projectKeys.board(space, row.project_id);
  }

  private async patchIssue(
    space: string,
    reff: string,
    patch: IssuePatch,
    predictions: readonly (readonly [Field, PredictionValue])[],
  ): Promise<boolean> {
    const row = this.selectRow(space, reff);
    if (!row) return false;
    const pending = this.beginChangeSet(
      space,
      row.doc_id,
      this.boardResourceFor(space, row),
      [{ op: "issue_patch", issue: reff, ...patch }],
      (operation) => {
        for (const [field, value] of predictions) {
          this.overlay.set(row.doc_id, field, value, operation);
        }
        this.notifyRows(space, [row.doc_id]);
      },
    );
    const feedback = await pending.completion;
    if (feedback.phase === "rolled_back") {
      throw new LaitError(
        feedback.error?.message ?? "the issue change was refused",
        400,
        feedback.error?.kind ?? "error",
      );
    }
    return true;
  }

  /**
   * Start one product-wide durable operation with frame-one feedback.
   *
   * The id is supplied to ChangeSet and becomes the signed Runtime RequestId.
   * `accepted` is published only after the RPC returns that durable receipt;
   * the matching exact publication must then be rendered before `committed`.
   */
  beginBoardChange(
    space: string,
    project: string | null,
    doc: string,
    reff: string,
    status: string | null,
    pos: BoardPos | null,
  ): PendingOperation {
    const resource = projectKeys.board(space, project);
    const position = pos == null
      ? null
      : pos.at === "before" || pos.at === "after"
        ? { at: pos.at, issue: pos.reff }
        : { at: pos.at };
    return this.beginChangeSet(space, doc, resource, [{
      op: "issue_board",
      issue: reff,
      ...(status !== null ? { status } : {}),
      ...(position !== null ? { position } : {}),
    }], (operation) => {
      if (status !== null) this.overlay.set(doc, "status", status, operation);
      if (pos !== null) this.boardMoves.set(doc, { resource, pos, operation });
      this.notifyRows(space, [doc]);
    });
  }

  private beginChangeSet(
    space: string,
    doc: string,
    resource: string,
    operations: IssuesChangeOperation[],
    apply: (operation: string) => void,
    resolveTarget?: (
      response: Extract<Response, { kind: "change_set" }>,
    ) => { doc: string; resource: string } | null,
    rollback?: () => void,
  ): PendingOperation {
    const operation = this.mintOperation().toLowerCase();
    const timestamp = Math.floor(Date.now() / 1_000);
    const sending: OperationFeedback = {
      operation,
      phase: "sending",
      space,
      doc,
      resource,
      timestamp,
      operations,
    };
    this.publishOperation(sending);
    if (rollback) this.operationRollbacks.set(operation, rollback);
    apply(operation);
    const completion = this.rpc(space, {
      cmd: "change_set",
      operation,
      timestamp,
      operations,
    }).then((response) => {
      const receipt: OperationReceipt | undefined = response.receipt;
      if (
        response.kind !== "change_set"
        || !receipt
        || receipt.phase !== "accepted"
        || receipt.operation !== operation
      ) {
        const indeterminate: OperationFeedback = {
          ...sending,
          phase: "indeterminate",
          error: {
            kind: "receipt_mismatch",
            message: "the durable operation receipt did not match the submitted operation",
          },
        };
        this.publishOperation(indeterminate);
        return indeterminate;
      }
      const accepted: OperationFeedback = {
        ...sending,
        ...(resolveTarget?.(response) ?? {}),
        phase: "accepted",
        publication: receipt.publication,
        results: response.results,
      };
      this.publishOperation(accepted);
      // A doorbell may arrive before the RPC continuation publishes Accepted.
      // Re-enter exact target hydration here as well as from handleDoorbell so
      // either delivery order reaches the same terminal phase.
      void this.hydrateOperationTarget(accepted).catch(() => undefined);
      const loaded = this.boardPublications.get(resource);
      if (loaded && worldPublicationKey(loaded) === worldPublicationKey(receipt.publication)) {
        this.reconcileOperations(resource, loaded, true);
        return this.operation(space, operation) ?? accepted;
      }
      return accepted;
    }).catch((error: unknown) => {
      const message = error instanceof Error ? error.message : String(error);
      const kind = error instanceof LaitError ? (error.errorKind ?? "error") : "transport";
      // OutcomeUnknown is neither a refusal nor a license to replay. Keep the
      // optimistic view until exact receipt reconciliation resolves it.
      const phase: OperationPhase = kind === "indeterminate" ? "indeterminate" : "rolled_back";
      const terminal: OperationFeedback = {
        ...sending,
        phase,
        error: { kind, message },
      };
      this.publishOperation(terminal);
      if (phase === "rolled_back") {
        this.rollbackOperation(terminal);
      } else {
        this.scheduleOperationStatus(terminal);
      }
      return terminal;
    });
    return { operation, completion };
  }

  private rollbackOperation(operation: OperationFeedback): void {
    this.overlay.clearOperation(operation.doc, operation.operation);
    if (this.boardMoves.get(operation.doc)?.operation === operation.operation) {
      this.boardMoves.delete(operation.doc);
    }
    this.operationRollbacks.get(operation.operation)?.();
    this.operationRollbacks.delete(operation.operation);
    const poll = this.operationPolls.get(operation.operation);
    if (poll !== undefined) clearTimeout(poll);
    this.operationPolls.delete(operation.operation);
    this.notifyRows(operation.space, [operation.doc]);
  }

  private scheduleOperationStatus(operation: OperationFeedback, delay = 1_000): void {
    if (this.operationPolls.has(operation.operation)) return;
    const timer = setTimeout(() => {
      this.operationPolls.delete(operation.operation);
      void this.refreshOperationStatus(operation.space, operation.operation).then((next) => {
        if (next?.phase === "indeterminate") {
          this.scheduleOperationStatus(next, Math.min(delay * 2, 30_000));
        }
      }).catch(() => this.scheduleOperationStatus(operation, Math.min(delay * 2, 30_000)));
    }, delay);
    this.operationPolls.set(operation.operation, timer);
  }

  async refreshOperationStatus(
    space: string,
    operationId: string,
  ): Promise<OperationFeedback | null> {
    const current = this.operation(space, operationId);
    if (!current || current.phase === "committed" || current.phase === "rolled_back") return current;
    const response = await this.rpc(space, {
      cmd: "operation_status",
      operation: current.operation,
      timestamp: current.timestamp,
      operations: [...current.operations],
    });
    if (response.kind !== "operation_status" || response.operation !== current.operation) {
      return current;
    }
    if (response.readiness === "absent") {
      const rolledBack: OperationFeedback = {
        ...current,
        phase: "rolled_back",
        error: {
          kind: "operation_absent",
          message: "no durable receipt exists for this operation",
        },
      };
      this.publishOperation(rolledBack);
      this.rollbackOperation(rolledBack);
      return rolledBack;
    }
    if (response.readiness !== "ready" || !response.publication) {
      const pending: OperationFeedback = {
        ...current,
        phase: "indeterminate",
        results: response.results,
        error: {
          kind: response.readiness,
          message: "the durable receipt exists; its exact publication is not locally ready",
        },
      };
      this.publishOperation(pending);
      return pending;
    }
    const issue = response.results.find((result) => result.kind === "issue");
    const { error: _previousError, ...withoutError } = current;
    const accepted: OperationFeedback = {
      ...withoutError,
      ...(current.doc.startsWith("local:issue-create") && issue
        ? { doc: issue.id, resource: projectKeys.issue(space, issue.id) }
        : {}),
      phase: "accepted",
      publication: response.publication,
      results: response.results,
    };
    this.publishOperation(accepted);
    this.observedOperations.set(accepted.operation, response.publication);
    await this.hydrateOperationTarget(accepted);
    return this.operation(space, operationId) ?? accepted;
  }

  async handleDoorbell(doorbell: SpaceDoorbell): Promise<void> {
    const space = doorbell.space;
    const scope = prefix(space);
    const attribution = doorbell.change?.attribution;
    const operationPublication = doorbell.publications?.find((entry) => entry.world === ISSUES_WORLD);
    if (attribution && operationPublication) {
      this.observedOperations.set(bytesHex(attribution.operation), operationPublication.publication);
    }
    if (doorbell.reset) {
      for (const doc of this.overlay.docs()) {
        const held = [...this.operations.values()].some((operation) =>
          operation.space === space
          && operation.doc === doc
          && (operation.phase === "sending"
            || operation.phase === "accepted"
            || operation.phase === "indeterminate"));
        if (!held) this.overlay.clearDoc(doc);
      }
      const keys = this.resources.reset((key) =>
        key.startsWith(scope) && !key.startsWith(`${scope}operation:`));
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
    for (const key of boards) {
      const publication = this.boardPublications.get(key);
      if (publication) this.reconcileOperations(key, publication);
    }
    for (const doc of dirty) {
      const held = [...this.operations.values()].some((operation) =>
        operation.space === space
        && operation.doc === doc
        && (operation.phase === "sending"
          || operation.phase === "accepted"
          || operation.phase === "indeterminate"));
      if (!held) this.overlay.clearDoc(doc);
    }
    this.notifyRows(space, dirty);
    for (const key of boards) this.resources.notify(key);
    await this.refreshActive(stale.filter((key) => !boards.includes(key)));
    if (attribution && operationPublication) {
      const operation = this.operation(space, bytesHex(attribution.operation));
      if (operation?.phase === "accepted") {
        await this.hydrateOperationTarget(operation).catch(() => undefined);
      }
    }
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
    this.ingestRow(space, {
      ...(existing ?? {
        reff: issue.reff,
        doc_id: issue.doc_id,
        project_id: issue.project_id,
        key_alias: issue.key_alias,
        assignee_summary: issue.assignees.length === 0
          ? ""
          : `${issue.assignees.length} assigned`,
        tombstone: false,
      }),
      title: issue.title,
      status: issue.status,
      priority: issue.priority,
      assignees: issue.assignees,
      ...(issue.due_date !== undefined ? { due_date: issue.due_date } : {}),
      ...(issue.estimate !== undefined ? { estimate: issue.estimate } : {}),
      label_names: issue.label_names,
      ...(issue.milestone !== undefined ? { milestone: issue.milestone } : {}),
      provisional: issue.provisional,
    });
  }

  private ingestRow(space: string, row: Row): void {
    const rows = this.rowsByDoc.get(space) ?? new Map<string, Row>();
    rows.set(row.doc_id, row);
    this.rowsByDoc.set(space, rows);
    this.resources.set(projectKeys.row(space, row.reff), row);
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
    return this.resources.ensure(key, loader, { force }).then((value) => {
      if (this.pageContinuations.has(key)) this.resources.set(key, value, false);
      return value;
    });
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

export function useLatestOperation(space: string | null): ResourceSnapshot<OperationFeedback> {
  const key = space ? projectKeys.latestOperation(space) : "space:_/operation:latest";
  return useWorldResource<OperationFeedback>(key, undefined);
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
    () => ({
      resource,
      board: space ? store.selectBoard(space, project) : null,
      nextCursor: store.pageContinuation(key)?.cursor ?? null,
      loadMore: () => space ? store.loadMoreBoard(space, project) : Promise.resolve(),
    }),
    [project, resource, space, store],
  );
}

/**
 * The space's teams, live.
 *
 * One resource for the whole space rather than one per team: the list is short,
 * it is read by the sidebar on every render, and a team's own membership
 * changes on the same plane as every other team's.
 */
export function useTeams(space: string | null): PagedResourceSnapshot<TeamDto> {
  const store = useProjectViewerStore();
  const key = space ? projectKeys.teams(space) : "project:none/teams";
  const resource = useWorldResource<TeamDto[]>(
    key,
    useCallback(
      () => (space ? store.ensureTeams(space) : Promise.resolve([])),
      [space, store],
    ),
  );
  return pagedResource(resource, key, store, () => space ? store.loadMoreTeams(space) : Promise.resolve());
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
/**
 * A project's compiled morphology.
 *
 * Two surfaces read this and they differ only in the seed. A Plan passes its
 * roots and, on a historical revision, the generation it was composed against;
 * the project's dependency view passes no roots at all, which the engine reads
 * as the whole project at the head. It was called `usePlanGeometry` while the
 * Plan was its only caller — the name was about who asked rather than what it
 * answers, and it stopped being true the moment a second surface asked.
 */
export function useGeometry(
  space: string,
  projectId: string | null | undefined,
  roots: readonly string[],
  publication?: PublicationId | null,
): ResourceSnapshot<GeometryView> {
  const store = useProjectViewerStore();
  const signature = [...new Set(roots)].sort().join(",");
  const canonicalRoots = useMemo(() => signature ? signature.split(",") : [], [signature]);
  return useWorldResource<GeometryView>(
    projectKeys.geometry(space, projectId ?? "_unknown", canonicalRoots, publication),
    useCallback(
      () => projectId
        ? store.ensureGeometry(space, projectId, canonicalRoots, publication)
        : Promise.resolve({
            key: {
              source: {
                publication: {
                  manifest_root: [],
                  implementation_digest: [],
                  extractor_schema_digest: [],
                },
                materialization: 0,
              },
              projection_schema: [],
              selection: [],
            },
            source: {
              publication: {
                manifest_root: [],
                implementation_digest: [],
                extractor_schema_digest: [],
              },
              materialization: 0,
            },
            estimate: {
              selected_nodes: 0,
              selected_edges: 0,
              reduction_candidates: 0,
              node_visits: 0,
              edge_visits: 0,
              reachability_visits: 0,
              working_bytes: 0,
            },
            readiness: { state: "ready" as const },
            summary: {
              schema_version: 0,
              project: "",
              roots: 0,
              nodes: 0,
              edges: 0,
              components: 0,
              regions: 0,
              residuals: 0,
              closure: { total: 0, closed: 0, ready: 0, blocked: 0, cyclic: 0, stalled: 0 },
              retained_bytes: 0,
            },
          }),
      [canonicalRoots, projectId, publication, space, store],
    ),
  );
}

/**
 * Fold independently observed resources into the keyed shape a workspace
 * surface consumes. Partial data stays usable while another project loads or
 * fails, but every member remains a first-class active resource underneath.
 */
function combineResources<T>(
  key: string,
  ids: readonly string[],
  snapshots: readonly ResourceSnapshot<T>[],
): ResourceSnapshot<Record<string, T>> {
  const data: Record<string, T> = {};
  let hasData = ids.length === 0;
  for (let i = 0; i < ids.length; i += 1) {
    const value = snapshots[i]?.data;
    if (value === undefined) continue;
    data[ids[i]!] = value;
    hasData = true;
  }
  const error = snapshots.find((snapshot) => snapshot.error !== null)?.error ?? null;
  const state: ResourceState = snapshots.every((snapshot) => snapshot.state === "ready")
    ? "ready"
    : snapshots.some((snapshot) => snapshot.state === "refreshing")
      ? "refreshing"
      : error !== null
        ? "error"
        : hasData
          ? "partial"
          : "cold";
  return Object.freeze({
    key,
    state,
    data: hasData ? data : undefined,
    error,
    stale: snapshots.some((snapshot) => snapshot.stale),
  });
}

/**
 * Every project's board at once, for the workspace sequence chart.
 *
 * Pass an empty list to park: the workspace chart is the only caller and every
 * other surface would be paying N requests for rows it does not draw.
 */
export function useSpaceBoards(
  space: string,
  projects: readonly string[],
): ResourceSnapshot<Record<string, BoardView>> {
  const store = useProjectViewerStore();
  const signature = [...projects].sort().join(",");
  const ids = useMemo(() => (signature ? signature.split(",") : []), [signature]);
  const keys = useMemo(() => ids.map((project) => projectKeys.board(space, project)), [ids, space]);
  const snapshots = useWorldResources<BoardView>(
    keys,
    useCallback(
      (_key: string, index: number) => store.ensureBoard(space, ids[index]!),
      [ids, space, store],
    ),
  );
  return useMemo(
    () => combineResources(`${prefix(space)}spaceboards:${signature}`, ids, snapshots),
    [ids, signature, snapshots, space],
  );
}

/** Every project's milestones at once, for the workspace roadmap. */
export function useSpaceMilestones(
  space: string,
  projectIds: readonly string[],
): ResourceSnapshot<Record<string, MilestoneDto[]>> {
  const store = useProjectViewerStore();
  const signature = [...projectIds].sort().join(",");
  const ids = useMemo(() => (signature ? signature.split(",") : []), [signature]);
  const keys = useMemo(
    () => ids.map((project) => projectKeys.milestones(space, project)),
    [ids, space],
  );
  const snapshots = useWorldResources<MilestoneDto[]>(
    keys,
    useCallback(
      (_key: string, index: number) => store.ensureMilestones(space, ids[index]!),
      [ids, space, store],
    ),
  );
  return useMemo(
    () => combineResources(`${prefix(space)}spacemilestones:${signature}`, ids, snapshots),
    [ids, signature, snapshots, space],
  );
}

export function useProjectMilestones(
  space: string,
  projectId: string | null | undefined,
): PagedResourceSnapshot<MilestoneDto> {
  const store = useProjectViewerStore();
  const key = projectKeys.milestones(space, projectId ?? "_unknown");
  const resource = useWorldResource<MilestoneDto[]>(
    key,
    useCallback(
      () => (projectId ? store.ensureMilestones(space, projectId) : Promise.resolve([])),
      [projectId, space, store],
    ),
  );
  return pagedResource(
    resource,
    key,
    store,
    () => projectId ? store.loadMoreMilestones(space, projectId) : Promise.resolve(),
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
): PagedResourceSnapshot<SpecSummary> {
  const store = useProjectViewerStore();
  const key = projectKeys.specs(space, project);
  const resource = useWorldResource<SpecSummary[]>(
    key,
    useCallback(() => store.ensureSpecs(space, project), [project, space, store]),
  );
  return pagedResource(resource, key, store, () => store.loadMoreSpecs(space, project));
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
): PagedResourceSnapshot<BaselineSummary> {
  const store = useProjectViewerStore();
  const key = projectKeys.baselines(space, project);
  const resource = useWorldResource<BaselineSummary[]>(
    key,
    useCallback(() => store.ensureBaselines(space, project), [project, space, store]),
  );
  return pagedResource(resource, key, store, () => store.loadMoreBaselines(space, project));
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
): PagedResourceSnapshot<BaselineRevisionDto> {
  const store = useProjectViewerStore();
  const load = useCallback(
    () => store.ensureBaselineHistory(space, baseline ?? ""),
    [space, baseline, store],
  );
  const key = projectKeys.baselineHistory(space, baseline ?? "_none");
  const resource = useWorldResource<BaselineRevisionDto[]>(
    key,
    baseline ? load : undefined,
  );
  return pagedResource(
    resource,
    key,
    store,
    () => baseline ? store.loadMoreBaselineHistory(space, baseline) : Promise.resolve(),
  );
}

/** Every typed link in scope, live — the incoming half of the graph, and what
 *  coverage is computed from. */
export function useSpecReferences(
  space: string,
  project: string | null,
): PagedResourceSnapshot<SpecReferenceFact> {
  const store = useProjectViewerStore();
  const key = projectKeys.specReferences(space, project);
  const resource = useWorldResource<SpecReferenceFact[]>(
    key,
    useCallback(() => store.ensureSpecReferences(space, project), [project, space, store]),
  );
  return pagedResource(resource, key, store, () => store.loadMoreSpecReferences(space, project));
}

/** Every note filed in scope, live. Both directions — an Observation names a
 *  subject and a target, and either end may be the document being read. */
export function useSpecObservations(
  space: string,
  project: string | null,
): PagedResourceSnapshot<SpecObservationRecord> {
  const store = useProjectViewerStore();
  const key = projectKeys.specObservations(space, project);
  const resource = useWorldResource<SpecObservationRecord[]>(
    key,
    useCallback(() => store.ensureSpecObservations(space, project), [project, space, store]),
  );
  return pagedResource(resource, key, store, () => store.loadMoreSpecObservations(space, project));
}

/** One Spec's whole revision DAG, live. */
export function useSpecHistory(
  space: string,
  spec: string | null,
): PagedResourceSnapshot<SpecRevision> {
  const store = useProjectViewerStore();
  const load = useCallback(
    () => store.ensureSpecHistory(space, spec ?? ""),
    [space, spec, store],
  );
  const key = projectKeys.specHistory(space, spec ?? "_none");
  const resource = useWorldResource<SpecRevision[]>(
    key,
    spec ? load : undefined,
  );
  return pagedResource(
    resource,
    key,
    store,
    () => spec ? store.loadMoreSpecHistory(space, spec) : Promise.resolve(),
  );
}

/** What is in force for one Issue, live. */
export function usePacket(space: string, reff: string): ResourceSnapshot<Packet> {
  const store = useProjectViewerStore();
  return useWorldResource<Packet>(
    projectKeys.packet(space, reff),
    useCallback(() => store.ensurePacket(space, reff), [reff, space, store]),
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
): PagedResourceSnapshot<ProjectUpdateDto> {
  const store = useProjectViewerStore();
  const key = projectKeys.updates(space, projectId ?? "_unknown");
  const resource = useWorldResource<ProjectUpdateDto[]>(
    key,
    useCallback(
      () => (projectId ? store.ensureUpdates(space, projectId) : Promise.resolve([])),
      [projectId, space, store],
    ),
  );
  return pagedResource(
    resource,
    key,
    store,
    () => projectId ? store.loadMoreUpdates(space, projectId) : Promise.resolve(),
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
  const history = useWorldResource<Page<ActivityEvent>>(
    projectKeys.history(space, reff),
    useCallback(() => store.ensureHistory(space, reff), [reff, space, store]),
  );
  const projectId = body.data?.project_id ?? row.data?.project_id;
  // The detail rail's milestone picker and the project overview's milestone list
  // are the same resource under the same key — one fetch, one invalidation, and
  // the two surfaces can never disagree about a milestone's progress.
  //
  // Its snapshot is a DEPENDENCY, not just a side effect, and leaving it out was
  // a real defect rather than a tidiness point. `selectIssueDetail` reads the
  // milestone resource and caches on it, but this memo decides whether that
  // function runs at all — so when the list landed, nothing in the old list
  // changed identity, the memo handed back the snapshot it had built *before*
  // the fetch resolved, and the rail kept the empty array forever. The picker's
  // face falls back to the id when the name is not in the list, so the rail
  // showed `mls_01jvf099ajqn6mhug6m8cl6291` where it meant "Launch" — and kept
  // showing it, because nothing was ever going to invalidate the memo again.
  const milestones = useProjectMilestones(space, projectId);
  return useMemo(
    () => store.selectIssueDetail(space, reff),
    // The resource objects are immutable change tokens.
    [body, graph, history, milestones, reff, row, space, store, projectId],
  );
}
