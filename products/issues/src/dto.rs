//! Layer-B data-transfer objects: the **stable, versioned, hand-maintained
//! projection** of Layer A. These are the shapes the CLI
//! `--json` contract emits and the MCP tools return; they are checked against
//! the MCP tool schemas (see `tests/mcp_parity.rs`) so agent and human surfaces
//! never drift. They are **not** an automatic dump of the storage layout — a
//! storage refactor must not break these.
//!
//! Also home to the shared plain-domain enums ([`Priority`], [`StatusCategory`],
//! [`WorkflowState`]) used by both the Layer-A wrappers and this projection. A
//! plain enum shared across layers is fine; what the three-layer rule forbids is
//! mirroring the *container layout* automatically.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

// No `DeviceId`. This layer had exactly one device-typed field and it was the
// one named `actor` — every other identity a client is shown is an actor, which
// is the shape the whole DTO surface already had and that one field did not.
use crate::ids::{ActorId, DocId, LabelId, ProjectId, SpaceId};

/// Space authority, projected. Defined in `mechanics` beside the
/// [`mechanics::assignment::Assignment`] it flattens — a capability naming
/// *which World* it belongs to is Space vocabulary, not any one World's.
/// Re-exported here because this product publishes it in its policy schema
/// bundle below.
pub use mechanics::assignment::AssignmentDto;

/// Generate the committed JSON Schema 2020-12 bundle for the product policy
/// surface — every role/access/workflow definition shape plan 04 names —
/// deterministically (used by the drift gate in `tests/product_schema.rs` and
/// the language-neutral validator in `ci/validate-dto-schema.py`).
pub fn product_policy_schema_bundle() -> serde_json::Value {
    use schemars::generate::SchemaSettings;
    let settings = SchemaSettings::draft2020_12();
    let generator = settings.into_generator();
    let mut defs = serde_json::Map::new();
    macro_rules! add {
        ($ty:ty, $name:literal) => {
            let schema = generator.clone().into_root_schema_for::<$ty>();
            defs.insert(
                $name.to_string(),
                serde_json::to_value(schema).expect("schema json"),
            );
        };
    }
    add!(crate::roles::RoleBody, "RoleBody");
    add!(crate::workflow::WorkflowBody, "WorkflowBody");
    add!(crate::workflow::WorkflowTransition, "WorkflowTransition");
    add!(crate::workflow::DemandTemplate, "DemandTemplate");
    add!(AssignmentDto, "AssignmentDto");
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://lait.dev/schema/product-policy/v1",
        "title": "LAIT Issues policy surface v1",
        "$defs": defs,
    })
}

/// Schema version gate. Every top-level DTO carries it so a reader
/// can detect drift; bump on any additive change.
///
/// v2: the actor-identity cutover (`lait/actor/1`) — members, assignees, and
/// attribution are keyed by `ActorId` over a self-managed device set, replacing
/// the `person ≡ key ≡ device` model.
///
/// v3: the space-vocabulary flag day — `genesis.json` keys the space id under
/// `space_id`, and every Loro document stamps it under `spaceId`. A v2 store
/// spells both the old way, so a v3 reader would open it and then project an
/// absent space id; see [`MIN_SUPPORTED_SCHEMA`].
pub const SCHEMA_VERSION: u32 = 3;

/// The oldest on-disk schema this build will open.
///
/// A lower bound exists because "older is fine" is only true while every older
/// shape is still *readable*. v2 stores are not: their space id sits under keys
/// v3 does not look at, so opening one succeeds and then silently projects a
/// store with no space. Refusing is the honest outcome — there is no migration,
/// and a store that opens wrong is worse than a store that will not open.
pub const MIN_SUPPORTED_SCHEMA: u32 = 3;

/// Issue priority. Stored inside the issue document as a lowercase
/// string leaf and projected here.
///
/// ```
/// use lait_issues::dto::Priority;
/// assert_eq!(Priority::parse("urgent"), Some(Priority::Urgent));
/// assert_eq!(Priority::parse("h"), Some(Priority::High)); // one-letter alias
/// assert!(Priority::Urgent > Priority::Low);              // orders low→high
/// assert_eq!(serde_json::to_string(&Priority::High).unwrap(), "\"high\"");
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    #[default]
    None,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Priority::None => "none",
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "none" | "" => Priority::None,
            "low" | "l" => Priority::Low,
            "medium" | "med" | "m" => Priority::Medium,
            "high" | "h" => Priority::High,
            "urgent" | "u" => Priority::Urgent,
            _ => return None,
        })
    }

    /// One-letter board badge: `·U/H/M/L·`.
    pub fn badge(&self) -> &'static str {
        match self {
            Priority::None => "-",
            Priority::Low => "L",
            Priority::Medium => "M",
            Priority::High => "H",
            Priority::Urgent => "U",
        }
    }
}

/// Workflow-state category. Governs board columns and the completion rule: a
/// `Done`-category status removes the issue from the
/// board movable list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusCategory {
    Backlog,
    Active,
    Done,
}

impl StatusCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            StatusCategory::Backlog => "backlog",
            StatusCategory::Active => "active",
            StatusCategory::Done => "done",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "backlog" => StatusCategory::Backlog,
            "active" => StatusCategory::Active,
            "done" => StatusCategory::Done,
            _ => return None,
        })
    }
}

/// An ordered status column. `id` is the `StatusId` stored on the
/// issue's `status` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    pub category: StatusCategory,
    pub color: String,
}

/// The default workflow seeded into a fresh catalog.
pub fn default_workflow() -> Vec<WorkflowState> {
    vec![
        WorkflowState {
            id: "backlog".into(),
            name: "Backlog".into(),
            category: StatusCategory::Backlog,
            color: "gray".into(),
        },
        WorkflowState {
            id: "in_progress".into(),
            name: "In Progress".into(),
            category: StatusCategory::Active,
            color: "blue".into(),
        },
        WorkflowState {
            id: "in_review".into(),
            name: "In Review".into(),
            category: StatusCategory::Active,
            color: "yellow".into(),
        },
        WorkflowState {
            id: "done".into(),
            name: "Done".into(),
            category: StatusCategory::Done,
            color: "green".into(),
        },
    ]
}

/// The default status id a brand-new issue lands in.
pub const DEFAULT_STATUS: &str = "backlog";

// ----------------------------------------------------------------------------
// Corruption (the projection honesty policy)
// ----------------------------------------------------------------------------

/// One stored record that could not be projected into its DTO.
///
/// The policy this type exists to enforce: **a projection never lies.** Three
/// states must stay distinct — *known* (stored and parsed), *unknown*
/// (legitimately not available yet, e.g. a provisional row whose body hasn't
/// synced), and *corrupt* (a value is stored and does not conform to its type).
/// Collapsing them is what produced the failure modes this replaces:
///
/// - `Option<ActorId>` on a field that is never optional in the schema, which
///   makes every consumer re-decide what a missing author means;
/// - a silent `continue`/`filter_map`, which makes the record vanish — counts go
///   wrong, positions shift, and a peer writing malformed keys becomes invisible;
/// - a sentinel like `act_0000…`, which is a *well-typed lie* and the worst of
///   the three, because nothing downstream can tell it from a real id.
///
/// A corrupt record is therefore neither dropped nor laundered: it is lifted out
/// of the typed collection and carried alongside it, so the DTO keeps its true
/// types and the corruption stays auditable under `--json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorruptRecord {
    /// Where the record sat, position included: `comments[3]`. This is what
    /// makes a sidecar list lossless — the index the record occupied in the
    /// valid collection is recoverable, so "3rd comment is corrupt" survives.
    pub locus: String,
    /// Which field failed and how: `author: not an ActorId`. Human-readable;
    /// diagnostics, not a machine contract.
    pub reason: String,
    /// Best-effort raw leaves, for forensics and eventual repair. Never
    /// interpreted — this is evidence, not data.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub raw: BTreeMap<String, String>,
}

impl CorruptRecord {
    /// A corrupt record with no salvaged leaves.
    pub fn new(locus: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            locus: locus.into(),
            reason: reason.into(),
            raw: BTreeMap::new(),
        }
    }

    /// Attach a salvaged raw leaf.
    pub fn with_raw(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.raw.insert(key.into(), value.into());
        self
    }
}

/// The result of projecting one stored record: either the DTO, or the reason it
/// isn't one. Layer-A readers return these so that **no read site has to choose
/// between dropping and laundering** — both bad options are off the table
/// because the type has somewhere honest to put the failure.
///
/// Deliberately **not** `Serialize`. A `Projected` cannot reach the wire; it has
/// to be [`partition`]ed first, which is what guarantees a UI consumer can never
/// receive a malformed record inside a field typed as a valid one. The invariant
/// is structural rather than a matter of caller discipline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projected<T> {
    Valid(T),
    Corrupt(CorruptRecord),
}

impl<T> Projected<T> {
    /// The DTO, if this record projected cleanly.
    pub fn valid(self) -> Option<T> {
        match self {
            Projected::Valid(v) => Some(v),
            Projected::Corrupt(_) => None,
        }
    }

    pub fn is_corrupt(&self) -> bool {
        matches!(self, Projected::Corrupt(_))
    }
}

/// Split a projected sequence into the valid DTOs and the corruption sidecar,
/// preserving the relative order of each. The single point where corruption
/// leaves the typed path — call it once, at the projection boundary.
pub fn partition<T>(items: impl IntoIterator<Item = Projected<T>>) -> (Vec<T>, Vec<CorruptRecord>) {
    let mut valid = Vec::new();
    let mut corrupt = Vec::new();
    for item in items {
        match item {
            Projected::Valid(v) => valid.push(v),
            Projected::Corrupt(c) => corrupt.push(c),
        }
    }
    (valid, corrupt)
}

// ----------------------------------------------------------------------------
// Projections (read DTOs)
// ----------------------------------------------------------------------------

/// A project registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectDto {
    pub id: ProjectId,
    pub name: String,
    pub key: String,
    pub color: String,
    /// Overview markdown (additive; empty when unset).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Lead actor key (empty = none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lead: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_date: Option<u64>,
    /// Soft-hidden (archived). Additive; absent-when-false so pre-archive
    /// consumers decode unchanged. Clients hide these from pickers and
    /// all-project lists but can still open one directly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// Owning team id (empty = none; GOV-7). Additive.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub team: String,
}

/// One project milestone with its derived progress (SCOPE-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MilestoneDto {
    pub id: String,
    pub name: String,
    /// The milestone's prose body (additive; empty when unset).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_date: Option<u64>,
    /// Live (non-tombstoned) issues targeting this milestone.
    pub total: u32,
    /// Of those, issues in a Done-category state.
    pub done: u32,
}

/// One cycle with its derived counts (BOARD-11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CycleDto {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub start: u64,
    #[serde(default)]
    pub end: u64,
    pub total: u32,
    pub done: u32,
}

/// One initiative with its derived roll-up (SCOPE-8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InitiativeDto {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub owner: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub health: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_date: Option<u64>,
    /// Member project KEYs (resolved; unknown ids are dropped).
    pub projects: Vec<String>,
    /// Live issues across the member projects.
    pub total: u32,
    pub done: u32,
}

/// One team (GOV-7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamDto {
    pub id: String,
    pub name: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub lead: String,
    pub members: Vec<String>,
    /// KEYs of the projects this team owns.
    pub projects: Vec<String>,
}

/// One triage-intake item (SCOPE-7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageDto {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    pub submitted_by: String,
    pub ts: u64,
    /// "" (pending) | `accepted` | `declined` | `duplicate`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub outcome: String,
    /// The canonical reff of the accepted/duplicated issue, when decided.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reff: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub decided_by: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// Attachment metadata as projected on an issue (CREATE-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMetaDto {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mime: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    pub ts: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub comment: String,
}

/// One project status update, projected for the updates feed (SCOPE-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectUpdateDto {
    pub id: String,
    /// The authoring actor key.
    pub author: String,
    /// Post time, unix seconds.
    pub ts: u64,
    pub body: String,
    /// `on_track` | `at_risk` | `off_track` | "" (none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub health: String,
}

/// A label registry entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelDto {
    pub id: LabelId,
    pub name: String,
    pub color: String,
}

/// One board or list row, projected from the `DocMeta` cache for rendering.
/// This projection never opens the issue document. A row whose issue body has
/// not arrived is `provisional`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    /// Canonical collision-free short handle, such as `iss_3f9`.
    pub reff: String,
    pub doc_id: DocId,
    pub project_id: ProjectId,
    /// Friendly alias `ENG-142` (may disambiguate to `ENG-142b`), advisory.
    pub key_alias: Option<String>,
    pub title: String,
    pub status: String,
    pub priority: Priority,
    /// Viewer-relative one-liner (`you +2`) — what a terminal row prints.
    pub assignee_summary: String,
    /// The assignee keys behind that summary.
    ///
    /// Both, not one. `assignee_summary` is *rendered* — it resolves "you" against
    /// the local `DeviceId` and collapses the tail into `+2`, which is exactly right
    /// for a CLI row and useless to a client that wants to draw faces. The keys are
    /// already in `RowMeta` (cached viewer-neutrally, precisely so the summary can
    /// be computed per-viewer), so this projects them rather than making every
    /// graphical client open N issue docs to learn what the catalog already knows.
    pub assignees: Vec<ActorId>,
    pub tombstone: bool,
    pub provisional: bool,
    /// Due date, unix seconds. Additive with absent-when-none serialization so
    /// pre-duedate consumers keep decoding the same bytes for undated rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<u64>,
    /// Estimate points (scale is the team's convention, not the schema's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u32>,
    /// Resolved label names (empty when none). Additive so a card can show label
    /// dots without a second fetch; older consumers ignore the field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub label_names: Vec<String>,
    /// The `mls_` milestone this issue targets, if any (SCOPE-1).
    ///
    /// On the row for the same reason `label_names` is: a client that wants to
    /// group or filter by milestone can do it against rows it already holds,
    /// rather than opening every issue to learn what the catalog already knows.
    /// The *id*, not the name — a rename must not move a filter, and the name is
    /// one `milestone_list` away on a surface that needs to print it.
    ///
    /// Additive/absent-when-none, so pre-milestone consumers decode unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// Sub-issue progress: done and total live (non-tombstoned) children. `None`
    /// when the issue has no children, so a card only draws a progress mini-bar
    /// for issues that actually parent others. Additive/absent-when-none, and set
    /// only where the child index is cheaply available (the board projection).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_done: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_total: Option<u32>,
}

/// A board column: an ordered slice of rows for one workflow state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardColumn {
    pub state: WorkflowState,
    pub rows: Vec<Row>,
}

/// A rendered board: workflow states with their ordered rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardView {
    pub schema_version: u32,
    pub project: ProjectDto,
    pub columns: Vec<BoardColumn>,
}

/// A comment projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentDto {
    /// The authoring **actor** — the person, stable across their devices.
    ///
    /// Not optional: the schema has no authorless comment, so an `Option` here
    /// would encode *storage corruption* in a *domain* type and push the
    /// decision onto every consumer. A comment whose stored author doesn't parse
    /// as an [`ActorId`] is not a `CommentDto` with a hole in it — it isn't a
    /// `CommentDto` at all, and is projected as a [`CorruptRecord`] instead.
    pub author: ActorId,
    pub author_nick: Option<String>,
    pub ts: u64,
    pub body: String,
    /// Canonical comment id (`cmt_…`). Absent on comments stored before
    /// comment identity existed — those cannot anchor reactions or replies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The comment this one replies to (one level of nesting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// Emoji reactions, grouped: each emoji with the actors who reacted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reactions: Vec<ReactionDto>,
    /// Where this comment attaches in the issue's collaborative text, resolved
    /// against the snapshot this projection was built from. Absent on an
    /// ordinary comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<CommentAnchorDto>,
}

/// A comment's attachment to a span of the issue's collaborative text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentAnchorDto {
    /// The collaborative text field the span lies in.
    pub field: String,
    pub state: CommentAnchorState,
}

/// Where a range-attached comment points, as of THIS read.
///
/// Computed on every read and never written back. A stored resolution is a
/// number that was right once, and every edit made after the comment is a
/// chance for it to become the silently wrong index the anchor algebra exists
/// to rule out. The transient plane states the same rule for carets
/// (`runtime::plane::live::CaretState`); this is the durable half, where the window
/// for going stale is the lifetime of the issue rather than a few seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CommentAnchorState {
    /// A span of the field as it stands now, in Unicode scalar offsets — the
    /// coordinates the convergence engine counts text in, not UTF-8 bytes and
    /// not UTF-16 code units.
    ///
    /// One edge is approximate, and it is the algebra's edge rather than one
    /// this World introduced: an anchor at offset 0 binds to no operation,
    /// because there is no character before it to bind to. A span works around
    /// that by binding its head to the first character INSIDE it, so a span is
    /// exact wherever it starts. A caret has no character of its own to bind
    /// to, so a caret at offset 0 stays an offset from the start: text inserted
    /// at the very front of a field grows past it instead of pushing it along.
    At { start: u64, end: u64 },
    /// The material the span was on is gone, the field it named has been
    /// emptied, the anchor predates what this replica retains, or the two ends
    /// resolved out of order and no longer describe a span.
    ///
    /// A drifted comment is still a comment somebody wrote: it renders, and it
    /// renders without a position. Dropping it would delete a person's words
    /// because an unrelated edit landed.
    Drifted,
    /// No answer was available — the stored anchor is not canonical bytes, it
    /// names a different field than the record it sits in, or it names a field
    /// with no positions in it at all.
    ///
    /// Every one of those is a fact about the record, not about the text: the
    /// span was never placeable here, rather than placeable once and not now.
    /// Distinct from `Drifted`, which IS an answer. Conflating them tells
    /// someone their text was deleted when nothing of the sort happened.
    Unresolved,
}

/// One emoji's reactions on one comment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionDto {
    pub emoji: String,
    pub actors: Vec<ActorId>,
}

/// One issue-owned target bound to a durable Runtime Run.
///
/// Lifecycle details are intentionally absent. Clients inspect those through
/// the generic Work capability using `run`; this record only explains what the
/// Issues product asked that Run to verify and what product decision landed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckDto {
    pub run: String,
    pub spec: String,
    pub version: u32,
    pub build: String,
    pub source: String,
    pub state: String,
    pub by: String,
    pub ts: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub package_filled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
}

/// The full issue projection — populated by lazily loading the issue doc
/// `provisional` is set when only the catalog row is known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueView {
    pub schema_version: u32,
    pub reff: String,
    pub doc_id: DocId,
    pub space_id: SpaceId,
    pub project_id: ProjectId,
    pub project_key: Option<String>,
    pub key_alias: Option<String>,
    pub title: String,
    pub description: String,
    /// Version of Lait's user-invisible issue document model. Zero identifies
    /// a legacy body that can be upgraded from the issue header.
    #[serde(default)]
    pub document_schema: u32,
    pub status: String,
    pub priority: Priority,
    pub assignees: Vec<ActorId>,
    pub labels: Vec<LabelId>,
    pub label_names: Vec<String>,
    /// Valid comments only. Every element satisfies the `CommentDto` schema —
    /// a consumer may render these as trusted objects without re-validating.
    pub comments: Vec<CommentDto>,
    pub created_by: ActorId,
    pub created_at: u64,
    /// Due date, unix seconds (absent = none). Additive, like `Row.due_date`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate: Option<u32>,
    /// Subscribed actors, independent of assignment (INBOX-9). Additive.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followers: Vec<ActorId>,
    /// The targeted milestone id (SCOPE-1). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone: Option<String>,
    /// The scheduled cycle id (BOARD-11). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<String>,
    /// Exact issued Baseline pinned to this Issue.
    pub baseline: Option<crate::spec::BaselineRef>,
    /// Attachment metadata (CREATE-5) — payloads come from `attachment get`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AttachmentMetaDto>,
    /// Stable issue-to-Run bindings; Runtime remains the lifecycle authority.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<CheckDto>,
    pub provisional: bool,
    /// Records under this issue that failed to project (see [`CorruptRecord`]).
    ///
    /// Carried beside the typed collections rather than inside them: the
    /// corruption stays auditable under `--json` for the operator who has to
    /// diagnose it, while a normal UI consumer iterating `comments` cannot
    /// accidentally render a malformed record as a trusted one. Absent from the
    /// JSON entirely when empty, so the healthy shape is unchanged and existing
    /// readers keep working.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corrupt_records: Vec<CorruptRecord>,
}

/// One derived activity transition. `changes` is a list so one request, one
/// commit, and one activity row remain equivalent even when several fields
/// change. Clients pull activity via `Activity { since }`; it is not streamed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// This row's ordinal in the issue's whole history, trimmed rows counted.
    /// A display number and a stable one — it does not restart when a busy
    /// issue drops its oldest events — but not a cursor. To resume a feed,
    /// send back [`Self::cursor`].
    pub seq: u64,
    /// The opaque token that names this row for resumption: `(ts, doc, entry
    /// id)`, comparable in the feed's own order. Empty for a row from a
    /// pre-log Body, which has no entry id to name.
    #[serde(default)]
    pub cursor: String,
    pub doc_id: Option<DocId>,
    pub reff: String,
    pub kind: String,
    pub changes: Vec<FieldChange>,
    /// Who did it, as an **actor** — which is what the field was always named
    /// and, until this landed, never carried. It held the committing `DeviceId`,
    /// so the member lookup that resolves a display name (keyed by actor) missed
    /// on every row and every author was drawn as a hex prefix, coloured by
    /// hashing that hex, in a colour nothing else on the screen agreed with.
    ///
    /// `None` for an event written before events carried one. That is the
    /// honest answer and the viewer already draws it as no name rather than
    /// inventing one.
    pub actor: Option<ActorId>,
    pub actor_nick: String,
    pub text: String,
    pub ts: u64,
    /// Non-blocking LWW collision note: a concurrent overwrite was detected.
    pub collision: bool,
}

/// A single field transition inside an [`ActivityEvent`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldChange {
    pub field: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// One issue link projected for the graph view. `direction`
/// is relative to the requested issue: `out` = it names the other, `in` = the
/// other names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkDto {
    /// `blocks` | `relates` | `duplicates`.
    pub kind: String,
    /// `out` | `in`.
    pub direction: String,
    pub row: Row,
}

/// An issue's graph neighborhood (reply to `IssueGraph`): sub-issue hierarchy,
/// links, and the transitively-open blockers — all read from the catalog
/// structure doc, no issue doc opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphView {
    pub schema_version: u32,
    pub reff: String,
    pub doc_id: DocId,
    pub parent: Option<Row>,
    pub children: Vec<Row>,
    pub links: Vec<LinkDto>,
    /// Issues that transitively block this one and are still open.
    pub blocked_by: Vec<Row>,
}

/// One structural edge between two issues of a project.
///
/// Doc ids, not refs. A ref is an alias and a rename moves it; the client joins
/// these against rows it already holds, which carry `doc_id` for exactly this.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdgeDto {
    pub from: String,
    /// `blocks` | `relates` | `duplicates`.
    pub kind: String,
    pub to: String,
}

/// A project's whole structure in one reply (reply to `ProjectGraph`).
///
/// `IssueGraph` answers the same question one issue at a time, which is the
/// right shape for a detail rail and the wrong one for a chart: drawing a
/// project's *sequence* needs every edge at once, and asking per issue is N
/// round trips for a graph the catalog already holds whole.
///
/// Scoped to one project and to live issues, because an edge with a
/// tombstoned or foreign end cannot be drawn and shipping it would only make
/// the client filter what the catalog already knows.
///
/// No transitive closure here, deliberately — unlike `GraphView::blocked_by`,
/// which computes one because a single issue wants to know everything standing
/// in its way. Given the direct edges the client can derive reachability itself,
/// and a transitive edge set drawn as connectors is unreadable: it draws the
/// shortcut across a chain as though it were a separate constraint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectGraphView {
    pub schema_version: u32,
    /// The project's id, echoed so a late reply can be matched to its request.
    pub project: String,
    /// Direct edges whose ends are both live issues of this project.
    pub edges: Vec<GraphEdgeDto>,
    /// `(child, parent)` for the sub-issue tree, same scoping as `edges`.
    pub parents: Vec<(String, String)>,
}

/// A disambiguation candidate when a reference resolves to multiple issues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub reff: String,
    pub key_alias: Option<String>,
    pub title: String,
}

/// One inbox item — a remote change **addressed to you**, derived at sync-import
/// time and persisted locally in `inbox.json`. Attribution remains conservative:
/// `actor_nick` is present only for comments (the one in-doc field that carries
/// a real author); assignment/status changes render actor-unknown rather than
/// guessing (S non-goal 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxEntry {
    /// Local receive time (Unix seconds), used as the advisory read-watermark axis.
    pub ts: u64,
    /// `assigned` | `comment` | `status`.
    pub kind: String,
    pub reff: String,
    pub doc_id: String,
    pub title: String,
    /// One human line: the comment body, or the status transition.
    pub detail: String,
    /// The attributed author's key (comments only — the one in-doc field with a
    /// real author; `None` = actor unknown). Durable truth in `inbox.json`.
    #[serde(default)]
    pub actor: Option<String>,
    /// The author's display nick, resolved by the daemon at read time from its
    /// live directory (presence nicks + local petnames). Never persisted.
    #[serde(default)]
    pub actor_nick: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_roundtrips() {
        for p in [
            Priority::None,
            Priority::Low,
            Priority::Medium,
            Priority::High,
            Priority::Urgent,
        ] {
            assert_eq!(Priority::parse(p.as_str()), Some(p));
        }
        assert_eq!(Priority::parse("U"), Some(Priority::Urgent));
        assert_eq!(Priority::parse("h"), Some(Priority::High));
        assert_eq!(Priority::parse("bogus"), None);
    }

    #[test]
    fn priority_orders_low_to_high() {
        assert!(Priority::Urgent > Priority::High);
        assert!(Priority::High > Priority::Low);
    }

    #[test]
    fn default_workflow_has_one_done_column() {
        let wf = default_workflow();
        assert_eq!(
            wf.iter()
                .filter(|w| w.category == StatusCategory::Done)
                .count(),
            1
        );
        assert!(wf.iter().any(|w| w.id == DEFAULT_STATUS));
    }

    #[test]
    fn priority_json_is_lowercase() {
        assert_eq!(
            serde_json::to_string(&Priority::Urgent).unwrap(),
            "\"urgent\""
        );
    }
}

// ----------------------------------------------------------------------------
// Doorbell planes (the dirty-set vocabulary, shared by the World and clients)
// ----------------------------------------------------------------------------

/// The project a dirty plane belongs to, named twice on purpose.
///
/// `id` is the stable identity a dependency matches on; `key` is a mutable
/// display alias that a rename changes underneath you. Matching on the key was
/// a latent bug waiting for the first `project edit --key`, and dropping the key
/// would force every client to resolve one before it could route or label. So
/// both travel together, with the roles stated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectRef {
    pub project_id: String,
    pub project_key: String,
}

/// Identifies which catalog structure became dirty.
///
/// One variant per plane of the catalog Body, because that Body holds every
/// structure the space has and a client should re-read only the one that moved.
/// The variants carrying a [`ProjectRef`] are planes the catalog groups by
/// project, so editing one project's milestones leaves another's alone.
///
/// Membership is deliberately absent: it is not in the catalog at all, and rings
/// from the doorbell's own authority flag rather than pretending to be a catalog
/// plane.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum CatalogScope {
    /// The space's own name and description.
    Space,
    Projects,
    Labels,
    /// The workflow states and their revision log.
    Workflow,
    Boards {
        project_id: String,
        project_key: String,
    },
    Milestones {
        project_id: String,
        project_key: String,
    },
    Cycles {
        project_id: String,
        project_key: String,
    },
    /// The project status-update feed.
    Updates {
        project_id: String,
        project_key: String,
    },
    Initiatives,
    Teams,
    Triage,
    Roles,
    /// Specs and Baselines — the only plane whose contents are not in the
    /// catalog at all. They are Bodies of their own, so this digests their
    /// version stamps rather than a region of `CatalogState`.
    Specs,
    /// The row index: which docs exist, their aliases and seqs, what is deleted.
    Docs,
    /// Issue links and parentage.
    Relations,
}
