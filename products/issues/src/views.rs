#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    clippy::string_slice,
    reason = "view layouts operate on validated product records and ASCII canonical identifiers"
)]
//! Parsed catalog/issue state and the legacy-shape projections (C4.2).
//!
//! `CatalogState`/`IssueState` decode the collaborative Body views into typed
//! state; the projection builders reproduce the legacy DTO shapes (schema
//! version 3) byte-for-byte, including alias derivation (`KEY-n` with base-26
//! collision suffixes and shortest-unique canonical `iss_` prefixes).

use std::collections::{BTreeMap, BTreeSet};

use fabric::CollaborativeView;
use serde::{Deserialize, Serialize};

use crate::dto::{
    BoardColumn, BoardView, CheckDto, CommentAnchorDto, CommentDto, CorruptRecord, IssueView,
    LabelDto, Priority, ProjectDto, Row, StatusCategory, WorkflowState,
};
use crate::ids::{ActorId, DocId, LabelId, ProjectId};

use super::contract::{
    CheckRecord, IssueEvent, StoredComment, DEFAULT_STATUS, VIEW_SCHEMA_VERSION,
};

const CANONICAL_MIN: usize = 7;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub name: String,
    pub key: String,
    pub color: String,
    /// The overview document — freeform markdown. Additive: projects minted
    /// before this field decode with an empty string.
    #[serde(default)]
    pub description: String,
    /// The project lead's actor key (empty = none).
    #[serde(default)]
    pub lead: String,
    /// Planned window, unix seconds (None = unset).
    #[serde(default)]
    pub start_date: Option<u64>,
    #[serde(default)]
    pub target_date: Option<u64>,
    /// Soft-hidden from pickers, default selection, and all-project lists — but
    /// still resolvable by id/KEY (so a direct link opens it) with its aliases
    /// intact. Additive: pre-archive projects decode as live. See CUSTOM-9.
    #[serde(default)]
    pub archived: bool,
    /// The owning team's id (empty = none). Additive (GOV-7).
    #[serde(default)]
    pub team: String,
}

/// One project status update — an immutable post in the project's updates feed
/// (SCOPE-1). Stored as a grow-only catalog log (`project_updates` keyed
/// `<project>/<id>`), mirroring how workflow revisions and roles are logged
/// rather than introducing a per-project collaborative Body: an update is
/// authored once and never edited, so a record is the honest shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectUpdate {
    pub id: String,
    pub project_id: String,
    /// The authoring actor key.
    pub author: String,
    /// Post time, unix seconds.
    pub ts: u64,
    pub body: String,
    /// `on_track` | `at_risk` | `off_track` | "" (none). A self-reported health
    /// signal, free of any derived-metric coupling.
    #[serde(default)]
    pub health: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LabelMeta {
    pub name: String,
    pub color: String,
}

/// One project milestone — an editable record in the Catalog's
/// `project_milestones` map (keyed `<project>/<milestone>`), LWW per record
/// like `projects` (milestones are renamed and retargeted; the whole record is
/// rewritten on edit so untouched fields never drop). Progress is derived
/// from issues' `milestone` registers, never stored (SCOPE-1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Milestone {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// The milestone's prose — what this stage is, in the project document.
    ///
    /// A catalog register on the record, like a project's description and for
    /// the same reason: it is an overview paragraph, not a wiki. The cost is
    /// stated plainly — the whole string is last-writer-wins, so two people
    /// editing one milestone's body at once keep one of the two versions. A
    /// per-milestone collaborative doc would fix that and would be a new doc
    /// type for prose nobody co-edits live.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub target_date: Option<u64>,
    /// Fractional index (`world::rank`), the project's manual milestone order.
    ///
    /// Additive: records written before ordering existed decode with `""`, and
    /// the first milestone write in a project backfills every one of them from
    /// the order they were already being read in — so a project is never half
    /// ordered by hand and half by date.
    #[serde(default)]
    pub rank: String,
    #[serde(default)]
    pub tombstone: bool,
}

/// One cycle (time-boxed iteration) — an editable record in the Catalog's
/// `cycles` map (keyed `<project>/<cycle>`), same LWW-record shape as
/// milestones (BOARD-11).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cycle {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// The box, unix seconds (0 = unset — a named backlog bucket).
    #[serde(default)]
    pub start: u64,
    #[serde(default)]
    pub end: u64,
    #[serde(default)]
    pub tombstone: bool,
}

/// One initiative — the strategic layer above projects (SCOPE-8): a named
/// goal grouping several projects, with owner/health/target date. Progress is
/// derived from the member projects' issues, never stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Initiative {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Owner actor key (empty = none).
    #[serde(default)]
    pub owner: String,
    /// `on_track` | `at_risk` | `off_track` | "" — self-reported, like
    /// project-update health.
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub target_date: Option<u64>,
    /// Ordered member project ids.
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub tombstone: bool,
}

/// One team — a durable work-owning group (GOV-7). Team membership is
/// product-level (actor keys), managed independently of the space ACL:
/// belonging to a team confers no authority, and the ACL confers no team.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    /// Short uppercase handle, immutable after creation (like a project key).
    pub key: String,
    #[serde(default)]
    pub icon: String,
    /// Lead actor key (empty = none).
    #[serde(default)]
    pub lead: String,
    /// Member actor keys, sorted.
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub tombstone: bool,
}

/// One triage-intake item (SCOPE-7): reported work reviewed BEFORE it enters
/// a project's workflow. Catalog-level (submission needs no project), decided
/// exactly once — the outcome fields are written by the review intent and the
/// record is never edited afterwards.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageItem {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Where this came from (free text: "cli", an integration name, …).
    #[serde(default)]
    pub source: String,
    /// The submitting actor key.
    pub submitted_by: String,
    pub ts: u64,
    /// "" (pending) | `accepted` | `declined` | `duplicate`.
    #[serde(default)]
    pub outcome: String,
    /// The issue the item became (accepted) or duplicates (duplicate).
    #[serde(default)]
    pub doc: String,
    #[serde(default)]
    pub decided_by: String,
    #[serde(default)]
    pub decided_ts: u64,
    #[serde(default)]
    pub note: String,
}

/// The parsed catalog Body.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogState {
    pub name: String,
    /// The space's overview/description — a plain catalog register beside `name`
    /// (SCOPE-2). Additive: a space that predates it decodes as empty.
    pub description: String,
    pub projects: BTreeMap<String, ProjectMeta>,
    pub labels: BTreeMap<String, LabelMeta>,
    pub workflow: Vec<WorkflowState>,
    /// Per-project alias seq high-water.
    pub aliases: BTreeMap<String, u32>,
    /// Per-issue seq.
    pub seqs: BTreeMap<String, u32>,
    pub tombstones: BTreeSet<String>,
    /// `(from, kind, to)` link edges.
    pub edges: BTreeSet<(String, String, String)>,
    /// child doc -> parent doc.
    pub parents: BTreeMap<String, String>,
    /// project id -> ordered `(stable element id, doc id)` board entries.
    pub boards: BTreeMap<String, Vec<(String, String)>>,
    /// project id -> grow-only workflow revision log (every revision ever
    /// committed; heads are revisions no successor names as a predecessor).
    pub workflow_revisions: BTreeMap<String, Vec<crate::workflow::WorkflowRevision>>,
    /// role id -> the immutable BUILT-IN definition (seeded at formation).
    pub roles: BTreeMap<String, StoredRoleRevision>,
    /// role id -> grow-only custom-role revision log.
    pub role_revisions: BTreeMap<String, Vec<StoredRoleRevision>>,
    /// project id -> grow-only status-update log (SCOPE-1 updates feed).
    pub project_updates: BTreeMap<String, Vec<ProjectUpdate>>,
    /// project id -> milestone id -> milestone (SCOPE-1).
    pub milestones: BTreeMap<String, BTreeMap<String, Milestone>>,
    /// project id -> cycle id -> cycle (BOARD-11).
    pub cycles: BTreeMap<String, BTreeMap<String, Cycle>>,
    /// initiative id -> initiative (SCOPE-8).
    pub initiatives: BTreeMap<String, Initiative>,
    /// team id -> team (GOV-7).
    pub teams: BTreeMap<String, Team>,
    /// triage-intake id -> item (SCOPE-7).
    pub triage: BTreeMap<String, TriageItem>,
}

/// Parsed state of one project-owned topology Body. Boolean edge values make
/// removal override an edge inherited from the legacy Catalog representation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelationState {
    pub edges: BTreeMap<(String, String, String), bool>,
    pub parents: BTreeMap<String, Option<String>>,
}

impl RelationState {
    pub fn from_view(view: &CollaborativeView) -> Self {
        let mut state = Self::default();
        if let Some(edges) = view.maps.get("edges") {
            for (key, raw) in edges {
                let mut parts = key.splitn(3, '|');
                let (Some(from), Some(kind), Some(to)) = (parts.next(), parts.next(), parts.next())
                else {
                    continue;
                };
                state.edges.insert(
                    (from.into(), kind.into(), to.into()),
                    raw.as_slice() == b"1",
                );
            }
        }
        let Some(nodes) = view.trees.get(super::contract::HIERARCHY_PATH) else {
            return state;
        };
        let by_node: BTreeMap<&str, &str> = nodes
            .iter()
            .filter_map(|node| Some((node.node.as_str(), node.anchor.as_deref()?)))
            .collect();
        for node in nodes {
            let Some(child) = node.anchor.as_deref() else {
                continue;
            };
            let parent = node
                .parent
                .as_deref()
                .and_then(|parent| by_node.get(parent))
                .map(|parent| (*parent).to_string());
            state.parents.insert(child.into(), parent);
        }
        state
    }

    pub fn apply_to(&self, catalog: &mut CatalogState) {
        for (edge, present) in &self.edges {
            if *present {
                catalog.edges.insert(edge.clone());
            } else {
                catalog.edges.remove(edge);
            }
        }
        for (child, parent) in &self.parents {
            if let Some(parent) = parent {
                catalog.parents.insert(child.clone(), parent.clone());
            } else {
                catalog.parents.remove(child);
            }
        }
    }
}

/// A role revision as stored in the catalog `roles` map: hex revision id,
/// predecessors, and the canonical body.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StoredRoleRevision {
    pub revision_id: String,
    #[serde(default)]
    pub predecessor_ids: Vec<String>,
    pub body: crate::roles::RoleBody,
}

fn reg_str(view: &CollaborativeView, path: &str) -> Option<String> {
    view.registers
        .get(path)
        .map(|b| String::from_utf8_lossy(b).into_owned())
}

fn map_str(view: &CollaborativeView, path: &str) -> BTreeMap<String, String> {
    view.maps
        .get(path)
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.clone(), String::from_utf8_lossy(v).into_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// An issue's history, from both places it can live, and how much of it there
/// has ever been.
///
/// Ordered by `(t, position in the converged sequence)`, which is what the
/// activity feed already sorted by and is now the only ordering: an append
/// lands at the end of the writing replica's own view, so a peer that is behind
/// appends into the middle of the sequence, and only the clock each event
/// carries puts the history back in the order it happened. The sequence
/// position stays as the tie-break, because events inside one second are
/// ordinarily one transaction's worth in the order they were staged, and the
/// entry id is random — breaking that tie on identity would shuffle "created"
/// after "assigned" for no reason.
///
/// The second return is the count of everything ever recorded, trimmed rows
/// included. `list:events` — the pre-log home, read forever — contributes both
/// its rows and its length, so an issue that spans the cutover reports one
/// history and one honest total.
fn read_events(view: &CollaborativeView) -> (Vec<IssueEvent>, u64) {
    let mut events: Vec<(u64, usize, IssueEvent)> = Vec::new();
    let mut recorded: u64 = 0;
    if let Some(legacy) = view.lists.get("events") {
        recorded = recorded.saturating_add(legacy.len() as u64);
        for (position, element) in legacy.iter().enumerate() {
            if let Ok(mut event) = serde_json::from_slice::<IssueEvent>(&element.value) {
                event.entry = element.element.clone();
                events.push((event.t, position, event));
            }
        }
    }
    if let Some(log) = view.logs.get(super::contract::EVENTS_PATH) {
        recorded = recorded.saturating_add(log.appended);
        // Legacy rows sort first within a tie because they are older than any
        // logged one — nothing has written the list since the cutover — so the
        // logged positions continue past the end of it.
        let offset = view.lists.get("events").map_or(0, Vec::len);
        for (position, element) in log.entries.iter().enumerate() {
            if let Ok(mut event) = serde_json::from_slice::<IssueEvent>(&element.value) {
                event.entry = element.element.clone();
                events.push((event.t, offset.saturating_add(position), event));
            }
        }
    }
    events.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    let ordered: Vec<IssueEvent> = events.into_iter().map(|(_, _, e)| e).collect();
    // A trimmed log reports more recorded than retained, which is the point.
    // The reverse cannot happen, but clamping keeps a malformed Body from
    // producing a count smaller than the rows it hands back.
    let recorded = recorded.max(ordered.len() as u64);
    (ordered, recorded)
}

/// The sub-issue hierarchy as child -> parent, from both places it can live.
///
/// The tree is authoritative and the legacy `map:parents` fills in only for
/// children the tree says nothing about. That order matters and is not
/// symmetric: an issue re-parented since the cutover has a stale entry in the
/// map — the map was never rewritten — and letting it win would put the issue
/// back under the parent it was moved out of.
///
/// A node with no anchor is skipped rather than guessed at. Anchors are how
/// this hierarchy names issues; a node without one was not placed by this
/// product.
fn read_hierarchy(view: &CollaborativeView) -> BTreeMap<String, String> {
    let mut parents = BTreeMap::new();
    for (child, parent) in map_str(view, "parents") {
        if !parent.is_empty() {
            parents.insert(child, parent);
        }
    }
    let Some(nodes) = view.trees.get(super::contract::HIERARCHY_PATH) else {
        return parents;
    };
    let by_node: BTreeMap<&str, &str> = nodes
        .iter()
        .filter_map(|n| Some((n.node.as_str(), n.anchor.as_deref()?)))
        .collect();
    for node in nodes {
        let Some(child) = node.anchor.as_deref() else {
            continue;
        };
        match node.parent.as_deref().and_then(|p| by_node.get(p)) {
            Some(parent) => {
                parents.insert(child.to_string(), (*parent).to_string());
            }
            // A root of the forest is an issue with no parent, and it says so
            // over any legacy entry: unparenting moves the node to a root and
            // writes nothing to the map.
            None => {
                parents.remove(child);
            }
        }
    }
    parents
}

/// An issue's reactions, from both places they can live, keyed by comment.
///
/// Every reaction on the issue is one member of `set:reactions`, naming its
/// comment in the value. Reactions written before that collapse each live in a
/// `reactions/<comment>` set of their own and are read forever — they are in
/// Bodies in the field, and nothing writes that shape any more.
///
/// The two merge per comment rather than one shadowing the other, because a
/// comment reacted to on both sides of the cutover has members in both places
/// and either alone would under-report it. `sort`/`dedup` is what makes that
/// safe: the same `(emoji, actor)` pair recorded in both sets is one reaction,
/// not two.
fn read_reactions(view: &CollaborativeView) -> BTreeMap<String, Vec<(String, String)>> {
    let mut reactions: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for (path, values) in &view.sets {
        if path == super::contract::REACTIONS_PATH {
            for value in values {
                if let Some((comment, emoji, actor)) = super::contract::parse_reaction_value(value)
                {
                    reactions.entry(comment).or_default().push((emoji, actor));
                }
            }
            continue;
        }
        let Some(comment) = path.strip_prefix("reactions/") else {
            continue;
        };
        for value in values {
            if let Some(pair) = super::contract::parse_legacy_reaction_value(value) {
                reactions.entry(comment.to_string()).or_default().push(pair);
            }
        }
    }
    for pairs in reactions.values_mut() {
        pairs.sort();
        pairs.dedup();
    }
    reactions.retain(|_, pairs| !pairs.is_empty());
    reactions
}

/// An issue's comments, from both places they can live, in one chronological
/// order.
///
/// **The hierarchy is the tree; the order is the clock.** A thread is written
/// as `tree:comments` — a reply is a child of what it answers, which is what
/// gives concurrent replies and re-parenting a defined outcome. But sibling
/// order in any sequence CRDT is *placement* order, and placement is a
/// statement about the writing replica's own view: a peer that appends while
/// fifty comments behind places its comment fifty back. So the order this
/// returns is `(t, id)` from the records themselves, which every replica
/// computes identically no matter what it had synced when it wrote.
///
/// `list:comments` is where comments lived before the tree. Nothing writes it
/// any more and those records are read forever — they are in Bodies in the
/// field, and a reader that dropped them would lose conversations rather than
/// migrate them. They sort into the same order by the same clock, so a thread
/// that spans the cutover reads as one thread.
///
/// A legacy record's `parent` field still names its reply target; a tree
/// record's parent edge is authoritative and its `parent` field is written too,
/// so a peer on an older build reads the same threading out of the same bytes.
fn read_comments(view: &CollaborativeView) -> Vec<StoredComment> {
    let legacy = view
        .lists
        .get("comments")
        .into_iter()
        .flatten()
        .filter_map(|e| {
            serde_json::from_slice::<StoredComment>(&e.value)
                .ok()
                .map(|record| (None, None, record))
        });
    // The node id is carried out of the projection so a later write — a reply
    // filed under this comment, an entry set on it — can name it without
    // re-deriving it from the record.
    let threaded = view
        .trees
        .get("comments")
        .into_iter()
        .flatten()
        .filter_map(|node| {
            serde_json::from_slice::<StoredComment>(&node.value)
                .ok()
                .map(|record| (Some(node.node.clone()), node.parent.clone(), record))
        });
    let mut comments: Vec<StoredComment> = legacy
        .chain(threaded)
        .map(|(node, parent_node, mut record)| {
            record.node = node;
            record.parent_node = parent_node;
            record
        })
        .collect();
    // `id` breaks a tie between two comments written in the same millisecond;
    // ULIDs are time-ordered, so the tiebreak is itself chronological. A
    // pre-identity record has no id and sorts before one that has, which is
    // where such records belong — nothing has written one since v0.6.
    comments.sort_by(|a, b| a.t.cmp(&b.t).then_with(|| a.id.cmp(&b.id)));
    comments
}

impl CatalogState {
    pub fn from_view(view: Option<&CollaborativeView>) -> Self {
        let Some(view) = view else {
            return Self::default();
        };
        let mut state = Self {
            name: reg_str(view, "name").unwrap_or_default(),
            description: reg_str(view, "description").unwrap_or_default(),
            ..Self::default()
        };
        for (id, raw) in map_str(view, "projects") {
            if let Ok(meta) = serde_json::from_str::<ProjectMeta>(&raw) {
                state.projects.insert(id, meta);
            }
        }
        for (id, raw) in map_str(view, "labels") {
            if let Ok(meta) = serde_json::from_str::<LabelMeta>(&raw) {
                state.labels.insert(id, meta);
            }
        }
        if let Some(list) = view.lists.get("workflow") {
            for element in list {
                if let Ok(ws) = serde_json::from_slice::<WorkflowState>(&element.value) {
                    state.workflow.push(ws);
                }
            }
        }
        if state.workflow.is_empty() {
            state.workflow = default_workflow_states();
        }
        for (key, raw) in map_str(view, "workflow_revisions") {
            // Key: `<project>/<revision hex>` — grow-only log entries.
            let Some((project, _hex)) = key.rsplit_once('/') else {
                continue;
            };
            if let Ok(rev) = serde_json::from_str::<crate::workflow::WorkflowRevision>(&raw) {
                state
                    .workflow_revisions
                    .entry(project.to_string())
                    .or_default()
                    .push(rev);
            }
        }
        for (key, raw) in map_str(view, "project_updates") {
            // Key: `<project>/<update id>` — grow-only log entries.
            let Some((project, _id)) = key.rsplit_once('/') else {
                continue;
            };
            if let Ok(update) = serde_json::from_str::<ProjectUpdate>(&raw) {
                state
                    .project_updates
                    .entry(project.to_string())
                    .or_default()
                    .push(update);
            }
        }
        for (key, raw) in map_str(view, "project_milestones") {
            let Some((project, _id)) = key.rsplit_once('/') else {
                continue;
            };
            if let Ok(m) = serde_json::from_str::<Milestone>(&raw) {
                state
                    .milestones
                    .entry(project.to_string())
                    .or_default()
                    .insert(m.id.clone(), m);
            }
        }
        for (key, raw) in map_str(view, "cycles") {
            let Some((project, _id)) = key.rsplit_once('/') else {
                continue;
            };
            if let Ok(c) = serde_json::from_str::<Cycle>(&raw) {
                state
                    .cycles
                    .entry(project.to_string())
                    .or_default()
                    .insert(c.id.clone(), c);
            }
        }
        for (id, raw) in map_str(view, "initiatives") {
            if let Ok(i) = serde_json::from_str::<Initiative>(&raw) {
                state.initiatives.insert(id, i);
            }
        }
        for (id, raw) in map_str(view, "teams") {
            if let Ok(t) = serde_json::from_str::<Team>(&raw) {
                state.teams.insert(id, t);
            }
        }
        for (id, raw) in map_str(view, "triage") {
            if let Ok(t) = serde_json::from_str::<TriageItem>(&raw) {
                state.triage.insert(id, t);
            }
        }
        for (id, raw) in map_str(view, "roles") {
            if let Ok(rev) = serde_json::from_str::<StoredRoleRevision>(&raw) {
                state.roles.insert(id, rev);
            }
        }
        for (key, raw) in map_str(view, "role_revisions") {
            let Some((role, _hex)) = key.rsplit_once('/') else {
                continue;
            };
            if let Ok(rev) = serde_json::from_str::<StoredRoleRevision>(&raw) {
                state
                    .role_revisions
                    .entry(role.to_string())
                    .or_default()
                    .push(rev);
            }
        }
        for (project, raw) in map_str(view, "aliases") {
            if let Ok(n) = raw.parse() {
                state.aliases.insert(project, n);
            }
        }
        for (doc, raw) in map_str(view, "seqs") {
            if let Ok(n) = raw.parse() {
                state.seqs.insert(doc, n);
            }
        }
        for (doc, raw) in map_str(view, "tombstones") {
            if raw == "1" {
                state.tombstones.insert(doc);
            }
        }
        if let Some(m) = view.maps.get("edges") {
            for key in m.keys() {
                let mut parts = key.splitn(3, '|');
                if let (Some(f), Some(k), Some(t)) = (parts.next(), parts.next(), parts.next()) {
                    state
                        .edges
                        .insert((f.to_string(), k.to_string(), t.to_string()));
                }
            }
        }
        state.parents = read_hierarchy(view);
        for (path, list) in &view.lists {
            if let Some(project_lower) = path.strip_prefix("board/") {
                // Board paths carry the lowercased project id; recover the
                // real id from the project set.
                let project = state
                    .projects
                    .keys()
                    .find(|p| p.to_ascii_lowercase() == project_lower)
                    .cloned()
                    .unwrap_or_else(|| project_lower.to_string());
                state.boards.insert(
                    project,
                    list.iter()
                        .map(|e| {
                            (
                                e.element.clone(),
                                String::from_utf8_lossy(&e.value).into_owned(),
                            )
                        })
                        .collect(),
                );
            }
        }
        state
    }

    /// Every known issue DocId (everything that ever got a seq).
    pub fn doc_ids(&self) -> Vec<String> {
        self.seqs.keys().cloned().collect()
    }

    /// Resolve a state from the project's sole workflow revision head.
    ///
    /// The Space-wide `workflow` vector is a v3 migration source only.  It is
    /// deliberately not a fallback here: doing so would let two collaborators
    /// observe different transition and completion semantics for the same
    /// project while a workflow revision is absent or conflicted.
    pub fn workflow_state(
        &self,
        project: &str,
        id: &str,
    ) -> Result<Option<&crate::workflow::WorkflowState>, WorkflowResolutionError> {
        Ok(self
            .resolved_workflow(project)?
            .body
            .states
            .iter()
            .find(|state| state.state_id == id))
    }

    pub fn first_state_in(
        &self,
        project: &str,
        category: StatusCategory,
    ) -> Result<Option<&crate::workflow::WorkflowState>, WorkflowResolutionError> {
        Ok(self
            .resolved_workflow(project)?
            .body
            .states
            .iter()
            .find(|state| StatusCategory::parse(&state.category) == Some(category)))
    }

    pub fn status_category(
        &self,
        project: &str,
        status: &str,
    ) -> Result<Option<StatusCategory>, WorkflowResolutionError> {
        Ok(self
            .workflow_state(project, status)?
            .and_then(|state| StatusCategory::parse(&state.category)))
    }
}

/// The parsed issue Body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueState {
    pub project: String,
    pub title: String,
    pub status: String,
    pub priority: Priority,
    pub created_by: Option<ActorId>,
    pub created_at: u64,
    pub description: String,
    /// Zero means a body written before the hidden Lait document model. The
    /// register is additive, so old replicas and old Bodies remain readable.
    pub document_schema: u32,
    /// Unix seconds; absent register = no due date.
    pub duedate: Option<u64>,
    pub estimate: Option<u32>,
    pub assignees: Vec<ActorId>,
    /// Subscribed actors, independent of assignment (INBOX-9): an add-wins
    /// set mirroring `assignees` storage.
    pub followers: Vec<ActorId>,
    /// The milestone this issue targets (empty register = none; SCOPE-1).
    pub milestone: Option<String>,
    /// The cycle this issue is scheduled in (BOARD-11).
    pub cycle: Option<String>,
    /// Exact issued Baseline revision governing this work, when pinned.
    pub baseline: Option<crate::spec::BaselineRef>,
    pub labels: Vec<String>,
    pub comments: Vec<StoredComment>,
    /// comment id -> sorted `(emoji, actor)` pairs, parsed from the
    /// `reactions/<comment id>` sets. Malformed values are dropped, not
    /// surfaced — a reaction is not worth a corrupt-record row.
    pub reactions: BTreeMap<String, Vec<(String, String)>>,
    /// Attachment records, metadata only — the base64 payload stays in the
    /// Body map and is served solely by the `Attachment` query, so the
    /// derived-snapshot cache never holds file bytes (CREATE-5).
    pub attachments: Vec<AttachmentMeta>,
    /// Product-owned issue-to-Run bindings, sorted by Run id.
    pub checks: Vec<(String, CheckRecord)>,
    /// Malformed check records remain visible rather than raising the cap while
    /// disappearing from the projection.
    pub check_corrupt_records: Vec<CorruptRecord>,
    pub events: Vec<IssueEvent>,
    /// How many events this issue has ever recorded, including any the log has
    /// trimmed out of state. Never `events.len()` — that is what survived, and
    /// a reader that conflated the two would renumber the whole history the
    /// first time an issue got busy enough to trim.
    pub events_recorded: u64,
}

/// The metadata half of a stored attachment record — everything except
/// `data_b64` (serde ignores it on decode, which is the point).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub mime: String,
    /// Raw (decoded) size in bytes.
    #[serde(default)]
    pub size: u64,
    /// The attaching actor key.
    #[serde(default)]
    pub by: String,
    #[serde(default)]
    pub ts: u64,
    /// The comment this file rode with, when any.
    #[serde(default)]
    pub comment: String,
}

impl IssueState {
    /// The current text of a field a comment may attach a span to.
    ///
    /// THE list of anchorable paths, and the reason it is a list rather than a
    /// pass-through: [`runtime::world::BodyReader::anchor_in_body`] mints an anchor
    /// for **any** path on a collaborative Body, including one that names a
    /// register and one no operation ever wrote. There is no text at such a
    /// path, so the anchor binds to no operation, and resolving it answers
    /// position zero forever and never reports drift — a well-typed lie, and
    /// the exact failure `AnchorResolution` is shaped to prevent everywhere
    /// else.
    ///
    /// So an issue's anchorable surface is not "its fields": it is the fields
    /// this build writes with a text operation. `title`, `status`, `priority`,
    /// `duedate` and the rest are registers — atomic values, replaced whole,
    /// with no positions inside them for the algebra to move. A comment
    /// attached to one is not a comment the algebra can keep pointing at, and
    /// it is refused at the seam rather than stored as an anchor nothing can
    /// resolve.
    pub fn anchorable_text(&self, field: &str) -> Option<&str> {
        match field {
            "description" => Some(self.description.as_str()),
            _ => None,
        }
    }

    pub fn from_view(view: &CollaborativeView) -> Self {
        let (checks, check_corrupt_records) = read_checks(view);
        let mut assignees: Vec<ActorId> = view
            .sets
            .get("assignees")
            .map(|s| {
                s.iter()
                    .filter_map(|v| ActorId::parse(&String::from_utf8_lossy(v)))
                    .collect()
            })
            .unwrap_or_default();
        assignees.sort();
        let mut followers: Vec<ActorId> = view
            .sets
            .get("followers")
            .map(|s| {
                s.iter()
                    .filter_map(|v| ActorId::parse(&String::from_utf8_lossy(v)))
                    .collect()
            })
            .unwrap_or_default();
        followers.sort();
        let mut attachments: Vec<AttachmentMeta> = view
            .maps
            .get("attachments")
            .map(|m| {
                m.values()
                    .filter_map(|v| serde_json::from_slice::<AttachmentMeta>(v).ok())
                    .collect()
            })
            .unwrap_or_default();
        attachments.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
        let mut labels: Vec<String> = view
            .sets
            .get("labels")
            .map(|s| {
                s.iter()
                    .map(|v| String::from_utf8_lossy(v).into_owned())
                    .collect()
            })
            .unwrap_or_default();
        labels.sort();
        let comments = read_comments(view);
        let (events, events_recorded) = read_events(view);
        let reactions = read_reactions(view);
        let mut state = Self {
            project: reg_str(view, "projectid").unwrap_or_default(),
            title: reg_str(view, "title").unwrap_or_default(),
            status: reg_str(view, "status").unwrap_or_else(|| DEFAULT_STATUS.to_string()),
            priority: Priority::parse(&reg_str(view, "priority").unwrap_or_default())
                .unwrap_or(Priority::None),
            created_by: reg_str(view, "createdby").and_then(|s| ActorId::parse(&s)),
            created_at: reg_str(view, "createdat")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            description: view.texts.get("description").cloned().unwrap_or_default(),
            document_schema: reg_str(view, "document_schema")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0),
            duedate: reg_str(view, "duedate").and_then(|s| s.parse().ok()),
            estimate: reg_str(view, "estimate").and_then(|s| s.parse().ok()),
            assignees,
            followers,
            milestone: reg_str(view, "milestone").filter(|m| !m.is_empty()),
            cycle: reg_str(view, "cycle").filter(|c| !c.is_empty()),
            baseline: reg_str(view, "baseline").and_then(|raw| serde_json::from_str(&raw).ok()),
            labels,
            comments,
            reactions,
            attachments,
            checks,
            check_corrupt_records,
            events,
            events_recorded,
        };
        if let Some(raw) = view.registers.get(crate::v4::roots::BOARD_PLACEMENT) {
            if let Ok(placement) =
                <crate::v4::BoardPlacement as crate::v4::CanonicalRecord>::decode_canonical(raw)
            {
                state.project = placement.project;
                state.status = placement.workflow_state;
            }
        }
        state
    }
}

fn exact_lower_hex(raw: &str, bytes: usize) -> bool {
    raw.len() == bytes.saturating_mul(2)
        && data_encoding::HEXLOWER
            .decode(raw.as_bytes())
            .is_ok_and(|decoded| decoded.len() == bytes)
}

fn read_checks(view: &CollaborativeView) -> (Vec<(String, CheckRecord)>, Vec<CorruptRecord>) {
    let mut checks = Vec::new();
    let mut corrupt = Vec::new();
    for (run, raw) in view.maps.get("checks").into_iter().flatten() {
        let parsed = serde_json::from_slice::<CheckRecord>(raw).ok();
        let valid = parsed.as_ref().is_some_and(|record| {
            exact_lower_hex(run, 16)
                && replica::body::SchemaId::parse(&record.spec).is_some()
                && record.v > 0
                && exact_lower_hex(&record.build, 32)
                && exact_lower_hex(&record.source, 32)
                && !record.state.is_empty()
                && record.state.len() <= 32
                && ActorId::parse(&record.by).is_some()
                && record.ts > 0
                && record
                    .attempt
                    .as_deref()
                    .is_none_or(|attempt| exact_lower_hex(attempt, 16))
                && record
                    .report
                    .as_deref()
                    .is_none_or(|report| exact_lower_hex(report, 32))
                && record
                    .verdict
                    .as_deref()
                    .is_none_or(|verdict| matches!(verdict, "pass" | "fail"))
        });
        if valid {
            checks.push((run.clone(), parsed.expect("validated present check")));
        } else {
            corrupt.push(
                CorruptRecord::new(format!("checks[{run}]"), "invalid check record")
                    .with_raw("run", run)
                    .with_raw("value", String::from_utf8_lossy(raw)),
            );
        }
    }
    checks.sort_by(|left, right| left.0.cmp(&right.0));
    (checks, corrupt)
}

/// The derived alias table for one catalog + doc set (deterministic; the
/// legacy `AliasTable` semantics).
#[derive(Debug, Clone, Default)]
pub struct DerivedAliases {
    pub by_doc: BTreeMap<String, String>,
    pub by_alias: BTreeMap<String, String>,
    pub canonical: BTreeMap<String, String>,
}

fn lcp_len(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

/// `1 -> "b", 2 -> "c", …, 25 -> "z", 26 -> "aa"` collision suffix (legacy).
fn collision_suffix(i: usize) -> String {
    let mut n = i;
    let mut s = String::new();
    let alphabet = b"abcdefghijklmnopqrstuvwxyz";
    loop {
        let rem = n % 26;
        s.insert(0, alphabet[rem] as char);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    s
}

pub fn derive_aliases<'a>(
    catalog: &CatalogState,
    project_of: impl Fn(&str) -> Option<&'a str>,
) -> DerivedAliases {
    let mut out = DerivedAliases::default();
    let mut docs: Vec<String> = catalog.doc_ids();
    docs.sort();
    // Canonical: shortest prefix (≥ CANONICAL_MIN) unshared with neighbours.
    for (i, doc) in docs.iter().enumerate() {
        let Some(ulid) = doc.strip_prefix(DocId::PREFIX) else {
            continue;
        };
        let lp = if i > 0 {
            docs[i - 1]
                .strip_prefix(DocId::PREFIX)
                .map(|p| lcp_len(ulid, p))
                .unwrap_or(0)
        } else {
            0
        };
        let ls = docs
            .get(i + 1)
            .and_then(|s| s.strip_prefix(DocId::PREFIX))
            .map(|s| lcp_len(ulid, s))
            .unwrap_or(0);
        let k = (lp.max(ls) + 1).clamp(CANONICAL_MIN, ulid.len());
        out.canonical
            .insert(doc.clone(), format!("{}{}", DocId::PREFIX, &ulid[..k]));
    }
    // KEY-n aliases with deterministic collision suffixes.
    let mut groups: BTreeMap<(String, u32), Vec<String>> = BTreeMap::new();
    for doc in &docs {
        let Some(&seq) = catalog.seqs.get(doc) else {
            continue;
        };
        // Live issues are present in board order. Done issues are deliberately
        // removed from that movable list, so their authoritative Issue body is
        // the fallback that keeps KEY-n aliases stable after completion.
        let project = catalog
            .boards
            .iter()
            .find(|(_, entries)| entries.iter().any(|(_, d)| d == doc))
            .map(|(p, _)| p.as_str())
            .or_else(|| project_of(doc));
        if let Some(project) = project {
            groups
                .entry((project.to_string(), seq))
                .or_default()
                .push(doc.clone());
        }
    }
    for ((project, seq), mut members) in groups {
        let Some(key) = catalog.projects.get(&project).map(|p| p.key.clone()) else {
            continue;
        };
        members.sort();
        for (i, doc) in members.iter().enumerate() {
            let alias = if i == 0 {
                format!("{key}-{seq}")
            } else {
                format!("{key}-{seq}{}", collision_suffix(i))
            };
            out.by_alias.insert(alias.to_ascii_lowercase(), doc.clone());
            out.by_doc.insert(doc.clone(), alias);
        }
    }
    out
}

pub fn canonical_for(aliases: &DerivedAliases, doc: &str) -> String {
    aliases.canonical.get(doc).cloned().unwrap_or_else(|| {
        DocId::parse(doc)
            .map(|d| d.short(CANONICAL_MIN))
            .unwrap_or_else(|| doc.to_string())
    })
}

fn assignee_summary(assignees: &[ActorId], me: Option<&ActorId>) -> String {
    let mine = me.is_some_and(|m| assignees.contains(m));
    match (assignees.len(), mine) {
        (0, _) => String::new(),
        (1, true) => "you".to_string(),
        (n, true) => format!("you +{}", n - 1),
        (n, false) => {
            let first = assignees[0].short();
            if n == 1 {
                first
            } else {
                format!("{first} +{}", n - 1)
            }
        }
    }
}

/// Build a legacy Row for one issue.
pub fn project_row(
    catalog: &CatalogState,
    aliases: &DerivedAliases,
    doc: &str,
    issue: Option<&IssueState>,
    me: Option<&ActorId>,
) -> Row {
    let (title, status, priority, assignees, project, due_date, estimate) = match issue {
        Some(i) => (
            i.title.clone(),
            i.status.clone(),
            i.priority,
            i.assignees.clone(),
            i.project.clone(),
            i.duedate,
            i.estimate,
        ),
        None => (
            String::new(),
            DEFAULT_STATUS.to_string(),
            Priority::None,
            Vec::new(),
            String::new(),
            None,
            None,
        ),
    };
    Row {
        reff: canonical_for(aliases, doc),
        doc_id: DocId::parse(doc).unwrap_or_else(|| {
            DocId::parse("iss_00000000000000000000000000").expect("zero doc id")
        }),
        project_id: ProjectId::parse(&project).unwrap_or_else(|| {
            ProjectId::parse("prj_00000000000000000000000000").expect("zero project id")
        }),
        key_alias: aliases.by_doc.get(doc).cloned(),
        title,
        status,
        priority,
        assignee_summary: assignee_summary(&assignees, me),
        assignees,
        enrichment_complete: true,
        tombstone: catalog.tombstones.contains(doc),
        provisional: issue.is_none(),
        due_date,
        estimate,
        label_names: issue
            .map(|i| {
                i.labels
                    .iter()
                    .map(|id| {
                        catalog
                            .labels
                            .get(id)
                            .map(|l| l.name.clone())
                            .unwrap_or_else(|| id.clone())
                    })
                    .collect()
            })
            .unwrap_or_default(),
        milestone: issue.and_then(|i| i.milestone.clone()),
        // Sub-issue progress is a board-projection concern (it needs the issues
        // map to classify each child's status); the base row leaves it absent.
        child_done: None,
        child_total: None,
    }
}

/// How one stored comment's span resolves against the snapshot the view is
/// built from.
///
/// Supplied by the caller because resolving needs the Body reader and this
/// module is a pure projection over already-parsed state. A parameter rather
/// than a field of [`IssueState`] on purpose: `IssueState` is memoized per Body
/// version stamp, and a memoized resolution outlives the Body it was true of.
pub type ResolveCommentAnchor<'a> = &'a dyn Fn(&StoredComment) -> Option<CommentAnchorDto>;

/// Build the legacy IssueView.
#[allow(clippy::too_many_arguments)]
pub fn issue_view(
    catalog: &CatalogState,
    aliases: &DerivedAliases,
    space: &crate::ids::SpaceId,
    doc: &str,
    issue: &IssueState,
    resolve_anchor: ResolveCommentAnchor<'_>,
) -> IssueView {
    let label_names = issue
        .labels
        .iter()
        .map(|id| {
            catalog
                .labels
                .get(id)
                .map(|l| l.name.clone())
                .unwrap_or_else(|| id.clone())
        })
        .collect();
    IssueView {
        schema_version: VIEW_SCHEMA_VERSION,
        reff: canonical_for(aliases, doc),
        doc_id: DocId::parse(doc).expect("doc id"),
        space_id: space.clone(),
        project_id: ProjectId::parse(&issue.project)
            .unwrap_or_else(|| ProjectId::parse("prj_00000000000000000000000000").expect("zero")),
        project_key: catalog.projects.get(&issue.project).map(|p| p.key.clone()),
        key_alias: aliases.by_doc.get(doc).cloned(),
        title: issue.title.clone(),
        description: issue.description.clone(),
        document_schema: issue.document_schema,
        status: issue.status.clone(),
        priority: issue.priority,
        assignees: issue.assignees.clone(),
        labels: issue
            .labels
            .iter()
            .filter_map(|l| LabelId::parse(l))
            .collect(),
        label_names,
        comments: issue
            .comments
            .iter()
            .filter_map(|c| {
                Some(CommentDto {
                    author: ActorId::parse(&c.a)?,
                    author_nick: None,
                    ts: c.t,
                    body: c.b.clone(),
                    id: c.id.clone(),
                    parent: c.parent.clone(),
                    reactions: c
                        .id
                        .as_deref()
                        .and_then(|id| issue.reactions.get(id))
                        .map(|pairs| group_reactions(pairs))
                        .unwrap_or_default(),
                    anchor: resolve_anchor(c),
                })
            })
            .collect(),
        created_by: issue
            .created_by
            .clone()
            .unwrap_or_else(|| ActorId::from_incept_hash(&"0".repeat(64))),
        created_at: issue.created_at,
        due_date: issue.duedate,
        estimate: issue.estimate,
        followers: issue.followers.clone(),
        milestone: issue.milestone.clone(),
        cycle: issue.cycle.clone(),
        baseline: issue.baseline.clone(),
        attachments: issue
            .attachments
            .iter()
            .map(|a| crate::dto::AttachmentMetaDto {
                id: a.id.clone(),
                name: a.name.clone(),
                mime: a.mime.clone(),
                size: a.size,
                by: a.by.clone(),
                ts: a.ts,
                comment: a.comment.clone(),
            })
            .collect(),
        checks: issue
            .checks
            .iter()
            .map(|(run, check)| CheckDto {
                run: run.clone(),
                spec: check.spec.clone(),
                version: check.v,
                build: check.build.clone(),
                source: check.source.clone(),
                state: check.state.clone(),
                by: check.by.clone(),
                ts: check.ts,
                attempt: check.attempt.clone(),
                report: check.report.clone(),
                verdict: check.verdict.clone(),
            })
            .collect(),
        provisional: false,
        corrupt_records: issue.check_corrupt_records.clone(),
    }
}

/// Group one comment's `(emoji, actor)` pairs into per-emoji actor lists,
/// first-appearance emoji order (the pairs arrive sorted, so this is
/// deterministic across replicas).
fn group_reactions(pairs: &[(String, String)]) -> Vec<crate::dto::ReactionDto> {
    let mut out: Vec<crate::dto::ReactionDto> = Vec::new();
    for (emoji, actor) in pairs {
        let Some(actor) = ActorId::parse(actor) else {
            continue;
        };
        match out.iter_mut().find(|r| &r.emoji == emoji) {
            Some(r) => r.actors.push(actor),
            None => out.push(crate::dto::ReactionDto {
                emoji: emoji.clone(),
                actors: vec![actor],
            }),
        }
    }
    out
}

pub fn project_dto(id: &str, meta: &ProjectMeta) -> Option<ProjectDto> {
    Some(ProjectDto {
        id: ProjectId::parse(id)?,
        name: meta.name.clone(),
        key: meta.key.clone(),
        color: meta.color.clone(),
        description: meta.description.clone(),
        lead: meta.lead.clone(),
        start_date: meta.start_date,
        target_date: meta.target_date,
        archived: meta.archived,
        team: meta.team.clone(),
        enrichment_complete: true,
    })
}

pub fn label_dto(id: &str, meta: &LabelMeta) -> Option<LabelDto> {
    Some(LabelDto {
        id: LabelId::parse(id)?,
        name: meta.name.clone(),
        color: meta.color.clone(),
    })
}

/// Build the legacy BoardView.
pub fn board_view(
    catalog: &CatalogState,
    aliases: &DerivedAliases,
    project_id: &str,
    issues: &BTreeMap<String, std::sync::Arc<IssueState>>,
    me: Option<&ActorId>,
) -> Result<Option<BoardView>, WorkflowResolutionError> {
    let Some(meta) = catalog.projects.get(project_id) else {
        return Ok(None);
    };
    let Some(project) = project_dto(project_id, meta) else {
        return Ok(None);
    };
    let workflow = &catalog.resolved_workflow(project_id)?.body;
    // Live members of this project.
    let members: Vec<&String> = issues
        .iter()
        .filter(|(doc, i)| i.project == project_id && !catalog.tombstones.contains(*doc))
        .map(|(doc, _)| doc)
        .collect();
    let board_order: Vec<String> = catalog
        .boards
        .get(project_id)
        .map(|b| b.iter().map(|(_, d)| d.clone()).collect())
        .unwrap_or_default();
    // Sub-issue progress per parent, computed once: total = live children,
    // done = children whose status is a Done-category state. Built from the same
    // `catalog.parents` edge map the graph view reads, minus tombstoned children.
    let mut child_progress: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for (child, parent) in &catalog.parents {
        if catalog.tombstones.contains(child) {
            continue;
        }
        let entry = child_progress.entry(parent.as_str()).or_insert((0, 0));
        entry.1 += 1;
        let done = issues.get(child).is_some_and(|i| {
            workflow.states.iter().any(|state| {
                state.state_id == i.status
                    && StatusCategory::parse(&state.category) == Some(StatusCategory::Done)
            })
        });
        if done {
            entry.0 += 1;
        }
    }
    // Build a board row and stamp its sub-issue progress (absent when childless).
    let row_of = |doc: &str| -> Row {
        let mut row = project_row(
            catalog,
            aliases,
            doc,
            issues.get(doc).map(|i| i.as_ref()),
            me,
        );
        if let Some((done, total)) = child_progress.get(doc) {
            row.child_done = Some(*done);
            row.child_total = Some(*total);
        }
        row
    };
    let mut columns = Vec::new();
    for state in &workflow.states {
        let mut rows: Vec<Row> = Vec::new();
        let in_state = |doc: &str| issues.get(doc).is_some_and(|i| i.status == state.state_id);
        if StatusCategory::parse(&state.category) == Some(StatusCategory::Done) {
            let mut done: Vec<&&String> = members.iter().filter(|d| in_state(d)).collect();
            done.sort_by(|a, b| {
                let ia = issues.get(**a).map(|i| i.created_at).unwrap_or(0);
                let ib = issues.get(**b).map(|i| i.created_at).unwrap_or(0);
                ib.cmp(&ia).then_with(|| b.cmp(a))
            });
            for doc in done {
                rows.push(row_of(doc));
            }
        } else {
            let mut seen = BTreeSet::new();
            for doc in &board_order {
                if members.contains(&doc) && in_state(doc) && seen.insert(doc.clone()) {
                    rows.push(row_of(doc));
                }
            }
            let mut unlisted: Vec<&&String> = members
                .iter()
                .filter(|d| in_state(d) && !seen.contains(**d))
                .collect();
            unlisted.sort();
            for doc in unlisted {
                rows.push(row_of(doc));
            }
        }
        let Some(category) = StatusCategory::parse(&state.category) else {
            return Ok(None);
        };
        columns.push(BoardColumn {
            state: WorkflowState {
                id: state.state_id.clone(),
                name: state.name.clone(),
                category,
                color: state.color.clone(),
            },
            rows,
        });
    }
    Ok(Some(BoardView {
        schema_version: VIEW_SCHEMA_VERSION,
        project,
        columns,
    }))
}

pub fn default_workflow_states() -> Vec<WorkflowState> {
    super::contract::default_workflow()
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect()
}

/// Revision-head computation over a grow-only log: the heads are entries no
/// other entry names as a predecessor. One head is usable; several are an
/// explicit conflict the caller must surface.
fn heads_of<T, I: Fn(&T) -> &str, P: Fn(&T) -> &[String]>(
    log: &[T],
    id_of: I,
    preds_of: P,
) -> Vec<&T> {
    use std::collections::BTreeSet;
    let referenced: BTreeSet<&str> = log
        .iter()
        .flat_map(|r| preds_of(r).iter().map(|s| s.as_str()))
        .collect();
    log.iter()
        .filter(|r| !referenced.contains(id_of(r)))
        .collect()
}

impl CatalogState {
    /// The workflow revision heads for a project (empty = never seeded;
    /// more than one = concurrent edits pending explicit resolution).
    pub fn workflow_heads(&self, project: &str) -> Vec<&crate::workflow::WorkflowRevision> {
        self.workflow_revisions
            .get(project)
            .map(|log| heads_of(log, |r| r.revision_id.as_str(), |r| &r.predecessor_ids))
            .unwrap_or_default()
    }

    /// The single usable workflow head, or `None` (missing or conflicted).
    pub fn workflow_head(&self, project: &str) -> Option<&crate::workflow::WorkflowRevision> {
        let heads = self.workflow_heads(project);
        match heads.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }

    pub fn resolved_workflow(
        &self,
        project: &str,
    ) -> Result<&crate::workflow::WorkflowRevision, WorkflowResolutionError> {
        let heads = self.workflow_heads(project);
        match heads.as_slice() {
            [revision] if !revision.body.tombstone => Ok(revision),
            [] | [_] => Err(WorkflowResolutionError::Missing),
            _ => Err(WorkflowResolutionError::Conflicted),
        }
    }

    /// The custom-role revision heads for a role id.
    pub fn role_heads(&self, role: &str) -> Vec<&StoredRoleRevision> {
        self.role_revisions
            .get(role)
            .map(|log| heads_of(log, |r| r.revision_id.as_str(), |r| &r.predecessor_ids))
            .unwrap_or_default()
    }

    /// The single usable role head: a built-in's immutable definition, or the
    /// custom role's sole head. `None` for unknown or conflicted roles.
    pub fn role_head(&self, role: &str) -> Option<&StoredRoleRevision> {
        if let Some(built_in) = self.roles.get(role) {
            return Some(built_in);
        }
        let heads = self.role_heads(role);
        match heads.as_slice() {
            [one] => Some(one),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowResolutionError {
    Missing,
    Conflicted,
}

#[cfg(test)]
mod event_history_tests {
    use super::*;
    use fabric::{ListElement, LogView};

    fn event(kind: &str, t: u64) -> Vec<u8> {
        serde_json::to_vec(&IssueEvent {
            k: kind.into(),
            d: "dev".into(),
            a: String::new(),
            t,
            c: vec![],
            x: String::new(),
            entry: String::new(),
        })
        .expect("encode")
    }

    fn kinds(events: &[IssueEvent]) -> Vec<&str> {
        events.iter().map(|e| e.k.as_str()).collect()
    }

    /// A history spanning the cutover is one history, in the order it happened,
    /// and the total counts both homes.
    #[test]
    fn history_merges_the_legacy_list_with_the_log_in_clock_order() {
        let mut view = CollaborativeView::default();
        view.lists.insert(
            "events".into(),
            vec![
                ListElement {
                    element: "e0".into(),
                    value: event("created", 10),
                },
                ListElement {
                    element: "e1".into(),
                    value: event("edited", 30),
                },
            ],
        );
        view.logs.insert(
            super::super::contract::EVENTS_PATH.into(),
            LogView {
                entries: vec![
                    ListElement {
                        element: "aa".into(),
                        value: event("assigned", 20),
                    },
                    ListElement {
                        element: "bb".into(),
                        value: event("commented", 40),
                    },
                ],
                appended: 2,
            },
        );
        let (events, recorded) = read_events(&view);
        assert_eq!(
            kinds(&events),
            vec!["created", "assigned", "edited", "commented"]
        );
        assert_eq!(recorded, 4, "both homes counted");
        assert_eq!(
            events[1].entry, "aa",
            "the entry id rides out for the cursor"
        );
    }

    /// Events inside one second keep the order they were staged in rather than
    /// being shuffled by an identity that means nothing chronologically.
    #[test]
    fn events_in_the_same_second_keep_their_sequence_order() {
        let mut view = CollaborativeView::default();
        view.logs.insert(
            super::super::contract::EVENTS_PATH.into(),
            LogView {
                entries: vec![
                    ListElement {
                        element: "ff".into(),
                        value: event("created", 7),
                    },
                    ListElement {
                        element: "aa".into(),
                        value: event("assigned", 7),
                    },
                ],
                appended: 2,
            },
        );
        assert_eq!(
            kinds(&read_events(&view).0),
            vec!["created", "assigned"],
            "same second: sequence order, not entry-id order"
        );
    }

    /// The count is of everything that ever happened, and the rows are what
    /// survived. Conflating them is what would renumber a busy issue's history.
    #[test]
    fn a_trimmed_log_reports_more_recorded_than_it_retains() {
        let mut view = CollaborativeView::default();
        view.logs.insert(
            super::super::contract::EVENTS_PATH.into(),
            LogView {
                entries: vec![ListElement {
                    element: "zz".into(),
                    value: event("edited", 99),
                }],
                appended: 900,
            },
        );
        let (events, recorded) = read_events(&view);
        assert_eq!(events.len(), 1);
        assert_eq!(recorded, 900);
    }
}

#[cfg(test)]
mod hierarchy_tests {
    use super::*;
    use fabric::TreeNode;

    fn node(id: &str, anchor: &str, parent: Option<&str>) -> TreeNode {
        TreeNode {
            node: id.into(),
            parent: parent.map(str::to_string),
            value: Vec::new(),
            entries: BTreeMap::new(),
            anchor: Some(anchor.into()),
        }
    }

    /// A Catalog written before the hierarchy still reports its parentage.
    #[test]
    fn a_hierarchy_stored_as_a_map_still_reads() {
        let mut view = CollaborativeView::default();
        view.maps.insert(
            "parents".into(),
            [("iss_b".to_string(), b"iss_a".to_vec())]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            read_hierarchy(&view).get("iss_b").map(String::as_str),
            Some("iss_a")
        );
    }

    /// The tree overrules the map, in both directions. The map is never
    /// rewritten after the cutover, so a stale entry left to win would put a
    /// re-parented issue back where it was moved from — and an unparented one
    /// back under a parent it no longer has.
    #[test]
    fn the_tree_overrules_a_stale_map_entry_including_when_it_says_no_parent() {
        let mut view = CollaborativeView::default();
        view.maps.insert(
            "parents".into(),
            [
                ("iss_b".to_string(), b"iss_a".to_vec()),
                ("iss_c".to_string(), b"iss_a".to_vec()),
            ]
            .into_iter()
            .collect(),
        );
        view.trees.insert(
            crate::contract::HIERARCHY_PATH.into(),
            vec![
                node("1@7", "iss_a", None),
                node("2@7", "iss_d", None),
                // b was re-parented under d since the cutover.
                node("3@7", "iss_b", Some("2@7")),
                // c was unparented: a root of the forest, and no entry.
                node("4@7", "iss_c", None),
            ],
        );
        let parents = read_hierarchy(&view);
        assert_eq!(parents.get("iss_b").map(String::as_str), Some("iss_d"));
        assert_eq!(parents.get("iss_c"), None, "unparenting is not forgotten");
        assert_eq!(parents.get("iss_a"), None);
    }

    /// A node placed by something other than this product names no issue, and
    /// is skipped rather than guessed at.
    #[test]
    fn a_node_without_an_anchor_names_no_issue() {
        let mut view = CollaborativeView::default();
        view.trees.insert(
            super::super::contract::HIERARCHY_PATH.into(),
            vec![
                TreeNode {
                    node: "1@7".into(),
                    parent: None,
                    value: Vec::new(),
                    entries: BTreeMap::new(),
                    anchor: None,
                },
                node("2@7", "iss_b", Some("1@7")),
            ],
        );
        // The child's parent node carries no anchor, so there is no issue to
        // name as its parent — it reads as a root, not as a child of nothing.
        assert_eq!(read_hierarchy(&view).get("iss_b"), None);
    }
}

#[cfg(test)]
mod reaction_tests {
    use super::*;
    use crate::contract::{reaction_value, reaction_value_legacy, REACTIONS_PATH};

    /// A comment reacted to on both sides of the collapse. Either set alone
    /// under-reports it, and the same pair recorded in both is one reaction.
    #[test]
    fn reactions_merge_across_the_cutover_without_double_counting() {
        let mut view = CollaborativeView::default();
        view.sets.insert(
            REACTIONS_PATH.into(),
            vec![
                reaction_value("cmt_a", "👍", "act_1"),
                reaction_value("cmt_a", "🎉", "act_2"),
                reaction_value("cmt_b", "👍", "act_1"),
            ],
        );
        view.sets.insert(
            "reactions/cmt_a".into(),
            vec![
                // The same reaction, recorded before the collapse.
                reaction_value_legacy("👍", "act_1"),
                reaction_value_legacy("🚀", "act_3"),
            ],
        );
        let reactions = read_reactions(&view);
        assert_eq!(
            reactions["cmt_a"],
            vec![
                ("🎉".to_string(), "act_2".to_string()),
                ("👍".to_string(), "act_1".to_string()),
                ("🚀".to_string(), "act_3".to_string()),
            ],
            "one reaction per distinct (emoji, actor), from both homes"
        );
        assert_eq!(reactions["cmt_b"].len(), 1);
    }

    /// The value grammar has to stay unambiguous now that the comment shares
    /// the field with the emoji and the actor.
    #[test]
    fn a_reaction_value_round_trips_and_a_malformed_one_is_dropped() {
        let raw = reaction_value("cmt_a", "👍", "act_1");
        assert_eq!(
            crate::contract::parse_reaction_value(&raw),
            Some(("cmt_a".into(), "👍".into(), "act_1".into()))
        );
        // A legacy two-field value is not a new three-field one, and a
        // four-field value is not either — neither may be read as the other.
        assert_eq!(
            crate::contract::parse_reaction_value(&reaction_value_legacy("👍", "act_1")),
            None
        );
        assert_eq!(
            crate::contract::parse_reaction_value(b"cmt_a\t\xf0\x9f\x91\x8d\tact_1\textra"),
            None
        );
        assert_eq!(
            crate::contract::parse_legacy_reaction_value(&raw),
            None,
            "a three-field value is not a legacy pair"
        );
    }
}

#[cfg(test)]
mod comment_thread_tests {
    use super::*;
    use fabric::{ListElement, TreeNode};

    fn record(id: &str, t: u64, parent: Option<&str>) -> Vec<u8> {
        serde_json::to_vec(&StoredComment {
            a: "act_1".into(),
            t,
            b: format!("body of {id}"),
            id: Some(id.into()),
            parent: parent.map(str::to_string),
            at: None,
            node: None,
            parent_node: None,
        })
        .expect("encode")
    }

    fn ids(comments: &[StoredComment]) -> Vec<&str> {
        comments
            .iter()
            .map(|c| c.id.as_deref().unwrap_or("?"))
            .collect()
    }

    /// A Body written before the hierarchy still reads. Nothing writes this
    /// shape any more, and it is in Bodies in the field.
    #[test]
    fn a_thread_stored_as_a_flat_list_still_reads() {
        let mut view = CollaborativeView::default();
        view.lists.insert(
            "comments".into(),
            vec![
                ListElement {
                    element: "e0".into(),
                    value: record("cmt_a", 10, None),
                },
                ListElement {
                    element: "e1".into(),
                    value: record("cmt_b", 20, Some("cmt_a")),
                },
            ],
        );
        let comments = read_comments(&view);
        assert_eq!(ids(&comments), vec!["cmt_a", "cmt_b"]);
        assert_eq!(comments[1].parent.as_deref(), Some("cmt_a"));
        assert!(
            comments.iter().all(|c| c.node.is_none()),
            "a legacy record has no node to name"
        );
    }

    /// The hierarchy's parent edge is carried out alongside the record, so a
    /// later write can file a reply under a comment without re-deriving where
    /// it lives.
    #[test]
    fn a_threaded_comment_carries_its_node_and_its_parent_edge() {
        let mut view = CollaborativeView::default();
        view.trees.insert(
            "comments".into(),
            vec![
                TreeNode {
                    node: "3@7".into(),
                    parent: None,
                    value: record("cmt_a", 10, None),
                    entries: BTreeMap::new(),
                    anchor: None,
                },
                TreeNode {
                    node: "5@7".into(),
                    parent: Some("3@7".into()),
                    value: record("cmt_b", 20, Some("cmt_a")),
                    entries: BTreeMap::new(),
                    anchor: None,
                },
            ],
        );
        let comments = read_comments(&view);
        assert_eq!(ids(&comments), vec!["cmt_a", "cmt_b"]);
        assert_eq!(comments[0].node.as_deref(), Some("3@7"));
        assert_eq!(comments[0].parent_node, None);
        assert_eq!(comments[1].parent_node.as_deref(), Some("3@7"));
    }

    /// The reason the order is the clock and not the sequence.
    ///
    /// The tree holds a comment written *before* the last list comment —
    /// which is what a peer that was behind produces, and what placement order
    /// cannot fix, since the two containers have no relative order at all.
    /// Reading by `t` puts the thread back in the order it was written.
    #[test]
    fn a_thread_spanning_the_cutover_reads_in_clock_order() {
        let mut view = CollaborativeView::default();
        view.lists.insert(
            "comments".into(),
            vec![
                ListElement {
                    element: "e0".into(),
                    value: record("cmt_a", 10, None),
                },
                ListElement {
                    element: "e1".into(),
                    value: record("cmt_c", 30, None),
                },
            ],
        );
        view.trees.insert(
            "comments".into(),
            vec![
                TreeNode {
                    node: "3@7".into(),
                    parent: None,
                    value: record("cmt_b", 20, None),
                    entries: BTreeMap::new(),
                    anchor: None,
                },
                TreeNode {
                    node: "5@7".into(),
                    parent: None,
                    value: record("cmt_d", 40, None),
                    entries: BTreeMap::new(),
                    anchor: None,
                },
            ],
        );
        assert_eq!(
            ids(&read_comments(&view)),
            vec!["cmt_a", "cmt_b", "cmt_c", "cmt_d"],
            "one thread, in the order it was written, across both containers"
        );
    }

    /// Two comments in the same millisecond need a tiebreak every replica
    /// computes the same way, or the thread reorders itself between peers.
    #[test]
    fn same_millisecond_comments_break_the_tie_on_id() {
        let mut view = CollaborativeView::default();
        view.trees.insert(
            "comments".into(),
            vec![
                TreeNode {
                    node: "5@7".into(),
                    parent: None,
                    value: record("cmt_z", 10, None),
                    entries: BTreeMap::new(),
                    anchor: None,
                },
                TreeNode {
                    node: "3@7".into(),
                    parent: None,
                    value: record("cmt_a", 10, None),
                    entries: BTreeMap::new(),
                    anchor: None,
                },
            ],
        );
        assert_eq!(
            ids(&read_comments(&view)),
            vec!["cmt_a", "cmt_z"],
            "the id decides, not the position the writer happened to place it at"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fabric::TreeNode;

    #[test]
    fn missing_document_schema_is_legacy_and_the_register_is_additive() {
        let mut view = CollaborativeView::default();
        view.texts.insert("description".into(), "old prose".into());
        assert_eq!(IssueState::from_view(&view).document_schema, 0);

        view.registers
            .insert("document_schema".into(), b"1".to_vec());
        let issue = IssueState::from_view(&view);
        assert_eq!(issue.document_schema, 1);
        assert_eq!(issue.description, "old prose");
    }

    #[test]
    fn completed_issue_keeps_alias_from_authoritative_project() {
        let doc = "iss_01JU6A5CHEI9UR3SGKEK05KIAR";
        let mut catalog = CatalogState::default();
        catalog.projects.insert(
            "prj_board".into(),
            ProjectMeta {
                name: "Board".into(),
                key: "BOARD".into(),
                color: "blue".into(),
                ..Default::default()
            },
        );
        catalog.seqs.insert(doc.into(), 5);

        let aliases = derive_aliases(&catalog, |candidate| {
            (candidate == doc).then_some("prj_board")
        });

        assert_eq!(aliases.by_doc.get(doc).map(String::as_str), Some("BOARD-5"));
        assert_eq!(
            aliases.by_alias.get("board-5").map(String::as_str),
            Some(doc)
        );
    }

    #[test]
    fn project_topology_overrides_legacy_edges_and_parentage() {
        let a = "iss_01JU6A5CHEI9UR3SGKEK05KIAR";
        let b = "iss_01JU6A5CHEI9UR3SGKEK05KIAS";
        let c = "iss_01JU6A5CHEI9UR3SGKEK05KIAT";
        let mut catalog = CatalogState::default();
        catalog.edges.insert((a.into(), "blocks".into(), b.into()));
        catalog.parents.insert(c.into(), a.into());

        let mut view = CollaborativeView::default();
        view.maps
            .entry("edges".into())
            .or_default()
            .insert(format!("{a}|blocks|{b}"), b"0".to_vec());
        view.maps
            .entry("edges".into())
            .or_default()
            .insert(format!("{b}|relates|{c}"), b"1".to_vec());
        view.trees.insert(
            super::super::contract::HIERARCHY_PATH.into(),
            vec![
                TreeNode {
                    node: "node-b".into(),
                    parent: None,
                    value: Vec::new(),
                    entries: BTreeMap::new(),
                    anchor: Some(b.into()),
                },
                TreeNode {
                    node: "node-c".into(),
                    parent: Some("node-b".into()),
                    value: Vec::new(),
                    entries: BTreeMap::new(),
                    anchor: Some(c.into()),
                },
            ],
        );

        RelationState::from_view(&view).apply_to(&mut catalog);
        assert!(!catalog
            .edges
            .contains(&(a.into(), "blocks".into(), b.into())));
        assert!(catalog
            .edges
            .contains(&(b.into(), "relates".into(), c.into())));
        assert_eq!(catalog.parents.get(c).map(String::as_str), Some(b));
    }
}
