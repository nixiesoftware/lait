#![allow(
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "geometry uses budget-checked compact vectors and saturating counters"
)]

//! Publication-pinned global analytics over Blueprint's canonical Corpus.
//!
//! Geometry is deliberately not a second Issue store. Exact Issue fields and
//! graph facts stay in Runtime's shared Corpus. This module compiles only the
//! globally sensitive analytics that cannot be answered by a bounded local
//! walk. One artifact owns a single string dictionary, generation-local `u32`
//! identities, fixed columns, and CSR membership/adjacency arrays. Strings are
//! materialized only for the bounded page a caller asks to return.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};

use runtime::publication::WorldPublicationId;
use serde::{Deserialize, Serialize};

const PROJECTION_SCHEMA_CONTEXT: &str = "lait.issues.geometry-projection.v3";
const SELECTION_CONTEXT: &str = "lait.issues.geometry-selection.v3";
const PROJECTION_SCHEMA_MATERIAL: &[u8] =
    b"compact:u32;dictionary:one;nodes:columns;edges:columns;membership:csr;reads:paged";
pub const MAX_GEOMETRY_PAGE: u32 = 1_000;
/// Recommended explicit-poll backoff after [`GeometryReadiness::Pending`].
/// Product transports may replace polling with a doorbell, but must not spin.
pub const GEOMETRY_POLL_AFTER_MS: u64 = 25;
const MAX_TERMINAL_GEOMETRY_RESULTS: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProjectionSchemaDigest([u8; 32]);

impl ProjectionSchemaDigest {
    pub fn current() -> Self {
        Self(blake3::derive_key(
            PROJECTION_SCHEMA_CONTEXT,
            PROJECTION_SCHEMA_MATERIAL,
        ))
    }

    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SelectionFingerprint([u8; 32]);

impl SelectionFingerprint {
    fn derive(project: &str, roots: &[String]) -> Self {
        let mut roots = roots.to_vec();
        roots.sort();
        roots.dedup();
        let mut material = Vec::new();
        push_bytes(&mut material, project.as_bytes());
        push_len(&mut material, roots.len());
        for root in roots {
            push_bytes(&mut material, root.as_bytes());
        }
        Self(blake3::derive_key(SELECTION_CONTEXT, &material))
    }

    pub const fn digest(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GeometryArtifactKey {
    pub source: WorldPublicationId,
    pub projection_schema: ProjectionSchemaDigest,
    pub selection: SelectionFingerprint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryBudget {
    pub node_visits: u64,
    pub edge_visits: u64,
    pub reachability_visits: u64,
    pub working_bytes: u64,
}

impl Default for GeometryBudget {
    fn default() -> Self {
        Self {
            node_visits: 2_000_000,
            edge_visits: 8_000_000,
            reachability_visits: 50_000_000,
            working_bytes: 512 * 1_024 * 1_024,
        }
    }
}

impl GeometryBudget {
    fn contains(self, estimate: GeometryEstimate) -> bool {
        estimate.node_visits <= self.node_visits
            && estimate.edge_visits <= self.edge_visits
            && estimate.reachability_visits <= self.reachability_visits
            && estimate.working_bytes <= self.working_bytes
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryEstimate {
    pub selected_nodes: u64,
    pub selected_edges: u64,
    pub reduction_candidates: u64,
    pub node_visits: u64,
    pub edge_visits: u64,
    pub reachability_visits: u64,
    pub working_bytes: u64,
}

impl GeometryEstimate {
    /// O(1) conservative admission hint for an asynchronous miss. The ready
    /// artifact replaces it with the exact estimate computed from canonical
    /// selected/deduplicated facts.
    pub fn conservative(
        request: &GeometryRequest,
        issue_count: usize,
        relation_count: usize,
    ) -> Self {
        let nodes = usize_u64(issue_count);
        let edges = usize_u64(relation_count);
        let root_count = usize_u64(request.roots.len());
        let root_bytes = request.roots.iter().fold(0u64, |total, root| {
            total.saturating_add(usize_u64(root.len()))
        });
        Self {
            selected_nodes: nodes,
            selected_edges: edges,
            reduction_candidates: edges,
            node_visits: nodes.saturating_mul(18).saturating_add(root_count),
            edge_visits: edges.saturating_mul(21),
            reachability_visits: edges.saturating_mul(nodes.saturating_add(edges)),
            working_bytes: nodes
                .saturating_mul(256)
                .saturating_add(edges.saturating_mul(80))
                .saturating_add(root_bytes)
                .saturating_add(root_count.saturating_mul(32)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryIssueFact {
    pub id: String,
    pub project: String,
    pub closed: bool,
    pub due: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GeometryRelationFact {
    pub from: String,
    pub relation: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryFacts {
    pub source: WorldPublicationId,
    pub issues: Vec<GeometryIssueFact>,
    pub relations: Vec<GeometryRelationFact>,
}

/// Build exact Geometry input from the shared publication in a bounded worker.
///
/// The project posting selects placement, topology, and workflow facts without
/// scanning unrelated Issues. Issue metadata is then fetched in canonical
/// 256-id batches. Workflow categories are resolved from the sole revision
/// head represented in the same Corpus publication; conflicts are typed
/// corruption rather than an invented `open`/`done` classification.
pub fn facts_from_find(
    find: &runtime::world::FindHandle,
    request: &GeometryRequest,
) -> Result<GeometryFacts, UnavailableReason> {
    use runtime::find as find_api;

    if find.publication() != request.source {
        return Err(UnavailableReason::SourceMismatch {
            requested: request.source,
            actual: find.publication(),
        });
    }

    let mut placements = BTreeMap::<String, String>::new();
    let mut revision_node_ids = BTreeMap::<String, String>::new();
    let mut live_revisions = BTreeSet::<String>::new();
    let mut predecessor_edges = Vec::<(String, String)>::new();
    let mut workflow_states = Vec::<(String, String, String)>::new();
    let mut raw_relations = Vec::<GeometryRelationFact>::new();
    let mut row_visits = 0u64;
    let mut working_bytes = 0u64;

    let fields = geometry_fact_fields();
    visit_find_pages(
        find,
        find_api::Seek::Field(find_api::Predicate {
            field: crate::find::field_ref(crate::find::field::PROJECT),
            test: find_api::Test::Equal,
            value: find_api::Atom::Text(request.project.clone()),
        }),
        fields,
        |row| {
            row_visits = row_visits.saturating_add(1);
            if row_visits > request.budget.node_visits {
                return Err(fact_budget_exceeded(
                    request,
                    placements.len(),
                    raw_relations.len(),
                    working_bytes,
                ));
            }
            let kind =
                row_text(row, crate::find::field::KIND).ok_or(UnavailableReason::SourceCorrupt)?;
            match kind.as_str() {
                "issue_placement" => {
                    let issue = required_text(row, crate::find::field::SOURCE_ID)?;
                    let state = required_text(row, crate::find::field::STATE)?;
                    working_bytes = working_bytes
                        .saturating_add(usize_u64(issue.len().saturating_add(state.len())));
                    if placements
                        .insert(issue, state.clone())
                        .is_some_and(|prior| prior != state)
                    {
                        return Err(UnavailableReason::SourceCorrupt);
                    }
                }
                "workflow_revision" => {
                    let id = required_text(row, crate::find::field::ID)?;
                    let revision = required_text(row, crate::find::field::REVISION)?;
                    if revision_node_ids.insert(id, revision.clone()).is_some() {
                        return Err(UnavailableReason::SourceCorrupt);
                    }
                    if !row_bool(row, crate::find::field::TOMBSTONE).unwrap_or(false) {
                        live_revisions.insert(revision);
                    }
                }
                "workflow_state" => {
                    workflow_states.push((
                        required_text(row, crate::find::field::REVISION)?,
                        required_text(row, crate::find::field::STATE)?,
                        required_text(row, crate::find::field::STATE_CATEGORY)?,
                    ));
                }
                "relation" => {
                    let relation = required_text(row, crate::find::field::RELATION_KIND)?;
                    let from = required_text(row, crate::find::field::SOURCE_ID)?;
                    let to = required_text(row, crate::find::field::TARGET_ID)?;
                    if relation == "predecessor" {
                        predecessor_edges.push((from, to));
                    } else {
                        working_bytes = working_bytes.saturating_add(usize_u64(
                            relation
                                .len()
                                .saturating_add(from.len())
                                .saturating_add(to.len()),
                        ));
                        raw_relations.push(GeometryRelationFact { from, relation, to });
                    }
                }
                _ => {}
            }
            if working_bytes > request.budget.working_bytes {
                return Err(fact_budget_exceeded(
                    request,
                    placements.len(),
                    raw_relations.len(),
                    working_bytes,
                ));
            }
            Ok(())
        },
    )?;

    let mut predecessor_revisions = BTreeSet::new();
    for (from, to) in predecessor_edges {
        if revision_node_ids.contains_key(&from) {
            let predecessor = revision_node_ids
                .get(&to)
                .ok_or(UnavailableReason::SourceCorrupt)?;
            predecessor_revisions.insert(predecessor.clone());
        }
    }
    let heads = live_revisions
        .difference(&predecessor_revisions)
        .cloned()
        .collect::<Vec<_>>();
    let [head] = heads.as_slice() else {
        return Err(UnavailableReason::SourceCorrupt);
    };
    let mut categories = BTreeMap::<String, String>::new();
    for (revision, state, category) in workflow_states {
        if &revision != head {
            continue;
        }
        if !matches!(category.as_str(), "backlog" | "active" | "done")
            || categories
                .insert(state, category.clone())
                .is_some_and(|prior| prior != category)
        {
            return Err(UnavailableReason::SourceCorrupt);
        }
    }

    let issue_ids = placements.keys().cloned().collect::<Vec<_>>();
    let mut metadata = BTreeMap::<String, (Option<i64>, bool)>::new();
    for ids in issue_ids.chunks(find_api::MAX_SEEK_IDS) {
        let ids = ids
            .iter()
            .map(|id| {
                find_api::NodeId::new(id.as_bytes().to_vec())
                    .map_err(|_| UnavailableReason::SourceCorrupt)
            })
            .collect::<Result<Vec<_>, _>>()?;
        visit_find_pages(
            find,
            find_api::Seek::Ids(ids),
            geometry_fact_fields(),
            |row| {
                if row_text(row, crate::find::field::KIND).as_deref() != Some("issue") {
                    return Err(UnavailableReason::SourceCorrupt);
                }
                let id = required_text(row, crate::find::field::ID)?;
                let due = row_unsigned(row, crate::find::field::DUE_AT)
                    .map(i64::try_from)
                    .transpose()
                    .map_err(|_| UnavailableReason::SourceCorrupt)?;
                let tombstone = row_bool(row, crate::find::field::TOMBSTONE).unwrap_or(false);
                if metadata.insert(id, (due, tombstone)).is_some() {
                    return Err(UnavailableReason::SourceCorrupt);
                }
                Ok(())
            },
        )?;
    }

    let mut issues = Vec::with_capacity(placements.len());
    for (id, state) in placements {
        let (due, tombstone) = metadata
            .remove(&id)
            .ok_or(UnavailableReason::SourceCorrupt)?;
        if tombstone {
            continue;
        }
        let category = categories
            .get(&state)
            .ok_or(UnavailableReason::SourceCorrupt)?;
        issues.push(GeometryIssueFact {
            id,
            project: request.project.clone(),
            closed: category == "done",
            due,
        });
    }
    issues.sort_by(|left, right| left.id.cmp(&right.id));
    let selected = issues
        .iter()
        .map(|issue| issue.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut relations = raw_relations
        .into_iter()
        .filter_map(|relation| {
            if !selected.contains(relation.from.as_str())
                || !selected.contains(relation.to.as_str())
            {
                return None;
            }
            if relation.relation == "parent" {
                Some(GeometryRelationFact {
                    from: relation.to,
                    relation: "contains".into(),
                    to: relation.from,
                })
            } else {
                Some(relation)
            }
        })
        .collect::<Vec<_>>();
    relations.sort();
    relations.dedup();
    if usize_u64(relations.len()) > request.budget.edge_visits {
        return Err(fact_budget_exceeded(
            request,
            issues.len(),
            relations.len(),
            working_bytes,
        ));
    }
    Ok(GeometryFacts {
        source: request.source,
        issues,
        relations,
    })
}

fn geometry_find_bound() -> runtime::find::Bound {
    runtime::find::Bound {
        decoded_bodies: 1,
        postings_read: 100_000,
        edges_visited: 1,
        nodes_visited: 100_000,
        paths_retained: 1,
        candidates_per_branch: 10_000,
        score_evaluations: 1,
        projected_bytes: 8 * 1_024 * 1_024,
        packed_tokens: 32_768,
        wall_millis: 10_000,
    }
}

fn geometry_fact_fields() -> Vec<runtime::find::FieldRef> {
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::STATE_CATEGORY,
        crate::find::field::REVISION,
        crate::find::field::SOURCE_ID,
        crate::find::field::TARGET_ID,
        crate::find::field::RELATION_KIND,
        crate::find::field::DUE_AT,
        crate::find::field::TOMBSTONE,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    fields
}

fn visit_find_pages<F>(
    find: &runtime::world::FindHandle,
    seek: runtime::find::Seek,
    fields: Vec<runtime::find::FieldRef>,
    mut visit: F,
) -> Result<(), UnavailableReason>
where
    F: FnMut(&runtime::find::ResultRow) -> Result<(), UnavailableReason>,
{
    use runtime::find as find_api;
    let bound = geometry_find_bound();
    let seek_id = find_api::StepId::new(1).ok_or(UnavailableReason::SourceCorrupt)?;
    let pack_id = find_api::StepId::new(2).ok_or(UnavailableReason::SourceCorrupt)?;
    let mut query = find_api::Query {
        schema: crate::find::entity_schema_ref(),
        publication: Some(find.publication().publication),
        mode: find_api::Mode::Exact,
        steps: vec![
            find_api::Step {
                id: seek_id,
                input: Vec::new(),
                op: find_api::Op::Seek(seek),
                bound,
            },
            find_api::Step {
                id: pack_id,
                input: vec![seek_id],
                op: find_api::Op::Pack(find_api::Pack { fields }),
                bound,
            },
        ],
        output: pack_id,
        bound,
        page_size: 2_048,
        cursor: None,
    };
    loop {
        let answer = find
            .find(query.clone())
            .map_err(|_| UnavailableReason::SourceUnavailable)?;
        if answer.coordinates().world_publication() != find.publication() {
            return Err(UnavailableReason::SourceMismatch {
                requested: find.publication(),
                actual: answer.coordinates().world_publication(),
            });
        }
        for row in answer.rows() {
            visit(row)?;
        }
        let Some(cursor) = answer.next_cursor().cloned() else {
            return Ok(());
        };
        query.cursor = Some(cursor);
    }
}

fn row_text(row: &runtime::find::ResultRow, name: &str) -> Option<String> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Text(value) => Some(value.to_string()),
                _ => None,
            })
    })
}

fn required_text(row: &runtime::find::ResultRow, name: &str) -> Result<String, UnavailableReason> {
    row_text(row, name).ok_or(UnavailableReason::SourceCorrupt)
}

fn row_unsigned(row: &runtime::find::ResultRow, name: &str) -> Option<u64> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Unsigned(value) => Some(*value),
                _ => None,
            })
    })
}

fn row_bool(row: &runtime::find::ResultRow, name: &str) -> Option<bool> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Bool(value) => Some(*value),
                _ => None,
            })
    })
}

fn fact_budget_exceeded(
    request: &GeometryRequest,
    issues: usize,
    relations: usize,
    working_bytes: u64,
) -> UnavailableReason {
    let mut estimate = GeometryEstimate::conservative(request, issues, relations);
    estimate.working_bytes = estimate.working_bytes.max(working_bytes);
    UnavailableReason::BudgetExceeded {
        budget: request.budget,
        estimate,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryRequest {
    pub source: WorldPublicationId,
    pub project: String,
    pub roots: Vec<String>,
    pub budget: GeometryBudget,
}

impl GeometryRequest {
    pub fn new(
        source: WorldPublicationId,
        project: impl Into<String>,
        mut roots: Vec<String>,
        budget: GeometryBudget,
    ) -> Self {
        roots.sort();
        roots.dedup();
        Self {
            source,
            project: project.into(),
            roots,
            budget,
        }
    }

    pub fn key(&self) -> GeometryArtifactKey {
        GeometryArtifactKey {
            source: self.source,
            projection_schema: ProjectionSchemaDigest::current(),
            selection: SelectionFingerprint::derive(&self.project, &self.roots),
        }
    }

    fn canonicalized(&self) -> Self {
        Self::new(
            self.source,
            self.project.clone(),
            self.roots.clone(),
            self.budget,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnavailableReason {
    SourceMismatch {
        requested: WorldPublicationId,
        actual: WorldPublicationId,
    },
    BudgetExceeded {
        budget: GeometryBudget,
        estimate: GeometryEstimate,
    },
    DuplicateIssue(String),
    DanglingRelation {
        from: String,
        relation: String,
        to: String,
    },
    MultipleParents(String),
    ContainmentCycle(Vec<String>),
    TooManyNodes,
    ExecutorSaturated {
        workers: u32,
        queued_builds: u32,
    },
    /// The exact gated Corpus publication could not be read by the worker.
    SourceUnavailable,
    /// Corpus rows at the exact publication violated the declared Issues
    /// projection contract or described conflicting workflow heads.
    SourceCorrupt,
    BuildFailed,
    RetentionExceeded {
        required_bytes: u64,
        retained_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum GeometryReadiness {
    Pending,
    Ready,
    Unavailable { reason: UnavailableReason },
}

#[derive(Debug, Clone, Serialize)]
pub struct GeometryArtifact {
    key: GeometryArtifactKey,
    source: WorldPublicationId,
    estimate: GeometryEstimate,
    readiness: GeometryReadiness,
    #[serde(skip)]
    store: Option<Arc<CompactGeometry>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessFailure {
    Expired {
        expected: GeometryArtifactKey,
        available: GeometryArtifactKey,
    },
    NotReady {
        key: GeometryArtifactKey,
    },
    Unavailable(UnavailableReason),
    InvalidPage(&'static str),
}

impl GeometryArtifact {
    pub fn pending(request: &GeometryRequest, estimate: GeometryEstimate) -> Self {
        let request = request.canonicalized();
        Self {
            key: request.key(),
            source: request.source,
            estimate,
            readiness: GeometryReadiness::Pending,
            store: None,
        }
    }

    pub const fn key(&self) -> GeometryArtifactKey {
        self.key
    }

    pub const fn source(&self) -> WorldPublicationId {
        self.source
    }

    pub const fn estimate(&self) -> GeometryEstimate {
        self.estimate
    }

    pub const fn readiness(&self) -> &GeometryReadiness {
        &self.readiness
    }

    pub fn summary(
        &self,
        expected: &GeometryArtifactKey,
    ) -> Result<GeometrySummary, AccessFailure> {
        let store = self.ready(expected)?;
        Ok(store.summary())
    }

    pub fn page(
        &self,
        expected: &GeometryArtifactKey,
        request: GeometryPageRequest,
    ) -> Result<GeometryPage, AccessFailure> {
        let store = self.ready(expected)?;
        store.page(self.key, request)
    }

    fn ready(&self, expected: &GeometryArtifactKey) -> Result<&CompactGeometry, AccessFailure> {
        if self.key != *expected {
            return Err(AccessFailure::Expired {
                expected: *expected,
                available: self.key,
            });
        }
        match &self.readiness {
            GeometryReadiness::Pending => Err(AccessFailure::NotReady { key: self.key }),
            GeometryReadiness::Unavailable { reason } => {
                Err(AccessFailure::Unavailable(reason.clone()))
            }
            GeometryReadiness::Ready => self
                .store
                .as_deref()
                .ok_or(AccessFailure::NotReady { key: self.key }),
        }
    }

    fn unavailable(
        request: &GeometryRequest,
        estimate: GeometryEstimate,
        reason: UnavailableReason,
    ) -> Self {
        Self {
            key: request.key(),
            source: request.source,
            estimate,
            readiness: GeometryReadiness::Unavailable { reason },
            store: None,
        }
    }

    fn retained_bytes(&self) -> u64 {
        self.store
            .as_deref()
            .map(|store| store.retained_bytes)
            .unwrap_or(0)
    }
}

/// Bounded retention for immutable Geometry artifacts.
///
/// The default admits at least one artifact built under the protocol's default
/// 512 MiB working budget while bounding a product package to 32 exact
/// publication/selection coordinates. Operators embedding Issues may choose a
/// smaller cache; an artifact that cannot be retained is still returned to its
/// initiating request and may be rebuilt after eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeometryCacheLimits {
    pub entries: usize,
    pub retained_bytes: u64,
    pub workers: usize,
    pub queued_builds: usize,
}

impl Default for GeometryCacheLimits {
    fn default() -> Self {
        Self {
            entries: 32,
            retained_bytes: 1_024 * 1_024 * 1_024,
            workers: 2,
            queued_builds: 8,
        }
    }
}

#[derive(Debug)]
struct CachedGeometry {
    artifact: Arc<GeometryArtifact>,
    retained_bytes: u64,
    last_used: u64,
}

#[derive(Debug, Default)]
struct GeometryRegistryState {
    artifacts: BTreeMap<GeometryArtifactKey, CachedGeometry>,
    /// Small terminal refusals for builds too large to retain. Keeping these
    /// separate prevents an exact poll from scheduling the same impossible
    /// global rebuild forever even when artifact retention is configured zero.
    terminal: BTreeMap<GeometryArtifactKey, CachedGeometry>,
    building: BTreeSet<GeometryArtifactKey>,
    retained_bytes: u64,
    clock: u64,
}

impl GeometryRegistryState {
    fn tick(&mut self) -> u64 {
        self.clock = self.clock.saturating_add(1);
        self.clock
    }
}

/// Product-owned exact-key artifact registry.
///
/// A Runtime publication remains the source of truth. This registry shares
/// only immutable global analytics derived from that exact source, and never
/// answers a different [`GeometryArtifactKey`]. A miss elects one bounded
/// background builder; concurrent callers observe the same typed Pending and
/// later reuse its ready Arc. Facts are supplied lazily so a page hit performs
/// neither a corpus scan nor global graph compilation.
#[derive(Debug)]
pub struct GeometryRegistry {
    shared: Arc<GeometryRegistryShared>,
    executor: GeometryExecutor,
}

#[derive(Debug)]
struct GeometryRegistryShared {
    limits: GeometryCacheLimits,
    state: Mutex<GeometryRegistryState>,
}

struct GeometryExecutor {
    sender: std::sync::mpsc::SyncSender<Box<dyn FnOnce() + Send + 'static>>,
}

impl std::fmt::Debug for GeometryExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("GeometryExecutor").finish()
    }
}

impl GeometryExecutor {
    fn new(workers: usize, queued_builds: usize) -> Self {
        let (sender, receiver) =
            std::sync::mpsc::sync_channel::<Box<dyn FnOnce() + Send + 'static>>(queued_builds);
        let receiver = Arc::new(Mutex::new(receiver));
        for ordinal in 0..workers.max(1) {
            let receiver = receiver.clone();
            std::thread::Builder::new()
                .name(format!("issues-geometry-{ordinal}"))
                .spawn(move || loop {
                    let job = receiver
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .recv();
                    match job {
                        Ok(job) => job(),
                        Err(_) => break,
                    }
                })
                .expect("spawn bounded Geometry worker");
        }
        Self { sender }
    }
}

impl Default for GeometryRegistry {
    fn default() -> Self {
        Self::new(GeometryCacheLimits::default())
    }
}

impl GeometryRegistry {
    pub fn new(limits: GeometryCacheLimits) -> Self {
        let shared = Arc::new(GeometryRegistryShared {
            limits,
            state: Mutex::new(GeometryRegistryState::default()),
        });
        Self {
            shared,
            executor: GeometryExecutor::new(limits.workers, limits.queued_builds),
        }
    }

    /// Return a retained exact artifact without starting materialization.
    pub fn get(&self, key: &GeometryArtifactKey) -> Option<Arc<GeometryArtifact>> {
        let mut state = self.lock();
        let tick = state.tick();
        if let Some(cached) = state.artifacts.get_mut(key) {
            cached.last_used = tick;
            return Some(cached.artifact.clone());
        }
        let cached = state.terminal.get_mut(key)?;
        cached.last_used = tick;
        Some(cached.artifact.clone())
    }

    /// Return, reserve, or poll one exact immutable artifact.
    ///
    /// A miss reserves a bounded worker slot and immediately returns `Pending`
    /// with the caller's conservative estimate; global compilation never runs
    /// on the query thread. `facts` runs only for the elected miss worker.
    /// Subsequent calls are explicit polls and return the same Pending or the
    /// ready exact Arc. Queue saturation is a typed refusal. Budget is
    /// admission, not semantic identity: workers compile under the protocol
    /// budget and each poll applies the caller's budget independently.
    pub fn materialize_cached<F>(
        &self,
        request: &GeometryRequest,
        pending_estimate: GeometryEstimate,
        facts: F,
    ) -> Arc<GeometryArtifact>
    where
        F: FnOnce() -> GeometryFacts + Send + 'static,
    {
        self.materialize_cached_with(request, pending_estimate, move || Ok(facts()))
    }

    /// Fallible form used by deferred Corpus projection. Source refusal is
    /// retained as a small typed exact-key result just like other terminal
    /// analytics failures; polling never retries it against mutable current
    /// state.
    pub fn materialize_cached_with<F>(
        &self,
        request: &GeometryRequest,
        pending_estimate: GeometryEstimate,
        facts: F,
    ) -> Arc<GeometryArtifact>
    where
        F: FnOnce() -> Result<GeometryFacts, UnavailableReason> + Send + 'static,
    {
        let request = request.canonicalized();
        let key = request.key();
        {
            let mut state = self.lock();
            let tick = state.tick();
            if let Some(cached) = state.artifacts.get_mut(&key) {
                cached.last_used = tick;
                let artifact = cached.artifact.clone();
                drop(state);
                return self.admit_budget(&request, artifact);
            }
            if let Some(cached) = state.terminal.get_mut(&key) {
                cached.last_used = tick;
                return cached.artifact.clone();
            }
            if state.building.contains(&key) {
                return Arc::new(GeometryArtifact::pending(&request, pending_estimate));
            }
            state.building.insert(key);
        }

        let shared = self.shared.clone();
        let worker_request = GeometryRequest::new(
            request.source,
            request.project.clone(),
            request.roots.clone(),
            GeometryBudget::default(),
        );
        let schedule = self.executor.sender.try_send(Box::new(move || {
            let artifact = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| facts()))
                .map(|result| match result {
                    Ok(facts) => Arc::new(materialize(&worker_request, &facts)),
                    Err(reason) => Arc::new(GeometryArtifact::unavailable(
                        &worker_request,
                        pending_estimate,
                        reason,
                    )),
                })
                .unwrap_or_else(|_| {
                    Arc::new(GeometryArtifact::unavailable(
                        &worker_request,
                        pending_estimate,
                        UnavailableReason::BuildFailed,
                    ))
                });
            GeometryRegistry::retain_shared(&shared, artifact);
            let mut state = shared
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.building.remove(&key);
        }));
        if schedule.is_err() {
            let mut state = self.lock();
            state.building.remove(&key);
            return Arc::new(GeometryArtifact::unavailable(
                &request,
                pending_estimate,
                UnavailableReason::ExecutorSaturated {
                    workers: usize_u32(self.shared.limits.workers.max(1)),
                    queued_builds: usize_u32(self.shared.limits.queued_builds),
                },
            ));
        }
        Arc::new(GeometryArtifact::pending(&request, pending_estimate))
    }

    fn admit_budget(
        &self,
        request: &GeometryRequest,
        artifact: Arc<GeometryArtifact>,
    ) -> Arc<GeometryArtifact> {
        if request.budget.contains(artifact.estimate()) {
            return artifact;
        }
        Arc::new(GeometryArtifact::unavailable(
            request,
            artifact.estimate(),
            UnavailableReason::BudgetExceeded {
                budget: request.budget,
                estimate: artifact.estimate(),
            },
        ))
    }

    fn retain_shared(shared: &GeometryRegistryShared, artifact: Arc<GeometryArtifact>) {
        let key = artifact.key();
        let retained_bytes = artifact.retained_bytes();
        let mut state = shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let last_used = state.tick();
        if matches!(artifact.readiness(), GeometryReadiness::Unavailable { .. }) {
            state.terminal.insert(
                key,
                CachedGeometry {
                    artifact,
                    retained_bytes: 0,
                    last_used,
                },
            );
            Self::bound_terminal(&mut state);
            return;
        }
        if matches!(artifact.readiness(), GeometryReadiness::Ready)
            && (shared.limits.entries == 0 || retained_bytes > shared.limits.retained_bytes)
        {
            let terminal = Arc::new(GeometryArtifact {
                key,
                source: artifact.source(),
                estimate: artifact.estimate(),
                readiness: GeometryReadiness::Unavailable {
                    reason: UnavailableReason::RetentionExceeded {
                        required_bytes: retained_bytes,
                        retained_bytes: shared.limits.retained_bytes,
                    },
                },
                store: None,
            });
            state.terminal.insert(
                key,
                CachedGeometry {
                    artifact: terminal,
                    retained_bytes: 0,
                    last_used,
                },
            );
            Self::bound_terminal(&mut state);
            return;
        }
        state.retained_bytes = state.retained_bytes.saturating_add(retained_bytes);
        state.artifacts.insert(
            key,
            CachedGeometry {
                artifact,
                retained_bytes,
                last_used,
            },
        );
        while state.artifacts.len() > shared.limits.entries
            || state.retained_bytes > shared.limits.retained_bytes
        {
            let Some(victim) = state
                .artifacts
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(evicted) = state.artifacts.remove(&victim) {
                state.retained_bytes = state.retained_bytes.saturating_sub(evicted.retained_bytes);
            }
        }
    }

    fn bound_terminal(state: &mut GeometryRegistryState) {
        while state.terminal.len() > MAX_TERMINAL_GEOMETRY_RESULTS {
            let victim = state
                .terminal
                .iter()
                .min_by_key(|(_, cached)| cached.last_used)
                .map(|(key, _)| *key);
            if let Some(victim) = victim {
                state.terminal.remove(&victim);
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GeometryRegistryState> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    fn retained(&self) -> (usize, u64) {
        let state = self.lock();
        (state.artifacts.len(), state.retained_bytes)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ComponentId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RegionId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ResidualId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationKind {
    Blocks,
    Duplicates,
    Contains,
    Association,
}

impl RelationKind {
    fn parse(value: &str) -> Self {
        match value {
            "blocks" => Self::Blocks,
            "duplicates" => Self::Duplicates,
            "contains" => Self::Contains,
            _ => Self::Association,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationRole {
    Constraint,
    Equivalence,
    Containment,
    Association,
}

impl From<RelationKind> for RelationRole {
    fn from(value: RelationKind) -> Self {
        match value {
            RelationKind::Blocks => Self::Constraint,
            RelationKind::Duplicates => Self::Equivalence,
            RelationKind::Contains => Self::Containment,
            RelationKind::Association => Self::Association,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClosureKind {
    Closed,
    Ready,
    Blocked,
    Cycle,
    Stalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResidualKind {
    RootMissing,
    DependencyCycle,
    BlockedFrontier,
    DueOrderConflict,
    Unattached,
    ClosureFrontier,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryClosure {
    pub total: u32,
    pub closed: u32,
    pub ready: u32,
    pub blocked: u32,
    pub cyclic: u32,
    pub stalled: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometrySummary {
    pub schema_version: u32,
    pub project: String,
    pub roots: u32,
    pub nodes: u32,
    pub edges: u32,
    pub components: u32,
    pub regions: u32,
    pub residuals: u32,
    pub closure: GeometryClosure,
    pub retained_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryNodePageRow {
    pub node: NodeId,
    pub id: String,
    pub component: ComponentId,
    pub layer: Option<u32>,
    pub ordinal: u32,
    pub hierarchy_depth: u32,
    pub region: Option<RegionId>,
    pub parent: Option<NodeId>,
    pub closure: ClosureKind,
    pub slack: Option<u32>,
    pub children: u32,
    pub blocked_by: u32,
    pub blocks: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryEdgePageRow {
    pub from: NodeId,
    pub from_id: String,
    pub relation: RelationKind,
    pub role: RelationRole,
    pub to: NodeId,
    pub to_id: String,
    pub implied: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryComponentPageRow {
    pub component: ComponentId,
    pub members: u32,
    pub roots: u32,
    pub terminals: u32,
    pub loops: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryRegionPageRow {
    pub region: RegionId,
    pub root: NodeId,
    pub root_id: String,
    pub members: u32,
    pub layer: Option<u32>,
    pub ordinal: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryResidualPageRow {
    pub residual: ResidualId,
    pub kind: ResidualKind,
    /// Missing canonical id for a `root_missing` locus. The compact artifact
    /// retains only a dictionary index and materializes the string per page.
    pub missing: Option<String>,
    pub component: Option<ComponentId>,
    pub layer: Option<u32>,
    pub at: u32,
    pub requires: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryMemberPageRow {
    pub node: NodeId,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryRootPageRow {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeometrySection {
    Roots,
    Nodes,
    Edges,
    Components,
    ComponentMembers(ComponentId),
    ComponentRoots(ComponentId),
    ComponentTerminals(ComponentId),
    Regions,
    RegionMembers(RegionId),
    Residuals,
    ResidualAt(ResidualId),
    ResidualRequires(ResidualId),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryCursor {
    artifact: GeometryArtifactKey,
    section: GeometrySection,
    offset: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryPageRequest {
    pub section: GeometrySection,
    pub limit: u32,
    pub cursor: Option<GeometryCursor>,
}

impl GeometryPageRequest {
    pub fn first(section: GeometrySection, limit: u32) -> Self {
        Self {
            section,
            limit,
            cursor: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeometryRows {
    Roots(Vec<GeometryRootPageRow>),
    Nodes(Vec<GeometryNodePageRow>),
    Edges(Vec<GeometryEdgePageRow>),
    Components(Vec<GeometryComponentPageRow>),
    Regions(Vec<GeometryRegionPageRow>),
    Residuals(Vec<GeometryResidualPageRow>),
    Members(Vec<GeometryMemberPageRow>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeometryPage {
    pub rows: GeometryRows,
    pub next: Option<GeometryCursor>,
}

#[derive(Debug, Clone)]
struct Span {
    start: u32,
    len: u32,
}

impl Span {
    fn range(&self, total: usize) -> std::ops::Range<usize> {
        let start = usize::try_from(self.start).unwrap_or(total).min(total);
        let end = start
            .saturating_add(usize::try_from(self.len).unwrap_or(usize::MAX))
            .min(total);
        start..end
    }
}

#[derive(Debug, Clone, Default)]
struct NodeColumns {
    name: Vec<u32>,
    component: Vec<ComponentId>,
    layer: Vec<Option<u32>>,
    ordinal: Vec<u32>,
    hierarchy_depth: Vec<u32>,
    region: Vec<Option<RegionId>>,
    parent: Vec<Option<NodeId>>,
    closure: Vec<ClosureKind>,
    slack: Vec<Option<u32>>,
    children: Vec<Span>,
    blocked_by: Vec<Span>,
    blocks: Vec<Span>,
}

#[derive(Debug, Clone, Default)]
struct EdgeColumns {
    from: Vec<NodeId>,
    relation: Vec<RelationKind>,
    to: Vec<NodeId>,
    implied: Vec<bool>,
}

#[derive(Debug, Clone)]
struct CompactComponent {
    members: Span,
    roots: Span,
    terminals: Span,
    loops: u32,
}

#[derive(Debug, Clone)]
struct CompactRegion {
    root: NodeId,
    members: Span,
    layer: Option<u32>,
    ordinal: u32,
}

#[derive(Debug, Clone)]
struct CompactResidual {
    kind: ResidualKind,
    missing: Option<u32>,
    component: Option<ComponentId>,
    layer: Option<u32>,
    at: Span,
    requires: Span,
}

#[derive(Debug, Clone)]
struct CompactGeometry {
    project: u32,
    roots: Vec<u32>,
    dictionary: Vec<String>,
    nodes: NodeColumns,
    edges: EdgeColumns,
    adjacency: Vec<NodeId>,
    component_members: Vec<NodeId>,
    components: Vec<CompactComponent>,
    region_members: Vec<NodeId>,
    regions: Vec<CompactRegion>,
    residual_members: Vec<NodeId>,
    residuals: Vec<CompactResidual>,
    closure: GeometryClosure,
    retained_bytes: u64,
}

impl CompactGeometry {
    fn dictionary_name(&self, index: u32) -> &str {
        self.dictionary
            .get(usize::try_from(index).unwrap_or(usize::MAX))
            .map(String::as_str)
            .unwrap_or("")
    }

    fn name(&self, node: NodeId) -> &str {
        let index = usize::try_from(node.0).unwrap_or(usize::MAX);
        let dictionary = self.nodes.name.get(index).copied().unwrap_or(u32::MAX);
        self.dictionary
            .get(usize::try_from(dictionary).unwrap_or(usize::MAX))
            .map(String::as_str)
            .unwrap_or("")
    }

    fn summary(&self) -> GeometrySummary {
        GeometrySummary {
            schema_version: 3,
            project: self.dictionary_name(self.project).to_owned(),
            roots: usize_u32(self.roots.len()),
            nodes: usize_u32(self.nodes.name.len()),
            edges: usize_u32(self.edges.from.len()),
            components: usize_u32(self.components.len()),
            regions: usize_u32(self.regions.len()),
            residuals: usize_u32(self.residuals.len()),
            closure: self.closure.clone(),
            retained_bytes: self.retained_bytes,
        }
    }

    fn page(
        &self,
        key: GeometryArtifactKey,
        request: GeometryPageRequest,
    ) -> Result<GeometryPage, AccessFailure> {
        if !(1..=MAX_GEOMETRY_PAGE).contains(&request.limit) {
            return Err(AccessFailure::InvalidPage("limit"));
        }
        let offset = match &request.cursor {
            None => 0,
            Some(cursor) => {
                if cursor.artifact != key || cursor.section != request.section {
                    return Err(AccessFailure::InvalidPage("cursor"));
                }
                cursor.offset
            }
        };
        let limit = usize::try_from(request.limit).unwrap_or(0);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let (rows, total) = match &request.section {
            GeometrySection::Roots => {
                let total = self.roots.len();
                let end = start.saturating_add(limit).min(total);
                let rows = self.roots[start.min(total)..end]
                    .iter()
                    .map(|root| GeometryRootPageRow {
                        id: self.dictionary_name(*root).to_owned(),
                    })
                    .collect();
                (GeometryRows::Roots(rows), total)
            }
            GeometrySection::Nodes => {
                let total = self.nodes.name.len();
                let end = start.saturating_add(limit).min(total);
                let rows = (start.min(total)..end)
                    .map(|index| GeometryNodePageRow {
                        node: NodeId(usize_u32(index)),
                        id: self.name(NodeId(usize_u32(index))).to_owned(),
                        component: self.nodes.component[index],
                        layer: self.nodes.layer[index],
                        ordinal: self.nodes.ordinal[index],
                        hierarchy_depth: self.nodes.hierarchy_depth[index],
                        region: self.nodes.region[index],
                        parent: self.nodes.parent[index],
                        closure: self.nodes.closure[index],
                        slack: self.nodes.slack[index],
                        children: self.nodes.children[index].len,
                        blocked_by: self.nodes.blocked_by[index].len,
                        blocks: self.nodes.blocks[index].len,
                    })
                    .collect();
                (GeometryRows::Nodes(rows), total)
            }
            GeometrySection::Edges => {
                let total = self.edges.from.len();
                let end = start.saturating_add(limit).min(total);
                let rows = (start.min(total)..end)
                    .map(|index| {
                        let from = self.edges.from[index];
                        let to = self.edges.to[index];
                        let relation = self.edges.relation[index];
                        GeometryEdgePageRow {
                            from,
                            from_id: self.name(from).to_owned(),
                            relation,
                            role: relation.into(),
                            to,
                            to_id: self.name(to).to_owned(),
                            implied: self.edges.implied[index],
                        }
                    })
                    .collect();
                (GeometryRows::Edges(rows), total)
            }
            GeometrySection::Components => {
                let total = self.components.len();
                let end = start.saturating_add(limit).min(total);
                let rows = (start.min(total)..end)
                    .map(|index| {
                        let component = &self.components[index];
                        GeometryComponentPageRow {
                            component: ComponentId(usize_u32(index)),
                            members: component.members.len,
                            roots: component.roots.len,
                            terminals: component.terminals.len,
                            loops: component.loops,
                        }
                    })
                    .collect();
                (GeometryRows::Components(rows), total)
            }
            GeometrySection::Regions => {
                let total = self.regions.len();
                let end = start.saturating_add(limit).min(total);
                let rows = (start.min(total)..end)
                    .map(|index| {
                        let region = &self.regions[index];
                        GeometryRegionPageRow {
                            region: RegionId(usize_u32(index)),
                            root: region.root,
                            root_id: self.name(region.root).to_owned(),
                            members: region.members.len,
                            layer: region.layer,
                            ordinal: region.ordinal,
                        }
                    })
                    .collect();
                (GeometryRows::Regions(rows), total)
            }
            GeometrySection::Residuals => {
                let total = self.residuals.len();
                let end = start.saturating_add(limit).min(total);
                let rows = (start.min(total)..end)
                    .map(|index| {
                        let residual = &self.residuals[index];
                        GeometryResidualPageRow {
                            residual: ResidualId(usize_u32(index)),
                            kind: residual.kind,
                            missing: residual
                                .missing
                                .and_then(|name| {
                                    self.dictionary
                                        .get(usize::try_from(name).unwrap_or(usize::MAX))
                                })
                                .cloned(),
                            component: residual.component,
                            layer: residual.layer,
                            at: residual.at.len,
                            requires: residual.requires.len,
                        }
                    })
                    .collect();
                (GeometryRows::Residuals(rows), total)
            }
            GeometrySection::ComponentMembers(component)
            | GeometrySection::ComponentRoots(component)
            | GeometrySection::ComponentTerminals(component) => {
                let compact = self
                    .components
                    .get(usize::try_from(component.0).unwrap_or(usize::MAX))
                    .ok_or(AccessFailure::InvalidPage("component"))?;
                let span = match request.section {
                    GeometrySection::ComponentMembers(_) => &compact.members,
                    GeometrySection::ComponentRoots(_) => &compact.roots,
                    GeometrySection::ComponentTerminals(_) => &compact.terminals,
                    _ => unreachable!(),
                };
                member_rows(self, &self.component_members, span, start, limit)
            }
            GeometrySection::RegionMembers(region) => {
                let span = &self
                    .regions
                    .get(usize::try_from(region.0).unwrap_or(usize::MAX))
                    .ok_or(AccessFailure::InvalidPage("region"))?
                    .members;
                member_rows(self, &self.region_members, span, start, limit)
            }
            GeometrySection::ResidualAt(residual) | GeometrySection::ResidualRequires(residual) => {
                let compact = self
                    .residuals
                    .get(usize::try_from(residual.0).unwrap_or(usize::MAX))
                    .ok_or(AccessFailure::InvalidPage("residual"))?;
                let span = match request.section {
                    GeometrySection::ResidualAt(_) => &compact.at,
                    GeometrySection::ResidualRequires(_) => &compact.requires,
                    _ => unreachable!(),
                };
                member_rows(self, &self.residual_members, span, start, limit)
            }
        };
        let next_offset = start.saturating_add(limit).min(total);
        let next = (next_offset < total).then(|| GeometryCursor {
            artifact: key,
            section: request.section,
            offset: usize_u32(next_offset),
        });
        Ok(GeometryPage { rows, next })
    }
}

fn member_rows(
    geometry: &CompactGeometry,
    values: &[NodeId],
    span: &Span,
    start: usize,
    limit: usize,
) -> (GeometryRows, usize) {
    let range = span.range(values.len());
    let total = range.len();
    let local_start = start.min(total);
    let local_end = local_start.saturating_add(limit).min(total);
    let rows = values
        [range.start.saturating_add(local_start)..range.start.saturating_add(local_end)]
        .iter()
        .map(|node| GeometryMemberPageRow {
            node: *node,
            id: geometry.name(*node).to_owned(),
        })
        .collect();
    (GeometryRows::Members(rows), total)
}

#[derive(Clone, Copy)]
struct Edge {
    from: NodeId,
    relation: RelationKind,
    to: NodeId,
}

struct Prepared {
    dictionary: Vec<String>,
    closed: Vec<bool>,
    due: Vec<Option<i64>>,
    edges: Vec<Edge>,
    roots: Vec<String>,
    estimate: GeometryEstimate,
}

pub fn estimate(
    request: &GeometryRequest,
    facts: &GeometryFacts,
) -> Result<GeometryEstimate, UnavailableReason> {
    prepare(&request.canonicalized(), facts).map(|prepared| prepared.estimate)
}

pub fn materialize(request: &GeometryRequest, facts: &GeometryFacts) -> GeometryArtifact {
    let request = request.canonicalized();
    if request.source != facts.source {
        return GeometryArtifact::unavailable(
            &request,
            GeometryEstimate::default(),
            UnavailableReason::SourceMismatch {
                requested: request.source,
                actual: facts.source,
            },
        );
    }
    let prepared = match prepare(&request, facts) {
        Ok(prepared) => prepared,
        Err(reason) => {
            return GeometryArtifact::unavailable(&request, GeometryEstimate::default(), reason)
        }
    };
    if !request.budget.contains(prepared.estimate) {
        return GeometryArtifact::unavailable(
            &request,
            prepared.estimate,
            UnavailableReason::BudgetExceeded {
                budget: request.budget,
                estimate: prepared.estimate,
            },
        );
    }
    let estimate = prepared.estimate;
    match compile_compact(&request, prepared) {
        Ok(store) => GeometryArtifact {
            key: request.key(),
            source: request.source,
            estimate,
            readiness: GeometryReadiness::Ready,
            store: Some(Arc::new(store)),
        },
        Err(reason) => GeometryArtifact::unavailable(&request, estimate, reason),
    }
}

fn prepare(
    request: &GeometryRequest,
    facts: &GeometryFacts,
) -> Result<Prepared, UnavailableReason> {
    if request.source != facts.source {
        return Err(UnavailableReason::SourceMismatch {
            requested: request.source,
            actual: facts.source,
        });
    }
    if facts.issues.len() > usize::try_from(u32::MAX).unwrap_or(usize::MAX) {
        return Err(UnavailableReason::TooManyNodes);
    }
    let mut by_name: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, issue) in facts.issues.iter().enumerate() {
        if by_name.insert(&issue.id, index).is_some() {
            return Err(UnavailableReason::DuplicateIssue(issue.id.clone()));
        }
    }
    let candidates: Vec<bool> = facts
        .issues
        .iter()
        .map(|issue| issue.project == request.project)
        .collect();
    let mut raw_edges = Vec::with_capacity(facts.relations.len());
    let mut adjacency = vec![Vec::<usize>::new(); facts.issues.len()];
    for relation in &facts.relations {
        let Some(&from) = by_name.get(relation.from.as_str()) else {
            return Err(UnavailableReason::DanglingRelation {
                from: relation.from.clone(),
                relation: relation.relation.clone(),
                to: relation.to.clone(),
            });
        };
        let Some(&to) = by_name.get(relation.to.as_str()) else {
            return Err(UnavailableReason::DanglingRelation {
                from: relation.from.clone(),
                relation: relation.relation.clone(),
                to: relation.to.clone(),
            });
        };
        if candidates[from] && candidates[to] {
            raw_edges.push((from, RelationKind::parse(&relation.relation), to));
            adjacency[from].push(to);
            adjacency[to].push(from);
        }
    }
    for neighbors in &mut adjacency {
        neighbors.sort_unstable();
        neighbors.dedup();
    }
    let mut selected = vec![request.roots.is_empty(); facts.issues.len()];
    for (index, candidate) in candidates.iter().enumerate() {
        selected[index] &= *candidate;
    }
    if !request.roots.is_empty() {
        let mut queue = VecDeque::new();
        for root in &request.roots {
            if let Some(&index) = by_name.get(root.as_str()) {
                if candidates[index] {
                    queue.push_back(index);
                }
            }
        }
        while let Some(index) = queue.pop_front() {
            if selected[index] {
                continue;
            }
            selected[index] = true;
            for &next in &adjacency[index] {
                if candidates[next] && !selected[next] {
                    queue.push_back(next);
                }
            }
        }
    }
    let mut selected_old: Vec<usize> = (0..facts.issues.len())
        .filter(|index| selected[*index])
        .collect();
    selected_old.sort_by(|left, right| facts.issues[*left].id.cmp(&facts.issues[*right].id));
    let mut remap = vec![None; facts.issues.len()];
    let mut dictionary = Vec::with_capacity(selected_old.len());
    let mut closed = Vec::with_capacity(selected_old.len());
    let mut due = Vec::with_capacity(selected_old.len());
    for (compact, old) in selected_old.into_iter().enumerate() {
        remap[old] = Some(NodeId(usize_u32(compact)));
        dictionary.push(facts.issues[old].id.clone());
        closed.push(facts.issues[old].closed);
        due.push(facts.issues[old].due);
    }
    let mut edges: Vec<Edge> = raw_edges
        .into_iter()
        .filter_map(|(from, relation, to)| {
            Some(Edge {
                from: remap[from]?,
                relation,
                to: remap[to]?,
            })
        })
        .collect();
    edges.sort_by_key(|edge| (edge.from, edge.relation as u8, edge.to));
    edges.dedup_by_key(|edge| (edge.from, edge.relation as u8, edge.to));
    let blocking_fanout = {
        let mut fanout = vec![0u64; dictionary.len()];
        for edge in &edges {
            if edge.relation == RelationKind::Blocks {
                fanout[usize::try_from(edge.from.0).unwrap_or(usize::MAX)] =
                    fanout[usize::try_from(edge.from.0).unwrap_or(usize::MAX)].saturating_add(1);
            }
        }
        fanout
    };
    let reduction_candidates = edges
        .iter()
        .filter(|edge| {
            edge.relation == RelationKind::Blocks
                && blocking_fanout[usize::try_from(edge.from.0).unwrap_or(usize::MAX)] > 1
        })
        .count();
    let nodes = usize_u64(dictionary.len());
    let edge_count = usize_u64(edges.len());
    let reductions = usize_u64(reduction_candidates);
    let root_count = usize_u64(request.roots.len());
    let root_bytes = request.roots.iter().fold(0u64, |total, root| {
        total.saturating_add(usize_u64(root.len()))
    });
    let estimate = GeometryEstimate {
        selected_nodes: nodes,
        selected_edges: edge_count,
        reduction_candidates: reductions,
        node_visits: usize_u64(facts.issues.len())
            .saturating_mul(2)
            .saturating_add(root_count)
            .saturating_add(nodes.saturating_mul(16)),
        edge_visits: usize_u64(facts.relations.len()).saturating_add(edge_count.saturating_mul(20)),
        reachability_visits: reductions.saturating_mul(nodes.saturating_add(edge_count)),
        working_bytes: usize_u64(facts.issues.len())
            .saturating_mul(64)
            .saturating_add(usize_u64(facts.relations.len()).saturating_mul(32))
            .saturating_add(nodes.saturating_mul(192))
            .saturating_add(edge_count.saturating_mul(48))
            .saturating_add(root_bytes)
            .saturating_add(root_count.saturating_mul(32)),
    };
    Ok(Prepared {
        dictionary,
        closed,
        due,
        edges,
        roots: request.roots.clone(),
        estimate,
    })
}

fn compile_compact(
    request: &GeometryRequest,
    prepared: Prepared,
) -> Result<CompactGeometry, UnavailableReason> {
    let count = prepared.dictionary.len();
    let mut blocked_by = vec![Vec::<NodeId>::new(); count];
    let mut blocks = vec![Vec::<NodeId>::new(); count];
    let mut undirected = vec![Vec::<NodeId>::new(); count];
    let mut parent = vec![None; count];
    for edge in &prepared.edges {
        let from = node_index(edge.from, count);
        let to = node_index(edge.to, count);
        undirected[from].push(edge.to);
        undirected[to].push(edge.from);
        match edge.relation {
            RelationKind::Blocks => {
                blocks[from].push(edge.to);
                blocked_by[to].push(edge.from);
            }
            RelationKind::Contains => {
                if parent[to].replace(edge.from).is_some() {
                    return Err(UnavailableReason::MultipleParents(
                        prepared.dictionary[to].clone(),
                    ));
                }
            }
            RelationKind::Duplicates | RelationKind::Association => {}
        }
    }
    for values in undirected
        .iter_mut()
        .chain(blocked_by.iter_mut())
        .chain(blocks.iter_mut())
    {
        values.sort_unstable();
        values.dedup();
    }
    validate_parent_cycles(&parent, &prepared.dictionary)?;

    let (component_of, component_sets) = components(&undirected);
    let (layer, loops, cyclic, stalled) = dependency_layers(&blocked_by, &blocks);
    let max_layer = layer.iter().flatten().copied().max().unwrap_or(0);
    let mut onward = vec![0u32; count];
    let mut placed: Vec<usize> = (0..count).filter(|index| layer[*index].is_some()).collect();
    placed.sort_by_key(|index| std::cmp::Reverse(layer[*index].unwrap_or(0)));
    for &index in &placed {
        onward[index] = blocks[index]
            .iter()
            .map(|next| onward[node_index(*next, count)].saturating_add(1))
            .max()
            .unwrap_or(0);
    }
    let slack: Vec<Option<u32>> = (0..count)
        .map(|index| {
            layer[index].map(|at| max_layer.saturating_sub(at.saturating_add(onward[index])))
        })
        .collect();

    let mut hierarchy_depth = vec![0u32; count];
    let mut hierarchy_root = vec![NodeId(0); count];
    for index in 0..count {
        let mut cursor = NodeId(usize_u32(index));
        let mut depth = 0u32;
        while let Some(next) = parent[node_index(cursor, count)] {
            depth = depth.saturating_add(1);
            cursor = next;
        }
        hierarchy_depth[index] = depth;
        hierarchy_root[index] = cursor;
    }
    let mut children = vec![Vec::<NodeId>::new(); count];
    for (child, parent) in parent.iter().enumerate() {
        if let Some(parent) = parent {
            children[node_index(*parent, count)].push(NodeId(usize_u32(child)));
        }
    }
    for values in &mut children {
        values.sort_unstable();
    }

    let (region_of, region_sets, region_layers) = regions(&hierarchy_root, &blocks, count);
    let mut layer_ordinals: BTreeMap<Option<u32>, u32> = BTreeMap::new();
    let mut ordinal = vec![0u32; count];
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|index| (layer[*index], hierarchy_depth[*index], *index));
    for index in order {
        let value = layer_ordinals.entry(layer[index]).or_default();
        ordinal[index] = *value;
        *value = value.saturating_add(1);
    }

    let mut closure = GeometryClosure {
        total: usize_u32(count),
        ..GeometryClosure::default()
    };
    let mut closure_kind = Vec::with_capacity(count);
    let mut residual_specs: Vec<(
        ResidualKind,
        Option<String>,
        Option<ComponentId>,
        Option<u32>,
        Vec<NodeId>,
        Vec<NodeId>,
    )> = Vec::new();
    for index in 0..count {
        let open_blockers: Vec<NodeId> = blocked_by[index]
            .iter()
            .copied()
            .filter(|blocker| !prepared.closed[node_index(*blocker, count)])
            .collect();
        let kind = if prepared.closed[index] {
            closure.closed = closure.closed.saturating_add(1);
            ClosureKind::Closed
        } else if cyclic[index] {
            closure.cyclic = closure.cyclic.saturating_add(1);
            ClosureKind::Cycle
        } else if stalled[index] {
            closure.stalled = closure.stalled.saturating_add(1);
            ClosureKind::Stalled
        } else if open_blockers.is_empty() {
            closure.ready = closure.ready.saturating_add(1);
            ClosureKind::Ready
        } else {
            closure.blocked = closure.blocked.saturating_add(1);
            ClosureKind::Blocked
        };
        closure_kind.push(kind);
        let node = NodeId(usize_u32(index));
        if kind == ClosureKind::Blocked {
            residual_specs.push((
                ResidualKind::BlockedFrontier,
                None,
                Some(component_of[index]),
                layer[index],
                vec![node],
                open_blockers,
            ));
        }
        if let Some(due) = prepared.due[index] {
            let conflicts: Vec<_> = blocked_by[index]
                .iter()
                .copied()
                .filter(|blocker| {
                    prepared.due[node_index(*blocker, count)]
                        .is_some_and(|blocker_due| blocker_due >= due)
                })
                .collect();
            if !conflicts.is_empty() {
                residual_specs.push((
                    ResidualKind::DueOrderConflict,
                    None,
                    Some(component_of[index]),
                    layer[index],
                    vec![node],
                    conflicts,
                ));
            }
        }
        if undirected[index].is_empty()
            && !prepared
                .roots
                .iter()
                .any(|root| root == &prepared.dictionary[index])
            && count > 1
        {
            residual_specs.push((
                ResidualKind::Unattached,
                None,
                Some(component_of[index]),
                layer[index],
                vec![node],
                Vec::new(),
            ));
        }
        if !prepared.closed[index] && blocks[index].is_empty() && children[index].is_empty() {
            residual_specs.push((
                ResidualKind::ClosureFrontier,
                None,
                Some(component_of[index]),
                layer[index],
                vec![node],
                Vec::new(),
            ));
        }
    }
    for loop_nodes in &loops {
        residual_specs.push((
            ResidualKind::DependencyCycle,
            None,
            loop_nodes
                .first()
                .map(|node| component_of[node_index(*node, count)]),
            None,
            loop_nodes.clone(),
            loop_nodes.clone(),
        ));
    }

    let mut dictionary = prepared.dictionary;
    for root in &prepared.roots {
        if !dictionary.iter().any(|value| value == root) {
            dictionary.push(root.clone());
            residual_specs.push((
                ResidualKind::RootMissing,
                Some(root.clone()),
                None,
                None,
                Vec::new(),
                Vec::new(),
            ));
        }
    }
    if !dictionary.iter().any(|value| value == &request.project) {
        dictionary.push(request.project.clone());
    }
    let dictionary_index: BTreeMap<&str, u32> = dictionary
        .iter()
        .enumerate()
        .map(|(index, value)| (value.as_str(), usize_u32(index)))
        .collect();
    let project = dictionary_index
        .get(request.project.as_str())
        .copied()
        .ok_or(UnavailableReason::TooManyNodes)?;
    let roots = request
        .roots
        .iter()
        .map(|root| {
            dictionary_index
                .get(root.as_str())
                .copied()
                .ok_or(UnavailableReason::TooManyNodes)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut adjacency = Vec::new();
    let mut node_columns = NodeColumns::default();
    for index in 0..count {
        node_columns.name.push(usize_u32(index));
        node_columns.component.push(component_of[index]);
        node_columns.layer.push(layer[index]);
        node_columns.ordinal.push(ordinal[index]);
        node_columns.hierarchy_depth.push(hierarchy_depth[index]);
        node_columns.region.push(region_of[index]);
        node_columns.parent.push(parent[index]);
        node_columns.closure.push(closure_kind[index]);
        node_columns.slack.push(slack[index]);
        node_columns
            .children
            .push(append_span(&mut adjacency, &children[index]));
        node_columns
            .blocked_by
            .push(append_span(&mut adjacency, &blocked_by[index]));
        node_columns
            .blocks
            .push(append_span(&mut adjacency, &blocks[index]));
    }

    let implied: BTreeSet<(NodeId, NodeId)> = prepared
        .edges
        .iter()
        .filter(|edge| edge.relation == RelationKind::Blocks)
        .filter(|edge| implied_by_longer_path(edge.from, edge.to, &blocks, &layer))
        .map(|edge| (edge.from, edge.to))
        .collect();
    let mut edge_columns = EdgeColumns::default();
    for edge in prepared.edges {
        edge_columns.from.push(edge.from);
        edge_columns.relation.push(edge.relation);
        edge_columns.to.push(edge.to);
        edge_columns
            .implied
            .push(implied.contains(&(edge.from, edge.to)));
    }

    let mut component_members = Vec::new();
    let mut components = Vec::new();
    for (index, members) in component_sets.iter().enumerate() {
        let member_set: BTreeSet<NodeId> = members.iter().copied().collect();
        let roots: Vec<_> = members
            .iter()
            .copied()
            .filter(|node| {
                blocked_by[node_index(*node, count)]
                    .iter()
                    .all(|candidate| !member_set.contains(candidate))
            })
            .collect();
        let terminals: Vec<_> = members
            .iter()
            .copied()
            .filter(|node| {
                blocks[node_index(*node, count)]
                    .iter()
                    .all(|candidate| !member_set.contains(candidate))
            })
            .collect();
        let loop_count = loops
            .iter()
            .filter(|nodes| {
                nodes.first().is_some_and(|node| {
                    component_of[node_index(*node, count)].0 == usize_u32(index)
                })
            })
            .count();
        components.push(CompactComponent {
            members: append_span(&mut component_members, members),
            roots: append_span(&mut component_members, &roots),
            terminals: append_span(&mut component_members, &terminals),
            loops: usize_u32(loop_count),
        });
    }

    let mut region_members = Vec::new();
    let mut compact_regions = Vec::new();
    let mut region_ordinals: BTreeMap<Option<u32>, u32> = BTreeMap::new();
    for (index, members) in region_sets.iter().enumerate() {
        let root = hierarchy_root[node_index(members[0], count)];
        let at = region_layers[index];
        let ordinal = region_ordinals.entry(at).or_default();
        compact_regions.push(CompactRegion {
            root,
            members: append_span(&mut region_members, members),
            layer: at,
            ordinal: *ordinal,
        });
        *ordinal = ordinal.saturating_add(1);
    }

    let mut residual_members = Vec::new();
    let residuals = residual_specs
        .into_iter()
        .map(
            |(kind, missing, component, layer, at, requires)| CompactResidual {
                kind,
                missing: missing
                    .as_deref()
                    .and_then(|name| dictionary_index.get(name).copied()),
                component,
                layer,
                at: append_span(&mut residual_members, &at),
                requires: append_span(&mut residual_members, &requires),
            },
        )
        .collect::<Vec<_>>();

    let string_bytes = dictionary.iter().fold(0u64, |total, value| {
        total.saturating_add(usize_u64(value.len()))
    });
    let retained_bytes = string_bytes
        .saturating_add(usize_u64(roots.len().saturating_add(1)).saturating_mul(4))
        .saturating_add(usize_u64(count).saturating_mul(80))
        .saturating_add(usize_u64(edge_columns.from.len()).saturating_mul(16))
        .saturating_add(usize_u64(adjacency.len()).saturating_mul(4))
        .saturating_add(usize_u64(component_members.len()).saturating_mul(4))
        .saturating_add(usize_u64(region_members.len()).saturating_mul(4))
        .saturating_add(usize_u64(residual_members.len()).saturating_mul(4));

    Ok(CompactGeometry {
        project,
        roots,
        dictionary,
        nodes: node_columns,
        edges: edge_columns,
        adjacency,
        component_members,
        components,
        region_members,
        regions: compact_regions,
        residual_members,
        residuals,
        closure,
        retained_bytes,
    })
}

fn append_span(target: &mut Vec<NodeId>, values: &[NodeId]) -> Span {
    let start = usize_u32(target.len());
    target.extend_from_slice(values);
    Span {
        start,
        len: usize_u32(values.len()),
    }
}

fn components(adjacency: &[Vec<NodeId>]) -> (Vec<ComponentId>, Vec<Vec<NodeId>>) {
    let mut component_of = vec![ComponentId(0); adjacency.len()];
    let mut seen = vec![false; adjacency.len()];
    let mut result = Vec::new();
    for start in 0..adjacency.len() {
        if seen[start] {
            continue;
        }
        let component = ComponentId(usize_u32(result.len()));
        let mut queue = VecDeque::from([NodeId(usize_u32(start))]);
        let mut members = Vec::new();
        while let Some(node) = queue.pop_front() {
            let index = node_index(node, adjacency.len());
            if seen[index] {
                continue;
            }
            seen[index] = true;
            component_of[index] = component;
            members.push(node);
            for &next in &adjacency[index] {
                if !seen[node_index(next, adjacency.len())] {
                    queue.push_back(next);
                }
            }
        }
        members.sort_unstable();
        result.push(members);
    }
    (component_of, result)
}

fn dependency_layers(
    blocked_by: &[Vec<NodeId>],
    blocks: &[Vec<NodeId>],
) -> (Vec<Option<u32>>, Vec<Vec<NodeId>>, Vec<bool>, Vec<bool>) {
    let count = blocks.len();
    let mut indegree: Vec<usize> = blocked_by.iter().map(Vec::len).collect();
    let mut layer: Vec<Option<u32>> = vec![None; count];
    let mut drained = vec![false; count];
    let mut ready: BTreeSet<NodeId> = indegree
        .iter()
        .enumerate()
        .filter(|(_, value)| **value == 0)
        .map(|(index, _)| NodeId(usize_u32(index)))
        .collect();
    while let Some(node) = ready.pop_first() {
        let index = node_index(node, count);
        let at = layer[index].unwrap_or(0);
        layer[index] = Some(at);
        drained[index] = true;
        for &dependent in &blocks[index] {
            let next = node_index(dependent, count);
            let deeper = at.saturating_add(1);
            layer[next] = Some(layer[next].unwrap_or(0).max(deeper));
            indegree[next] = indegree[next].saturating_sub(1);
            if indegree[next] == 0 {
                ready.insert(dependent);
            }
        }
    }
    for index in 0..count {
        if !drained[index] {
            layer[index] = None;
        }
    }
    let active: Vec<bool> = drained.iter().map(|value| !*value).collect();
    let sccs = strongly_connected(blocks, &active);
    let mut cyclic = vec![false; count];
    let mut loops = Vec::new();
    for component in sccs {
        let self_loop = component
            .first()
            .is_some_and(|node| blocks[node_index(*node, count)].contains(node));
        if component.len() > 1 || self_loop {
            for node in &component {
                cyclic[node_index(*node, count)] = true;
            }
            loops.push(component);
        }
    }
    let stalled = (0..count)
        .map(|index| active[index] && !cyclic[index])
        .collect();
    (layer, loops, cyclic, stalled)
}

fn strongly_connected(edges: &[Vec<NodeId>], active: &[bool]) -> Vec<Vec<NodeId>> {
    let count = edges.len();
    let mut reverse = vec![Vec::<NodeId>::new(); count];
    for (from, targets) in edges.iter().enumerate() {
        if !active[from] {
            continue;
        }
        for &to in targets {
            if active[node_index(to, count)] {
                reverse[node_index(to, count)].push(NodeId(usize_u32(from)));
            }
        }
    }
    let mut seen = vec![false; count];
    let mut order = Vec::new();
    for start in 0..count {
        if !active[start] || seen[start] {
            continue;
        }
        seen[start] = true;
        let mut stack = vec![(NodeId(usize_u32(start)), 0usize)];
        while let Some((node, offset)) = stack.last_mut() {
            let index = node_index(*node, count);
            let next = edges[index].get(*offset).copied();
            if let Some(next) = next {
                *offset = offset.saturating_add(1);
                let next_index = node_index(next, count);
                if active[next_index] && !seen[next_index] {
                    seen[next_index] = true;
                    stack.push((next, 0));
                }
            } else if let Some((finished, _)) = stack.pop() {
                order.push(finished);
            }
        }
    }
    seen.fill(false);
    let mut result = Vec::new();
    for start in order.into_iter().rev() {
        let start_index = node_index(start, count);
        if seen[start_index] {
            continue;
        }
        seen[start_index] = true;
        let mut stack = vec![start];
        let mut component = Vec::new();
        while let Some(node) = stack.pop() {
            component.push(node);
            for &next in reverse[node_index(node, count)].iter().rev() {
                let next_index = node_index(next, count);
                if !seen[next_index] {
                    seen[next_index] = true;
                    stack.push(next);
                }
            }
        }
        component.sort_unstable();
        result.push(component);
    }
    result
}

fn validate_parent_cycles(
    parent: &[Option<NodeId>],
    dictionary: &[String],
) -> Result<(), UnavailableReason> {
    for start in 0..parent.len() {
        let mut positions = BTreeMap::<NodeId, usize>::new();
        let mut path = Vec::new();
        let mut cursor = NodeId(usize_u32(start));
        loop {
            if let Some(offset) = positions.insert(cursor, path.len()) {
                let names = path[offset..]
                    .iter()
                    .map(|node| dictionary[node_index(*node, dictionary.len())].clone())
                    .collect();
                return Err(UnavailableReason::ContainmentCycle(names));
            }
            path.push(cursor);
            let Some(next) = parent[node_index(cursor, parent.len())] else {
                break;
            };
            cursor = next;
        }
    }
    Ok(())
}

fn regions(
    hierarchy_root: &[NodeId],
    blocks: &[Vec<NodeId>],
    count: usize,
) -> (Vec<Option<RegionId>>, Vec<Vec<NodeId>>, Vec<Option<u32>>) {
    let mut grouped: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for (index, root) in hierarchy_root.iter().enumerate() {
        grouped
            .entry(*root)
            .or_default()
            .push(NodeId(usize_u32(index)));
    }
    grouped.retain(|_, members| members.len() > 1);
    let mut sets: Vec<(NodeId, Vec<NodeId>)> = grouped.into_iter().collect();
    sets.sort_by_key(|(root, _)| *root);
    let mut region_of = vec![None; count];
    for (index, (_, members)) in sets.iter().enumerate() {
        let region = RegionId(usize_u32(index));
        for member in members {
            region_of[node_index(*member, count)] = Some(region);
        }
    }
    let mut onward = vec![BTreeSet::<RegionId>::new(); sets.len()];
    let mut indegree = vec![0usize; sets.len()];
    for (from, targets) in blocks.iter().enumerate() {
        let Some(source) = region_of[from] else {
            continue;
        };
        for target in targets {
            let Some(destination) = region_of[node_index(*target, count)] else {
                continue;
            };
            if source != destination
                && onward[node_index_region(source, sets.len())].insert(destination)
            {
                indegree[node_index_region(destination, sets.len())] =
                    indegree[node_index_region(destination, sets.len())].saturating_add(1);
            }
        }
    }
    let mut layers: Vec<Option<u32>> = vec![None; sets.len()];
    let mut ready: BTreeSet<RegionId> = indegree
        .iter()
        .enumerate()
        .filter(|(_, value)| **value == 0)
        .map(|(index, _)| RegionId(usize_u32(index)))
        .collect();
    while let Some(region) = ready.pop_first() {
        let index = node_index_region(region, sets.len());
        let at = layers[index].unwrap_or(0);
        layers[index] = Some(at);
        for &next in &onward[index] {
            let next_index = node_index_region(next, sets.len());
            layers[next_index] = Some(layers[next_index].unwrap_or(0).max(at.saturating_add(1)));
            indegree[next_index] = indegree[next_index].saturating_sub(1);
            if indegree[next_index] == 0 {
                ready.insert(next);
            }
        }
    }
    (
        region_of,
        sets.into_iter().map(|(_, members)| members).collect(),
        layers,
    )
}

fn implied_by_longer_path(
    from: NodeId,
    to: NodeId,
    blocks: &[Vec<NodeId>],
    layer: &[Option<u32>],
) -> bool {
    let count = blocks.len();
    let Some(arrival) = layer[node_index(to, count)] else {
        return false;
    };
    let depart = layer[node_index(from, count)].unwrap_or(0);
    if arrival <= depart.saturating_add(1) {
        return false;
    }
    let mut seen = vec![false; count];
    let mut stack = Vec::new();
    for &next in &blocks[node_index(from, count)] {
        if next != to {
            seen[node_index(next, count)] = true;
            stack.push(next);
        }
    }
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        let index = node_index(node, count);
        if layer[index].is_none_or(|at| at >= arrival) {
            continue;
        }
        for &next in &blocks[index] {
            let next_index = node_index(next, count);
            if !seen[next_index] {
                seen[next_index] = true;
                stack.push(next);
            }
        }
    }
    false
}

fn node_index(node: NodeId, total: usize) -> usize {
    usize::try_from(node.0)
        .unwrap_or(usize::MAX)
        .min(total.saturating_sub(1))
}

fn node_index_region(region: RegionId, total: usize) -> usize {
    usize::try_from(region.0)
        .unwrap_or(usize::MAX)
        .min(total.saturating_sub(1))
}

fn usize_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn usize_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn push_len(out: &mut Vec<u8>, len: usize) {
    out.extend_from_slice(&usize_u64(len).to_be_bytes());
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    push_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use runtime::publication::{ExtractorSchemaDigest, MaterializationId, PublicationId};

    fn source(root: u8, materialization: u64) -> WorldPublicationId {
        WorldPublicationId::new(
            PublicationId::new(
                [root; 32],
                [2; 32],
                ExtractorSchemaDigest::from_digest([3; 32]),
            ),
            MaterializationId::from_u64(materialization).expect("materialization"),
        )
    }

    fn issue(id: &str) -> GeometryIssueFact {
        GeometryIssueFact {
            id: id.into(),
            project: "project".into(),
            closed: false,
            due: None,
        }
    }

    fn relation(from: &str, relation: &str, to: &str) -> GeometryRelationFact {
        GeometryRelationFact {
            from: from.into(),
            relation: relation.into(),
            to: to.into(),
        }
    }

    fn facts(source: WorldPublicationId, relations: Vec<GeometryRelationFact>) -> GeometryFacts {
        GeometryFacts {
            source,
            issues: ["a", "b", "c", "d"].into_iter().map(issue).collect(),
            relations,
        }
    }

    fn request(source: WorldPublicationId) -> GeometryRequest {
        GeometryRequest::new(source, "project", Vec::new(), GeometryBudget::default())
    }

    fn all_nodes(artifact: &GeometryArtifact) -> Vec<GeometryNodePageRow> {
        match artifact
            .page(
                &artifact.key(),
                GeometryPageRequest::first(GeometrySection::Nodes, MAX_GEOMETRY_PAGE),
            )
            .expect("node page")
            .rows
        {
            GeometryRows::Nodes(rows) => rows,
            _ => panic!("nodes"),
        }
    }

    fn wait_artifact(
        registry: &GeometryRegistry,
        request: &GeometryRequest,
    ) -> Arc<GeometryArtifact> {
        for _ in 0..200 {
            if let Some(artifact) = registry.get(&request.key()) {
                return artifact;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("Geometry worker did not publish its exact artifact")
    }

    #[test]
    fn compact_dag_pages_global_layers_regions_and_reduction() {
        let source = source(1, 1);
        let artifact = materialize(
            &request(source),
            &facts(
                source,
                vec![
                    relation("a", "blocks", "b"),
                    relation("b", "blocks", "c"),
                    relation("a", "blocks", "c"),
                    relation("a", "contains", "d"),
                ],
            ),
        );
        let summary = artifact.summary(&artifact.key()).expect("summary");
        assert_eq!(summary.nodes, 4);
        assert_eq!(summary.regions, 1);
        assert!(summary.retained_bytes < 4 * 1_024);
        assert_eq!(
            all_nodes(&artifact)
                .into_iter()
                .find(|node| node.id == "c")
                .and_then(|node| node.layer),
            Some(2)
        );
        let page = artifact
            .page(
                &artifact.key(),
                GeometryPageRequest::first(GeometrySection::Edges, 10),
            )
            .unwrap();
        let GeometryRows::Edges(edges) = page.rows else {
            panic!("edges")
        };
        assert!(edges
            .iter()
            .any(|edge| edge.from_id == "a" && edge.to_id == "c" && edge.implied));
    }

    #[test]
    fn source_and_artifact_coordinates_are_never_silently_current() {
        let requested = source(1, 1);
        let actual = source(2, 2);
        let artifact = materialize(&request(requested), &facts(actual, Vec::new()));
        assert!(matches!(
            artifact.summary(&artifact.key()),
            Err(AccessFailure::Unavailable(
                UnavailableReason::SourceMismatch { .. }
            ))
        ));
        let ready = materialize(&request(actual), &facts(actual, Vec::new()));
        assert!(matches!(
            ready.summary(&request(requested).key()),
            Err(AccessFailure::Expired { .. })
        ));
    }

    #[test]
    fn missing_root_uses_dictionary_identity_without_fabricating_a_node() {
        let source = source(1, 1);
        let request = GeometryRequest::new(
            source,
            "project",
            vec!["missing".into()],
            GeometryBudget::default(),
        );
        let artifact = materialize(&request, &facts(source, Vec::new()));
        let summary = artifact.summary(&artifact.key()).unwrap();
        assert_eq!(summary.nodes, 0);
        assert_eq!(summary.roots, 1);
        let roots = artifact
            .page(
                &artifact.key(),
                GeometryPageRequest::first(GeometrySection::Roots, 10),
            )
            .unwrap();
        let GeometryRows::Roots(roots) = roots.rows else {
            panic!("roots")
        };
        assert_eq!(roots[0].id, "missing");
        let page = artifact
            .page(
                &artifact.key(),
                GeometryPageRequest::first(GeometrySection::Residuals, 10),
            )
            .unwrap();
        let GeometryRows::Residuals(rows) = page.rows else {
            panic!("residuals")
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, ResidualKind::RootMissing);
        assert_eq!(rows[0].missing.as_deref(), Some("missing"));
        assert_eq!(rows[0].at, 0);
    }

    #[test]
    fn pathological_scc_recomputes_layers_and_pages_members_boundedly() {
        let source = source(2, 2);
        let artifact = materialize(
            &request(source),
            &facts(
                source,
                vec![
                    relation("a", "blocks", "b"),
                    relation("b", "blocks", "c"),
                    relation("c", "blocks", "d"),
                    relation("d", "blocks", "b"),
                ],
            ),
        );
        let summary = artifact.summary(&artifact.key()).unwrap();
        assert_eq!(summary.closure.cyclic, 3);
        assert_eq!(
            all_nodes(&artifact)
                .iter()
                .find(|node| node.id == "a")
                .and_then(|node| node.layer),
            Some(0)
        );
        assert!(all_nodes(&artifact)
            .iter()
            .filter(|node| node.id != "a")
            .all(|node| node.layer.is_none()));
        let residuals = artifact
            .page(
                &artifact.key(),
                GeometryPageRequest::first(GeometrySection::Residuals, 100),
            )
            .unwrap();
        let GeometryRows::Residuals(residuals) = residuals.rows else {
            panic!("residuals")
        };
        let cycle = residuals
            .iter()
            .find(|residual| residual.kind == ResidualKind::DependencyCycle)
            .unwrap();
        assert_eq!(cycle.at, 3);
        let members = artifact
            .page(
                &artifact.key(),
                GeometryPageRequest::first(GeometrySection::ResidualAt(cycle.residual), 2),
            )
            .unwrap();
        assert!(members.next.is_some());
    }

    #[test]
    fn page_cursor_is_bound_to_artifact_section_and_limit() {
        let source = source(1, 1);
        let artifact = materialize(&request(source), &facts(source, Vec::new()));
        let first = artifact
            .page(
                &artifact.key(),
                GeometryPageRequest::first(GeometrySection::Nodes, 2),
            )
            .unwrap();
        let cursor = first.next.unwrap();
        let second = artifact
            .page(
                &artifact.key(),
                GeometryPageRequest {
                    section: GeometrySection::Nodes,
                    limit: 2,
                    cursor: Some(cursor.clone()),
                },
            )
            .unwrap();
        assert!(second.next.is_none());
        assert!(matches!(
            artifact.page(
                &artifact.key(),
                GeometryPageRequest {
                    section: GeometrySection::Edges,
                    limit: 2,
                    cursor: Some(cursor),
                }
            ),
            Err(AccessFailure::InvalidPage("cursor"))
        ));
    }

    #[test]
    fn estimate_refuses_before_global_materialization() {
        let source = source(1, 1);
        let graph = facts(
            source,
            vec![
                relation("a", "blocks", "b"),
                relation("b", "blocks", "c"),
                relation("a", "blocks", "c"),
            ],
        );
        let ordinary = request(source);
        let estimate = estimate(&ordinary, &graph).unwrap();
        assert_eq!(estimate.selected_nodes, 4);
        assert_eq!(estimate.selected_edges, 3);
        assert_eq!(estimate.reduction_candidates, 2);
        let tight = GeometryRequest::new(
            source,
            "project",
            Vec::new(),
            GeometryBudget {
                working_bytes: estimate.working_bytes.saturating_sub(1),
                ..GeometryBudget::default()
            },
        );
        let artifact = materialize(&tight, &graph);
        assert!(matches!(
            artifact.summary(&artifact.key()),
            Err(AccessFailure::Unavailable(
                UnavailableReason::BudgetExceeded { .. }
            ))
        ));
    }

    #[test]
    fn registry_reuses_exact_artifact_without_rebuilding_facts() {
        let source = source(1, 1);
        let request = request(source);
        let registry = GeometryRegistry::default();
        let hint = GeometryEstimate::conservative(&request, 4, 0);
        let pending =
            registry.materialize_cached(&request, hint, move || facts(source, Vec::new()));
        assert!(matches!(pending.readiness(), GeometryReadiness::Pending));
        let first = wait_artifact(&registry, &request);
        let second = registry.materialize_cached(&request, hint, || {
            panic!("a cache hit must not rebuild complete Geometry facts")
        });
        assert!(Arc::ptr_eq(&first, &second));
        assert!(Arc::ptr_eq(
            &second,
            &registry
                .get(&request.key())
                .expect("retained exact artifact")
        ));
    }

    #[test]
    fn registry_single_flights_concurrent_exact_misses() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Barrier;

        let source = source(2, 7);
        let request = request(source);
        let graph = facts(source, vec![relation("a", "blocks", "b")]);
        let registry = Arc::new(GeometryRegistry::default());
        let starts = Arc::new(Barrier::new(8));
        let builds = Arc::new(AtomicUsize::new(0));
        let handles = (0..8)
            .map(|_| {
                let registry = registry.clone();
                let request = request.clone();
                let graph = graph.clone();
                let starts = starts.clone();
                let builds = builds.clone();
                std::thread::spawn(move || {
                    starts.wait();
                    let hint = GeometryEstimate::conservative(&request, 4, 1);
                    registry.materialize_cached(&request, hint, move || {
                        builds.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        graph
                    })
                })
            })
            .collect::<Vec<_>>();
        let pending = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert!(pending
            .iter()
            .all(|artifact| matches!(artifact.readiness(), GeometryReadiness::Pending)));
        let ready = wait_artifact(&registry, &request);
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(matches!(ready.readiness(), GeometryReadiness::Ready));
    }

    #[test]
    fn registry_is_exact_keyed_bounded_and_budget_admitted_per_request() {
        let registry = GeometryRegistry::new(GeometryCacheLimits {
            entries: 2,
            retained_bytes: u64::MAX,
            workers: 1,
            queued_builds: 2,
        });
        for root in 1..=3 {
            let source = source(root, u64::from(root));
            let request = request(source);
            let hint = GeometryEstimate::conservative(&request, 4, 0);
            registry.materialize_cached(&request, hint, move || facts(source, Vec::new()));
            wait_artifact(&registry, &request);
        }
        assert_eq!(registry.retained().0, 2);
        assert!(registry.get(&request(source(1, 1)).key()).is_none());
        assert!(registry.get(&request(source(3, 3)).key()).is_some());

        let source = source(3, 3);
        let ordinary = request(source);
        let ready = registry
            .get(&ordinary.key())
            .expect("latest exact artifact retained");
        let tight = GeometryRequest::new(
            source,
            "project",
            Vec::new(),
            GeometryBudget {
                working_bytes: ready.estimate().working_bytes.saturating_sub(1),
                ..GeometryBudget::default()
            },
        );
        let refused = registry.materialize_cached(&tight, ready.estimate(), || {
            panic!("budget admission over a retained artifact needs no facts")
        });
        assert!(matches!(
            refused.readiness(),
            GeometryReadiness::Unavailable {
                reason: UnavailableReason::BudgetExceeded { .. }
            }
        ));
        assert!(matches!(ready.readiness(), GeometryReadiness::Ready));
    }

    #[test]
    fn registry_refuses_typed_when_bounded_executor_is_saturated() {
        let registry = GeometryRegistry::new(GeometryCacheLimits {
            entries: 4,
            retained_bytes: u64::MAX,
            workers: 1,
            queued_builds: 1,
        });
        let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let first_source = source(1, 1);
        let first = request(first_source);
        let first_release = release.clone();
        let pending = registry.materialize_cached(
            &first,
            GeometryEstimate::conservative(&first, 4, 0),
            move || {
                let _ = started_tx.send(());
                let (released, wake) = &*first_release;
                let mut released = released.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
                facts(first_source, Vec::new())
            },
        );
        assert!(matches!(pending.readiness(), GeometryReadiness::Pending));
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("first worker started");

        let second_source = source(2, 2);
        let second = request(second_source);
        let queued = registry.materialize_cached(
            &second,
            GeometryEstimate::conservative(&second, 4, 0),
            move || facts(second_source, Vec::new()),
        );
        assert!(matches!(queued.readiness(), GeometryReadiness::Pending));

        let third_source = source(3, 3);
        let third = request(third_source);
        let refused = registry.materialize_cached(
            &third,
            GeometryEstimate::conservative(&third, 4, 0),
            move || facts(third_source, Vec::new()),
        );
        assert!(matches!(
            refused.readiness(),
            GeometryReadiness::Unavailable {
                reason: UnavailableReason::ExecutorSaturated { .. }
            }
        ));

        let (released, wake) = &*release;
        *released.lock().unwrap() = true;
        wake.notify_all();
        wait_artifact(&registry, &first);
        wait_artifact(&registry, &second);
    }

    #[test]
    fn unretainable_artifact_becomes_small_terminal_result_without_rebuild_loop() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let registry = GeometryRegistry::new(GeometryCacheLimits {
            entries: 0,
            retained_bytes: 0,
            workers: 1,
            queued_builds: 1,
        });
        let source = source(9, 9);
        let request = request(source);
        let builds = Arc::new(AtomicUsize::new(0));
        let worker_builds = builds.clone();
        let pending = registry.materialize_cached(
            &request,
            GeometryEstimate::conservative(&request, 4, 0),
            move || {
                worker_builds.fetch_add(1, Ordering::SeqCst);
                facts(source, Vec::new())
            },
        );
        assert!(matches!(pending.readiness(), GeometryReadiness::Pending));
        let terminal = wait_artifact(&registry, &request);
        assert!(matches!(
            terminal.readiness(),
            GeometryReadiness::Unavailable {
                reason: UnavailableReason::RetentionExceeded { .. }
            }
        ));
        let second = registry.materialize_cached(&request, GeometryEstimate::default(), || {
            panic!("terminal exact result must suppress another build")
        });
        assert!(Arc::ptr_eq(&terminal, &second));
        assert_eq!(builds.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pending_is_typed_and_pinned() {
        let request = request(source(1, 7));
        let pending = GeometryArtifact::pending(&request, GeometryEstimate::default());
        assert!(matches!(
            pending.summary(&pending.key()),
            Err(AccessFailure::NotReady { .. })
        ));
    }
}
