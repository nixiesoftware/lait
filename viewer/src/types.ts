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
  status: string;
  priority: Priority;
  assignees: string[];
  labels: string[];
  label_names: string[];
  comments: CommentDto[];
  created_by: string;
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
export interface SpecBody {
  spec: string; project: string; kind: SpecKind; title: string; text: string;
  state: SpecState; links: SpecLink[]; author: string; ts: number;
}
export interface SpecView {
  spec: string; project: string; kind: SpecKind; title: string; state: SpecState;
  revision: string; heads: string[]; issued: string[]; body: SpecBody;
}
export interface BaselineBody {
  baseline: string; project: string; name: string; state: SpecState;
  members: SpecRef[]; author: string; ts: number;
}
export interface BaselineView {
  baseline: string; project: string; name: string; state: SpecState;
  revision: string; heads: string[]; issued: string[]; body: BaselineBody;
}
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

/**
 * An issue's graph neighborhood — `dto.rs` `GraphView`, reply to `IssueGraph`.
 *
 * Read from the catalog *structure* doc without opening any issue doc, so it is
 * cheap. `parent`/`children` are the sub-issue tree (a tree-move CRDT, so concurrent
 * reparents can't converge to a cycle); `blocked_by` is the transitive set of open
 * issues that block this one, computed by the daemon (not just direct `blocks` edges).
 */
export interface GraphView {
  schema_version: number;
  reff: string;
  doc_id: string;
  parent: Row | null;
  children: Row[];
  links: LinkDto[];
  blocked_by: Row[];
}

export interface ActivityEvent {
  seq: number;
  doc_id: string | null;
  reff: string;
  kind: string;
  changes: FieldChange[];
  actor: string | null;
  actor_nick: string;
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
  kind: "presence" | "caret" | "selection" | "typing" | "residency";
  /** How long ago the daemon saw it — its clock, not the peer's. */
  age_ms: number;
  /** Past the caret grace window: still shown, no longer known to be right. */
  uncertain: boolean;
  caret: CaretPosition | null;
  focus: CaretPosition | null;
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
  issues: number;
  projects: number;
  /** True means zero counts are unavailable, not an empty space. */
  counts_unavailable?: boolean;
  /** `admin` | `member` | `pending`. */
  membership: string;
  degraded_recovery?: DegradedHolder[];
  recovery?: RecoveryState | null;
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
  name: string;
  path: string;
  origin: string;
  last_opened: number;
  /** `up` | `idle` | `missing`. */
  status: "up" | "idle" | "missing";
  identity: SpaceIdentity;
  projects: ProjectBrief[];
}

export interface SpacesReply {
  spaces: SpaceRow[];
}

/** An agent's space is observable, not operable — writes are refused server-side. */
export const isReadOnly = (s: SpaceRow): boolean => s.identity.kind === "agent";

// ---- the doorbell -----------------------------------------------------------

/**
 * The catalog planes a doorbell can name. Closed on purpose: a resource declares
 * the planes it projects (`Derivation` in `projectStore`), and a plane the engine
 * does not emit should fail to compile rather than silently never fire.
 */
export type CatalogPlane =
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
  /** The row index: which docs exist, their aliases and seqs, what is deleted. */
  | "docs"
  /** Issue links and parentage. */
  | "relations";

/**
 * The project a dirty plane belongs to, named twice on purpose: `project_id` is
 * the stable identity a dependency matches on, `project_key` is a mutable
 * display alias a rename changes underneath you.
 */
export interface ProjectRef {
  project_id: string;
  project_key: string;
}

/** A dirty catalog plane. The project-scoped planes carry a `ProjectRef`. */
export type CatalogScope = { scope: CatalogPlane } & Partial<ProjectRef>;

/** Which docs moved, in which project. */
export interface DirtyProject extends ProjectRef {
  docs: string[];
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
  dirty_by_project: DirtyProject[];
  dirty_catalog: CatalogScope[];
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
  | { cmd: "issue_new"; title: string; project?: string | null; project_hint?: string | null; assignees?: string[]; priority?: Priority | null; labels?: string[]; body?: string | null; due?: string | null; estimate?: number | null }
  /** `due`: `YYYY-MM-DD` (UTC), unix seconds, or `"none"` to clear; `estimate`:
   *  a number as a string, or `"none"` to clear. Absent = untouched. */
  | { cmd: "issue_edit"; reff: string; title?: string | null; status?: string | null; priority?: string | null; description?: string | null; due?: string | null; estimate?: string | null }
  | { cmd: "issue_move"; reff: string; project?: string | null; pos?: BoardPos | null }
  | { cmd: "assign"; reff: string; who: string[]; add?: boolean }
  | { cmd: "label"; reff: string; add?: string[]; remove?: string[] }
  | { cmd: "comment"; reff: string; body: string; reply_to?: string | null }
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
  | { cmd: "list"; project?: string | null; filter?: Filter }
  | { cmd: "board"; project?: string | null; project_hint?: string | null }
  | { cmd: "history"; reff: string }
  | { cmd: "issue_graph"; reff: string }
  | { cmd: "project_new"; name: string; key: string; color?: string | null }
  | { cmd: "project_list" }
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
    }
  /** Reply is `updates` — the project's status feed, newest first. */
  | { cmd: "project_updates"; project: string }
  | { cmd: "project_update_post"; project: string; body: string; health?: string | null }
  /** Subscribe to an issue without being assigned (INBOX-9). */
  | { cmd: "follow"; reff: string; on?: boolean }
  /** Reply is `milestones` — the project's milestones with progress (SCOPE-1). */
  | { cmd: "milestone_list"; project: string }
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
  | { cmd: "label_list" }
  | { cmd: "label_edit"; label: string; name?: string | null; color?: string | null }
  | { cmd: "label_delete"; label: string }
  | { cmd: "space_rename"; name: string }
  | { cmd: "space_describe"; description: string }
  | { cmd: "activity"; since?: number }
  | { cmd: "inbox"; clear?: boolean }
  | { cmd: "member_add"; who: string; admin?: boolean; as_name?: string | null }
  | { cmd: "member_remove"; who: string }
  | { cmd: "key_rotate" }
  | { cmd: "members" }
  | { cmd: "member_log" }
  | { cmd: "member_alias"; who: string; name: string }
  | { cmd: "status" }
  | { cmd: "diagnose"; expected_space?: string | null }
  | { cmd: "id" }
  | { cmd: "invite"; role?: string | null; reusable?: boolean; ttl_hours?: number | null }
  /** Admin-only. Accepts the invite ticket or its 32-hex nonce. */
  | { cmd: "invite_revoke"; invite: string }
  /** Reply is `text` — the revision as pretty JSON (same shape the CLI prints). */
  | { cmd: "workflow_show"; project: string }
  | { cmd: "workflow_set"; project: string; expect_heads: string[]; body_json: string }
  | { cmd: "spec_list"; project?: string | null }
  | { cmd: "spec_show"; spec: string }
  /** Reply is `spec_revisions` — the whole DAG, oldest first. */
  | { cmd: "spec_history"; spec: string }
  | { cmd: "baseline_history"; baseline: string }
  | { cmd: "spec_new"; project: string; kind: SpecKind; title: string; text?: string; links?: SpecLink[] }
  | { cmd: "spec_revise"; spec: string; expected: string; title?: string | null; text?: string | null; links?: SpecLink[] | null }
  | { cmd: "spec_state"; spec: string; expected: string; state: SpecState }
  | { cmd: "spec_resolve"; spec: string; expected_heads: string[]; body_json: string }
  | { cmd: "baseline_list"; project?: string | null }
  | { cmd: "baseline_show"; baseline: string }
  | { cmd: "baseline_new"; project: string; name: string; members: SpecRef[] }
  | { cmd: "baseline_revise"; baseline: string; expected: string; name?: string | null; members?: SpecRef[] | null }
  | { cmd: "baseline_state"; baseline: string; expected: string; state: SpecState }
  | { cmd: "baseline_resolve"; baseline: string; expected_heads: string[]; body_json: string }
  | { cmd: "issue_baseline"; reff: string; baseline?: BaselineRef | null }
  | { cmd: "packet"; reff: string }
  /** Reply is `text` — every role definition as pretty JSON. */
  | { cmd: "role_list" }
  /** Reply is `assignments` — effective scoped grants, optionally one actor. */
  | { cmd: "access_list"; actor?: string | null }
  /** Expand a role's pinned caps and install them for an actor (Space- or
   *  project-scoped). All-or-nothing; authority-first. */
  | { cmd: "access_grant"; actor: string; role: string; project?: string | null }
  /** Revoke one effective capability assignment by its 64-hex grant id. */
  | { cmd: "access_revoke"; grant_id: string }
  | { cmd: "join"; ticket: string }
  | { cmd: "seed_list" }
  | { cmd: "log"; since: number }
  | { cmd: "who" }
  /** Who is doing what right now. `since_generation` is the generation the
   *  caller already holds — the reply is `live_unchanged` while it stands.
   *  Omit it on the first read: generation starts at zero, so sending zero is
   *  indistinguishable from holding an empty table. `issue` is an `iss_` doc
   *  id and narrows to that issue's presence. */
  | { cmd: "live"; since_generation?: number | null; issue?: string | null }
  /** Drains. Every signal is answered once, so two callers on one space take
   *  half each — a browser and an agent must not both poll it. */
  | { cmd: "signals" };

export type SpaceRequest = Extract<
  Request,
  {
    cmd:
      | "member_add"
      | "member_remove"
      | "key_rotate"
      | "members"
      | "member_log"
      | "member_alias"
      | "status"
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
 * `control.rs` `Response`, internally tagged by `kind`.
 *
 * The newtype variants (`Issue(Box<IssueView>)`, `Board(Box<BoardView>)`,
 * `Status(Box<StatusInfo>)`, `Diagnosis(..)`) serialize **flattened** under an
 * internal tag — hence the intersections rather than a nested payload field.
 */
export type Response =
  | { kind: "hello"; protocol_version: number }
  | { kind: "ok"; message: string | null }
  | { kind: "ref"; reff: string }
  | ({ kind: "issue" } & IssueView)
  | { kind: "list"; rows: Row[] }
  | ({ kind: "board" } & BoardView)
  | ({ kind: "graph" } & GraphView)
  | { kind: "activity"; events: ActivityEvent[]; last: number }
  | { kind: "inbox"; entries: InboxEntry[]; unread: number }
  | { kind: "projects"; projects: ProjectDto[] }
  | { kind: "updates"; updates: ProjectUpdateDto[] }
  | { kind: "milestones"; milestones: MilestoneDto[] }
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
  | { kind: "labels"; labels: LabelDto[] }
  | { kind: "members"; members: MemberDto[] }
  | { kind: "assignments"; rows: AssignmentDto[] }
  | { kind: "member_log"; entries: MemberLogEntry[] }
  | { kind: "seeds"; seeds: SeedDto[] }
  /** A ref resolved to several — a first-class outcome (exit 2), never an error. */
  | { kind: "candidates"; candidates: Candidate[]; near_miss_for: string | null }
  | ({ kind: "status" } & StatusInfo)
  | { kind: "diagnosis"; [k: string]: unknown }
  | { kind: "text"; text: string }
  /** Named, not flattened: a `SpecView` has a `kind` of its own, and a spread
   *  would put it where the response tag lives — `JSON.parse` keeps the last
   *  duplicate, so the reply would arrive claiming to be a `requirement`. */
  | { kind: "spec"; spec: SpecView }
  | { kind: "specs"; specs: SpecView[] }
  | { kind: "spec_revisions"; revisions: SpecRevision[] }
  | { kind: "baseline_revisions"; revisions: BaselineRevisionDto[] }
  | ({ kind: "baseline" } & BaselineView)
  | { kind: "baselines"; baselines: BaselineView[] }
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
  | { kind: "error"; message: string; error_kind: "error" | "not_found" };
