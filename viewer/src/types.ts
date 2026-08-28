/**
 * TypeScript mirrors of the engine's Layer-B contract — `src/dto.rs` (the DTOs)
 * and `src/control.rs` (the `Request`/`Response` envelopes).
 *
 * These are hand-maintained on purpose, mirroring a projection that is itself
 * hand-maintained: "not an automatic dump of the Loro layout — a storage refactor
 * must not break these" (dto.rs). Drift here is silent, so two rules:
 *
 * - **Timestamps are unix SECONDS**, not millis. The previous viewer passed
 *   `created_at` straight to `new Date(ms)` and rendered every issue as 1970.
 *   Use `tsToDate` and never construct a Date from these directly.
 * - **`SCHEMA_VERSION` does not move for renames.** It is still 1 across the
 *   `StatusInfo.room` → `name` rename that broke the old viewer's sidebar, so a
 *   version check would not have caught it. Only reading dto.rs catches it.
 */

/** dto.rs `SCHEMA_VERSION` — every top-level DTO carries it. */
export const SCHEMA_VERSION = 3;

/** Unix seconds → Date. The one place the units are converted. */
export const tsToDate = (ts: number): Date => new Date(ts * 1000);

// ---- plain domain enums -----------------------------------------------------

export type Priority = "none" | "low" | "medium" | "high" | "urgent";
export type StatusCategory = "backlog" | "active" | "done";

/** Priority order, low → high. Mirrors the Rust enum's `Ord`. */
export const PRIORITY_ORDER: readonly Priority[] = ["none", "low", "medium", "high", "urgent"];

/**
 * The words a menu uses for a priority.
 *
 * `none` is a real engine value, not an absence, and three surfaces had three
 * answers for what to call it in a list: the composer and the issue rail both
 * offered the literal `none`, and the row chip offered "No priority". A menu
 * row is the reader's, not the protocol's — so the map is here, beside the
 * order it is read in, and every picker projects it.
 *
 * Sentence case, because the *trigger* capitalises the same value: a menu that
 * offers "low" and a chip that then says "Low" are describing one thing in two
 * voices.
 */
export const PRIORITY_LABEL: Record<Priority, string> = {
  none: "No priority",
  low: "Low",
  medium: "Medium",
  high: "High",
  urgent: "Urgent",
};

/** Board badge: `·U/H/M/L·`. */
export const PRIORITY_BADGE: Record<Priority, string> = {
  none: "-",
  low: "L",
  medium: "M",
  high: "H",
  urgent: "U",
};

export interface WorkflowState {
  id: string;
  name: string;
  category: StatusCategory;
  color: string;
}

// ---- projections ------------------------------------------------------------

export interface ProjectDto {
  id: string;
  name: string;
  key: string;
  color: string;
  /** Overview markdown (absent/empty = none). */
  description?: string;
  /** Lead actor key (absent/empty = none). */
  lead?: string;
  /** Planned window, unix seconds. */
  start_date?: number | null;
  target_date?: number | null;
  /** Soft-hidden from pickers and all-project lists (still openable directly). */
  archived?: boolean;
  /**
   * The `tm_` team that owns this project (absent/empty = none).
   *
   * The id, not the key — a team rename must not orphan a project, for the same
   * reason `Row.milestone` holds an `mls_`.
   */
  team?: string;
}

/**
 * A team: a named slice of the space that owns some of its projects.
 *
 * The engine has had this since GOV-7 and the viewer never read it. `projects`
 * is a back-reference the projection maintains, so a sidebar can group without
 * joining anything — but `ProjectDto.team` is the authority, and the two are
 * kept in step by the same write.
 */
export interface TeamDto {
  id: string;
  name: string;
  key: string;
  /** Icon name (absent/empty = none). Unused by this client. */
  icon?: string;
  /** Lead actor key (absent/empty = none). */
  lead?: string;
  members: string[];
  /** KEYs — not ids — of the projects this team owns. */
  projects: string[];
}

export interface LabelDto {
  id: string;
  name: string;
  color: string;
}

/**
 * One board/list row — the `DocMeta` cache, never the issue doc.
 * `provisional` means the row is known but its body hasn't arrived (UI.md §9).
 */
export interface Row {
  reff: string;
  doc_id: string;
  project_id: string;
  key_alias: string | null;
  title: string;
  status: string;
  priority: Priority;
  /** Viewer-relative one-liner (`you +2`) — the shape a terminal row prints. */
  assignee_summary: string;
  /** The keys behind that summary, for clients that draw faces instead. */
  assignees: string[];
  /** Authored Card names for assignees, when the book could name them. */
  assignee_names?: Record<string, string>;
  tombstone: boolean;
  provisional: boolean;
  /** Due date, unix seconds. Absent = none. */
  due_date?: number | null;
  /** Estimate points (the scale is the team's convention). */
  estimate?: number | null;
  /** Resolved label names (absent/empty = none). */
  label_names?: string[];
  /** The `mls_` milestone this issue targets (absent = none). The id, not the
   *  name — a rename must not move a filter. */
  milestone?: string | null;
  /** Sub-issue progress: done / total live children. Absent = no children.
   *  Populated by the board projection only. */
  child_done?: number | null;
  child_total?: number | null;
}

export interface BoardColumn {
  state: WorkflowState;
  rows: Row[];
}

export interface BoardView {
  schema_version: number;
  project: ProjectDto;
  columns: BoardColumn[];
  /**
   * How many rows match, as the engine counted them — not how many are loaded.
   *
   * `null` is *unmeasured*, never zero. The engine returns `exact_total: None`
   * when a page was post-filtered after its source posting was counted, and a
   * client that read that absence as a number would be inventing one. This is
   * the same distinction `Row.enrichment_complete` draws one level down.
   *
   * It existed on the wire all along and `boardView` dropped it, so every
   * surface counted the rows it happened to hold and called that the total.
   * That is how a filter came to report "3 of 100" about a project holding
   * five hundred Issues.
   */
  total: number | null;
  /** Whether every matching row is loaded. False while a continuation remains,
   *  which is what makes a count over these rows a count of a page. */
  complete: boolean;
}

/** One bounded board page; clients group rows through the pinned workflow. */
export interface BoardPage {
  schema_version: number;
  project: ProjectDto;
  workflow: WorkflowState[];
  rows: Page<Row>;
}

export interface CommentDto {
  author: string;
  author_nick: string | null;
  /** Unix seconds. */
  ts: number;
  body: string;
  /** Canonical comment id (`cmt_…`). Absent on comments stored before comment
   *  identity existed — those cannot anchor reactions or replies. */
  id?: string | null;
  /** The comment this one replies to (one level of nesting). */
  parent?: string | null;
  /** Emoji reactions, grouped per emoji with the actors who reacted. */
  reactions?: ReactionDto[];
  /** The span of the description this comment marks, if it marks one.
   *
   *  Absent is the ordinary case and most comments. Present is resolved by the
   *  engine on every read — never stored resolved, because a stored position is
   *  a number that was right once. */
  anchor?: CommentAnchorDto | null;
}

/** Where a comment is attached, and what became of that place. */
export interface CommentAnchorDto {
  /** The collaborative field the span lies in. `description` today. */
  field: string;
  state: CommentAnchorState;
}

/** The three answers, and they are three different facts.
 *
 *  `at` is a position. `drifted` says the material this marked is gone — the
 *  comment is still shown, because somebody wrote it and the text moving out
 *  from under it does not unwrite it. `unresolved` says nobody worked out where
 *  it is. Neither of the last two is a position and neither may render as one:
 *  a stale offset drawn as a number is a highlight over the wrong words. */
export type CommentAnchorState =
  | { kind: "at"; start: number; end: number }
  | { kind: "drifted" }
  | { kind: "unresolved" };

export interface ReactionDto {
  emoji: string;
  actors: string[];
}

export interface ReactionRecord {
  issue: string;
  comment: string;
  emoji: string;
  actor: string;
  on: boolean;
}

export interface CheckDto {
  run: string;
  spec: string;
  version: number;
  build: string;
  source: string;
  state: string;
  by: string;
  ts: number;
  attempt?: string | null;
  report?: string | null;
  verdict?: string | null;
}

export interface IssueView {
  schema_version: number;
  reff: string;
  doc_id: string;
  space_id: string;
  project_id: string;
  project_key: string | null;
  key_alias: string | null;
  title: string;
  description: string;
  /** Hidden Lait document-model version. Zero/absent is a legacy issue body. */
  document_schema?: number;
  status: string;
  priority: Priority;
  assignees: string[];
  /** Authored Card names for assignees, when the book could name them. */
  assignee_names?: Record<string, string>;
  labels: string[];
  label_names: string[];
  comments: CommentDto[];
  created_by: string;
  /** Authored Card name for the creator, when the book could name them. */
  created_by_name?: string;
  /** Unix seconds. */
  created_at: number;
  /** Due date, unix seconds. Absent = none. */
  due_date?: number | null;
  estimate?: number | null;
  /** Subscribed actors, independent of assignment (INBOX-9). */
  followers?: string[];
  /** Targeted milestone id (SCOPE-1). */
  milestone?: string | null;
  /** Scheduled cycle id (BOARD-11). */
  cycle?: string | null;
  /** Exact issued Baseline pinned to this Issue. */
  baseline?: BaselineRef | null;
  /** Attachment metadata (CREATE-5). */
  attachments?: AttachmentMetaDto[];
  checks?: CheckDto[];
  /** Exact coordinate and independent continuations for bounded enrichment. */
  enrichment?: {
    publication: WorldPublicationId;
    /** Current record-addressed reaction facts already visited. Kept so a
     * later comment page can be enriched without replaying an earlier reaction
     * page or silently crossing publications. */
    reaction_records?: ReactionRecord[];
    comments?: string | null;
    reactions?: string | null;
    attachments?: string | null;
    checks?: string | null;
    outgoing_relations?: string | null;
    incoming_relations?: string | null;
  };
  provisional: boolean;
  /** Malformed stored records, kept beside the valid projection rather than
   * silently dropped or laundered into sentinel values. */
  corrupt_records?: CorruptRecord[];
}

export type SpecKind =
  | "goal" | "requirement" | "plan" | "design" | "order"
  | "guide" | "proof" | "verdict" | "waiver" | "record";
export type SpecState = "draft" | "review" | "issued" | "withdrawn";
export type SpecRel =
  | "derives" | "decomposes" | "implements" | "governs" | "amends"
  | "supersedes" | "clarifies" | "incorporates" | "references"
  | "verifies" | "validates" | "waives" | "records" | "conflicts" | "depends";
export type SpecTarget =
  | { kind: "spec"; spec: string; revision: string }
  | { kind: "baseline"; baseline: string; revision: string }
  | { kind: "issue"; issue: string };
export interface SpecLink { rel: SpecRel; target: SpecTarget }
export interface SpecRef { spec: string; revision: string }
export interface BaselineRef { baseline: string; revision: string }
export interface PlanData {
  /** Issue seeds only. Topology, order, closure, and gaps are derived. */
  roots: string[];
}
export interface SpecBody {
  spec: string; project: string; kind: SpecKind; title: string; text: string;
  state: SpecState; links: SpecLink[]; plan?: PlanData | null; author: string; ts: number;
  /** Exact portable World publication against which this immutable revision was composed. */
  publication: PublicationId;
}
export interface SpecView {
  spec: string; project: string; kind: SpecKind; title: string; state: SpecState;
  revision: string; heads: string[]; issued: string[]; body: SpecBody;
}
/** The one head of a register row, as its corpus row states it. */
export interface SpecHead {
  revision: string; title: string; state: SpecState; author: string; ts: number;
}
export interface SpecSummary {
  spec: string;
  project: string;
  kind: SpecKind;
  heads: string[];
  issued: string[];
  conflicted: boolean;
  /** Present only with exactly one head; absent until the corpus posts it. */
  head?: SpecHead | null;
}
export interface BaselineBody {
  baseline: string; project: string; name: string; state: SpecState;
  members: SpecRef[]; author: string; ts: number;
}
export interface BaselineView {
  baseline: string; project: string; name: string; state: SpecState;
  revision: string; heads: string[]; issued: string[]; body: BaselineBody;
}
export interface BaselineHead {
  revision: string; name: string; state: SpecState; author: string; ts: number;
}
export interface BaselineSummary {
  baseline: string;
  project: string;
  heads: string[];
  issued: string[];
  conflicted: boolean;
  head?: BaselineHead | null;
}

export type GeometryRole = "constraint" | "containment" | "equivalence" | "association";
export type GeometryClosureState = "closed" | "ready" | "blocked" | "cycle" | "stalled";
export interface GeometryFacet { kind: string; id: string; label: string }
export interface GeometryNode {
  row: Row;
  component: string;
  layer?: number | null;
  ordinal: number;
  hierarchy_depth: number;
  /** The containment block this issue belongs to, by `GeometryRegion.id`.
   *  Absent when it has no selected parent and no selected children. */
  region?: string | null;
  parent?: string | null;
  children: string[];
  blocked_by: string[];
  blocks: string[];
  closure: GeometryClosureState;
  slack?: number | null;
  facets: GeometryFacet[];
}
export interface GeometryEdge {
  from: string;
  relation: string;
  role: GeometryRole;
  to: string;
  /** A longer dependency path already asserts this constraint, so drawing it
   *  adds a line and no information. Only ever true of a `constraint` edge, and
   *  absent from an engine that predates the reduction. */
  implied?: boolean;
}
/**
 * One block of the containment partition, contracted to a single vertex.
 *
 * `layer` is the block's place in the *quotient* graph — an edge exists wherever
 * any member of one block blocks any member of another — so it is the plan's
 * phase order, computed by the same longest-path pass the issues get. `null`
 * when the contraction is cyclic, which is reachable even though the issue graph
 * is not: two blocks can block each other through different members.
 *
 * Only groupings of two or more appear, so this is empty for a project that has
 * never used sub-issues.
 */
export interface GeometryRegion {
  id: string;
  nodes: string[];
  layer?: number | null;
  ordinal: number;
}
export interface GeometryComponent {
  id: string;
  nodes: string[];
  roots: string[];
  terminals: string[];
  loops: string[][];
}
export interface GeometryResidual {
  kind: "root_missing" | "dependency_cycle" | "blocked_frontier" | "due_order_conflict" | "unattached" | "closure_frontier";
  component?: string | null;
  layer?: number | null;
  at: string[];
  requires: string[];
}
export interface GeometryClosure {
  total: number;
  closed: number;
  ready: number;
  blocked: number;
  cyclic: number;
  stalled: number;
}
export interface GeometryArtifactKey {
  source: WorldPublicationId;
  projection_schema: number[];
  selection: number[];
}
export interface GeometryEstimate {
  selected_nodes: number;
  selected_edges: number;
  reduction_candidates: number;
  node_visits: number;
  edge_visits: number;
  reachability_visits: number;
  working_bytes: number;
}
export type GeometryReadiness =
  | { state: "pending" }
  | { state: "ready" }
  | { state: "unavailable"; reason: unknown };
export interface GeometrySummary {
  schema_version: number;
  project: string;
  roots: number;
  nodes: number;
  edges: number;
  components: number;
  regions: number;
  residuals: number;
  closure: GeometryClosure;
  retained_bytes: number;
}
export type GeometrySection =
  | "Roots" | "Nodes" | "Edges" | "Components" | "Regions" | "Residuals"
  | { ComponentMembers: number }
  | { ComponentRoots: number }
  | { ComponentTerminals: number }
  | { RegionMembers: number }
  | { ResidualAt: number }
  | { ResidualRequires: number };
export interface GeometryCursor {
  artifact: GeometryArtifactKey;
  section: GeometrySection;
  offset: number;
}
export interface GeometryPageRequest {
  section: GeometrySection;
  limit: number;
  cursor?: GeometryCursor | null;
}
export type GeometryRows =
  | { Roots: unknown[] }
  | { Nodes: unknown[] }
  | { Edges: unknown[] }
  | { Components: unknown[] }
  | { Regions: unknown[] }
  | { Residuals: unknown[] }
  | { Members: unknown[] };
export interface GeometryPage {
  rows: GeometryRows;
  next?: GeometryCursor | null;
}
/** Publication-pinned geometry readiness/summary plus at most one bounded page. */
export interface GeometryView {
  key: GeometryArtifactKey;
  source: WorldPublicationId;
  estimate: GeometryEstimate;
  readiness: GeometryReadiness;
  summary?: GeometrySummary | null;
  page?: GeometryPage | null;
}
/**
 * One typed assertion seen from the far end — `spec.rs` `SpecReference`.
 *
 * Links live on the revision that asserts them, so incoming edges cannot be
 * derived from the target and a head-only scan loses any edge asserted by an
 * issued predecessor. `head`/`issued` describe the *asserting* revision, which
 * is what separates a current claim from one nobody stands behind any more.
 */
export interface SpecReference {
  spec: string;
  revision: string;
  kind: SpecKind;
  title: string;
  link: SpecLink;
  head: boolean;
  issued: boolean;
}
export type SpecReferenceFact = Omit<SpecReference, "head" | "issued">;

/**
 * One retractable note about the graph — `spec.rs` `Observation`.
 *
 * The inverse of a `SpecLink` on every axis that matters: it carries its own
 * observer rather than the document's author, it sits in no revision so issuing
 * the document neither adopts nor freezes it, and it never reaches a Packet or
 * counts as verification coverage. It is what somebody *noticed*, not what the
 * document *says*.
 */
export interface SpecObservation {
  observation: string;
  /** The Spec it is filed against — whose set it lives in. */
  spec: string;
  observer: string;
  /** Unix seconds. */
  ts: number;
  rel: SpecRel;
  target: SpecTarget;
  note: string;
}
export type SpecObservationRecord =
  | { kind: "assert"; project: string; observation: SpecObservation }
  | {
      kind: "retract";
      project: string;
      observation: string;
      spec: string;
      actor: string;
      timestamp: number;
    };

/** One immutable revision and the revisions it descends from. */
export interface SpecRevision {
  revision: string;
  predecessors: string[];
  body: SpecBody;
}
export interface BaselineRevisionDto {
  revision: string;
  predecessors: string[];
  body: BaselineBody;
}

/**
 * How an exact revision reached a Packet — `spec.rs` `PacketSource`.
 *
 * Typed rather than prose because the reader has to act on the difference: an
 * incorporated Guide lands in the governing set, and the only thing that keeps
 * it from reading as an order is being able to say what pulled it in.
 */
export type PacketSource =
  | { route: "baseline"; baseline: string }
  | { route: "direct" }
  | { route: "incorporated"; spec: string; revision: string };

/** Why a Packet is not whole — `spec.rs` `PacketConflict`. Distinct variants
 *  because "missing" waits for a sync and "not issued" waits for a person. */
export type PacketConflict =
  | { reason: "missing_baseline"; baseline: string }
  | { reason: "missing_baseline_revision"; baseline: string; revision: string }
  | { reason: "baseline_not_issued"; baseline: string; revision: string }
  | { reason: "missing_spec"; spec: string }
  | { reason: "missing_spec_revision"; spec: string; revision: string }
  | { reason: "issued_spec_conflict"; spec: string }
  | { reason: "missing_incorporated"; spec: string; revision: string };

export interface PacketSpec {
  spec: string; revision: string; kind: SpecKind; title: string; state: SpecState;
  source: PacketSource; links: SpecLink[];
}
export interface Packet {
  issue: string; baseline?: BaselineRef | null; governing: PacketSpec[];
  guidance: PacketSpec[]; proof: PacketSpec[]; record: PacketSpec[];
  conflicts: PacketConflict[];
}

/** Attachment metadata on an issue (CREATE-5) — payloads via `attachment_get`. */
export interface AttachmentMetaDto {
  id: string;
  name: string;
  mime?: string;
  size: number;
  by?: string;
  ts: number;
  comment?: string;
  /** The content id, for records written after the attachment cutover.
   *  Absent on a legacy record, whose bytes are inline in the Body. */
  content?: string;
}

/** One project milestone with derived progress (SCOPE-1). */
export interface MilestoneDto {
  id: string;
  name: string;
  /** Prose body (absent/empty = none). */
  description?: string;
  target_date?: number | null;
  total: number;
  done: number;
}

export interface CorruptRecord {
  locus: string;
  reason: string;
  raw?: Record<string, string>;
}

export interface FieldChange {
  field: string;
  from: string | null;
  to: string | null;
}

/** One edge in the issue graph — `dto.rs` `LinkDto`. */
export interface LinkDto {
  /** `blocks` | `relates` | `duplicates`. */
  kind: string;
  /** `out` | `in` — whether this issue is the source or the target of the edge. */
  direction: string;
  row: Row;
}
export type RelationDirection = "out" | "in";
export interface IssueRelationDto {
  kind: string;
  direction: RelationDirection;
  row: Row;
}

/** One bounded relation neighborhood at an exact World publication.
 * Parent/children and direct links are hydrated from independently continued
 * incoming/outgoing Corpus pages; the cursors make partialness explicit. */
export interface GraphView {
  schema_version: number;
  reff: string;
  doc_id: string;
  publication: WorldPublicationId;
  parent: Row | null;
  parent_conflicted?: boolean;
  children: Row[];
  links: LinkDto[];
  blocked_by: Row[];
  next_outgoing?: string | null;
  next_incoming?: string | null;
}

export interface ActivityEvent {
  /** Ordinal in the issue's whole history, trimmed rows counted. Display
   * only — to resume a feed, send `cursor` back as `since`. */
  seq: number;
  /** Opaque resume token naming this row. Empty for a row from a Body
   * written before the history log. */
  cursor?: string;
  doc_id: string | null;
  reff: string;
  kind: string;
  changes: FieldChange[];
  actor: string | null;
  actor_nick: string;
  /** Authored Card name, when the book could name `actor`. Absent is not empty. */
  authored_name?: string | null;
  text: string;
  /** Unix seconds. */
  ts: number;
  /** Non-blocking LWW collision note (A§9). */
  collision: boolean;
}

/** A ref that resolved to several issues (UI.md §2). A first-class outcome, not an error. */
export interface Candidate {
  reff: string;
  key_alias: string | null;
  title: string;
}

export interface InboxEntry {
  /** Unix seconds — the read-watermark axis. */
  ts: number;
  /** `assigned` | `comment` | `status`. */
  kind: string;
  reff: string;
  doc_id: string;
  title: string;
  detail: string;
  /** Comments only — the one in-doc field with a real author. `null` = actor unknown. */
  actor?: string | null;
  actor_nick?: string | null;
  /** Authored Card name, when the book could name `actor`. */
  authored_name?: string | null;
}

export interface MemberDto {
  key: string;
  /** "admin" | "member" | "viewer" — the coarse label of the ACL grant set. A
   * sponsored member (agent) is not a separate role; `sponsor` marks it. */
  role: string;
  /** The member's did:key — the self-certifying, synced-safe interop handle
   * (`z6Mk…`); a pure function of the key, unlike the local `alias`. */
  did?: string | null;
  me: boolean;
  /** Present for agents: the actor whose standing sponsors this identity. */
  sponsor?: string | null;
  /** Local petname; never synced. The trusted half of the identity model. */
  alias: string;
}

/**
 * One entry in the membership audit log — `dto.rs` `MemberLogEntry`.
 *
 * Unlike in-doc activity attribution (advisory, non-goal 6), `actor` here is
 * **verified**: the signature covers the op, so this is who really signed it.
 * `authorized` is the replay verdict — `false` means the op was rejected as
 * unauthorized or couldn't be decoded, which is a real thing to be able to see.
 */
export interface MemberLogEntry {
  /** The op's content-address (its signed-DAG node id). */
  op: string;
  /** The signing author's key — verified, not claimed. */
  actor: string;
  /** `add_member` | `remove_member` | `set_role` | `add_agent` | `unknown`. */
  kind: string;
  /** The key the op acts on. Absent for an undecodable op. */
  subject?: string | null;
  /** `admin` | `member`, for role-bearing ops. */
  role?: string | null;
  /** Whether replay honored the op (false = unauthorized or undecodable). */
  authorized: boolean;
}

/**
 * One effective scoped capability assignment — `dto.rs` `AssignmentDto`.
 *
 * A role grant (`access_grant`) expands the role's pinned definition into one of
 * these per capability, each with its own `grant_id` (the revocation handle).
 * `resource` empty = the Space; `[projectId]` = that project's scope.
 */
export interface AssignmentDto {
  grant_id: string;
  actor: string;
  world: string;
  capability: string;
  resource: string[];
  /** Why the assignment exists, when its grant op said. Absent means *not
   *  recorded* — a grant authored before origins were — and is not any kind
   *  in particular: never fold it into membership, never into "granted here". */
  origin?: AssignmentOrigin;
}

/** `founder`, `admission`, `membership` and `sponsorship` all mean "came with
 *  being a member" — the base role, expanded. `grant` is the one kind that is
 *  an extra on top of it, and the only one a surface may offer to revoke as
 *  such. */
export type AssignmentOriginKind = "founder" | "admission" | "membership" | "grant" | "sponsorship";

/** `mechanics::assignment::AssignmentOrigin`. */
export interface AssignmentOrigin {
  kind: AssignmentOriginKind;
  /** The opaque role reference (hex) the expansion came from, for the kinds
   *  that carry one. Only the owning World can read it. */
  definition_ref?: string;
  /** The role id the World resolved `definition_ref` to — filled in by the
   *  Issues product on its own rows, absent on another World's. */
  role?: string;
}


/** One project status update — `dto.rs` `ProjectUpdateDto` (SCOPE-1). */
export interface ProjectUpdateDto {
  id: string;
  /** Authoring actor key. */
  author: string;
  /** Post time, unix seconds. */
  ts: number;
  body: string;
  /** `on_track` | `at_risk` | `off_track` | "" (none). */
  health?: string;
}

/** A pinned seed ("remote") — a bootstrap + backfill anchor, never trust. */
export interface SeedDto {
  id: string;
  nick: string;
  space: string;
  state: string;
  online: boolean;
}

export interface PresenceEntry {
  id: string;
  nick: string;
  state: string;
  online: boolean;
  last_seen_secs: number;
}

/**
 * `control.rs` `LiveScope` — what a transient item is about.
 *
 * `body` is lowercase base32 and `content` is hex, because the wire is JSON and
 * the raw byte arrays would arrive as lists of numbers. The derivation from an
 * issue's doc id to its Body id runs one way only, so a `body` here cannot be
 * turned back into anything the viewer displays — which is why `live` takes an
 * `issue` and narrows server-side rather than making the client match ids.
 */
export type LiveScope =
  | { scope: "issue_view"; world: string; body: string }
  | { scope: "document_view"; world: string; body: string }
  | { scope: "text_caret"; world: string; body: string; field: string }
  | { scope: "text_preview"; world: string; body: string; field: string }
  | { scope: "typing"; world: string; body: string; field: string }
  | { scope: "content_residency"; content: string }
  | { scope: "custom_world"; world: string; schema: string; key: string };

/**
 * `control.rs` `CaretPosition`.
 *
 * `drifted` is an answer — the material this position was attached to is gone.
 * `unresolved` is the absence of one. Rendering them the same shows a live
 * caret as lost.
 */
export type CaretPosition =
  | { caret: "at"; position: number }
  | { caret: "drifted" }
  | { caret: "unresolved" };

export interface TextPreview {
  base: string;
  result: string;
  index: number;
  delete: number;
  insert: string;
  anchor: number | null;
  focus: number | null;
}

/**
 * `control.rs` `LiveEntry` — one thing a peer is doing right now.
 *
 * `actor` is an actor id, resolved by the daemon, and never a device id: it is
 * the same string space as `MemberDto.key`, so an avatar coloured from it
 * matches the same person everywhere else on the page. `PresenceEntry.id` is
 * the other thing and the two must not be mixed.
 */
export interface LiveEntry {
  actor: string;
  scope: LiveScope;
  kind: "presence" | "caret" | "selection" | "preview" | "typing" | "residency";
  /** How long ago the daemon saw it — its clock, not the peer's. */
  age_ms: number;
  /** Past the caret grace window: still shown, no longer known to be right. */
  uncertain: boolean;
  caret: CaretPosition | null;
  focus: CaretPosition | null;
  preview?: TextPreview | null;
}

/** `control.rs` `SignalBody` — what one delivered signal says. */
export type SignalBody =
  | { signal: "ping"; nonce: string }
  | { signal: "acknowledge"; nonce: string }
  | { signal: "attention"; scope: LiveScope }
  | { signal: "session_invite"; invite: string; scope: LiveScope }
  | {
      signal: "file_offer";
      content: string;
      plaintext_len: number;
      display_name: string;
      media_type: string;
    }
  | { signal: "world_signal"; world: string; schema: string; payload_b64: string };

/** `control.rs` `SignalEntry`. `actor` follows the same rule as `LiveEntry`. */
export interface SignalEntry {
  actor: string;
  /** 32-hex. Compared for equality, never ordered. */
  session_id: string;
  session_epoch: string;
  signal: SignalBody;
}

export interface Event {
  seq: number;
  kind: string;
  id: string;
  nick: string;
  text: string;
  ts: number;
}

/**
 * `control.rs` `StatusInfo`.
 *
 * `space` is nullable, and `membership` is how a still-unadmitted joiner
 * learns admission is in progress rather than staring at an empty board.
 */
export interface StatusInfo {
  id: string;
  nick: string;
  /** Space display name. (Was `room` in the pre-v0.4.2 shape.) */
  name: string;
  /** Space overview description (SCOPE-2; empty when unset). */
  description?: string;
  online_peers: number;
  space: string | null;
  items: number;
  scopes: number;
  /** True means zero counts are unavailable, not an empty space. */
  counts_unavailable?: boolean;
  /**
   * `true` when `name` is UNAVAILABLE rather than empty — the World is not
   * docked, so nobody read a name. Never render it, and never substitute one.
   */
  name_unavailable?: boolean;
  /** `admin` | `member` | `pending`. */
  membership: string;
  degraded_recovery?: DegradedHolder[];
  recovery?: RecoveryState | null;
}

/**
 * `dto.rs` `WhoamiDto` — identity + standing + view completeness, one shot.
 *
 * This is **standing** (what this node's actor may do), as distinct from
 * `isReadOnly`'s custody question (whose key this surface signs with). The
 * viewer uses it to gate write affordances honestly instead of letting every
 * one of them be discovered dead at RPC time.
 */
export interface WhoamiInfo {
  actor?: string | null;
  device: string;
  space?: string | null;
  /** `admin` | `member` | `viewer`, or `none` when not a member yet. */
  role: string;
  member: boolean;
  can_write: boolean;
  capabilities?: string[];
  policy_admin: boolean;
  /** Sponsoring actor id when this identity is a sponsored agent. */
  sponsor?: string | null;
  name?: string | null;
  partial_view: boolean;
  divergence?: string[];
  /** True when an unsponsored named agent has asked this machine to sponsor it. */
  sponsorship_asked?: boolean;
  /** True when this reading delivered a just-approved sponsorship. */
  sponsorship_granted?: boolean;
  /** Heads to pass to `wait` (Work Watch). */
  wait_heads?: string[];
}

export type RecoveryIoKind =
  | "not_found"
  | "interrupted"
  | "invalid_data"
  | "other"
  | "already_exists";

export type RecoveryArtifactCause =
  | { kind: "wrong_protector" }
  | { kind: "permission_denied" }
  | { kind: "corrupt" }
  | { kind: "io"; detail: RecoveryIoKind };

export interface DegradedHolder {
  transcript: string;
  reason: RecoveryArtifactCause;
  is_current_authority?: boolean | null;
}

export interface RecoveryState {
  authority: { public_key: string; configuration: number[] } | null;
  configuration: number[];
  generation: number;
  custody:
    | { state: "not_holder" }
    | { state: "ready" }
    | { state: "missing" }
    | { state: "backup_unverified" }
    | { state: "unreadable"; detail: RecoveryArtifactCause };
  backing: { holders: string[]; satisfies_configuration: boolean };
  availability:
    | { state: "unknown" }
    | { state: "observed"; holders: string[]; qualifies: boolean; enabling: string[] };
}

// ---- the Orbit directory projection (serve-level, not control-plane) --------

/** Whose key a placed Station signs with (`daemon::StationIdentity`). */
export type SpaceIdentity = { kind: "own" } | { kind: "agent"; name: string };

export interface ProjectBrief {
  key: string;
  name: string;
}

export interface SpaceRow {
  id: string;
  space: string;
  /**
   * The Catalog name, read from a live Station. `null` when it could not be
   * read — `unnamed` then says why. There is no remembered fallback: rendering
   * one is what made a renamed Space answer to its founding name.
   */
  name: string | null;
  /** Why `name` is null. Absent when a name was read. */
  unnamed?: "store-missing" | "not-probed" | "unreachable" | "not-docked";
  /**
   * What this device last saw the Space named, and when (unix seconds) --
   * written by the Station that held its store. A past reading beside the
   * live one, never in its place: draw it with its time.
   */
  seen?: { name: string; observed_at: number };
  path: string;
  origin: string;
  last_opened: number;
  /** `up` | `idle` | `missing` | `unknown` — where `unknown` is the absence of
   * a reading, never a fourth degree of down. */
  status: "up" | "idle" | "missing" | "unknown";
  identity: SpaceIdentity;
}

export interface SpacesReply {
  spaces: SpaceRow[];
  /**
   * The mount this head serves this World at.
   *
   * A head answers for exactly one World and refuses every other mount by name.
   * The mount is the World's own published API — `issues` — right up until the
   * host assigns one, which it does for a local World so a tree being worked on
   * can sit beside the release it was copied from. Then it is `local_issues`,
   * and nothing in the page can tell.
   *
   * Optional because a head older than this field simply does not say, and the
   * name the World publishes is the right thing to fall back to.
   */
  world?: string;
}

/**
 * Whether this surface may write to a space: no, when the write would be signed
 * with a key this node merely hosts.
 *
 * **Custody, not standing.** It is deliberately sourced from the identity the
 * Station signs with and not from the holder's grants, and it does not become
 * stale when a sponsored agent gains write standing — a write to an agent's
 * Station goes out over the *agent's* signature whatever anybody's grants say,
 * which is why the server refuses it (`serve::borrowed_key_refusal`). Re-sourcing
 * this from reported standing would make the viewer offer buttons the server is
 * required to refuse. Writing as yourself means opening that space through your
 * own node.
 */
export const isReadOnly = (s: SpaceRow): boolean => s.identity.kind === "agent";

// ---- the doorbell (delivery layer — world-opaque) --------------------------
//
// These types name no product and must not acquire one: the vocabulary in
// `kind` and `plane` is declared by whichever World rang, and this layer only
// carries it.

/**
 * What a dirty scope or plane belongs to — a container the World names, which
 * the delivery layer never interprets. `kind` and `id` are the World's own
 * vocabulary.
 *
 * Named twice on purpose: `id` is the stable identity a dependency matches on;
 * `label` is a mutable display alias a rename changes underneath you. Never
 * match on `label`.
 */
export interface ScopeRef {
  kind: string;
  id: string;
  label?: string | null;
}

/** Which docs moved, under which scope — the item plane of a doorbell. */
export interface DirtyScope extends ScopeRef {
  /** The DocIds whose rows must be re-read. Item-level invalidation. */
  docs: string[];
}

/**
 * A dirty structural plane. `plane` is a World-declared string the engine never
 * interprets; `scope` is present when the plane is grouped by a container, so
 * editing one project's milestones leaves another's alone.
 */
export interface DirtyPlane {
  plane: string;
  scope?: ScopeRef | null;
}

/** One product's invalidations, isolated under its stable World id. */
export interface RoutedInvalidation {
  world: string;
  dirty: DirtyScope[];
  planes: DirtyPlane[];
}

/** Authenticated authorship for one durable operation. */
export interface DurableAttribution {
  operation: number[];
  actor: string;
  device: string;
}

export type DurableMutation =
  | "Atomic"
  | "Register"
  | "Map"
  | "List"
  | "Text"
  | "Set"
  | "Counter"
  | "Lifecycle"
  | "Tree"
  | "Log";

export interface DurableStableTextRange {
  /**
   * Exact scalar range in the Observation's committed publication. Apply it
   * only when the current projection has the matching WorldPublicationId;
   * otherwise re-query and use the anchors only through the server.
   */
  start: number;
  end: number;
  /** Canonical Fabric anchors for server-side re-resolution after later edits. */
  start_anchor: number[];
  end_anchor: number[];
  deleted: number;
  inserted: number;
  inserted_bytes: number;
}

/** Value-free description of one operation in the committed Body batch. */
export interface DurablePathChange {
  mutation: DurableMutation;
  path?: string | null;
  locator?: string | null;
  text?: DurableStableTextRange | null;
}

export type DurableDetail = { Exact: DurablePathChange[] } | "Dirty";

export interface DurableBodyChange {
  body: { world: string; body: number[] };
  detail: DurableDetail;
}

/** Same durable feedback vocabulary for human, agent, CLI and API writes. */
export interface DurableChange {
  attribution?: DurableAttribution | null;
  bodies: DurableBodyChange[];
}

/** Portable semantic identity of one immutable World read generation. */
export interface PublicationId {
  manifest_root: number[];
  implementation_digest: number[];
  extractor_schema_digest: number[];
}

/** Browser-safe form accepted by the Issues app protocol. */
export interface PublicationCoordinate {
  manifest_root: string;
  implementation_digest: string;
  extractor_schema_digest: string;
}

/** Complete Station-local coordinate of one immutable World read image. */
export interface WorldPublicationId {
  publication: PublicationId;
  materialization: number;
}

/** Durable acknowledgement of the same signed Issues action observed later
 * through `SpaceDoorbell.change.attribution.operation`. */
export interface OperationReceipt {
  operation: string;
  phase: "accepted";
  publication: WorldPublicationId;
}

/** Opaque, exact-publication continuation requested by every unbounded list. */
export interface PageRequest {
  limit: number;
  cursor?: string | null;
}

/** Uniform bounded collection envelope from the Issues World. */
export interface Page<T> {
  publication: WorldPublicationId;
  items: T[];
  next_cursor?: string | null;
  exact_total?: number | null;
}

export interface RoleRevision {
  revision_id: string;
  predecessor_ids?: string[];
  body: {
    role_id?: string;
    name: string;
    description: string;
    scope_kind: string;
    scope_project?: string | null;
    capabilities: string[];
  };
}

export interface RoleProjection {
  summary: {
    role_id: string;
    built_in: boolean;
    revision?: string | null;
    /** Omitted by the runner when empty — read it as none, not as a crash. */
    conflict_heads?: string[];
  };
  revision?: RoleRevision | null;
}


export interface IssueDetailPages {
  comments: PageRequest;
  reactions: PageRequest;
  attachments: PageRequest;
  checks: PageRequest;
  outgoing_relations: PageRequest;
  incoming_relations: PageRequest;
}

export interface IssueDetailProjection {
  publication: WorldPublicationId;
  issue: IssueView;
  comments: Page<CommentDto>;
  reactions: Page<ReactionRecord>;
  attachments: Page<AttachmentMetaDto>;
  checks: Page<CheckDto>;
  outgoing_relations: Page<IssueRelationDto>;
  incoming_relations: Page<IssueRelationDto>;
}

/** One World whose exact read publication changed in this doorbell. */
export interface AffectedWorldPublication {
  world: string;
  publication: WorldPublicationId;
}

/**
 * A dirty-set frame, tagged with the space it rang for.
 *
 * Never state: the client re-reads the authoritative projection and never patches
 * from the frame (UI.md §5). `reset` — or an `epoch` change, which is a daemon
 * restart — means rebaseline from scratch; `App` treats them identically.
 *
 * `activity_advanced` and `presence_advanced` are carried faithfully but not yet
 * read: this client re-reads on any ring rather than per dirty scope. See
 * `doorbell.ts`.
 */
export interface SpaceDoorbell {
  space: string;
  epoch: number;
  seq: number;
  reset: boolean;
  invalidations: RoutedInvalidation[];
  /**
   * Sorted exact read coordinates for affected Worlds. Empty on reset and
   * authority-only rings. A missing or mismatched coordinate forbids applying
   * scalar offsets from `change`; re-query the authoritative projection.
   */
  publications?: AffectedWorldPublication[];
  /**
   * Exact paths/ranges when bounded, otherwise an explicit Body-dirty marker.
   * No product values ride this stream; authoritative state is fetched after
   * the publication notification.
   */
  change?: DurableChange;
  /**
   * Membership, roles, devices or keys advanced. Its own flag, not a catalog
   * plane: authority is not in the catalog and can move with no Body touched.
   */
  authority_advanced: boolean;
  activity_advanced: boolean;
  presence_advanced: boolean;
}

// ---- the control-plane envelopes -------------------------------------------

/** A board position for `issue_move` — `control.rs` `BoardPos`, tagged by `at`. */
export type BoardPos =
  | { at: "top" }
  | { at: "bottom" }
  | { at: "before"; reff: string }
  | { at: "after"; reff: string };

export type ChangePosition =
  | { at: "top" }
  | { at: "bottom" }
  | { at: "before"; issue: string }
  | { at: "after"; issue: string };

export type ChangeProject =
  | { source: "existing"; project: string }
  | { source: "created"; operation: number };

export type ChangeLabel =
  | { source: "existing"; label: string }
  | { source: "created"; operation: number };

export type IssuesChangeOperation =
  | { op: "project_create"; name: string; key: string; color: string }
  | {
      op: "spec_create";
      project: ChangeProject;
      kind: SpecKind;
      title: string;
      text: string;
      links?: SpecLink[];
    }
  | {
      op: "issue_create";
      project: ChangeProject;
      title: string;
      priority?: string | null;
      status?: string | null;
      parent?: string | null;
      assignees?: string[];
      labels?: ChangeLabel[];
      body?: string | null;
      due?: number | null;
      estimate?: number | null;
    }
  | {
      op: "issue_board";
      issue: string;
      status?: string | null;
      position?: ChangePosition | null;
    }
  | {
      op: "issue_patch";
      issue: string;
      title?: string | null;
      status?: string | null;
      priority?: string | null;
      due?: number | null;
      clear_due?: boolean;
      estimate?: number | null;
      clear_estimate?: boolean;
      assignees?: string[] | null;
      labels?: ChangeLabel[] | null;
    }
  | { op: "issue_work"; issue: string; action: "start" | "done" | "stop" }
  | { op: "issue_tombstone"; issue: string; on: boolean }
  | { op: "issue_comment"; issue: string; body: string; parent?: string | null }
  | {
      op: "issue_comment_at";
      issue: string;
      body: string;
      field: string;
      start: number;
      end?: number | null;
      parent?: string | null;
      source: WorldPublicationId;
    }
  | { op: "issue_reaction"; issue: string; comment: string; emoji: string; on: boolean }
  | { op: "issue_link"; issue: string; kind: string; target: string; on: boolean }
  | { op: "issue_parent"; issue: string; parent?: string | null }
  | {
      op: "issue_move";
      issue: string;
      project?: ChangeProject | null;
      position?: ChangePosition | null;
    }
  | { op: "issue_milestone"; issue: string; milestone?: string | null }
  | { op: "label_create"; name: string; color: string }
  | { op: "label_edit"; label: string; name?: string | null; color?: string | null }
  | { op: "label_delete"; label: string };

export interface DocumentSplice {
  index: number;
  delete: number;
  insert: string;
  /**
   * Scalar length of the document these offsets were measured against.
   *
   * The World refuses a splice whose base length disagrees with what it holds:
   * positional offsets are meaningless without agreement about which document
   * they index into.
   */
  base_len?: number;
}

export interface Filter {
  mine?: boolean;
  status?: string | null;
  label?: string | null;
  /** Milestone name or `mls_` id, resolved within `project` — which the daemon
   *  requires, because a milestone belongs to exactly one project. */
  milestone?: string | null;
  /** Include done + tombstoned rows. */
  all?: boolean;
}

/**
 * The installed Issues application protocol plus the browser-safe root control
 * requests, internally tagged by `cmd`.
 *
 * Field names are the Rust ones, verbatim — several are *not* what the CLI flag
 * suggests, and guessing them is how the old viewer broke. The ones that bite:
 * `issue_edit` takes `description` (not `body`); `assign` takes `add: bool` (not
 * `remove`); `label` takes `add[]`/`remove[]` (not the CLI's `+x -y` tokens — the
 * daemon wants them already split); and the `--as NAME` flag is `as_name`,
 * because `as` is a Rust keyword.
 *
 * Anything `#[serde(default)]` in Rust is optional here. `subscribe`, `connect`,
 * `seed_add`, `seed_remove`, `config_reload` and `stop` are deliberately absent:
 * `subscribe` is refused on the RPC path (use the doorbell stream), and the rest
 * have no browser surface yet — add them here when they grow one.
 */
export type Request =
  | { cmd: "change_set"; operation?: string | null; timestamp?: number | null; operations: IssuesChangeOperation[] }
  | { cmd: "operation_status"; operation: string; timestamp: number; operations: IssuesChangeOperation[] }
  | { cmd: "issue_new"; title: string; project?: string | null; project_hint?: string | null; assignees?: string[]; priority?: Priority | null; labels?: string[]; body?: string | null; due?: string | null; estimate?: number | null }
  /** `due`: `YYYY-MM-DD` (UTC), unix seconds, or `"none"` to clear; `estimate`:
   *  a number as a string, or `"none"` to clear. Absent = untouched. */
  | { cmd: "issue_edit"; reff: string; title?: string | null; status?: string | null; priority?: string | null; description?: string | null; due?: string | null; estimate?: string | null }
  /** Unicode-scalar offsets into the collaborative description text. */
  | {
      cmd: "issue_text_splice";
      reff: string;
      index: number;
      delete: number;
      insert: string;
      /** Scalar length of the document the offsets were measured against. */
      base_len?: number;
    }
  /** Atomic upgrade from a legacy body into Lait's hidden document model. */
  | { cmd: "issue_document_upgrade"; reff: string; expected: string; splices: DocumentSplice[] }
  /** Group a burst of live splices into one activity entry. */
  | { cmd: "issue_text_checkpoint"; reff: string }
  | { cmd: "issue_move"; reff: string; project?: string | null; pos?: BoardPos | null }
  | { cmd: "assign"; reff: string; who: string[]; add?: boolean }
  | { cmd: "label"; reff: string; add?: string[]; remove?: string[] }
  | { cmd: "comment"; reff: string; body: string; reply_to?: string | null }
  | { cmd: "comment_at"; reff: string; body: string; field: string; start: number; end?: number | null; reply_to?: string | null; source: WorldPublicationId }
  /** Toggle an emoji reaction on a comment. Writes no history event. */
  | { cmd: "react"; reff: string; comment: string; emoji: string; on?: boolean }
  | { cmd: "issue_delete"; reff: string }
  /** Clears the tombstone. Restore wins over a concurrent delete. */
  | { cmd: "issue_restore"; reff: string }
  /** `kind` is `blocks` | `relates` | `duplicates`; `reff` is the edge's source
   *  (`reff` blocks `target`), so "blocked by" is the same verb with the ends
   *  swapped. `relates` is symmetric — the daemon canonicalizes the endpoints. */
  | { cmd: "issue_link"; reff: string; kind: string; target: string }
  | { cmd: "issue_unlink"; reff: string; kind: string; target: string }
  /** `parent: null` clears. The daemon refuses cycles (tree-move CRDT). */
  | { cmd: "issue_parent"; reff: string; parent?: string | null }
  | { cmd: "issue_start"; reff: string }
  | { cmd: "issue_done"; reff: string }
  | { cmd: "issue_stop"; reff: string }
  | { cmd: "issue_view"; reff: string }
  | { cmd: "issue_detail"; reff: string; publication?: WorldPublicationId | null }
  | { cmd: "list"; project?: string | null; filter?: Filter; page: PageRequest }
  | { cmd: "board"; project?: string | null; project_hint?: string | null; page: PageRequest }
  | { cmd: "history"; reff: string; publication?: PublicationCoordinate | null; page: PageRequest }
  | { cmd: "issue_relations"; reff: string; direction: RelationDirection; publication?: PublicationCoordinate | null; page: PageRequest }
  | { cmd: "issue_comments"; reff: string; publication?: PublicationCoordinate | null; page: PageRequest }
  | { cmd: "issue_reactions"; reff: string; publication?: PublicationCoordinate | null; page: PageRequest }
  | { cmd: "issue_attachments"; reff: string; publication?: PublicationCoordinate | null; page: PageRequest }
  | { cmd: "issue_checks"; reff: string; publication?: PublicationCoordinate | null; page: PageRequest }
  | { cmd: "project_new"; name: string; key: string; color?: string | null }
  | { cmd: "project_list"; page: PageRequest }
  | { cmd: "team_list"; page: PageRequest }
  /**
   * Create, edit or delete a team — one verb for all three.
   *
   * `team` omitted mints a new one; `remove: true` deletes the one named. The
   * engine shape, kept rather than split into three client-side commands: a
   * rename and a create differ only by whether the id already exists.
   */
  | {
      cmd: "team_set";
      team?: string;
      name?: string;
      key?: string;
      icon?: string;
      lead?: string;
      add_members?: string[];
      remove_members?: string[];
      remove?: boolean;
    }
  | {
      cmd: "project_edit";
      project: string;
      name?: string | null;
      color?: string | null;
      description?: string | null;
      lead?: string | null;
      start?: string | null;
      target?: string | null;
      /** Soft-hide toggle: true archives, false restores, absent leaves it. */
      archived?: boolean | null;
      /** Owning team, by key or `tm_` id. `""` clears it. */
      team?: string | null;
    }
  /** Reply is `updates` — the project's status feed, newest first. */
  | { cmd: "project_updates"; project: string; page: PageRequest }
  | { cmd: "project_update_post"; project: string; body: string; health?: string | null }
  /** Subscribe to an issue without being assigned (INBOX-9). */
  | { cmd: "follow"; reff: string; on?: boolean }
  /** Reply is `milestones` — the project's milestones with progress (SCOPE-1). */
  | { cmd: "milestone_list"; project: string; page: PageRequest }
  | { cmd: "milestone_set"; project: string; milestone?: string | null; name?: string | null; description?: string | null; target?: string | null; pos?: BoardPos | null; remove?: boolean }
  /** Point an issue at a milestone in its project (`null`/"none" clears). */
  | { cmd: "issue_milestone"; reff: string; milestone?: string | null }
  /** Attach a file already sealed onto the content plane.
   *
   *  The bytes went up first, through `content.ts`; this names what came back.
   *  The engine refuses a content id it has no committed descriptor for, so the
   *  order is enforced rather than assumed. */
  | { cmd: "attach"; reff: string; name: string; mime?: string | null; content: string; size: number; comment?: string | null }
  | { cmd: "detach"; reff: string; id: string }
  /** Reply is `attachment` — the record, and either a content id or, for a
   *  record written before the cutover, its inline payload. */
  | { cmd: "attachment_get"; reff: string; id: string }
  | { cmd: "label_new"; name: string; color?: string | null }
  | { cmd: "label_list"; page: PageRequest }
  | { cmd: "label_show"; label: string; publication: WorldPublicationId }
  | { cmd: "label_edit"; label: string; name?: string | null; color?: string | null }
  | { cmd: "label_delete"; label: string }
  | { cmd: "space_rename"; name: string }
  | { cmd: "space_describe"; description: string }
  | { cmd: "activity"; page: PageRequest }
  | {
      cmd: "inbox";
      clear?: boolean;
      page: PageRequest;
      publication?: PublicationCoordinate | null;
    }
  | { cmd: "member_add"; who: string; admin?: boolean; as_name?: string | null }
  | { cmd: "member_remove"; who: string }
  /** Mint (or reuse) a co-located agent's seed, self-incept it, and sponsor it
   *  with write standing — the one-step form. `agent_add` sponsors a key that
   *  already exists somewhere else; this creates the identity too, which is what
   *  `install_mcp` tells people to come here for. */
  | { cmd: "agent_provision"; name: string }
  | { cmd: "key_rotate" }
  /** Reply is `text` — `"<actor_id> <space_id>"`, the token the other machine
   *  signs with `host_device_consent`. */
  | { cmd: "device_invite" }
  /** The hex consent blob that machine handed back. Completes enrolment. */
  | { cmd: "device_add"; consent: string }
  | { cmd: "device_revoke"; device: string }
  /** Reply is `text` — this actor's devices, one per line. */
  | { cmd: "device_list" }
  /** Write this holder's recovery share to a passphrase-sealed file at `path`,
   *  a path on the machine running the daemon. */
  | { cmd: "space_custody_export"; path: string; passphrase: string }
  /** Read a share back in — the documented remedy for the "share unreadable" /
   *  "backup unverified" warning the status panel draws. */
  | { cmd: "space_custody_import"; path: string; passphrase: string; force: boolean }
  | { cmd: "members" }
  | { cmd: "member_log" }
  | { cmd: "status" }
  /** Identity + standing + view-completeness, one shot — `dto.rs` `WhoamiDto`. */
  | { cmd: "whoami" }
  | { cmd: "diagnose"; expected_space?: string | null }
  | { cmd: "id" }
  | { cmd: "invite"; role?: string | null; reusable?: boolean; ttl_hours?: number | null }
  /** Admin-only. Accepts the invite ticket or its 32-hex nonce. */
  | { cmd: "invite_revoke"; invite: string }
  /** Reply is `text` — the revision as pretty JSON (same shape the CLI prints). */
  | { cmd: "workflow_show"; project: string }
  | { cmd: "workflow_set"; project: string; expect_heads: string[]; body_json: string }
  | { cmd: "spec_list"; project?: string | null; page: PageRequest }
  | { cmd: "geometry"; project: string; roots?: string[]; publication?: PublicationCoordinate | null; page?: GeometryPageRequest | null }
  | { cmd: "spec_show"; spec: string }
  /** Reply is one exact-publication revision page. */
  | { cmd: "spec_history"; spec: string; page: PageRequest }
  | { cmd: "spec_references"; project?: string | null; page: PageRequest }
  | { cmd: "baseline_history"; baseline: string; page: PageRequest }
  | { cmd: "spec_new"; project: string; kind: SpecKind; title: string; text?: string; links?: SpecLink[] }
  | { cmd: "spec_revise"; spec: string; expected: string; title?: string | null; text?: string | null; links?: SpecLink[] | null; plan?: PlanData | null }
  | { cmd: "spec_document_upgrade"; spec: string; expected: string; text: string }
  | { cmd: "spec_state"; spec: string; expected: string; state: SpecState }
  | { cmd: "spec_resolve"; spec: string; expected_heads: string[]; body_json: string }
  | { cmd: "spec_observations"; project?: string | null; page: PageRequest }
  | { cmd: "spec_observe"; spec: string; rel: SpecRel; target: SpecTarget; note?: string }
  | { cmd: "spec_retract"; spec: string; observation: string }
  | { cmd: "baseline_list"; project?: string | null; page: PageRequest }
  | { cmd: "baseline_show"; baseline: string }
  | { cmd: "baseline_new"; project: string; name: string; members: SpecRef[] }
  | { cmd: "baseline_revise"; baseline: string; expected: string; name?: string | null; members?: SpecRef[] | null }
  | { cmd: "baseline_state"; baseline: string; expected: string; state: SpecState }
  | { cmd: "baseline_resolve"; baseline: string; expected_heads: string[]; body_json: string }
  | { cmd: "issue_baseline"; reff: string; baseline?: BaselineRef | null }
  | { cmd: "packet"; reff: string }
  /** Reply is `text` — every role definition as pretty JSON. */
  | { cmd: "role_list"; page: PageRequest }
  /** Reply is `assignments` — effective scoped grants, optionally one actor. */
  | { cmd: "access_list"; actor?: string | null }
  /** Expand a role's pinned caps and install them for an actor (Space- or
   *  project-scoped). All-or-nothing; authority-first. */
  | { cmd: "access_grant"; actor: string; role: string; project?: string | null }
  /** Revoke effective assignments by 64-hex grant id, as one all-or-nothing
   *  set — a role grant's whole expansion, the way it was granted. */
  | { cmd: "access_revoke"; grant_ids: string[] }

  | { cmd: "join"; ticket: string }
  | { cmd: "seed_list" }
  | { cmd: "log"; since: number }
  | { cmd: "who" }
  /** Product-neutral transient state. `since_generation` is the generation the
   *  caller already holds — the reply is `live_unchanged` while it stands.
   *  Omit it on the first read: generation starts at zero, so sending zero is
   *  indistinguishable from holding an empty table. World clients resolve
   *  product coordinates to the optional 128-bit Body id. */
  | { cmd: "live"; world: string; since_generation?: number | null; body?: number[] | null }
  /** Drains. Every signal is answered once, so two callers on one space take
   *  half each — a browser and an agent must not both poll it. */
  | { cmd: "signals" };

export type SpaceRequest = Extract<
  Request,
  {
    cmd:
      | "member_add"
      | "member_remove"
      | "agent_provision"
      | "key_rotate"
      | "members"
      | "member_log"
      | "device_invite"
      | "device_add"
      | "device_revoke"
      | "device_list"
      | "space_custody_export"
      | "space_custody_import"
      | "status"
      | "whoami"
      | "diagnose"
      | "id"
      | "invite"
      | "invite_revoke"
      | "join"
      | "seed_list"
      | "log"
      | "who"
      | "live"
      | "signals";
  }
>;

export type WorldRequest = Exclude<Request, SpaceRequest>;

/**
 * The host plane: `POST /api/host/rpc`, `control.rs`'s `Request::Host*`.
 *
 * Its own union because it is the one plane that answers when there is nothing
 * to answer *about* — no Orbit, no store, no membership. Everything else in this
 * file is addressed to `/api/spaces/{id}/…`, which is unreachable at the only
 * moment founding and entering matter. `serve/policy.rs::is_host_plane` is the
 * gate; a `cmd` missing from it is refused there, not here.
 */
export type HostRequest =
  /** `home` is the exact store directory, on the machine running the daemon —
   *  not a directory to put a `.lait` inside. */
  | { cmd: "host_space_found"; home: string; name: string; nick?: string | null }
  /** Bootstraps the store from an invite link *and* drives admission before it
   *  answers, so `admitted` distinguishes "you're in" from "the board stays
   *  encrypted until they come online". */
  | { cmd: "host_space_enter"; link: string; home: string; nick?: string | null }
  /** Sign this machine's consent to join an existing actor. Store-free: the
   *  machine running it holds no membership anywhere yet. */
  | { cmd: "host_device_consent"; token: string }
  | { cmd: "host_context" }
  | { cmd: "host_orbit_forget"; selector: string }
  | { cmd: "host_orbit_prune" }
  | { cmd: "host_orbit_rebuild"; orbit: string }
  | { cmd: "host_update" }
  /** Stops the daemon under this server once the reply is out. The server
   *  survives and stands a fresh one up on its next request — which is how a
   *  swapped binary or a raised control protocol takes effect. */
  | { cmd: "host_restart" }
  /** ── The identity-scoped address book: the one namer. Host plane — the
   *  book belongs to the identity, never to a Space route. Trimmed to the
   *  verbs the browser uses; Astrolabe's book window is the full surface. ── */
  | { cmd: "book_lookup"; handle: string }
  | { cmd: "book_put"; card?: string | null; name: string; note?: string | null }
  | { cmd: "book_link"; card: string; handle: string }
  /** Scoped decoration: an orbit this head already authorized plus handles
   *  present in that answer. How the tracker resolves names and faces. */
  | { cmd: "book_resolve"; orbit: string; handles: string[] };

/** `control.rs` `HostReply`, tagged by `host` inside a `kind: "host"` response. */
/**
 * `update::watch::Standing` — what a machine knows about its channel as of
 * the last completed check. Every arm is its own fact: "could not ask", a
 * verification refusal, and a replayed stale pointer are three different
 * things, and only `current` may be rendered as up to date.
 */
export type UpdateStanding =
  | { standing: "current"; channel_version: string }
  | { standing: "available"; version: string }
  | { standing: "staged"; version: string }
  | { standing: "could_not_ask"; why: string }
  | { standing: "refused"; why: string }
  | { standing: "stale"; why: string };

export type HostReply =
  | {
      host: "founded";
      space: string;
      home: string;
      device: string;
      name: string;
      project_key: string;
      project_name: string;
    }
  | {
      host: "entered";
      space: string;
      home: string;
      device: string;
      approach: string;
      host_nick: string;
      fresh: boolean;
      admitted: boolean;
      contacted: boolean;
      last_error?: string | null;
    }
  | { host: "device_consent"; consent: string }
  | { host: "forgotten"; entries: OrbitEntry[] }
  | { host: "pruned"; entries: OrbitEntry[] }
  | { host: "rebuilt"; generation: string; effects: number; bodies: number; receipts: number; evidence: string }
  | {
      host: "updated";
      from: string;
      to: string;
      replaced: boolean;
      /** The channel this node follows. */
      channel?: string;
      /** The newest release the channel points at. */
      available?: string | null;
      /** The published floor, when the release declares a satisfiable one. */
      floor?: string | null;
      /**
       * Set when this daemon is a client's sidecar and declined to replace
       * itself; carries the client's path. `replaced` is false and `available`
       * may still name a newer release — the two together are "an update
       * exists, and *that* is what installs it", which is a different fact from
       * "you are up to date".
       */
      managed_by?: string | null;
      /**
       * What the resident staging watcher last learned about this
       * installation's channel. Absent means no check has ever completed —
       * which is neither "up to date" nor "could not ask", and must not be
       * rendered as either.
       */
      standing?: UpdateStanding | null;
    }
  | { host: "restarting"; pid?: number | null }
  | {
      host: "context";
      /** The build answering — the only place a running lait says which one. */
      version: string;
      identity_home: string;
      /** Where to offer to put a new store. A browser has no working directory. */
      spaces_root: string;
      worlds: string[];
      identities: string[];
      orbits: OrbitEntry[];
    };

/** One authored Card of the identity's address book (`BookCardView`). */
export interface BookCardDto {
  card: string;
  name: string;
  note: string;
  handles: string[];
  /** The phone-book reading of `handles`: addresses (`actor:` spellings),
   *  devices (bare device ids), co-located agents (`agent:` spellings). */
  addresses: string[];
  devices: string[];
  agents: string[];
  /** The stored picture (`<mime>;base64,<data>`); absent means the default
   *  face. */
  picture?: string | null;
  groups: string[];
  self_claim: boolean;
}

/** One authored hit of a scoped resolution (`BookHitView`). */
export interface BookHitDto {
  card: string;
  handle: string;
  name: string;
  /** The stored picture (`<mime>;base64,<data>`); absent means none authored. */
  picture?: string | null;
}

/** One row of the local Orbit registry (`orbits::Entry`). */
export interface OrbitEntry {
  space: string;
  name: string;
  path: string;
  origin: string;
  host_nick: string;
  last_opened: number;
  projects: { key: string; name: string }[];
}

/**
 * `control.rs` `Response`, internally tagged by `kind`.
 *
 * The newtype variants (`Issue(Box<IssueView>)`, `Board(Box<BoardView>)`,
 * `Status(Box<StatusInfo>)`, `Diagnosis(..)`) serialize **flattened** under an
 * internal tag — hence the intersections rather than a nested payload field.
 */
type ResponseBody =
  | { kind: "hello"; protocol_version: number }
  | { kind: "ok"; message: string | null }
  | { kind: "change_set"; results: { operation: number; kind: string; id: string }[] }
  | {
      kind: "operation_status";
      operation: string;
      readiness: "absent" | "ready" | "building" | "capacity" | "implementation_unavailable" | "generation_unavailable" | "unavailable";
      publication?: WorldPublicationId | null;
      results: { operation: number; kind: string; id: string }[];
    }
  | { kind: "ref"; reff: string }
  | ({ kind: "issue" } & IssueView)
  | ({ kind: "issue_detail" } & IssueDetailProjection)
  | { kind: "list"; page: Page<Row> }
  | ({ kind: "board" } & BoardPage)
  | ({ kind: "geometry" } & GeometryView)
  | { kind: "activity"; page: Page<ActivityEvent> }
  | { kind: "relations"; page: Page<IssueRelationDto> }
  | { kind: "comments"; page: Page<CommentDto> }
  | { kind: "reactions"; page: Page<ReactionRecord> }
  | { kind: "attachments"; page: Page<AttachmentMetaDto> }
  | { kind: "checks"; page: Page<CheckDto> }
  | { kind: "inbox"; page: Page<InboxEntry>; unread_on_page: number }
  | { kind: "projects"; page: Page<ProjectDto> }
  | { kind: "teams"; page: Page<TeamDto> }
  | { kind: "updates"; page: Page<ProjectUpdateDto> }
  | { kind: "milestones"; page: Page<MilestoneDto> }
  /** Exactly one of `content` and `data_b64` is present, and which one says
   *  which era the record is from. */
  | {
      kind: "attachment";
      name: string;
      mime: string;
      content?: string;
      data_b64?: string;
      size?: number;
    }
  | { kind: "labels"; page: Page<LabelDto> }
  | { kind: "label"; publication: WorldPublicationId; label?: LabelDto | null }
  | { kind: "roles"; page: Page<RoleProjection> }
  /** `control.rs` `BookView`, trimmed to what the browser reads. */
  | { kind: "book"; cards: BookCardDto[] }
  /** `control.rs` `BookResolutionView` — authored hits for resolved handles. */
  | { kind: "book_resolution"; hits: BookHitDto[]; coverage?: string | null }
  | { kind: "members"; members: MemberDto[] }
  | { kind: "assignments"; rows: AssignmentDto[] }
  | { kind: "member_log"; entries: MemberLogEntry[] }
  | { kind: "seeds"; seeds: SeedDto[] }
  /** A ref resolved to several — a first-class outcome (exit 2), never an error. */
  | { kind: "candidates"; candidates: Candidate[]; near_miss_for: string | null }
  | ({ kind: "status" } & StatusInfo)
  | ({ kind: "whoami" } & WhoamiInfo)
  | { kind: "diagnosis"; [k: string]: unknown }
  | { kind: "text"; text: string }
  /** Named, not flattened: a `SpecView` has a `kind` of its own, and a spread
   *  would put it where the response tag lives — `JSON.parse` keeps the last
   *  duplicate, so the reply would arrive claiming to be a `requirement`. */
  | { kind: "spec"; spec: SpecView }
  | { kind: "specs"; page: Page<SpecSummary> }
  | { kind: "spec_revisions"; page: Page<SpecRevision> }
  | { kind: "spec_references"; page: Page<SpecReferenceFact> }
  | { kind: "spec_observations"; page: Page<SpecObservationRecord> }
  | { kind: "baseline_revisions"; page: Page<BaselineRevisionDto> }
  | ({ kind: "baseline" } & BaselineView)
  | { kind: "baselines"; page: Page<BaselineSummary> }
  | ({ kind: "packet" } & Packet)
  | { kind: "events"; events: Event[]; last: number }
  | { kind: "who"; peers: PresenceEntry[] }
  /** `partial` means this node is not hearing from everyone it could be.
   *  Carried rather than inferred: showing three of five people with no
   *  indication is a confident lie. */
  | { kind: "live"; generation: number; partial: boolean; entries: LiveEntry[] }
  /** The generation the caller sent still stands. Its own tag rather than an
   *  absent `entries`, so a client branches on `kind` like it does everywhere
   *  else and an empty table does not look like an unchanged one. */
  | { kind: "live_unchanged"; generation: number }
  /** `dropped` counts what the daemon's queue lost for want of room, oldest
   *  first — a signal is not superseded by the next one the way progress is. */
  | { kind: "signals"; signals: SignalEntry[]; dropped: number }
  /** Every host-plane answer, under one `kind` and its own `host` tag. */
  | ({ kind: "host" } & HostReply)
  | {
      kind: "error";
      message: string;
      error_kind: "error" | "invalid" | "not_found" | "denied" | "retry" | "indeterminate";
    };

/** Application responses are unwrapped from the on-wire operation envelope,
 * but retain its durable receipt for feedback/reconciliation. Generic Space
 * responses simply omit it. */
export type Response = ResponseBody & { receipt?: OperationReceipt };

/** Exact product wire shape. Only Issues writes use the operation envelope. */
export type IssuesWireResponse = Response | {
  kind: "operation";
  receipt: OperationReceipt;
  response: Response;
};
