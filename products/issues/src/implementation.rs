#![allow(
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "Issues validates command, schema, and projection shapes before fixed contract operations and canonical serialization"
)]
//! The issue product's semantic Runtime World implementation.
//!
//! `IssuesWorld` implements the public `runtime::world::World` contract over the
//! frozen mapping in `contract.rs`: current Issues behavior expressed as
//! collaborative Body operations. It is deliberately **not** a reusable
//! privileged Runtime path: it registers through the same `Builder` any
//! consumer uses and touches nothing below the World boundary. The World is
//! pure: ids, timestamps, and resolved refs
//! arrive inside the intent; validation is re-checked here (the daemon
//! pre-validates for friendly errors), and every accepted intent stages one
//! atomic multi-Body transaction (issue + catalog together — the legacy split
//! `persist_issue_and_row` failure mode does not exist here).

use runtime::poison::LockRecovering;
use std::collections::BTreeMap;
use std::sync::Arc;

use replica::body::BodyKey;
use replica::body::{CollaborativeSchema, MutationModel, Op, Schema};
use runtime::{
    world::BodyDeclaration, world::Context, world::Effect, world::Intent, world::Projection,
    world::Query, world::Rejection, world::World,
};

use crate::dto::{ActivityEvent, CatalogScope, FieldChange, Priority, StatusCategory};
use crate::ids::{ActorId, DocId};

use super::contract::{
    self, baseline_key, board_path, catalog_key, issue_key, spec_key, EventChange, IssueEffect,
    IssueEvent, IssueIntent, IssueQuery, NewLabel, Pos, StoredComment, WorkAction, DEFAULT_STATUS,
    DOCUMENT_SCHEMA_VERSION, LINK_KINDS, VIEW_SCHEMA_VERSION,
};
use super::rank;
use super::views::{
    board_view, canonical_for, derive_aliases, issue_view, label_dto, project_dto, project_row,
    CatalogState, DerivedAliases, IssueState, Milestone, RelationState,
};

/// The order milestones read in, and the only place that decides it.
///
/// Rank first, so a project that has been ordered by hand stays that way. An
/// unranked record — one written before ordering existed, in a project nobody has
/// touched since — falls back to the target date it used to sort by, so the list
/// looks the same as it did until someone deliberately moves something.
///
/// The id breaks every remaining tie. Two replicas can independently place a
/// milestone at the same rank; agreeing on *an* order matters more than agreeing
/// on whose move won, and the id is the one key both sides always have.
fn milestone_order(a: &Milestone, b: &Milestone) -> std::cmp::Ordering {
    if !a.rank.is_empty() || !b.rank.is_empty() {
        // An unranked record sorts last rather than first: `""` is below every
        // rank, and a legacy milestone jumping to the head of a hand-ordered list
        // is the one outcome the backfill exists to prevent.
        let key = |m: &Milestone| (m.rank.is_empty(), m.rank.clone());
        return key(a).cmp(&key(b)).then_with(|| a.id.cmp(&b.id));
    }
    let date = |d: Option<u64>| d.unwrap_or(u64::MAX);
    date(a.target_date)
        .cmp(&date(b.target_date))
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.id.cmp(&b.id))
}

/// The rank that puts `id` where `pos` says, within `ordered` (which is sorted
/// and excludes tombstones). `None` when `pos` names a milestone that is not in
/// this project — a placement relative to nothing is a mistake, not a default.
fn place(ordered: &[Milestone], id: &str, pos: &Pos) -> Option<String> {
    // The milestone being moved is not its own neighbour. Leaving it in would
    // make "after the one directly above me" resolve to a gap I already occupy.
    let others: Vec<&Milestone> = ordered.iter().filter(|m| m.id != id).collect();
    let rank_at = |i: usize| others.get(i).map(|m| m.rank.as_str());
    let (lo, hi) = match pos {
        Pos::Top => ("", rank_at(0)),
        Pos::Bottom => (others.last().map(|m| m.rank.as_str()).unwrap_or(""), None),
        Pos::Before { doc } | Pos::After { doc } => {
            let at = others.iter().position(|m| m.id == *doc)?;
            match pos {
                Pos::Before { .. } => (
                    if at == 0 {
                        ""
                    } else {
                        others[at - 1].rank.as_str()
                    },
                    Some(others[at].rank.as_str()),
                ),
                _ => (others[at].rank.as_str(), rank_at(at + 1)),
            }
        }
    };
    Some(rank::between(lo, hi))
}

/// The registered product World.
pub struct IssuesWorld {
    id: replica::body::WorldId,
    schemas: Vec<Schema>,
    /// Owned rather than built on demand, because the trait hands back a slice
    /// and the registry compares it against the registration byte for byte —
    /// two constructions of "the same" list is how they come to differ.
    signal_schemas: Vec<runtime::world::SignalSchema>,
    /// The derived read-model cache, keyed by the EXACT Manifest root each
    /// query is pinned to — registered in `tests/mixed_root_guard.rs` with its
    /// mixed-root rejection proof. A hit is only ever the same root, so output
    /// mixing two roots is unrepresentable; per-issue entries are additionally
    /// reused across roots ONLY under a reader-issued version stamp
    /// ([`runtime::world::BodyReader::body_stamp`]) that guarantees
    /// byte-equivalence.
    cache: std::sync::Mutex<RootKeyedCache>,
}

/// See [`IssuesWorld::cache`].
#[derive(Default)]
struct RootKeyedCache {
    /// `(manifest root, derived snapshot)` — a bounded, most-recent-last list.
    roots: Vec<([u8; 32], Arc<DerivedSnapshot>)>,
    /// Per-issue parsed state: `doc -> (stamp, state)`.
    issues: std::collections::HashMap<String, (Vec<u8>, Arc<IssueState>)>,
    /// Project topology parses, reused across generations under Body stamps.
    relations: std::collections::HashMap<String, (Vec<u8>, Arc<RelationState>)>,
    /// Fully compiled Plan morphologies. Compilation is linear, but a warm
    /// reader should pay only serialization; the exact generation is part of
    /// the key, so historical and current phenotypes cannot contaminate one
    /// another.
    geometries: Vec<GeometryCacheEntry>,
}

struct GeometryCacheEntry {
    root: [u8; 32],
    project: String,
    roots: Vec<String>,
    view: Arc<crate::geometry::GeometryView>,
}

/// The immutable read model every query arm consumes: the integrity-checked
/// catalog, its derived aliases, and every issue's parsed state — all from ONE
/// committed snapshot (one Manifest root).
struct DerivedSnapshot {
    catalog: Arc<CatalogState>,
    aliases: Arc<DerivedAliases>,
    issues: BTreeMap<String, Arc<IssueState>>,
}

/// How many recent roots stay warm: the current root plus the previous one
/// (a doorbell-raced query may still be pinned to the prior root).
const CACHED_ROOTS: usize = 2;
const CACHED_GEOMETRIES: usize = 16;

impl IssuesWorld {
    /// The derived read model for THIS context's Manifest root: served from
    /// the cache when the root is warm, else built from the committed snapshot
    /// (reusing per-issue parses whose reader stamp is unchanged) and cached
    /// under the root. A zero root (fixture contexts without a snapshot
    /// identity) is never cached.
    fn derived_snapshot(&self, ctx: &Context<'_>) -> Result<Arc<DerivedSnapshot>, Rejection> {
        let root = ctx.manifest_root();
        let identified = root != [0u8; 32];
        if identified {
            let cache = self.cache.lock_recovering();
            if let Some((_, snap)) = cache.roots.iter().find(|(r, _)| r == &root) {
                return Ok(snap.clone());
            }
        }
        let mut catalog = catalog_state(ctx)?;
        let mut cache = self.cache.lock_recovering();
        let relation_keys =
            ctx.bodies_with_schema(&contract::world_id(), &contract::relation_schema());
        let mut live_relations = std::collections::HashSet::new();
        for key in relation_keys {
            let rendered = key.body.render();
            live_relations.insert(rendered.clone());
            let stamp = ctx.body_stamp(&key);
            let state = match (&stamp, cache.relations.get(&rendered)) {
                (Some(stamp), Some((cached_stamp, state))) if stamp == cached_stamp => {
                    state.clone()
                }
                _ => {
                    let Ok(view) = ctx.read_collaborative(&key) else {
                        continue;
                    };
                    Arc::new(RelationState::from_view(&view))
                }
            };
            if let Some(stamp) = stamp {
                cache.relations.insert(rendered, (stamp, state.clone()));
            }
            state.apply_to(&mut catalog);
        }
        cache
            .relations
            .retain(|body, _| live_relations.contains(body));
        let catalog = Arc::new(catalog);
        let mut issues: BTreeMap<String, Arc<IssueState>> = BTreeMap::new();
        for doc in catalog.doc_ids() {
            let stamp = ctx.body_stamp(&issue_key(&doc));
            let state = match (&stamp, cache.issues.get(&doc)) {
                (Some(stamp), Some((cached_stamp, state))) if stamp == cached_stamp => {
                    state.clone()
                }
                _ => match issue_state(ctx, &doc) {
                    Some(state) => Arc::new(state),
                    None => continue,
                },
            };
            if let Some(stamp) = stamp {
                cache.issues.insert(doc.clone(), (stamp, state.clone()));
            }
            issues.insert(doc, state);
        }
        let aliases = Arc::new(derive_aliases(&catalog, |doc| {
            issues.get(doc).map(|issue| issue.project.as_str())
        }));
        // Registered docs are the live set: drop parses for departed docs.
        cache.issues.retain(|doc, _| issues.contains_key(doc));
        let snap = Arc::new(DerivedSnapshot {
            catalog,
            aliases,
            issues,
        });
        if identified {
            cache.roots.retain(|(r, _)| r != &root);
            cache.roots.push((root, snap.clone()));
            if cache.roots.len() > CACHED_ROOTS {
                let drop_count = cache.roots.len() - CACHED_ROOTS;
                cache.roots.drain(..drop_count);
            }
        }
        Ok(snap)
    }

    fn geometry_view(
        &self,
        ctx: &Context<'_>,
        snap: &DerivedSnapshot,
        project: &str,
        roots: &[String],
    ) -> Arc<crate::geometry::GeometryView> {
        let root = ctx.manifest_root();
        let identified = root != [0u8; 32];
        if identified {
            let cache = self.cache.lock_recovering();
            if let Some(entry) = cache.geometries.iter().rev().find(|entry| {
                entry.root == root && entry.project == project && entry.roots == roots
            }) {
                return entry.view.clone();
            }
        }
        let generation = data_encoding::HEXLOWER.encode(&root);
        let view = Arc::new(crate::geometry::compile(
            &snap.catalog,
            &snap.aliases,
            &snap.issues,
            project,
            roots,
            generation,
        ));
        if identified {
            let mut cache = self.cache.lock_recovering();
            cache.geometries.retain(|entry| {
                !(entry.root == root && entry.project == project && entry.roots == roots)
            });
            cache.geometries.push(GeometryCacheEntry {
                root,
                project: project.into(),
                roots: roots.to_vec(),
                view: view.clone(),
            });
            if cache.geometries.len() > CACHED_GEOMETRIES {
                let drop_count = cache.geometries.len() - CACHED_GEOMETRIES;
                cache.geometries.drain(..drop_count);
            }
        }
        view
    }
}

impl Default for IssuesWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl IssuesWorld {
    pub fn new() -> Self {
        Self {
            id: contract::world_id(),
            cache: std::sync::Mutex::new(RootKeyedCache::default()),
            signal_schemas: contract::signal_schemas(),
            schemas: vec![
                Schema {
                    id: contract::issue_schema(),
                    version: contract::ISSUE_SCHEMA_VERSION,
                    encoding: contract::issue_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    // Both predecessors are read by this version: v1 keeps its
                    // comments in `list:comments` and its history in
                    // `list:events`, v2 moved the comments, and the readers
                    // take every home. Writing always uses this version's shape.
                    readable_predecessors: vec![1, 2],
                },
                // Current intents may still update Bodies created under either
                // readable predecessor. Runtime contains every operation
                // against the Body's immutable *exact* binding, so readable
                // predecessors also have to be registered here; otherwise the
                // compatibility reader can open an old issue but no migration
                // or ordinary edit can ever advance it.
                Schema {
                    id: contract::issue_schema(),
                    version: 2,
                    encoding: contract::issue_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    readable_predecessors: vec![1],
                },
                Schema {
                    id: contract::issue_schema(),
                    version: 1,
                    encoding: contract::issue_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    readable_predecessors: vec![],
                },
                Schema {
                    id: contract::spec_schema(),
                    version: contract::SPEC_SCHEMA_VERSION,
                    encoding: contract::spec_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    readable_predecessors: vec![],
                },
                Schema {
                    id: contract::baseline_schema(),
                    version: contract::BASELINE_SCHEMA_VERSION,
                    encoding: contract::baseline_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    readable_predecessors: vec![],
                },
                Schema {
                    id: contract::relation_schema(),
                    version: contract::RELATION_SCHEMA_VERSION,
                    encoding: contract::relation_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    readable_predecessors: vec![],
                },
                Schema {
                    id: contract::catalog_schema(),
                    version: contract::CATALOG_SCHEMA_VERSION,
                    encoding: contract::catalog_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    // v1 Catalogs are read: their `map:parents` entries still
                    // supply parentage for issues the tree says nothing about.
                    readable_predecessors: vec![1],
                },
                Schema {
                    id: contract::catalog_schema(),
                    version: 1,
                    encoding: contract::catalog_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    readable_predecessors: vec![],
                },
            ],
        }
    }

    /// The reviewed implementation descriptor this build ships. Its canonical
    /// id is the authority identity the founder activates and every product
    /// transaction pins.
    pub fn implementation_descriptor() -> runtime::world::Implementation {
        let world = Self::new();
        runtime::world::Implementation::from_registration(
            &world.descriptor(),
            2,
            *blake3::hash(b"lait.issues.policy-table.v2").as_bytes(),
            *blake3::hash(b"lait.issues.spec-lifecycle.v2").as_bytes(),
        )
    }
}

/// A staged transaction under construction.
struct Staging {
    /// The Space the transaction commits in — the deterministic Catalog's
    /// identity input.
    space: mechanics::ids::SpaceId,
    ops: Vec<(BodyKey, Op)>,
    bodies: Vec<BodyKey>,
    declarations: Vec<BodyDeclaration>,
    /// The complete content set for each Body this transaction declares one for.
    ///
    /// Sparse on purpose. `content_refs` on an effect *replaces* what a Body
    /// declared, so an entry for a Body that did not mean to say anything would
    /// erase its set — which is what would happen on the next comment if every
    /// staged Body got an entry. Only a key that explicitly declares appears
    /// here.
    declared: std::collections::BTreeMap<BodyKey, Vec<replica::content::ContentRef>>,
    /// Whether a catalog op must carry the creation declaration — true exactly
    /// when the committed snapshot holds no Catalog yet (first-ever write).
    declare_catalog_on_use: bool,
    /// The canonical demand this mutation requires (defaults to contributor).
    demand: Option<Vec<u8>>,
}

impl Staging {
    fn for_space(space: mechanics::ids::SpaceId, declare_catalog_on_use: bool) -> Self {
        Self {
            space,
            ops: Vec::new(),
            bodies: Vec::new(),
            declarations: Vec::new(),
            declared: std::collections::BTreeMap::new(),
            declare_catalog_on_use,
            demand: None,
        }
    }
}

impl Staging {
    /// Declarations ride ONLY the transaction that may create a Body.
    ///
    /// A Body's `(schema, version)` binding is immutable once recorded, and a
    /// later declaration must equal it exactly — so declaring the compiled-in
    /// version on every write would turn the first schema-version bump into a
    /// `ContractViolation` against every pre-existing Body. An existing Body
    /// resolves its own binding without any declaration; only creation needs
    /// one, so only creation carries one.
    fn declare_issue(&mut self, key: &BodyKey) {
        if !self.declarations.iter().any(|d| &d.key == key) {
            self.declarations.push(BodyDeclaration {
                key: key.clone(),
                schema: contract::issue_schema(),
                schema_version: contract::ISSUE_SCHEMA_VERSION,
            });
        }
    }

    /// See [`Self::declare_issue`] — attached exactly when this transaction
    /// may bring the Catalog into being (`declare_catalog_on_use`). Joiners
    /// adopt the Catalog through Manifest synchronization and never
    /// re-declare it.
    fn declare_catalog(&mut self) {
        let key = catalog_key(&self.space);
        if !self.declarations.iter().any(|d| d.key == key) {
            self.declarations.push(BodyDeclaration {
                key: key.clone(),
                schema: contract::catalog_schema(),
                schema_version: contract::CATALOG_SCHEMA_VERSION,
            });
        }
    }

    fn declare_spec(&mut self, key: &BodyKey) {
        if !self
            .declarations
            .iter()
            .any(|declaration| &declaration.key == key)
        {
            self.declarations.push(BodyDeclaration {
                key: key.clone(),
                schema: contract::spec_schema(),
                schema_version: contract::SPEC_SCHEMA_VERSION,
            });
        }
    }

    fn declare_baseline(&mut self, key: &BodyKey) {
        if !self
            .declarations
            .iter()
            .any(|declaration| &declaration.key == key)
        {
            self.declarations.push(BodyDeclaration {
                key: key.clone(),
                schema: contract::baseline_schema(),
                schema_version: contract::BASELINE_SCHEMA_VERSION,
            });
        }
    }

    fn declare_relation(&mut self, key: &BodyKey) {
        if !self
            .declarations
            .iter()
            .any(|declaration| &declaration.key == key)
        {
            self.declarations.push(BodyDeclaration {
                key: key.clone(),
                schema: contract::relation_schema(),
                schema_version: contract::RELATION_SCHEMA_VERSION,
            });
        }
    }

    fn issue(&mut self, key: &BodyKey, op: Op) {
        if matches!(op, Op::Create) {
            self.declare_issue(key);
        }
        if !self.bodies.contains(key) {
            self.bodies.push(key.clone());
        }
        self.ops.push((key.clone(), op));
    }

    fn catalog(&mut self, op: Op) {
        if self.declare_catalog_on_use {
            self.declare_catalog();
        }
        let key = catalog_key(&self.space);
        if !self.bodies.contains(&key) {
            self.bodies.push(key.clone());
        }
        self.ops.push((key, op));
    }

    fn spec(&mut self, key: &BodyKey, op: Op) {
        if matches!(op, Op::Create) {
            self.declare_spec(key);
        }
        if !self.bodies.contains(key) {
            self.bodies.push(key.clone());
        }
        self.ops.push((key.clone(), op));
    }

    fn baseline(&mut self, key: &BodyKey, op: Op) {
        if matches!(op, Op::Create) {
            self.declare_baseline(key);
        }
        if !self.bodies.contains(key) {
            self.bodies.push(key.clone());
        }
        self.ops.push((key.clone(), op));
    }

    fn relation(&mut self, project: &str, create: bool, op: Op) {
        let key = contract::relation_key(project);
        if create && !self.bodies.contains(&key) {
            self.declare_relation(&key);
            self.bodies.push(key.clone());
            self.ops.push((key.clone(), Op::Create));
        } else if !self.bodies.contains(&key) {
            self.bodies.push(key.clone());
        }
        self.ops.push((key, op));
    }

    /// Set the demand this mutation requires (an admin-only intent overrides
    /// the contributor default).
    fn require(&mut self, demand: Vec<u8>) {
        self.demand = Some(demand);
    }

    /// Declare the complete content set for one Body.
    ///
    /// Complete, not additive: `content_refs` on an effect replaces whatever
    /// the Body declared before, so an entry naming one file detaches the rest.
    /// Only a key that calls this appears in the effect at all — a blanket
    /// declaration would erase the set on the next comment, which is exactly
    /// the failure this shape exists to make impossible.
    fn declare(&mut self, key: &BodyKey, refs: Vec<replica::content::ContentRef>) {
        self.declared.insert(key.clone(), refs);
    }

    fn into_effect(self, doc: Option<String>) -> Effect {
        let demand = self.demand.unwrap_or_else(contract::demand_contributor);
        Effect {
            content_refs: self.declared.into_iter().collect(),
            operations: self.ops,
            bodies: self.bodies,
            effect: IssueEffect {
                doc,
                unchanged: false,
            }
            .to_json(),
            declarations: self.declarations,
            demand,
        }
    }
}

/// A content id as a World writes it: 32 bytes of lowercase hex.
fn parse_content_ref(raw: &str) -> Option<replica::content::ContentRef> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    Some(replica::content::ContentRef {
        content_id: <[u8; 32]>::try_from(bytes.as_slice()).ok()?,
    })
}

/// The attachment records exactly as they sit in the Body, undecoded.
///
/// The decoded list is the wrong input for anything that has to be complete: it
/// silently drops a record this build cannot read, and a dropped record is one
/// that does not count toward the cap, cannot be detached, and — worst — is
/// missing from a declaration that is supposed to name everything this Body
/// references.
fn raw_attachments(ctx: &Context<'_>, doc: &str) -> BTreeMap<String, Vec<u8>> {
    ctx.read_collaborative(&issue_key(doc))
        .ok()
        .and_then(|view| view.maps.get("attachments").cloned())
        .unwrap_or_default()
}

/// The content set a record map references, refusing rather than guessing.
///
/// Fail-closed: a record that does not decode, or names a content id that is not
///32 bytes of hex, refuses the whole transaction. The alternative is to skip it
/// — and skipping means publishing a declaration that omits content the Body
/// still references, which makes those bytes collectable while something still
/// points at them.
fn content_of(
    records: &BTreeMap<String, Vec<u8>>,
) -> Result<Vec<replica::content::ContentRef>, Rejection> {
    let mut refs = Vec::new();
    for value in records.values() {
        let record: serde_json::Value =
            serde_json::from_slice(value).map_err(|_| Rejection::ContractViolation)?;
        let Some(content) = record.get("content").and_then(|c| c.as_str()) else {
            // A legacy record carries its bytes inline and references no
            // content at all. That is not a failure to decode; it is a record
            // from before the content plane, and it declares nothing.
            continue;
        };
        let reference = parse_content_ref(content).ok_or(Rejection::ContractViolation)?;
        if !refs.contains(&reference) {
            refs.push(reference);
        }
    }
    Ok(refs)
}

fn reg(path: &str, value: impl Into<Vec<u8>>) -> Op {
    Op::RegisterSet {
        path: path.into(),
        value: value.into(),
    }
}

fn map_set(path: &str, key: impl Into<String>, value: impl Into<Vec<u8>>) -> Op {
    Op::MapSet {
        path: path.into(),
        key: key.into(),
        value: value.into(),
    }
}

fn unchanged_effect(doc: Option<String>) -> Effect {
    Effect {
        // A no-op declares nothing, which is not the same as declaring nothing
        // *for* a Body: an empty list here means no key is named at all, so no
        // Body's existing declaration is touched.
        content_refs: Vec::new(),
        operations: vec![],
        bodies: vec![],
        effect: IssueEffect {
            doc,
            unchanged: true,
        }
        .to_json(),
        declarations: vec![],
        // A no-op still declares a demand (the read baseline every member
        // holds); it commits nothing, so the receipt is over an empty tx.
        demand: contract::demand_read(),
    }
}

/// The committed Catalog view with singleton-integrity enforcement: exactly
/// the ONE deterministic Catalog key for this Space, or nothing (not yet
/// initialized/adopted). Any other catalog-schema Body — wrong key, a
/// duplicate semantic Catalog, an unrelated Catalog-shaped Body — is typed
/// [`Rejection::StateCorrupt`]; the World never selects among, merges,
/// repairs, or silently recreates Catalogs.
fn checked_catalog_view(ctx: &Context<'_>) -> Result<Option<fabric::CollaborativeView>, Rejection> {
    let expected = catalog_key(&ctx.principal().space);
    let catalogs = ctx.bodies_with_schema(&contract::world_id(), &contract::catalog_schema());
    match catalogs.as_slice() {
        [] => Ok(None),
        [one] if one == &expected => match ctx.read_collaborative(&expected) {
            Ok(view) => Ok(Some(view)),
            // Bound as a catalog but unreadable: a wrong-model/encoding Body,
            // or one carrying a collaborative type this build cannot project.
            // Either way not a missing catalog.
            Err(_) => Err(Rejection::StateCorrupt),
        },
        _ => Err(Rejection::StateCorrupt),
    }
}

/// Load the catalog state from the committed snapshot (integrity-checked).
fn catalog_state(ctx: &Context<'_>) -> Result<CatalogState, Rejection> {
    Ok(CatalogState::from_view(checked_catalog_view(ctx)?.as_ref()))
}

/// Resolve caller-proposed new labels against the catalog this write actually
/// lands on. Returns the labels still worth creating, and the full id set to
/// apply to the issue.
///
/// A caller resolves label names against *its* snapshot. On a lagging Station
/// that snapshot is older than the Replica the write lands on — and the staler
/// it is, the more names fail to resolve and the more rival ids it mints for
/// labels this Space already has. Resolving again here, where the catalog is
/// read under the same lock as the write, is what stops one stale label
/// becoming a permanent pair of same-named labels that keeps the Catalog — the
/// single Space-wide Body every concurrent writer contends on — churning.
///
/// It also collapses duplicates *within* one request, which no caller loop can
/// do: the loop never sees its own mints, so `--label bug --label bug` minted
/// two ids for one name every time, with no concurrency involved at all.
///
/// It cannot stop two Stations minting the same name concurrently — nothing
/// short of coordination can, and this is a CRDT. But that window is now
/// genuinely concurrent instead of being as wide as the caller's snapshot is
/// stale, which is the difference between a rare collision and a desync that
/// widens itself every time somebody types a label name.
fn reconcile_new_labels(
    catalog: &CatalogState,
    existing: &[String],
    proposed: &[NewLabel],
) -> (Vec<NewLabel>, Vec<String>) {
    let mut create: Vec<NewLabel> = Vec::new();
    let mut apply: Vec<String> = Vec::new();
    for id in existing {
        if !apply.contains(id) {
            apply.push(id.clone());
        }
    }
    let adopt = |id: &String, apply: &mut Vec<String>| {
        if !apply.contains(id) {
            apply.push(id.clone());
        }
    };
    for proposal in proposed {
        let name = proposal.name.trim();
        if let Some((id, _)) = catalog
            .labels
            .iter()
            .find(|(_, meta)| meta.name.eq_ignore_ascii_case(name))
        {
            adopt(id, &mut apply);
            continue;
        }
        if let Some(minted) = create.iter().find(|c| c.name.eq_ignore_ascii_case(name)) {
            let id = minted.id.clone();
            adopt(&id, &mut apply);
            continue;
        }
        create.push(proposal.clone());
        adopt(&proposal.id, &mut apply);
    }
    (create, apply)
}

fn issue_state(ctx: &Context<'_>, doc: &str) -> Option<IssueState> {
    ctx.read_collaborative(&issue_key(doc))
        .ok()
        .map(|v| IssueState::from_view(&v))
}

fn spec_state(ctx: &Context<'_>, spec: &str) -> Option<crate::spec::Spec> {
    ctx.read_collaborative(&spec_key(spec))
        .ok()
        .map(|view| crate::spec::Spec::from_view(&view))
}

fn baseline_state(ctx: &Context<'_>, baseline: &str) -> Option<crate::spec::Baseline> {
    ctx.read_collaborative(&baseline_key(baseline))
        .ok()
        .map(|view| crate::spec::Baseline::from_view(&view))
}

fn all_specs(ctx: &Context<'_>) -> Vec<crate::spec::Spec> {
    let mut specs: Vec<_> = ctx
        .bodies_with_schema(&contract::world_id(), &contract::spec_schema())
        .iter()
        .filter_map(|key| ctx.read_collaborative(key).ok())
        .map(|view| crate::spec::Spec::from_view(&view))
        .filter(|spec| !spec.revisions.is_empty())
        .collect();
    specs.sort_by(|a, b| {
        let a = a
            .revisions
            .first()
            .map(|revision| revision.body.spec.as_str());
        let b = b
            .revisions
            .first()
            .map(|revision| revision.body.spec.as_str());
        a.cmp(&b)
    });
    specs
}

fn all_baselines(ctx: &Context<'_>) -> Vec<crate::spec::Baseline> {
    let mut baselines: Vec<_> = ctx
        .bodies_with_schema(&contract::world_id(), &contract::baseline_schema())
        .iter()
        .filter_map(|key| ctx.read_collaborative(key).ok())
        .map(|view| crate::spec::Baseline::from_view(&view))
        .filter(|baseline| !baseline.revisions.is_empty())
        .collect();
    baselines.sort_by(|a, b| {
        let a = a
            .revisions
            .first()
            .map(|revision| revision.body.baseline.as_str());
        let b = b
            .revisions
            .first()
            .map(|revision| revision.body.baseline.as_str());
        a.cmp(&b)
    });
    baselines
}

fn relation_state(ctx: &Context<'_>, project: &str) -> Option<RelationState> {
    let key = contract::relation_key(project);
    ctx.read_collaborative(&key)
        .ok()
        .map(|view| RelationState::from_view(&view))
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn migrated_spec_body(
    head: &crate::spec::Revision,
    generation: &str,
    actor: &str,
    ts: u64,
) -> Option<crate::spec::Body> {
    let plan_pending = head.body.kind == crate::spec::Kind::Plan && head.body.plan.is_none();
    if !head.body.generation.is_empty() && !plan_pending {
        return None;
    }
    let mut body = head.body.clone();
    body.generation = generation.into();
    if plan_pending {
        body.plan = Some(crate::spec::PlanData { roots: Vec::new() });
    }
    body.author = actor.into();
    body.ts = ts;
    Some(body)
}

/// Audit the current representation without treating immutable history as work
/// still to do. The derived catalog is the visible truth; relation Bodies are
/// checked against it to find facts still supplied only by the compatibility
/// overlay.
fn structure_report(ctx: &Context<'_>, snap: &DerivedSnapshot) -> contract::StructureReport {
    let catalog = &snap.catalog;
    let mut relation_bodies = 0u64;
    let mut relation_projects_pending = 0u64;
    let mut relation_edges_pending = 0u64;
    let mut relation_parents_pending = 0u64;

    for project in catalog.projects.keys() {
        let state = relation_state(ctx, project);
        if state.is_some() {
            relation_bodies = relation_bodies.saturating_add(1);
        }
        let mut project_pending = false;
        for edge in &catalog.edges {
            let belongs = snap
                .issues
                .get(&edge.0)
                .is_some_and(|issue| &issue.project == project);
            if belongs && state.as_ref().and_then(|state| state.edges.get(edge)) != Some(&true) {
                relation_edges_pending = relation_edges_pending.saturating_add(1);
                project_pending = true;
            }
        }
        for (child, parent) in &catalog.parents {
            let belongs = snap
                .issues
                .get(child)
                .is_some_and(|issue| &issue.project == project);
            if belongs
                && state.as_ref().and_then(|state| state.parents.get(child))
                    != Some(&Some(parent.clone()))
            {
                relation_parents_pending = relation_parents_pending.saturating_add(1);
                project_pending = true;
            }
        }
        if project_pending {
            relation_projects_pending = relation_projects_pending.saturating_add(1);
        }
    }

    let specs = all_specs(ctx);
    let mut spec_heads_pending = 0u64;
    let mut spec_conflicts = 0u64;
    let mut plans_without_roots = 0u64;
    for spec in &specs {
        let heads = spec.heads();
        if heads.len() != 1 {
            spec_conflicts = spec_conflicts.saturating_add(1);
            let pending = heads.iter().any(|head| {
                head.body.generation.is_empty()
                    || (head.body.kind == crate::spec::Kind::Plan && head.body.plan.is_none())
            });
            if pending {
                spec_heads_pending = spec_heads_pending.saturating_add(1);
            }
            plans_without_roots = plans_without_roots.saturating_add(count(
                heads
                    .iter()
                    .filter(|head| {
                        head.body.kind == crate::spec::Kind::Plan && head.body.plan.is_none()
                    })
                    .count(),
            ));
            continue;
        }
        let body = &heads[0].body;
        let plan_pending = body.kind == crate::spec::Kind::Plan && body.plan.is_none();
        if plan_pending {
            plans_without_roots = plans_without_roots.saturating_add(1);
        }
        if body.generation.is_empty() || plan_pending {
            spec_heads_pending = spec_heads_pending.saturating_add(1);
        }
    }
    let issue_documents_pending = count(
        snap.issues
            .values()
            .filter(|issue| issue.document_schema != DOCUMENT_SCHEMA_VERSION)
            .count(),
    );
    let complete = relation_edges_pending == 0
        && relation_parents_pending == 0
        && spec_heads_pending == 0
        && issue_documents_pending == 0;

    contract::StructureReport {
        generation: data_encoding::HEXLOWER.encode(&ctx.manifest_root()),
        projects: count(catalog.projects.len()),
        issues: count(snap.issues.len()),
        visible_edges: count(catalog.edges.len()),
        visible_parents: count(catalog.parents.len()),
        relation_bodies,
        relation_projects_pending,
        relation_edges_pending,
        relation_parents_pending,
        specs: count(specs.len()),
        spec_heads_pending,
        spec_conflicts,
        plans_without_roots,
        issue_documents_pending,
        baselines: count(all_baselines(ctx).len()),
        complete,
    }
}

fn spec_view(spec: &crate::spec::Spec) -> Option<crate::spec::SpecView> {
    let heads = spec.heads();
    let selected = heads.first().copied()?;
    let issued = match spec.issued() {
        crate::spec::Issued::None => vec![],
        crate::spec::Issued::One(revision) => vec![revision.revision.clone()],
        crate::spec::Issued::Conflict(revisions) => revisions
            .into_iter()
            .map(|revision| revision.revision.clone())
            .collect(),
    };
    Some(crate::spec::SpecView {
        spec: selected.body.spec.clone(),
        project: selected.body.project.clone(),
        kind: selected.body.kind,
        title: selected.body.title.clone(),
        state: selected.body.state,
        revision: selected.revision.clone(),
        heads: heads
            .into_iter()
            .map(|revision| revision.revision.clone())
            .collect(),
        issued,
        body: selected.body.clone(),
    })
}

fn baseline_view(baseline: &crate::spec::Baseline) -> Option<crate::spec::BaselineView> {
    let heads = baseline.heads();
    let selected = heads.first().copied()?;
    let issued = match baseline.issued() {
        crate::spec::BaselineIssued::None => vec![],
        crate::spec::BaselineIssued::One(revision) => vec![revision.revision.clone()],
        crate::spec::BaselineIssued::Conflict(revisions) => revisions
            .into_iter()
            .map(|revision| revision.revision.clone())
            .collect(),
    };
    Some(crate::spec::BaselineView {
        baseline: selected.body.baseline.clone(),
        project: selected.body.project.clone(),
        name: selected.body.name.clone(),
        state: selected.body.state,
        revision: selected.revision.clone(),
        heads: heads
            .into_iter()
            .map(|revision| revision.revision.clone())
            .collect(),
        issued,
        body: selected.body.clone(),
    })
}

fn validate_spec_ref(
    ctx: &Context<'_>,
    member: &crate::spec::SpecRef,
    project: &str,
) -> Result<(), Rejection> {
    let spec = spec_state(ctx, &member.spec).ok_or(Rejection::InvalidRequest)?;
    let revision = spec
        .revision(&member.revision)
        .ok_or(Rejection::InvalidRequest)?;
    if revision.body.project != project || revision.body.state != crate::spec::State::Issued {
        return Err(Rejection::InvalidRequest);
    }
    Ok(())
}

fn validate_spec_links(ctx: &Context<'_>, links: &[crate::spec::Link]) -> Result<(), Rejection> {
    for link in links {
        match &link.target {
            crate::spec::Target::Spec { spec, revision } => {
                let target = spec_state(ctx, spec).ok_or(Rejection::InvalidRequest)?;
                if target.revision(revision).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
            }
            crate::spec::Target::Baseline { baseline, revision } => {
                let target = baseline_state(ctx, baseline).ok_or(Rejection::InvalidRequest)?;
                if target.revision(revision).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
            }
            crate::spec::Target::Issue { issue } => {
                if issue_state(ctx, issue).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
            }
        }
    }
    Ok(())
}

fn validate_plan(
    ctx: &Context<'_>,
    _catalog: &CatalogState,
    project: &str,
    plan: Option<&crate::spec::PlanData>,
) -> Result<(), Rejection> {
    let Some(plan) = plan else { return Ok(()) };
    plan.validate().map_err(|_| Rejection::InvalidRequest)?;
    for issue in &plan.roots {
        let target = issue_state(ctx, issue).ok_or(Rejection::InvalidRequest)?;
        if target.project != project {
            return Err(Rejection::InvalidRequest);
        }
    }
    Ok(())
}

fn packet(ctx: &Context<'_>, doc: &str) -> Result<crate::spec::Packet, Rejection> {
    let issue = issue_state(ctx, doc).ok_or(Rejection::InvalidRequest)?;
    let specs = all_specs(ctx);
    let mut exact: BTreeMap<
        (String, String),
        (&crate::spec::Revision, crate::spec::PacketSource, bool),
    > = BTreeMap::new();
    let mut conflicts = Vec::new();

    if let Some(binding) = &issue.baseline {
        let Some(baseline) = baseline_state(ctx, &binding.baseline) else {
            conflicts.push(crate::spec::PacketConflict::MissingBaseline {
                baseline: binding.baseline.clone(),
            });
            return Ok(crate::spec::Packet {
                issue: doc.into(),
                baseline: issue.baseline,
                governing: vec![],
                guidance: vec![],
                proof: vec![],
                record: vec![],
                conflicts,
            });
        };
        let Some(revision) = baseline.revision(&binding.revision) else {
            conflicts.push(crate::spec::PacketConflict::MissingBaselineRevision {
                baseline: binding.baseline.clone(),
                revision: binding.revision.clone(),
            });
            return Ok(crate::spec::Packet {
                issue: doc.into(),
                baseline: issue.baseline,
                governing: vec![],
                guidance: vec![],
                proof: vec![],
                record: vec![],
                conflicts,
            });
        };
        if revision.body.state != crate::spec::State::Issued {
            conflicts.push(crate::spec::PacketConflict::BaselineNotIssued {
                baseline: binding.baseline.clone(),
                revision: binding.revision.clone(),
            });
        }
        for member in &revision.body.members {
            let Some(spec) = specs.iter().find(|candidate| {
                candidate
                    .revisions
                    .first()
                    .is_some_and(|revision| revision.body.spec == member.spec)
            }) else {
                conflicts.push(crate::spec::PacketConflict::MissingSpec {
                    spec: member.spec.clone(),
                });
                continue;
            };
            let Some(revision) = spec.revision(&member.revision) else {
                conflicts.push(crate::spec::PacketConflict::MissingSpecRevision {
                    spec: member.spec.clone(),
                    revision: member.revision.clone(),
                });
                continue;
            };
            exact.insert(
                (member.spec.clone(), member.revision.clone()),
                (
                    revision,
                    crate::spec::PacketSource::Baseline {
                        baseline: binding.baseline.clone(),
                    },
                    false,
                ),
            );
        }
    }

    // Issued Specs may supplement one Issue directly. Concurrent controlling
    // revisions remain a visible conflict; no timestamp winner is selected.
    for spec in &specs {
        match spec.issued() {
            crate::spec::Issued::One(revision) => {
                if revision.body.links.iter().any(|link| {
                    link.rel == crate::spec::Rel::Governs
                        && matches!(&link.target, crate::spec::Target::Issue { issue } if issue == doc)
                }) {
                    exact.insert(
                        (revision.body.spec.clone(), revision.revision.clone()),
                        (revision, crate::spec::PacketSource::Direct, false),
                    );
                }
            }
            crate::spec::Issued::Conflict(revisions) => {
                if revisions.iter().any(|revision| {
                    revision.body.links.iter().any(|link| {
                        link.rel == crate::spec::Rel::Governs
                            && matches!(&link.target, crate::spec::Target::Issue { issue } if issue == doc)
                    })
                }) {
                    let id = revisions
                        .first()
                        .map(|revision| revision.body.spec.clone())
                        .unwrap_or_else(|| "unknown".into());
                    conflicts.push(crate::spec::PacketConflict::IssuedSpecConflict { spec: id });
                }
            }
            crate::spec::Issued::None => {}
        }
    }

    // Incorporation, unlike reference, pulls the exact target into the
    // governing set. Traverse to a fixed point over exact revisions.
    loop {
        let mut added = false;
        let snapshot: Vec<_> = exact.values().map(|(revision, _, _)| *revision).collect();
        for revision in snapshot {
            for link in &revision.body.links {
                if link.rel != crate::spec::Rel::Incorporates {
                    continue;
                }
                let crate::spec::Target::Spec {
                    spec,
                    revision: target_revision,
                } = &link.target
                else {
                    continue;
                };
                if exact.contains_key(&(spec.clone(), target_revision.clone())) {
                    continue;
                }
                let Some(target) = specs
                    .iter()
                    .find_map(|candidate| candidate.revision(target_revision))
                    .filter(|candidate| candidate.body.spec == *spec)
                else {
                    conflicts.push(crate::spec::PacketConflict::MissingIncorporated {
                        spec: spec.clone(),
                        revision: target_revision.clone(),
                    });
                    continue;
                };
                exact.insert(
                    (spec.clone(), target_revision.clone()),
                    (
                        target,
                        crate::spec::PacketSource::Incorporated {
                            spec: revision.body.spec.clone(),
                            revision: revision.revision.clone(),
                        },
                        true,
                    ),
                );
                added = true;
            }
        }
        if !added {
            break;
        }
    }

    let mut governing = Vec::new();
    let mut guidance = Vec::new();
    let mut proof = Vec::new();
    let mut record = Vec::new();
    for (_, (revision, source, incorporated)) in exact {
        let item = crate::spec::PacketSpec {
            spec: revision.body.spec.clone(),
            revision: revision.revision.clone(),
            kind: revision.body.kind,
            title: revision.body.title.clone(),
            state: revision.body.state,
            source,
            links: revision.body.links.clone(),
        };
        if incorporated || revision.body.kind.governs() {
            governing.push(item);
        } else {
            match revision.body.kind {
                crate::spec::Kind::Goal | crate::spec::Kind::Plan | crate::spec::Kind::Guide => {
                    guidance.push(item)
                }
                crate::spec::Kind::Proof | crate::spec::Kind::Verdict => proof.push(item),
                _ => record.push(item),
            }
        }
    }
    Ok(crate::spec::Packet {
        issue: doc.into(),
        baseline: issue.baseline,
        governing,
        guidance,
        proof,
        record,
        conflicts,
    })
}

/// The preconditions both comment verbs share, the issue they hold for, and
/// the hierarchy node a reply hangs under.
///
/// The daemon mints the id; the World re-validates it — including uniqueness,
/// because a duplicated id would fuse two comments' reactions, replies and
/// spans.
///
/// The returned node is `None` for a root comment *and* for a reply to a
/// comment that predates the hierarchy: a legacy `list:comments` record has no
/// node to hang under, so the reply is filed at the root and threads through
/// its `parent` field alone, exactly as it did before. Refusing instead would
/// make old comments unanswerable, which is a worse answer than the one the
/// product already gives them.
fn check_comment(
    ctx: &Context<'_>,
    doc: &str,
    body: &str,
    actor: &str,
    id: Option<&str>,
    parent: Option<&str>,
) -> Result<(IssueState, Option<String>), Rejection> {
    if body.is_empty() || ActorId::parse(actor).is_none() {
        return Err(Rejection::InvalidRequest);
    }
    let issue = issue_state(ctx, doc).ok_or(Rejection::InvalidRequest)?;
    if let Some(id) = id {
        if !contract::is_comment_id(id)
            || issue.comments.iter().any(|c| c.id.as_deref() == Some(id))
        {
            return Err(Rejection::InvalidRequest);
        }
    }
    let Some(parent) = parent else {
        return Ok((issue, None));
    };
    // A reply needs an addressable target: an existing comment that carries
    // an id (pre-identity comments cannot anchor threads) and is itself a
    // root — one level, no ladders.
    let target = issue
        .comments
        .iter()
        .find(|c| c.id.as_deref() == Some(parent))
        .ok_or(Rejection::InvalidRequest)?;
    // A root is a comment that answers nothing by either account. Both are
    // checked, not just the hierarchy, because they can legitimately disagree:
    // a reply to a comment that predates the hierarchy has no parent edge to
    // hang from and threads through its `parent` field alone. Trusting the
    // edge there would read that reply as a root and let a reply hang off it —
    // the ladder the one-level rule exists to refuse, rebuilt through the one
    // case the cutover creates.
    if id.is_none() || target.parent.is_some() || target.parent_node.is_some() {
        return Err(Rejection::InvalidRequest);
    }
    let node = target.node.clone();
    Ok((issue, node))
}

/// File a comment into the thread and record its history event.
///
/// The comment is a node of `tree:comments`, hanging under the comment it
/// answers or at the root of the forest. That is two changes from the flat list
/// it replaces, and both are about what a long thread does to a peer that is
/// behind:
///
/// - **No index.** `ListInsert` took `index: issue.comments.len()` — the length
///   of the thread *as this replica had synced it*. A peer fifty comments
///   behind computed "the end" as position ten and wrote into the middle of a
///   conversation it had not finished reading, and the error grew with the
///   thread. A node names its parent, and a parent is not a position.
/// - **The reply edge is real.** Threading was a `parent` field over flat
///   storage, so two peers re-parenting concurrently had no defined outcome.
///   The hierarchy resolves that in the engine.
///
/// The record still carries its `parent` field, and it is still the same bytes
/// it always was, so a peer on an older build reads this comment and its
/// threading exactly as before.
fn stage_comment(
    staging: &mut Staging,
    ctx: &Context<'_>,
    doc: &str,
    parent_node: Option<String>,
    record: StoredComment,
    device: &str,
    ts: u64,
) {
    let mut ev = event("commented", device, ts);
    ev.x = record.b.clone();
    staging.issue(
        &issue_key(doc),
        Op::TreeInsert {
            path: "comments".into(),
            parent: parent_node,
            // Placement is the writer's own view either way, so there is
            // nothing to gain by naming a sibling — the read side orders the
            // thread by each record's own clock. See `views::read_comments`.
            after: None,
            value: serde_json::to_vec(&record).expect("comment json"),
        },
    );
    push_event(staging, ctx, doc, &ev);
}

/// Mint the durable anchors for a range-attached comment, or refuse.
///
/// Every refusal here has the same shape: the alternative is storing an anchor
/// nothing can resolve, which reads back as a confident position that was never
/// true.
///
/// - A field this build does not write with a text operation. See
///   [`IssueState::anchorable_text`] — `anchor_in_body` answers `Some` for any
///   path, so an anchor into a register is minted happily and then answers
///   position zero forever.
/// - A field with no material yet. A span of an empty text names nothing; the
///   anchor the algebra returns for it binds to no operation and can therefore
///   never report drift.
/// - A span running backwards or past the end of what it names.
/// - A Body the algebra will not anchor in at all — absent, or not
///   collaborative. `anchor_in_body` returning `None` is the substrate saying
///   there is no position here, and the World does not invent one.
///
/// A span with material binds its head to the first character INSIDE it rather
/// than to the character in front of it. `BodyReader::anchor_in_body` binds
/// position `p` to whatever wrote character `p - 1`, so minting the head at
/// `start` would tie the comment to a character nobody marked: deleting the
/// space in front of a marked word would then report the word as gone. Minting
/// the head at `start + 1` binds it to the word's own first character, and the
/// read half subtracts the one back. An empty span has no first character to
/// bind to, so it is stored as the caret it is — `end` absent — and keeps the
/// character-in-front binding, which is the only one a caret at the very end of
/// a text can have.
fn mint_comment_anchor(
    ctx: &Context<'_>,
    doc: &str,
    issue: &IssueState,
    field: &str,
    start: u64,
    end: Option<u64>,
) -> Result<contract::StoredAnchor, Rejection> {
    let text = issue
        .anchorable_text(field)
        .ok_or(Rejection::InvalidRequest)?;
    // Unicode scalars: the coordinate system `Op::TextSplice` is validated
    // in, so a span counted any other way would name a different place.
    let length = text.chars().count() as u64;
    let last = end.unwrap_or(start);
    if length == 0 || start > last || last > length {
        return Err(Rejection::InvalidRequest);
    }
    let key = issue_key(doc);
    let mint = |position: u64| -> Result<String, Rejection> {
        ctx.anchor(&key, field, position)
            .map(|anchor| data_encoding::HEXLOWER.encode(&anchor.encode()))
            .ok_or(Rejection::InvalidRequest)
    };
    let span = start < last;
    Ok(contract::StoredAnchor {
        field: field.to_string(),
        start: mint(if span { start + 1 } else { start })?,
        end: span.then(|| mint(last)).transpose()?,
    })
}

/// Resolve one stored comment's span against the snapshot THIS query is pinned
/// to.
///
/// Called per read and never memoized. The parsed [`IssueState`] is cached
/// under a Body version stamp, so a resolution placed in it would be served
/// against a Body it was never true of — the stale index the algebra exists to
/// prevent. What is cached is the anchor; what is computed is the position.
///
/// The two preconditions [`mint_comment_anchor`] refuses on are checked again
/// here, against the record instead of the request. A comment is a list element
/// of a shared Body, so a record arrives over Contact from peers running builds
/// this one does not control; `anchor_in_body` validates no path, and an anchor
/// naming a register resolves to a confident position zero that can never
/// drift. Refusing that only at the write seam would leave the read seam
/// affirming the exact lie the write seam exists to stop.
fn resolve_comment_anchor(
    ctx: &Context<'_>,
    doc: &str,
    issue: &IssueState,
    comment: &StoredComment,
) -> Option<crate::dto::CommentAnchorDto> {
    use crate::dto::CommentAnchorState;
    let at = comment.at.as_ref()?;
    let dto = |state| {
        Some(crate::dto::CommentAnchorDto {
            field: at.field.clone(),
            state,
        })
    };
    match issue.anchorable_text(&at.field) {
        // A field with no text in it has no positions for the algebra to move.
        // That is not a lost position — this reader has no answer at all, and
        // `Drifted` would assert one.
        None => return dto(CommentAnchorState::Unresolved),
        // The mint side's rule, applied to the material as it stands rather
        // than as it stood: a span of an empty text names nothing.
        Some("") => return dto(CommentAnchorState::Drifted),
        Some(_) => {}
    }
    let key = issue_key(doc);
    let one = |hex: &str| -> Option<fabric::AnchorResolution> {
        let raw = data_encoding::HEXLOWER.decode(hex.as_bytes()).ok()?;
        let anchor = fabric::Anchor::decode_canonical(&raw).ok()?;
        // The record names a field and so does the anchor inside it. This
        // build writes them together and they always agree; a record from
        // anywhere else that disagrees cannot say which one its writer meant,
        // and resolving the anchor while reporting the record's field would
        // hand back the right offset of the wrong value.
        (anchor.path == at.field).then_some(())?;
        Some(ctx.resolve_anchor(&key, &anchor))
    };
    let Some(head) = one(&at.start) else {
        return dto(CommentAnchorState::Unresolved);
    };
    let tail = match &at.end {
        None => Some(head),
        Some(hex) => one(hex),
    };
    let state = match (head, tail) {
        (fabric::AnchorResolution::Resolved(h), Some(fabric::AnchorResolution::Resolved(t))) => {
            // A resolved anchor sits one past the character it bound to. For a
            // span that character is the first one inside it, so the span's
            // start is one back; a caret bound to the character in front of it
            // already resolves to itself.
            let start = if at.end.is_some() {
                h.saturating_sub(1)
            } else {
                h
            };
            // Out of order is no longer a span, and half a span is the guess
            // the algebra forbids.
            if t >= start {
                CommentAnchorState::At { start, end: t }
            } else {
                CommentAnchorState::Drifted
            }
        }
        (_, Some(_)) => CommentAnchorState::Drifted,
        (_, None) => CommentAnchorState::Unresolved,
    };
    dto(state)
}

/// The resumable token for one activity row: `(ts, doc, ordinal, entry id)`.
///
/// **This is the feed's sort key, not a separate encoding of it.** Both queries
/// order rows by comparing these strings, so "the next page" and "after this
/// token" cannot drift apart. They did, in the first cut: the feed sorted by
/// ordinal within a `(ts, doc)` group while the token ended in the entry id,
/// which sorts differently, and a resume re-served rows whose id happened to
/// sort above the last one's. The two orders are now one order because they are
/// one string.
///
/// `ordinal` is the row's place in its issue's *whole* history, trimmed rows
/// included, which is what makes the token survive trimming: dropping the
/// oldest events raises the trimmed count by exactly what it removes, so every
/// surviving row keeps the ordinal it had. The entry id rides along so the
/// token names the row's identity as well as its place.
///
/// Both numbers are zero-padded to twenty digits — `u64::MAX` is twenty long —
/// because the comparison is lexicographic and an unpadded `9` would sort after
/// an unpadded `10`.
fn activity_cursor(event: &IssueEvent, doc: &str, ordinal: u64) -> String {
    format!("{:020}\t{doc}\t{ordinal:020}\t{}", event.t, event.entry)
}

/// Who a history row is attributed to.
///
/// `None` rather than the device it was committed on. An event written before
/// events carried an actor has no honest name, and the viewer already draws that
/// as no name — where a device id would be drawn as a name, in a colour derived
/// from hashing hex, that nothing else on the screen agrees with.
fn actor_of(event: &IssueEvent) -> Option<ActorId> {
    ActorId::parse(&event.a)
}

/// Append one history event to an issue's `events` list.
fn push_event(staging: &mut Staging, ctx: &Context<'_>, doc: &str, event: &IssueEvent) {
    // Stamped here rather than at each construction site, which is why the
    // eleven of them and the intents carrying `device` are untouched. The actor
    // is the Session's own, re-derived by the authority view at every submit —
    // never a string the caller supplied, which is the whole reason it is worth
    // showing.
    let event = &IssueEvent {
        a: ctx.principal().actor.as_str().to_string(),
        ..event.clone()
    };
    staging.issue(
        &issue_key(doc),
        Op::LogAppend {
            path: contract::EVENTS_PATH.into(),
            value: serde_json::to_vec(event).expect("event json"),
            retain: contract::EVENTS_RETAINED,
        },
    );
}

/// Resolve the deterministic transition gate `from -> to` for a project: the
/// demand template stored on the selected transition of the project's current
/// workflow revision, plus the receipt-bound transition evidence. A missing
/// revision on an existing project is corrupt catalog state; an edge the
/// workflow does not define is an invalid transition — never inferred.
fn transition_gate(
    catalog: &CatalogState,
    project: &str,
    from: &str,
    to: &str,
) -> Result<(Vec<u8>, crate::workflow::WorkflowTransitionEvidence), Rejection> {
    // The single usable head gates transitions; concurrent heads block them
    // (and further ordinary edits) until `workflow set --expect-head`
    // resolves. A project with NO revision at all is corrupt catalog state.
    if !catalog.workflow_revisions.contains_key(project) {
        return Err(Rejection::StateCorrupt);
    }
    let revision = catalog.workflow_head(project).ok_or(Rejection::Conflict)?;
    let transition = revision
        .body
        .transition_for(from, to)
        .ok_or(Rejection::InvalidRequest)?;
    let demand = transition.demand_template.resolve(project);
    let bytes = demand
        .encode_canonical()
        .map_err(|_| Rejection::ContractViolation)?;
    let digest = demand.digest().map_err(|_| Rejection::ContractViolation)?;
    let evidence = crate::workflow::WorkflowTransitionEvidence {
        transition_id: transition.transition_id.clone(),
        workflow_revision_id: revision.revision_id.clone(),
        source_state: from.to_string(),
        destination_state: to.to_string(),
        resolved_demand_digest: data_encoding::HEXLOWER.encode(&digest),
    };
    Ok((bytes, evidence))
}

/// Whether every capability id is registered for the declared scope kind
/// (sorted, unique, non-empty).
fn validate_role_caps(caps: &[String], scope: crate::roles::ScopeKind) -> Result<(), Rejection> {
    if caps.is_empty() {
        return Err(Rejection::InvalidRequest);
    }
    let mut sorted = caps.to_vec();
    sorted.sort();
    sorted.dedup();
    if sorted.len() != caps.len() {
        return Err(Rejection::InvalidRequest);
    }
    let registered = |c: &str| match scope {
        crate::roles::ScopeKind::Space => contract::is_space_capability(c),
        crate::roles::ScopeKind::Project => contract::is_project_capability(c),
    };
    if caps.iter().any(|c| !registered(c)) {
        return Err(Rejection::InvalidRequest);
    }
    Ok(())
}

/// The single usable custom-role head, which must match `expected` exactly.
/// Multiple heads are a typed conflict that blocks edits until resolved.
fn expect_single_head<'a>(
    catalog: &'a CatalogState,
    role_id: &str,
    expected: &str,
) -> Result<&'a crate::views::StoredRoleRevision, Rejection> {
    let heads = catalog.role_heads(role_id);
    match heads.as_slice() {
        [] => Err(Rejection::InvalidRequest),
        [one] if one.body.tombstone => Err(Rejection::InvalidRequest),
        [one] if one.revision_id == expected => Ok(one),
        [_one] => Err(Rejection::Conflict),
        _ => Err(Rejection::Conflict),
    }
}

fn decode_hex32(hex: &str) -> Result<[u8; 32], Rejection> {
    let raw = data_encoding::HEXLOWER
        .decode(hex.as_bytes())
        .map_err(|_| Rejection::InvalidRequest)?;
    raw.as_slice()
        .try_into()
        .map_err(|_| Rejection::InvalidRequest)
}

/// Stage one role revision into the grow-only log.
fn stage_role_revision(staging: &mut Staging, revision: &crate::roles::RoleRevision) {
    let stored = crate::views::StoredRoleRevision {
        revision_id: data_encoding::HEXLOWER.encode(&revision.revision_id),
        predecessor_ids: revision
            .predecessor_ids
            .iter()
            .map(|p| data_encoding::HEXLOWER.encode(p))
            .collect(),
        body: revision.body.clone(),
    };
    staging.catalog(map_set(
        "role_revisions",
        format!("{}/{}", revision.body.role_id, stored.revision_id),
        serde_json::to_vec(&stored).expect("role revision json"),
    ));
}

fn event(kind: &str, device: &str, ts: u64) -> IssueEvent {
    IssueEvent {
        k: kind.into(),
        d: device.into(),
        // Filled by `push_event` from the Session's own principal. Left empty
        // here so no construction site can supply one: an actor a caller passed
        // in is a claim, and the whole value of showing this is that it is not.
        a: String::new(),
        t: ts,
        c: vec![],
        x: String::new(),
        // Filled by the projection from the log entry this lands in — there is
        // no entry until it is committed.
        entry: String::new(),
    }
}

/// Board helpers, staged against the CURRENT catalog view.
fn board_entries(catalog: &CatalogState, project: &str) -> Vec<(String, String)> {
    catalog.boards.get(project).cloned().unwrap_or_default()
}

fn board_insert_top(staging: &mut Staging, catalog: &CatalogState, project: &str, doc: &str) {
    if board_entries(catalog, project)
        .iter()
        .any(|(_, d)| d == doc)
    {
        return;
    }
    staging.catalog(Op::ListInsert {
        path: board_path(project),
        index: 0,
        value: doc.as_bytes().to_vec(),
    });
}

fn board_remove(staging: &mut Staging, catalog: &CatalogState, project: &str, doc: &str) {
    if let Some((element, _)) = board_entries(catalog, project)
        .into_iter()
        .find(|(_, d)| d == doc)
    {
        staging.catalog(Op::ListRemove {
            path: board_path(project),
            element,
        });
    }
}

/// The legacy `board_move` index math over the current entries.
fn board_move(
    staging: &mut Staging,
    catalog: &CatalogState,
    project: &str,
    doc: &str,
    anchor: &str,
    after: bool,
) {
    let entries = board_entries(catalog, project);
    let len = entries.len();
    let doc_pos = entries.iter().position(|(_, d)| d == doc);
    let anchor_pos = entries.iter().position(|(_, d)| d == anchor);
    match (doc_pos, anchor_pos) {
        (Some(from), Some(a)) => {
            use std::cmp::Ordering;
            let to = match from.cmp(&a) {
                Ordering::Equal => return,
                Ordering::Greater => {
                    if after {
                        a + 1
                    } else {
                        a
                    }
                }
                Ordering::Less => {
                    if after {
                        a
                    } else {
                        a.saturating_sub(1)
                    }
                }
            };
            let to = to.min(len.saturating_sub(1));
            staging.catalog(Op::ListMove {
                path: board_path(project),
                element: entries[from].0.clone(),
                index: to as u64,
            });
        }
        (None, Some(a)) => {
            let at = if after { a + 1 } else { a }.min(len);
            staging.catalog(Op::ListInsert {
                path: board_path(project),
                index: at as u64,
                value: doc.as_bytes().to_vec(),
            });
        }
        (Some(from), None) => {
            if len > 0 {
                staging.catalog(Op::ListMove {
                    path: board_path(project),
                    element: entries[from].0.clone(),
                    index: (len - 1) as u64,
                });
            }
        }
        (None, None) => {
            staging.catalog(Op::ListInsert {
                path: board_path(project),
                index: len as u64,
                value: doc.as_bytes().to_vec(),
            });
        }
    }
}

/// A minimal char-coordinate splice from `old` to `new` (legacy `LoroText
/// update` behavior: concurrent edits merge instead of last-write-wins).
fn text_splice(old: &str, new: &str) -> Option<(u64, u64, String)> {
    if old == new {
        return None;
    }
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();
    let mut prefix = 0;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_chars.len() - prefix
        && suffix < new_chars.len() - prefix
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let delete = (old_chars.len() - prefix - suffix) as u64;
    let insert: String = new_chars[prefix..new_chars.len() - suffix].iter().collect();
    Some((prefix as u64, delete, insert))
}

/// Walk the parent map from `start` upward, returning true if `needle` is an
/// ancestor (cycle-safe).
fn is_ancestor(catalog: &CatalogState, start: &str, needle: &str) -> bool {
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = start.to_string();
    while let Some(parent) = catalog.parents.get(&cursor) {
        if !seen.insert(parent.clone()) {
            return false; // pre-existing cycle: stop, do not loop
        }
        if parent == needle {
            return true;
        }
        cursor = parent.clone();
    }
    false
}

impl World for IssuesWorld {
    fn descriptor(&self) -> runtime::world::Descriptor {
        runtime::world::Descriptor {
            id: self.id.clone(),
            implementation_version: runtime::world::Version(2),
            schemas: self.schemas.clone(),
            limits: runtime::world::Limits::default(),
            scope_schemas: Vec::new(),
            signal_schemas: self.signal_schemas.clone(),
        }
    }

    fn id(&self) -> replica::body::WorldId {
        self.id.clone()
    }

    fn schemas(&self) -> &[Schema] {
        &self.schemas
    }

    fn signal_schemas(&self) -> &[runtime::world::SignalSchema] {
        &self.signal_schemas
    }

    fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let intent = IssueIntent::from_json(&intent.payload).ok_or(Rejection::InvalidRequest)?;
        let catalog_view = checked_catalog_view(ctx)?;
        // Writes validate against the same composed topology that reads expose.
        // The Catalog remains the compatibility source for older Worlds, while
        // project relation Bodies override its links and hierarchy.
        let derived = self.derived_snapshot(ctx)?;
        let catalog = derived.catalog.clone();
        let mut staging = Staging::for_space(ctx.principal().space.clone(), catalog_view.is_none());
        drop(catalog_view);
        match intent {
            IssueIntent::InitializeTracker {
                name,
                ts,
                project_id,
                project_name,
                project_key,
                device: _,
                built_in_roles,
                capability_registry_commitment,
                default_workflow_commitment,
            } => {
                // A deterministic pure validator/stager: every captured value
                // arrives in the intent (the composition root persisted the
                // signed bytes); the World calls no clock and mints no id.
                let project_key = project_key.trim().to_ascii_uppercase();
                if project_name.trim().is_empty()
                    || project_key.is_empty()
                    || project_key.len() > 8
                    || !project_key.bytes().all(|b| b.is_ascii_alphabetic())
                    || project_id.is_empty()
                    || ts == 0
                {
                    return Err(Rejection::InvalidRequest);
                }
                // The golden commitments must match this implementation's
                // compiled-in definitions exactly.
                let registry_hex =
                    data_encoding::HEXLOWER.encode(&contract::capability_registry_commitment());
                if capability_registry_commitment != registry_hex {
                    return Err(Rejection::InvalidRequest);
                }
                let workflow_revision = crate::workflow::default_workflow_revision(&project_id);
                if default_workflow_commitment != workflow_revision.revision_id {
                    return Err(Rejection::InvalidRequest);
                }
                let mut goldens: Vec<(String, String, String)> = Vec::new();
                for id in crate::roles::BUILT_IN_ROLE_IDS {
                    let rev = crate::roles::built_in(id).expect("built-in role");
                    goldens.push((
                        id.to_string(),
                        data_encoding::HEXLOWER.encode(&rev.revision_id),
                        data_encoding::HEXLOWER.encode(&rev.body.definition_digest()),
                    ));
                }
                if built_in_roles != goldens {
                    return Err(Rejection::InvalidRequest);
                }
                // The deterministic Catalog must not exist yet: joiners adopt
                // it through Manifest synchronization and never create it, and
                // a second initialization never merges into the first. An
                // exact replay is answered by the request receipt before the
                // World runs; a content-identical re-run is a no-op.
                if let Some(view) = checked_catalog_view(ctx)? {
                    let initialized = view.lists.get("workflow").is_some_and(|l| !l.is_empty());
                    if initialized {
                        return Ok(unchanged_effect(None));
                    }
                    return Err(Rejection::Conflict);
                }
                // ---- one atomic Catalog transaction: display name, legacy
                // workflow states, the workflow revision, the initial project,
                // the built-in role definitions, and the registry commitment.
                staging.catalog(reg("name", name.into_bytes()));
                staging.catalog(reg("initialized_at", ts.to_string().into_bytes()));
                staging.catalog(reg(
                    "capability_registry",
                    registry_hex.clone().into_bytes(),
                ));
                for (i, state) in contract::default_workflow().into_iter().enumerate() {
                    staging.catalog(Op::ListInsert {
                        path: "workflow".into(),
                        index: i as u64,
                        value: serde_json::to_vec(&state).expect("workflow json"),
                    });
                }
                staging.catalog(map_set(
                    "workflow_revisions",
                    format!("{project_id}/{}", workflow_revision.revision_id),
                    serde_json::to_vec(&workflow_revision).expect("workflow revision json"),
                ));
                staging.catalog(map_set(
                    "projects",
                    project_id.clone(),
                    serde_json::to_vec(&serde_json::json!({
                        "name": project_name.trim(),
                        "key": project_key,
                        "color": "blue",
                    }))
                    .expect("project json"),
                ));
                for id in crate::roles::BUILT_IN_ROLE_IDS {
                    let rev = crate::roles::built_in(id).expect("built-in role");
                    staging.catalog(map_set(
                        "roles",
                        id,
                        serde_json::to_vec(&serde_json::json!({
                            "revision_id": data_encoding::HEXLOWER.encode(&rev.revision_id),
                            "predecessor_ids": [],
                            "body": serde_json::from_slice::<serde_json::Value>(
                                &rev.body.canonical_json()
                            )
                            .expect("role body json"),
                        }))
                        .expect("role json"),
                    ));
                }
                // Tracker initialization is a founder-composition admin action.
                staging.require(contract::demand_admin());
                Ok(staging.into_effect(None))
            }
            IssueIntent::StructureMigrate {
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() || ts == 0 {
                    return Err(Rejection::InvalidRequest);
                }

                // Copy only facts that are visible after every existing
                // relation overlay has been applied. Re-running therefore
                // cannot resurrect a legacy edge that a newer Body removed.
                for project in catalog.projects.keys() {
                    let key = contract::relation_key(project);
                    let create = ctx.body_version(&key).is_none();
                    let state = relation_state(ctx, project).unwrap_or_default();
                    for edge in &catalog.edges {
                        let belongs = derived
                            .issues
                            .get(&edge.0)
                            .is_some_and(|issue| issue.project == *project);
                        if belongs && state.edges.get(edge) != Some(&true) {
                            staging.relation(
                                project,
                                create,
                                map_set("edges", format!("{}|{}|{}", edge.0, edge.1, edge.2), "1"),
                            );
                        }
                    }
                    for (child, parent) in &catalog.parents {
                        let belongs = derived
                            .issues
                            .get(child)
                            .is_some_and(|issue| issue.project == *project);
                        if belongs
                            && state.parents.get(child).and_then(Option::as_deref)
                                != Some(parent.as_str())
                        {
                            staging.relation(
                                project,
                                create,
                                Op::TreeAnchor {
                                    path: contract::HIERARCHY_PATH.into(),
                                    anchor: child.clone(),
                                    parent: Some(parent.clone()),
                                },
                            );
                        }
                    }
                }

                // Current heads receive equivalent immutable successors. The
                // predecessor remains exact history, lifecycle state is kept,
                // and a Plan that predates structured seeds becomes the
                // canonical empty-roots form (the whole project).
                let generation = data_encoding::HEXLOWER.encode(&ctx.manifest_root());
                for spec in all_specs(ctx) {
                    let heads = spec.heads();
                    if heads.len() != 1 {
                        continue;
                    }
                    let head = heads[0];
                    let Some(body) = migrated_spec_body(head, &generation, &actor, ts) else {
                        continue;
                    };
                    validate_plan(ctx, &catalog, &body.project, body.plan.as_ref())?;
                    let predecessor = crate::spec::decode_revision(&head.revision)
                        .ok_or(Rejection::StateCorrupt)?;
                    let revision = crate::spec::build_revision(body, vec![predecessor])
                        .map_err(|_| Rejection::StateCorrupt)?;
                    staging.spec(
                        &spec_key(&head.body.spec),
                        map_set(
                            "revisions",
                            revision.revision.clone(),
                            serde_json::to_vec(&revision).expect("Spec revision JSON"),
                        ),
                    );
                }

                if staging.ops.is_empty() {
                    return Ok(unchanged_effect(None));
                }
                staging.require(contract::demand_admin());
                Ok(staging.into_effect(None))
            }
            IssueIntent::IssueNew {
                doc,
                project,
                title,
                priority,
                assignees,
                labels,
                new_labels,
                body,
                duedate,
                estimate,
                actor,
                device,
                ts,
            } => {
                if title.trim().is_empty() || DocId::parse(&doc).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                if !catalog.projects.contains_key(&project) {
                    return Err(Rejection::InvalidRequest);
                }
                if Priority::parse(&priority).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                for label in &labels {
                    if !catalog.labels.contains_key(label) {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                if duedate == Some(0) || estimate.is_some_and(|e| e > contract::MAX_ESTIMATE) {
                    return Err(Rejection::InvalidRequest);
                }
                let key = issue_key(&doc);
                staging.issue(&key, Op::Create);
                staging.issue(&key, reg("projectid", project.as_bytes().to_vec()));
                staging.issue(&key, reg("title", title.as_bytes().to_vec()));
                staging.issue(&key, reg("status", DEFAULT_STATUS.as_bytes().to_vec()));
                staging.issue(&key, reg("priority", priority.as_bytes().to_vec()));
                staging.issue(&key, reg("createdby", actor.as_bytes().to_vec()));
                staging.issue(&key, reg("createdat", ts.to_string().into_bytes()));
                if body
                    .as_deref()
                    .is_some_and(|body| body.starts_with(contract::DOCUMENT_PREFIX))
                {
                    staging.issue(
                        &key,
                        reg(
                            "document_schema",
                            DOCUMENT_SCHEMA_VERSION.to_string().into_bytes(),
                        ),
                    );
                }
                if let Some(due) = duedate {
                    staging.issue(&key, reg("duedate", due.to_string().into_bytes()));
                }
                if let Some(points) = estimate {
                    staging.issue(&key, reg("estimate", points.to_string().into_bytes()));
                }
                if let Some(body) = body.filter(|b| !b.is_empty()) {
                    staging.issue(
                        &key,
                        Op::TextSplice {
                            path: "description".into(),
                            index: 0,
                            delete: 0,
                            insert: body,
                        },
                    );
                }
                for who in &assignees {
                    staging.issue(
                        &key,
                        Op::SetAdd {
                            path: "assignees".into(),
                            value: who.as_bytes().to_vec(),
                        },
                    );
                }
                let (new_labels, label_ids) = reconcile_new_labels(&catalog, &labels, &new_labels);
                for new_label in &new_labels {
                    staging.catalog(map_set(
                        "labels",
                        new_label.id.clone(),
                        serde_json::to_vec(&serde_json::json!({
                            "name": new_label.name,
                            "color": new_label.color,
                        }))
                        .expect("label json"),
                    ));
                }
                for label in &label_ids {
                    staging.issue(
                        &key,
                        Op::SetAdd {
                            path: "labels".into(),
                            value: label.as_bytes().to_vec(),
                        },
                    );
                }
                // Alias seq + board, in the same atomic transaction.
                let next = catalog.aliases.get(&project).copied().unwrap_or(0) + 1;
                staging.catalog(map_set("aliases", project.clone(), next.to_string()));
                staging.catalog(map_set("seqs", doc.clone(), next.to_string()));
                board_insert_top(&mut staging, &catalog, &project, &doc);
                push_event(&mut staging, ctx, &doc, &event("created", &device, ts));
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueEdit {
                doc,
                title,
                status,
                priority,
                description,
                duedate,
                estimate,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if title.is_none()
                    && status.is_none()
                    && priority.is_none()
                    && description.is_none()
                    && duedate.is_none()
                    && estimate.is_none()
                {
                    return Err(Rejection::InvalidRequest);
                }
                if duedate == Some(Some(0))
                    || estimate
                        .flatten()
                        .is_some_and(|e| e > contract::MAX_ESTIMATE)
                {
                    return Err(Rejection::InvalidRequest);
                }
                if let Some(status) = &status {
                    if catalog.workflow_state(status).is_none() {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                if let Some(priority) = &priority {
                    if Priority::parse(priority).is_none() {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                let key = issue_key(&doc);
                let mut changes = Vec::new();
                if let Some(title) = &title {
                    changes.push(EventChange {
                        f: "title".into(),
                        from: Some(issue.title.clone()),
                        to: Some(title.clone()),
                    });
                    staging.issue(&key, reg("title", title.as_bytes().to_vec()));
                }
                let mut transition_evidence = None;
                if let Some(status) = &status {
                    if *status != issue.status {
                        // The deterministic transition gate: the demand
                        // template stored on the workflow's selected edge, and
                        // the evidence the receipt binds through the demand,
                        // intent and operations digests.
                        let (demand, evidence) =
                            transition_gate(&catalog, &issue.project, &issue.status, status)?;
                        staging.require(demand);
                        transition_evidence = Some(evidence);
                    }
                    changes.push(EventChange {
                        f: "status".into(),
                        from: Some(issue.status.clone()),
                        to: Some(status.clone()),
                    });
                    staging.issue(&key, reg("status", status.as_bytes().to_vec()));
                    let was_done = catalog.status_category(&issue.status) == StatusCategory::Done;
                    let is_done = catalog.status_category(status) == StatusCategory::Done;
                    if is_done && !was_done {
                        board_remove(&mut staging, &catalog, &issue.project, &doc);
                    } else if was_done && !is_done {
                        board_insert_top(&mut staging, &catalog, &issue.project, &doc);
                    }
                }
                if let Some(priority) = &priority {
                    changes.push(EventChange {
                        f: "priority".into(),
                        from: Some(issue.priority.as_str().to_string()),
                        to: Some(priority.clone()),
                    });
                    staging.issue(&key, reg("priority", priority.as_bytes().to_vec()));
                }
                if let Some(description) = &description {
                    if let Some((index, delete, insert)) =
                        text_splice(&issue.description, description)
                    {
                        staging.issue(
                            &key,
                            Op::TextSplice {
                                path: "description".into(),
                                index,
                                delete,
                                insert,
                            },
                        );
                        changes.push(EventChange {
                            f: "description".into(),
                            from: None,
                            to: None,
                        });
                    }
                }
                if let Some(duedate) = duedate {
                    if duedate != issue.duedate {
                        changes.push(EventChange {
                            f: "duedate".into(),
                            from: issue.duedate.map(|d| d.to_string()),
                            to: duedate.map(|d| d.to_string()),
                        });
                        match duedate {
                            Some(due) => {
                                staging.issue(&key, reg("duedate", due.to_string().into_bytes()))
                            }
                            None => staging.issue(
                                &key,
                                Op::RegisterClear {
                                    path: "duedate".into(),
                                },
                            ),
                        }
                    }
                }
                if let Some(estimate) = estimate {
                    if estimate != issue.estimate {
                        changes.push(EventChange {
                            f: "estimate".into(),
                            from: issue.estimate.map(|e| e.to_string()),
                            to: estimate.map(|e| e.to_string()),
                        });
                        match estimate {
                            Some(points) => staging
                                .issue(&key, reg("estimate", points.to_string().into_bytes())),
                            None => staging.issue(
                                &key,
                                Op::RegisterClear {
                                    path: "estimate".into(),
                                },
                            ),
                        }
                    }
                }
                if staging.ops.is_empty() {
                    return Ok(unchanged_effect(Some(doc)));
                }
                let mut ev = event("edited", &device, ts);
                ev.c = changes;
                if let Some(evidence) = &transition_evidence {
                    // The transition evidence rides the durable history event,
                    // inside the operations digest the receipt binds.
                    ev.x = serde_json::to_string(evidence).expect("transition evidence json");
                }
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueTextSplice {
                doc,
                index,
                delete,
                insert,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if delete == 0 && insert.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                if issue.document_schema == DOCUMENT_SCHEMA_VERSION
                    && index < contract::DOCUMENT_PREFIX.chars().count() as u64
                {
                    return Err(Rejection::InvalidRequest);
                }
                staging.issue(
                    &issue_key(&doc),
                    Op::TextSplice {
                        path: "description".into(),
                        index,
                        delete,
                        insert,
                    },
                );
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueDocumentUpgrade {
                doc,
                expected,
                splices,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if issue.document_schema != 0 || issue.description != expected {
                    return Err(Rejection::InvalidRequest);
                }

                let key = issue_key(&doc);
                let mut working: Vec<char> = expected.chars().collect();
                for splice in &splices {
                    if splice.delete == 0 && splice.insert.is_empty() {
                        return Err(Rejection::InvalidRequest);
                    }
                    let start =
                        usize::try_from(splice.index).map_err(|_| Rejection::InvalidRequest)?;
                    let delete =
                        usize::try_from(splice.delete).map_err(|_| Rejection::InvalidRequest)?;
                    let end = start
                        .checked_add(delete)
                        .filter(|end| *end <= working.len())
                        .ok_or(Rejection::InvalidRequest)?;
                    working.splice(start..end, splice.insert.chars());
                    staging.issue(
                        &key,
                        Op::TextSplice {
                            path: "description".into(),
                            index: splice.index,
                            delete: splice.delete,
                            insert: splice.insert.clone(),
                        },
                    );
                }
                if !working
                    .iter()
                    .collect::<String>()
                    .starts_with(contract::DOCUMENT_PREFIX)
                {
                    return Err(Rejection::InvalidRequest);
                }
                staging.issue(
                    &key,
                    reg(
                        "document_schema",
                        DOCUMENT_SCHEMA_VERSION.to_string().into_bytes(),
                    ),
                );
                push_event(
                    &mut staging,
                    ctx,
                    &doc,
                    &event("document_upgraded", &device, ts),
                );
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueTextCheckpoint { doc, device, ts } => {
                issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let mut ev = event("edited", &device, ts);
                ev.c.push(EventChange {
                    f: "description".into(),
                    from: None,
                    to: None,
                });
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueMove {
                doc,
                project,
                pos,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let mut effective = issue.project.clone();
                if let Some(target) = &project {
                    if !catalog.projects.contains_key(target) {
                        return Err(Rejection::InvalidRequest);
                    }
                    if target != &issue.project {
                        staging.issue(
                            &issue_key(&doc),
                            reg("projectid", target.as_bytes().to_vec()),
                        );
                        board_remove(&mut staging, &catalog, &issue.project, &doc);
                        board_insert_top(&mut staging, &catalog, target, &doc);
                        effective = target.clone();
                    }
                }
                match pos {
                    None => {}
                    Some(Pos::Top) => board_insert_top(&mut staging, &catalog, &effective, &doc),
                    Some(Pos::Bottom) => {
                        board_remove(&mut staging, &catalog, &effective, &doc);
                        // Insert computed against the current view minus doc.
                        let len = board_entries(&catalog, &effective)
                            .iter()
                            .filter(|(_, d)| d != &doc)
                            .count();
                        staging.catalog(Op::ListInsert {
                            path: board_path(&effective),
                            index: len as u64,
                            value: doc.as_bytes().to_vec(),
                        });
                    }
                    Some(Pos::Before { doc: anchor }) => {
                        board_move(&mut staging, &catalog, &effective, &doc, &anchor, false)
                    }
                    Some(Pos::After { doc: anchor }) => {
                        board_move(&mut staging, &catalog, &effective, &doc, &anchor, true)
                    }
                }
                if staging.ops.is_empty() {
                    return Ok(unchanged_effect(Some(doc)));
                }
                push_event(&mut staging, ctx, &doc, &event("moved", &device, ts));
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Assign {
                doc,
                who,
                add,
                device,
                ts,
            } => {
                let _issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let key = issue_key(&doc);
                for actor in &who {
                    if ActorId::parse(actor).is_none() {
                        return Err(Rejection::InvalidRequest);
                    }
                    let op = if add {
                        Op::SetAdd {
                            path: "assignees".into(),
                            value: actor.as_bytes().to_vec(),
                        }
                    } else {
                        Op::SetRemove {
                            path: "assignees".into(),
                            value: actor.as_bytes().to_vec(),
                        }
                    };
                    staging.issue(&key, op);
                }
                let mut ev = event(if add { "assigned" } else { "unassigned" }, &device, ts);
                ev.c = who
                    .iter()
                    .map(|w| EventChange {
                        f: "assignees".into(),
                        from: (!add).then(|| w.clone()),
                        to: add.then(|| w.clone()),
                    })
                    .collect();
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Label {
                doc,
                add,
                new_labels,
                remove,
                device,
                ts,
            } => {
                let _issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                for label in &add {
                    if !catalog.labels.contains_key(label) {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                for label in &remove {
                    if !catalog.labels.contains_key(label) {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                let key = issue_key(&doc);
                let (new_labels, label_ids) = reconcile_new_labels(&catalog, &add, &new_labels);
                for new_label in &new_labels {
                    staging.catalog(map_set(
                        "labels",
                        new_label.id.clone(),
                        serde_json::to_vec(&serde_json::json!({
                            "name": new_label.name,
                            "color": new_label.color,
                        }))
                        .expect("label json"),
                    ));
                }
                for label in &label_ids {
                    staging.issue(
                        &key,
                        Op::SetAdd {
                            path: "labels".into(),
                            value: label.as_bytes().to_vec(),
                        },
                    );
                }
                for label in &remove {
                    staging.issue(
                        &key,
                        Op::SetRemove {
                            path: "labels".into(),
                            value: label.as_bytes().to_vec(),
                        },
                    );
                }
                push_event(&mut staging, ctx, &doc, &event("labeled", &device, ts));
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Comment {
                doc,
                body,
                id,
                parent,
                actor,
                device,
                ts,
            } => {
                let (_issue, parent_node) =
                    check_comment(ctx, &doc, &body, &actor, id.as_deref(), parent.as_deref())?;
                stage_comment(
                    &mut staging,
                    ctx,
                    &doc,
                    parent_node,
                    StoredComment {
                        a: actor,
                        t: ts,
                        b: body,
                        id,
                        parent,
                        at: None,
                        node: None,
                        parent_node: None,
                    },
                    &device,
                    ts,
                );
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::CommentAt {
                doc,
                body,
                field,
                start,
                end,
                id,
                parent,
                actor,
                device,
                ts,
            } => {
                let (issue, parent_node) =
                    check_comment(ctx, &doc, &body, &actor, Some(&id), parent.as_deref())?;
                let at = mint_comment_anchor(ctx, &doc, &issue, &field, start, end)?;
                stage_comment(
                    &mut staging,
                    ctx,
                    &doc,
                    parent_node,
                    StoredComment {
                        a: actor,
                        t: ts,
                        b: body,
                        id: Some(id),
                        parent,
                        at: Some(at),
                        node: None,
                        parent_node: None,
                    },
                    &device,
                    ts,
                );
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::React {
                doc,
                comment,
                emoji,
                actor,
                on,
                device: _,
                ts: _,
            } => {
                if ActorId::parse(&actor).is_none()
                    || !contract::is_comment_id(&comment)
                    || !contract::is_reaction_emoji(&emoji)
                {
                    return Err(Rejection::InvalidRequest);
                }
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if !issue
                    .comments
                    .iter()
                    .any(|c| c.id.as_deref() == Some(comment.as_str()))
                {
                    return Err(Rejection::InvalidRequest);
                }
                let value = contract::reaction_value(&comment, &emoji, &actor);
                let path = contract::REACTIONS_PATH.to_string();
                // Un-reacting has to reach the old home too. A reaction added
                // before the sets were collapsed lives in `reactions/<comment>`,
                // and removing only from the new set would leave it standing —
                // the button would report the reaction gone and the next read
                // would bring it back. Both removes in one atomic batch; the
                // one with nothing to remove is a no-op, not an error.
                if on {
                    staging.issue(&issue_key(&doc), Op::SetAdd { path, value });
                } else {
                    staging.issue(
                        &issue_key(&doc),
                        Op::SetRemove {
                            path,
                            value: value.clone(),
                        },
                    );
                    staging.issue(
                        &issue_key(&doc),
                        Op::SetRemove {
                            path: contract::reaction_path(&comment),
                            value: contract::reaction_value_legacy(&emoji, &actor),
                        },
                    );
                }
                // No history event, deliberately — see the intent's contract
                // note: a reaction is a social signal, not a change of record.
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::SetTombstone {
                doc,
                on,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                staging.catalog(map_set(
                    "tombstones",
                    doc.clone(),
                    if on { "1" } else { "0" },
                ));
                if on {
                    board_remove(&mut staging, &catalog, &issue.project, &doc);
                } else {
                    board_insert_top(&mut staging, &catalog, &issue.project, &doc);
                }
                push_event(
                    &mut staging,
                    ctx,
                    &doc,
                    &event(if on { "deleted" } else { "restored" }, &device, ts),
                );
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Link {
                doc,
                kind,
                target,
                add,
                device,
                ts,
            } => {
                let kind = kind.to_ascii_lowercase();
                if !LINK_KINDS.contains(&kind.as_str()) || doc == target {
                    return Err(Rejection::InvalidRequest);
                }
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let other = issue_state(ctx, &target).ok_or(Rejection::InvalidRequest)?;
                // `relates` is symmetric: canonicalize by sorted endpoints.
                let (from, to) = if kind == "relates" && target < doc {
                    (target.clone(), doc.clone())
                } else {
                    (doc.clone(), target.clone())
                };
                let edge = format!("{from}|{kind}|{to}");
                if !add {
                    if !catalog
                        .edges
                        .contains(&(from.clone(), kind.clone(), to.clone()))
                    {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                let relation_project = if from == doc {
                    &issue.project
                } else {
                    &other.project
                };
                let relation_key = contract::relation_key(relation_project);
                staging.relation(
                    relation_project,
                    ctx.body_version(&relation_key).is_none(),
                    map_set("edges", edge, if add { "1" } else { "0" }),
                );
                let mut ev = event(if add { "linked" } else { "unlinked" }, &device, ts);
                ev.x = format!("{kind} {target}");
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Parent {
                doc,
                parent,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if let Some(parent) = &parent {
                    if parent == &doc {
                        return Err(Rejection::Conflict);
                    }
                    let parent_issue = issue_state(ctx, parent).ok_or(Rejection::InvalidRequest)?;
                    if parent_issue.project != issue.project {
                        return Err(Rejection::InvalidRequest);
                    }
                    // Kept as a fast, local refusal so the caller gets
                    // `Conflict` for the obvious case rather than a commit
                    // failure. It is no longer the guarantee: this walks the
                    // catalog as THIS replica has it, so two peers parenting
                    // A under B and B under A concurrently both passed it and
                    // the merge held a cycle. The hierarchy is now a tree, and
                    // the engine refuses the cycle on whichever replica applies
                    // the second move — including one that arrives by sync.
                    if is_ancestor(&catalog, parent, &doc) {
                        return Err(Rejection::Conflict);
                    }
                }
                let relation_key = contract::relation_key(&issue.project);
                staging.relation(
                    &issue.project,
                    ctx.body_version(&relation_key).is_none(),
                    Op::TreeAnchor {
                        path: contract::HIERARCHY_PATH.into(),
                        anchor: doc.clone(),
                        parent: parent.clone(),
                    },
                );
                let mut ev = event("parented", &device, ts);
                ev.x = parent.unwrap_or_else(|| "unparented".into());
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::WorkState {
                doc,
                action,
                actor,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if ActorId::parse(&actor).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                let (category, kind) = match action {
                    WorkAction::Start => (StatusCategory::Active, "started"),
                    WorkAction::Done => (StatusCategory::Done, "finished"),
                    WorkAction::Stop => (StatusCategory::Backlog, "stopped"),
                };
                let target = catalog
                    .first_state_in(category)
                    .ok_or(Rejection::Conflict)?
                    .clone();
                let key = issue_key(&doc);
                let mut changes = Vec::new();
                let mut transition_evidence = None;
                if issue.status != target.id {
                    // The category target's resulting edge must exist in the
                    // project's workflow revision and authorize.
                    let (demand, evidence) =
                        transition_gate(&catalog, &issue.project, &issue.status, &target.id)?;
                    staging.require(demand);
                    transition_evidence = Some(evidence);
                    changes.push(EventChange {
                        f: "status".into(),
                        from: Some(issue.status.clone()),
                        to: Some(target.id.clone()),
                    });
                    staging.issue(&key, reg("status", target.id.as_bytes().to_vec()));
                    let was_done = catalog.status_category(&issue.status) == StatusCategory::Done;
                    let is_done = category == StatusCategory::Done;
                    if is_done && !was_done {
                        board_remove(&mut staging, &catalog, &issue.project, &doc);
                    } else if was_done && !is_done {
                        board_insert_top(&mut staging, &catalog, &issue.project, &doc);
                    }
                }
                let me = ActorId::parse(&actor).expect("validated above");
                let assigned = issue.assignees.contains(&me);
                match action {
                    WorkAction::Start if !assigned => {
                        changes.push(EventChange {
                            f: "assignees".into(),
                            from: None,
                            to: Some("@me".into()),
                        });
                        staging.issue(
                            &key,
                            Op::SetAdd {
                                path: "assignees".into(),
                                value: actor.as_bytes().to_vec(),
                            },
                        );
                    }
                    WorkAction::Stop if assigned => {
                        changes.push(EventChange {
                            f: "assignees".into(),
                            from: Some("@me".into()),
                            to: None,
                        });
                        staging.issue(
                            &key,
                            Op::SetRemove {
                                path: "assignees".into(),
                                value: actor.as_bytes().to_vec(),
                            },
                        );
                    }
                    _ => {}
                }
                if staging.ops.is_empty() {
                    // The idempotent no-op: nothing committed, nothing rung.
                    return Ok(unchanged_effect(Some(doc)));
                }
                let mut ev = event(kind, &device, ts);
                ev.c = changes;
                if let Some(evidence) = &transition_evidence {
                    ev.x = serde_json::to_string(evidence).expect("transition evidence json");
                }
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::ProjectNew {
                id,
                name,
                key,
                color,
                device: _,
                ts: _,
            } => {
                let key = key.trim().to_ascii_uppercase();
                if name.trim().is_empty()
                    || key.is_empty()
                    || key.len() > 8
                    || !key.bytes().all(|b| b.is_ascii_alphabetic())
                {
                    return Err(Rejection::InvalidRequest);
                }
                if catalog.projects.values().any(|p| p.key == key) {
                    return Err(Rejection::Conflict);
                }
                staging.catalog(map_set(
                    "projects",
                    id.clone(),
                    serde_json::to_vec(&serde_json::json!({
                        "name": name.trim(),
                        "key": key,
                        "color": color,
                    }))
                    .expect("project json"),
                ));
                // Every project carries a workflow revision from birth: the
                // deterministic default (free movement, every edge an explicit
                // replaceable gate).
                let revision = crate::workflow::default_workflow_revision(&id);
                staging.catalog(map_set(
                    "workflow_revisions",
                    format!("{id}/{}", revision.revision_id),
                    serde_json::to_vec(&revision).expect("workflow revision json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::LabelNew {
                id,
                name,
                color,
                device: _,
                ts: _,
            } => {
                if name.trim().is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                if catalog
                    .labels
                    .values()
                    .any(|l| l.name.eq_ignore_ascii_case(&name))
                {
                    return Err(Rejection::Conflict);
                }
                staging.catalog(map_set(
                    "labels",
                    id,
                    serde_json::to_vec(&serde_json::json!({
                        "name": name,
                        "color": color,
                    }))
                    .expect("label json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::ProjectEdit {
                id,
                name,
                color,
                description,
                lead,
                start_date,
                target_date,
                archived,
                team,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_space_any("project.configure"));
                let current = catalog.projects.get(&id).ok_or(Rejection::InvalidRequest)?;
                let mut meta = current.clone();
                if let Some(name) = name {
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        return Err(Rejection::InvalidRequest);
                    }
                    // No name-uniqueness guard: projects are unique on KEY, not
                    // name (which stays immutable here), so two may share a name.
                    meta.name = name;
                }
                if let Some(color) = color {
                    meta.color = color;
                }
                if let Some(description) = description {
                    meta.description = description;
                }
                if let Some(lead) = lead {
                    meta.lead = lead;
                }
                if let Some(start) = start_date {
                    meta.start_date = start;
                }
                if let Some(target) = target_date {
                    meta.target_date = target;
                }
                if let Some(archived) = archived {
                    meta.archived = archived;
                }
                if let Some(team) = team {
                    // Empty clears; a set names a live team.
                    if !team.is_empty() && !catalog.teams.get(&team).is_some_and(|t| !t.tombstone) {
                        return Err(Rejection::InvalidRequest);
                    }
                    meta.team = team;
                }
                // Nothing changed: don't emit an op that would look like an edit.
                if meta == *current {
                    return Ok(staging.into_effect(None));
                }
                // Serialize the whole record so an edit never drops a field the
                // caller didn't touch.
                staging.catalog(map_set(
                    "projects",
                    id.clone(),
                    serde_json::to_vec(&meta).expect("project json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::ProjectUpdatePost {
                project_id,
                id,
                author,
                body,
                health,
                device: _,
                ts,
            } => {
                staging.require(contract::demand_space_any("project.configure"));
                if !catalog.projects.contains_key(&project_id) {
                    return Err(Rejection::InvalidRequest);
                }
                let body = body.trim();
                if body.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                let update = crate::views::ProjectUpdate {
                    id: id.clone(),
                    project_id: project_id.clone(),
                    author,
                    ts,
                    body: body.to_string(),
                    health,
                };
                staging.catalog(map_set(
                    "project_updates",
                    format!("{project_id}/{id}"),
                    serde_json::to_vec(&update).expect("project update json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::LabelEdit {
                id,
                name,
                color,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_space_any("catalog.label.configure"));
                let current = catalog.labels.get(&id).ok_or(Rejection::InvalidRequest)?;
                let mut meta = current.clone();
                if let Some(name) = name {
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        return Err(Rejection::InvalidRequest);
                    }
                    // Case-insensitive uniqueness against the OTHER labels — the
                    // same guard `LabelNew` applies, minus this label itself.
                    if catalog
                        .labels
                        .iter()
                        .any(|(lid, l)| lid != &id && l.name.eq_ignore_ascii_case(&name))
                    {
                        return Err(Rejection::Conflict);
                    }
                    meta.name = name;
                }
                if let Some(color) = color {
                    meta.color = color;
                }
                if meta == *current {
                    return Ok(staging.into_effect(None));
                }
                staging.catalog(map_set(
                    "labels",
                    id.clone(),
                    serde_json::to_vec(&serde_json::json!({
                        "name": meta.name,
                        "color": meta.color,
                    }))
                    .expect("label json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::LabelDelete {
                id,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_space_any("catalog.label.configure"));
                if !catalog.labels.contains_key(&id) {
                    return Err(Rejection::InvalidRequest);
                }
                staging.catalog(Op::MapRemove {
                    path: "labels".into(),
                    key: id,
                });
                Ok(staging.into_effect(None))
            }
            IssueIntent::SpaceRename {
                name,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_admin());
                let name = name.trim();
                if name.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                if catalog.name == name {
                    return Ok(staging.into_effect(None));
                }
                staging.catalog(reg("name", name.to_string().into_bytes()));
                Ok(staging.into_effect(None))
            }
            IssueIntent::SpaceDescribe {
                description,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_admin());
                // Empty clears; no trim so intentional leading/trailing prose is
                // preserved. LWW on the catalog `description` register.
                if catalog.description == description {
                    return Ok(staging.into_effect(None));
                }
                staging.catalog(reg("description", description.into_bytes()));
                Ok(staging.into_effect(None))
            }
            IssueIntent::RoleCreate {
                role_id,
                scope_project,
                name,
                description,
                capabilities,
                device: _,
                ts: _,
            } => {
                // Custom ids only: `role_<ULID>`; built-in ids and free-form
                // ids reject. The daemon mints the id; the World re-validates.
                if !role_id.starts_with("role_")
                    || role_id.len() > 64
                    || crate::roles::built_in(&role_id).is_some()
                {
                    return Err(Rejection::InvalidRequest);
                }
                if catalog.roles.contains_key(&role_id)
                    || catalog.role_revisions.contains_key(&role_id)
                {
                    return Err(Rejection::Conflict);
                }
                let scope_kind = match &scope_project {
                    None => crate::roles::ScopeKind::Space,
                    Some(project) => {
                        if !catalog.projects.contains_key(project) {
                            return Err(Rejection::InvalidRequest);
                        }
                        crate::roles::ScopeKind::Project
                    }
                };
                validate_role_caps(&capabilities, scope_kind)?;
                let body = crate::roles::RoleBody {
                    role_id: role_id.clone(),
                    scope_kind,
                    name,
                    description,
                    capabilities,
                    tombstone: false,
                };
                let revision = crate::roles::build_revision(body, vec![])
                    .map_err(|_| Rejection::InvalidRequest)?;
                stage_role_revision(&mut staging, &revision);
                staging.require(contract::demand_space_any("policy.configure"));
                Ok(staging.into_effect(None))
            }
            IssueIntent::RoleEdit {
                role_id,
                expected_revision,
                name,
                description,
                capabilities,
                device: _,
                ts: _,
            } => {
                if catalog.roles.contains_key(&role_id) {
                    // Built-ins are immutable in every field.
                    return Err(Rejection::InvalidRequest);
                }
                let head = expect_single_head(&catalog, &role_id, &expected_revision)?;
                let mut body = head.body.clone();
                if let Some(name) = name {
                    body.name = name;
                }
                if let Some(description) = description {
                    body.description = description;
                }
                if let Some(capabilities) = capabilities {
                    validate_role_caps(&capabilities, body.scope_kind)?;
                    body.capabilities = capabilities;
                }
                let predecessor = decode_hex32(&expected_revision)?;
                let revision = crate::roles::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                stage_role_revision(&mut staging, &revision);
                staging.require(contract::demand_space_any("policy.configure"));
                Ok(staging.into_effect(None))
            }
            IssueIntent::RoleDelete {
                role_id,
                expected_revision,
                device: _,
                ts: _,
            } => {
                if catalog.roles.contains_key(&role_id) {
                    return Err(Rejection::InvalidRequest);
                }
                let head = expect_single_head(&catalog, &role_id, &expected_revision)?;
                let mut body = head.body.clone();
                body.tombstone = true;
                let predecessor = decode_hex32(&expected_revision)?;
                let revision = crate::roles::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                stage_role_revision(&mut staging, &revision);
                staging.require(contract::demand_space_any("policy.configure"));
                Ok(staging.into_effect(None))
            }
            IssueIntent::RoleResolve {
                role_id,
                expected_heads,
                body_json,
                device: _,
                ts: _,
            } => {
                if catalog.roles.contains_key(&role_id) {
                    return Err(Rejection::InvalidRequest);
                }
                let mut current: Vec<String> = catalog
                    .role_heads(&role_id)
                    .iter()
                    .map(|h| h.revision_id.clone())
                    .collect();
                current.sort();
                let mut expected = expected_heads.clone();
                expected.sort();
                expected.dedup();
                if current.is_empty() || current != expected {
                    return Err(Rejection::Conflict);
                }
                let body: crate::roles::RoleBody =
                    serde_json::from_str(&body_json).map_err(|_| Rejection::InvalidRequest)?;
                if body.role_id != role_id {
                    return Err(Rejection::InvalidRequest);
                }
                validate_role_caps(&body.capabilities, body.scope_kind)?;
                let predecessors: Vec<[u8; 32]> = expected
                    .iter()
                    .map(|h| decode_hex32(h))
                    .collect::<Result<_, _>>()?;
                let revision = crate::roles::build_revision(body, predecessors)
                    .map_err(|_| Rejection::InvalidRequest)?;
                stage_role_revision(&mut staging, &revision);
                staging.require(contract::demand_space_any("policy.configure"));
                Ok(staging.into_effect(None))
            }
            IssueIntent::WorkflowReplace {
                project_id,
                expected_heads,
                body_json,
                device: _,
                ts: _,
            } => {
                if !catalog.projects.contains_key(&project_id) {
                    return Err(Rejection::InvalidRequest);
                }
                let mut current: Vec<String> = catalog
                    .workflow_heads(&project_id)
                    .iter()
                    .map(|h| h.revision_id.clone())
                    .collect();
                current.sort();
                let mut expected = expected_heads.clone();
                expected.sort();
                expected.dedup();
                if current.is_empty() || current != expected {
                    return Err(Rejection::Conflict);
                }
                let body: crate::workflow::WorkflowBody =
                    serde_json::from_str(&body_json).map_err(|_| Rejection::InvalidRequest)?;
                if body.project_id != project_id {
                    return Err(Rejection::InvalidRequest);
                }
                let predecessors: Vec<[u8; 32]> = expected
                    .iter()
                    .map(|h| decode_hex32(h))
                    .collect::<Result<_, _>>()?;
                let revision = crate::workflow::build_revision(body, predecessors)
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.catalog(map_set(
                    "workflow_revisions",
                    format!("{project_id}/{}", revision.revision_id),
                    serde_json::to_vec(&revision).expect("workflow revision json"),
                ));
                staging.require(contract::demand_space_any("catalog.workflow.configure"));
                Ok(staging.into_effect(None))
            }
            IssueIntent::SpecCreate {
                spec,
                project,
                kind,
                title,
                text,
                links,
                actor,
                device: _,
                ts,
            } => {
                if crate::ids::SpecId::parse(&spec).is_none()
                    || ActorId::parse(&actor).is_none()
                    || !catalog.projects.contains_key(&project)
                    || spec_state(ctx, &spec).is_some()
                {
                    return Err(Rejection::InvalidRequest);
                }
                validate_spec_links(ctx, &links)?;
                let plan = (kind == crate::spec::Kind::Plan)
                    .then(|| crate::spec::PlanData { roots: Vec::new() });
                let revision = crate::spec::build_revision(
                    crate::spec::Body {
                        spec: spec.clone(),
                        project: project.clone(),
                        kind,
                        generation: data_encoding::HEXLOWER.encode(&ctx.manifest_root()),
                        title,
                        text,
                        state: crate::spec::State::Draft,
                        links,
                        plan,
                        author: actor,
                        ts,
                    },
                    vec![],
                )
                .map_err(|_| Rejection::InvalidRequest)?;
                let key = spec_key(&spec);
                staging.spec(&key, Op::Create);
                staging.spec(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Spec revision JSON"),
                    ),
                );
                staging.require(contract::demand_project_work("spec.write", &project));
                Ok(staging.into_effect(Some(spec)))
            }
            IssueIntent::SpecRevise {
                spec,
                expected,
                title,
                text,
                links,
                plan,
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let heads = current.heads();
                if heads.len() != 1 || heads[0].revision != expected {
                    return Err(Rejection::Conflict);
                }
                let head = heads[0];
                let mut body = head.body.clone();
                if let Some(title) = title {
                    body.title = title;
                }
                if let Some(text) = text {
                    body.text = text;
                }
                if let Some(links) = links {
                    validate_spec_links(ctx, &links)?;
                    body.links = links;
                }
                if let Some(plan) = plan {
                    body.plan = plan;
                }
                validate_plan(ctx, &catalog, &body.project, body.plan.as_ref())?;
                body.generation = data_encoding::HEXLOWER.encode(&ctx.manifest_root());
                body.state = crate::spec::State::Draft;
                body.author = actor;
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                let key = spec_key(&spec);
                staging.spec(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Spec revision JSON"),
                    ),
                );
                staging.require(contract::demand_project_work(
                    "spec.write",
                    &head.body.project,
                ));
                Ok(staging.into_effect(Some(spec)))
            }
            IssueIntent::SpecDocumentUpgrade {
                spec,
                expected,
                text,
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() || !text.starts_with(contract::DOCUMENT_PREFIX)
                {
                    return Err(Rejection::InvalidRequest);
                }
                let current = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let heads = current.heads();
                if heads.len() != 1 || heads[0].revision != expected {
                    return Err(Rejection::Conflict);
                }
                let head = heads[0];
                if head.body.text.starts_with(contract::DOCUMENT_PREFIX) {
                    return Err(Rejection::InvalidRequest);
                }
                let mut body = head.body.clone();
                body.text = text;
                body.generation = data_encoding::HEXLOWER.encode(&ctx.manifest_root());
                body.author = actor;
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                let key = spec_key(&spec);
                staging.spec(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Spec revision JSON"),
                    ),
                );
                let demand = if matches!(
                    head.body.state,
                    crate::spec::State::Issued | crate::spec::State::Withdrawn
                ) {
                    contract::demand_project_any("spec.issue", &head.body.project)
                } else {
                    contract::demand_project_work("spec.write", &head.body.project)
                };
                staging.require(demand);
                Ok(staging.into_effect(Some(spec)))
            }
            IssueIntent::SpecState {
                spec,
                expected,
                state,
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() || state == crate::spec::State::Draft {
                    return Err(Rejection::InvalidRequest);
                }
                let current = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let heads = current.heads();
                if heads.len() != 1 || heads[0].revision != expected {
                    return Err(Rejection::Conflict);
                }
                let head = heads[0];
                let valid = match state {
                    crate::spec::State::Review => head.body.state == crate::spec::State::Draft,
                    crate::spec::State::Issued => matches!(
                        head.body.state,
                        crate::spec::State::Draft | crate::spec::State::Review
                    ),
                    crate::spec::State::Withdrawn => {
                        !matches!(current.issued(), crate::spec::Issued::None)
                    }
                    crate::spec::State::Draft => false,
                };
                if !valid {
                    return Err(Rejection::InvalidRequest);
                }
                let mut body = head.body.clone();
                body.state = state;
                body.generation = data_encoding::HEXLOWER.encode(&ctx.manifest_root());
                body.author = actor;
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                let key = spec_key(&spec);
                staging.spec(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Spec revision JSON"),
                    ),
                );
                let capability = if state == crate::spec::State::Review {
                    "spec.write"
                } else {
                    "spec.issue"
                };
                let demand = if state == crate::spec::State::Review {
                    contract::demand_project_work(capability, &head.body.project)
                } else {
                    contract::demand_project_any(capability, &head.body.project)
                };
                staging.require(demand);
                Ok(staging.into_effect(Some(spec)))
            }
            IssueIntent::SpecResolve {
                spec,
                expected_heads,
                body_json,
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let mut heads: Vec<String> = current
                    .heads()
                    .into_iter()
                    .map(|revision| revision.revision.clone())
                    .collect();
                heads.sort();
                let mut expected = expected_heads;
                expected.sort();
                expected.dedup();
                if heads.is_empty() || heads != expected {
                    return Err(Rejection::Conflict);
                }
                let mut body: crate::spec::Body =
                    serde_json::from_str(&body_json).map_err(|_| Rejection::InvalidRequest)?;
                let first = current.revisions.first().ok_or(Rejection::InvalidRequest)?;
                if body.spec != spec
                    || body.project != first.body.project
                    || body.kind != first.body.kind
                {
                    return Err(Rejection::InvalidRequest);
                }
                validate_spec_links(ctx, &body.links)?;
                validate_plan(ctx, &catalog, &body.project, body.plan.as_ref())?;
                body.generation = data_encoding::HEXLOWER.encode(&ctx.manifest_root());
                body.state = crate::spec::State::Draft;
                body.author = actor;
                body.ts = ts;
                let predecessors = expected
                    .iter()
                    .map(|revision| {
                        crate::spec::decode_revision(revision).ok_or(Rejection::InvalidRequest)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let revision = crate::spec::build_revision(body, predecessors)
                    .map_err(|_| Rejection::InvalidRequest)?;
                let key = spec_key(&spec);
                staging.spec(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Spec revision JSON"),
                    ),
                );
                staging.require(contract::demand_project_work(
                    "spec.write",
                    &first.body.project,
                ));
                Ok(staging.into_effect(Some(spec)))
            }
            IssueIntent::SpecObserve {
                observation,
                spec,
                rel,
                target,
                note,
                actor,
                device: _,
                ts,
            } => {
                if crate::ids::ObservationId::parse(&observation).is_none()
                    || ActorId::parse(&actor).is_none()
                {
                    return Err(Rejection::InvalidRequest);
                }
                let current = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let first = current.revisions.first().ok_or(Rejection::InvalidRequest)?;
                // The same existence check a Link's target gets. A note about a
                // document nobody here holds is a note nobody can follow, and
                // the reader would have no way to tell it from a typo.
                validate_spec_links(
                    ctx,
                    std::slice::from_ref(&crate::spec::Link {
                        rel,
                        target: target.clone(),
                    }),
                )?;
                let entry = crate::spec::Observation {
                    observation,
                    spec: spec.clone(),
                    observer: actor,
                    ts,
                    rel,
                    target,
                    note,
                };
                entry.validate().map_err(|_| Rejection::InvalidRequest)?;
                staging.spec(
                    &spec_key(&spec),
                    Op::SetAdd {
                        path: "observations".into(),
                        value: serde_json::to_vec(&entry).expect("Observation JSON"),
                    },
                );
                // Ordinary contributor standing. Noticing that two documents
                // disagree is not an act of authority over either, and pricing
                // it at the issuing capability would mean the people who read
                // the most specs are the least able to say so.
                staging.require(contract::demand_project_work(
                    "spec.write",
                    &first.body.project,
                ));
                Ok(staging.into_effect(Some(spec)))
            }
            IssueIntent::SpecRetract {
                spec,
                observation,
                actor,
                device: _,
                ts: _,
            } => {
                if ActorId::parse(&actor).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let first = current.revisions.first().ok_or(Rejection::InvalidRequest)?;
                let entry = current
                    .observation(&observation)
                    .ok_or(Rejection::InvalidRequest)?;
                // `SetRemove` matches the exact member, so the retraction has to
                // name the bytes that were added — re-encoding the entry we just
                // read is what makes that exact rather than approximate.
                let value = serde_json::to_vec(entry).expect("Observation JSON");
                let project = first.body.project.clone();
                let own = entry.observer == actor;
                staging.spec(
                    &spec_key(&spec),
                    Op::SetRemove {
                        path: "observations".into(),
                        value,
                    },
                );
                // Taking your own note back is part of writing it. Removing
                // somebody else's is a judgement about the record, which is the
                // same authority that decides what governs.
                staging.require(if own {
                    contract::demand_project_work("spec.write", &project)
                } else {
                    contract::demand_project_any("spec.issue", &project)
                });
                Ok(staging.into_effect(Some(spec)))
            }
            IssueIntent::BaselineCreate {
                baseline,
                project,
                name,
                members,
                actor,
                device: _,
                ts,
            } => {
                if crate::ids::BaselineId::parse(&baseline).is_none()
                    || ActorId::parse(&actor).is_none()
                    || !catalog.projects.contains_key(&project)
                    || baseline_state(ctx, &baseline).is_some()
                {
                    return Err(Rejection::InvalidRequest);
                }
                for member in &members {
                    validate_spec_ref(ctx, member, &project)?;
                }
                let revision = crate::spec::build_baseline_revision(
                    crate::spec::BaselineBody {
                        baseline: baseline.clone(),
                        project: project.clone(),
                        name,
                        state: crate::spec::State::Draft,
                        members,
                        author: actor,
                        ts,
                    },
                    vec![],
                )
                .map_err(|_| Rejection::InvalidRequest)?;
                let key = baseline_key(&baseline);
                staging.baseline(&key, Op::Create);
                staging.baseline(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Baseline revision JSON"),
                    ),
                );
                staging.require(contract::demand_project_work("baseline.write", &project));
                Ok(staging.into_effect(Some(baseline)))
            }
            IssueIntent::BaselineRevise {
                baseline,
                expected,
                name,
                members,
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = baseline_state(ctx, &baseline).ok_or(Rejection::InvalidRequest)?;
                let heads = current.heads();
                if heads.len() != 1 || heads[0].revision != expected {
                    return Err(Rejection::Conflict);
                }
                let head = heads[0];
                let mut body = head.body.clone();
                if let Some(name) = name {
                    body.name = name;
                }
                if let Some(members) = members {
                    for member in &members {
                        validate_spec_ref(ctx, member, &body.project)?;
                    }
                    body.members = members;
                }
                body.state = crate::spec::State::Draft;
                body.author = actor;
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_baseline_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                let key = baseline_key(&baseline);
                staging.baseline(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Baseline revision JSON"),
                    ),
                );
                staging.require(contract::demand_project_work(
                    "baseline.write",
                    &head.body.project,
                ));
                Ok(staging.into_effect(Some(baseline)))
            }
            IssueIntent::BaselineState {
                baseline,
                expected,
                state,
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() || state == crate::spec::State::Draft {
                    return Err(Rejection::InvalidRequest);
                }
                let current = baseline_state(ctx, &baseline).ok_or(Rejection::InvalidRequest)?;
                let heads = current.heads();
                if heads.len() != 1 || heads[0].revision != expected {
                    return Err(Rejection::Conflict);
                }
                let head = heads[0];
                let valid = match state {
                    crate::spec::State::Review => head.body.state == crate::spec::State::Draft,
                    crate::spec::State::Issued => matches!(
                        head.body.state,
                        crate::spec::State::Draft | crate::spec::State::Review
                    ),
                    crate::spec::State::Withdrawn => {
                        !matches!(current.issued(), crate::spec::BaselineIssued::None)
                    }
                    crate::spec::State::Draft => false,
                };
                if !valid {
                    return Err(Rejection::InvalidRequest);
                }
                if state == crate::spec::State::Issued {
                    for member in &head.body.members {
                        validate_spec_ref(ctx, member, &head.body.project)?;
                    }
                }
                let mut body = head.body.clone();
                body.state = state;
                body.author = actor;
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_baseline_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                let key = baseline_key(&baseline);
                staging.baseline(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Baseline revision JSON"),
                    ),
                );
                let demand = if state == crate::spec::State::Review {
                    contract::demand_project_work("baseline.write", &head.body.project)
                } else {
                    contract::demand_project_any("baseline.issue", &head.body.project)
                };
                staging.require(demand);
                Ok(staging.into_effect(Some(baseline)))
            }
            IssueIntent::BaselineResolve {
                baseline,
                expected_heads,
                body_json,
                actor,
                device: _,
                ts,
            } => {
                if ActorId::parse(&actor).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = baseline_state(ctx, &baseline).ok_or(Rejection::InvalidRequest)?;
                let mut heads: Vec<String> = current
                    .heads()
                    .into_iter()
                    .map(|revision| revision.revision.clone())
                    .collect();
                heads.sort();
                let mut expected = expected_heads;
                expected.sort();
                expected.dedup();
                if heads.is_empty() || heads != expected {
                    return Err(Rejection::Conflict);
                }
                let mut body: crate::spec::BaselineBody =
                    serde_json::from_str(&body_json).map_err(|_| Rejection::InvalidRequest)?;
                let first = current.revisions.first().ok_or(Rejection::InvalidRequest)?;
                if body.baseline != baseline || body.project != first.body.project {
                    return Err(Rejection::InvalidRequest);
                }
                for member in &body.members {
                    validate_spec_ref(ctx, member, &body.project)?;
                }
                body.state = crate::spec::State::Draft;
                body.author = actor;
                body.ts = ts;
                let predecessors = expected
                    .iter()
                    .map(|revision| {
                        crate::spec::decode_revision(revision).ok_or(Rejection::InvalidRequest)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let revision = crate::spec::build_baseline_revision(body, predecessors)
                    .map_err(|_| Rejection::InvalidRequest)?;
                let key = baseline_key(&baseline);
                staging.baseline(
                    &key,
                    map_set(
                        "revisions",
                        revision.revision.clone(),
                        serde_json::to_vec(&revision).expect("Baseline revision JSON"),
                    ),
                );
                staging.require(contract::demand_project_work(
                    "baseline.write",
                    &first.body.project,
                ));
                Ok(staging.into_effect(Some(baseline)))
            }
            IssueIntent::IssueBaseline {
                doc,
                baseline,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if let Some(binding) = &baseline {
                    let baseline_state =
                        baseline_state(ctx, &binding.baseline).ok_or(Rejection::InvalidRequest)?;
                    let revision = baseline_state
                        .revision(&binding.revision)
                        .ok_or(Rejection::InvalidRequest)?;
                    if revision.body.project != issue.project
                        || revision.body.state != crate::spec::State::Issued
                    {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                staging.issue(
                    &issue_key(&doc),
                    reg(
                        "baseline",
                        baseline
                            .as_ref()
                            .map(|binding| {
                                serde_json::to_vec(binding).expect("Baseline binding JSON")
                            })
                            .unwrap_or_default(),
                    ),
                );
                staging.require(contract::demand_project_any("issue.bind", &issue.project));
                let mut event = event("baseline", &device, ts);
                event.x = baseline
                    .map(|binding| format!("{}@{}", binding.baseline, binding.revision))
                    .unwrap_or_default();
                push_event(&mut staging, ctx, &doc, &event);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::ProjectDelete {
                id,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_project_any("project.delete", &id));
                if !catalog.projects.contains_key(&id) {
                    return Err(Rejection::InvalidRequest);
                }
                // The safe v1 (CUSTOM-10): a project still referenced by ANY
                // issue — live or tombstoned — refuses. Every doc's alias keys
                // off its project; deleting under one would orphan it
                // silently. Reassign (`issue move`) or archive instead.
                let referenced = ctx
                    .bodies_with_schema(&contract::world_id(), &contract::issue_schema())
                    .iter()
                    .filter_map(|key| ctx.read_collaborative(key).ok())
                    .any(|view| IssueState::from_view(&view).project == id);
                if referenced {
                    return Err(Rejection::Conflict);
                }
                let map_remove = |path: &str, key: String| Op::MapRemove {
                    path: path.into(),
                    key,
                };
                staging.catalog(map_remove("projects", id.clone()));
                if catalog.aliases.contains_key(&id) {
                    staging.catalog(map_remove("aliases", id.clone()));
                }
                for rev in catalog.workflow_revisions.get(&id).into_iter().flatten() {
                    staging.catalog(map_remove(
                        "workflow_revisions",
                        format!("{id}/{}", rev.revision_id),
                    ));
                }
                for update in catalog.project_updates.get(&id).into_iter().flatten() {
                    staging.catalog(map_remove("project_updates", format!("{id}/{}", update.id)));
                }
                for mid in catalog
                    .milestones
                    .get(&id)
                    .into_iter()
                    .flat_map(|m| m.keys())
                {
                    staging.catalog(map_remove("project_milestones", format!("{id}/{mid}")));
                }
                for cid in catalog.cycles.get(&id).into_iter().flat_map(|c| c.keys()) {
                    staging.catalog(map_remove("cycles", format!("{id}/{cid}")));
                }
                // Initiatives referencing the project drop it from their
                // member list in the same transaction.
                for (iid, initiative) in &catalog.initiatives {
                    if initiative.projects.contains(&id) {
                        let mut updated = initiative.clone();
                        updated.projects.retain(|p| p != &id);
                        staging.catalog(map_set(
                            "initiatives",
                            iid.clone(),
                            serde_json::to_vec(&updated).expect("initiative json"),
                        ));
                    }
                }
                Ok(staging.into_effect(None))
            }
            IssueIntent::Follow {
                doc,
                actor,
                on,
                device: _,
                ts: _,
            } => {
                if ActorId::parse(&actor).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                let _issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let value = actor.into_bytes();
                staging.issue(
                    &issue_key(&doc),
                    if on {
                        Op::SetAdd {
                            path: "followers".into(),
                            value,
                        }
                    } else {
                        Op::SetRemove {
                            path: "followers".into(),
                            value,
                        }
                    },
                );
                // No history event, like `React` — following is a personal
                // signal, not a change of record.
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::MilestoneSet {
                project_id,
                id,
                name,
                description,
                target_date,
                pos,
                tombstone,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_space_any("project.configure"));
                if !catalog.projects.contains_key(&project_id) || id.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }

                // The project's live milestones in the order a reader sees them,
                // and the backfill that makes that order durable.
                //
                // Records written before ranks existed have none, and a list that
                // was half hand-ordered and half date-ordered would have no
                // answer to "where does this one go". So the first milestone
                // write in a project stamps a rank on every one of its
                // milestones, taken from the legacy order they are already being
                // read in — nothing moves, the order just stops being derived.
                let mut ordered: Vec<crate::views::Milestone> = catalog
                    .milestones
                    .get(&project_id)
                    .into_iter()
                    .flat_map(|m| m.values())
                    .filter(|m| !m.tombstone)
                    .cloned()
                    .collect();
                ordered.sort_by(milestone_order);
                let backfilling = ordered.iter().any(|m| m.rank.is_empty());
                if backfilling {
                    let mut previous = String::new();
                    for milestone in ordered.iter_mut() {
                        previous = rank::between(&previous, None);
                        milestone.rank = previous.clone();
                    }
                }

                let current = ordered.iter().find(|m| m.id == id).cloned().or_else(|| {
                    catalog
                        .milestones
                        .get(&project_id)
                        .and_then(|m| m.get(&id))
                        .cloned()
                });
                let mut record = match current.clone() {
                    Some(m) => m,
                    None => {
                        let name = name.clone().unwrap_or_default();
                        if name.trim().is_empty() {
                            return Err(Rejection::InvalidRequest);
                        }
                        crate::views::Milestone {
                            id: id.clone(),
                            project_id: project_id.clone(),
                            name: name.trim().to_string(),
                            description: String::new(),
                            target_date: None,
                            // Appended, so a new milestone lands where you can
                            // see it rather than sorted into the middle by a date
                            // you have not set yet.
                            rank: rank::after_all(
                                &ordered.iter().map(|m| m.rank.clone()).collect::<Vec<_>>(),
                            ),
                            tombstone: false,
                        }
                    }
                };
                if let Some(pos) = &pos {
                    record.rank = place(&ordered, &id, pos).ok_or(Rejection::InvalidRequest)?;
                }
                if current.is_some() {
                    if let Some(name) = &name {
                        if name.trim().is_empty() {
                            return Err(Rejection::InvalidRequest);
                        }
                        record.name = name.trim().to_string();
                    }
                }
                if let Some(description) = &description {
                    record.description = description.clone();
                }
                if let Some(target) = target_date {
                    record.target_date = target;
                }
                if let Some(tombstone) = tombstone {
                    record.tombstone = tombstone;
                }
                if current.as_ref() == Some(&record) && !backfilling {
                    return Ok(staging.into_effect(None));
                }
                if backfilling {
                    // Every sibling whose rank we just derived, so the order this
                    // write assumes is the order the next reader gets.
                    for milestone in &ordered {
                        if milestone.id == id {
                            continue;
                        }
                        staging.catalog(map_set(
                            "project_milestones",
                            format!("{project_id}/{}", milestone.id),
                            serde_json::to_vec(milestone).expect("milestone json"),
                        ));
                    }
                }
                staging.catalog(map_set(
                    "project_milestones",
                    format!("{project_id}/{id}"),
                    serde_json::to_vec(&record).expect("milestone json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::IssueMilestone {
                doc,
                milestone,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let label = match &milestone {
                    Some(m) => {
                        let record = catalog
                            .milestones
                            .get(&issue.project)
                            .and_then(|ms| ms.get(m))
                            .filter(|r| !r.tombstone)
                            .ok_or(Rejection::InvalidRequest)?;
                        staging.issue(&issue_key(&doc), reg("milestone", m.as_bytes().to_vec()));
                        record.name.clone()
                    }
                    None => {
                        staging.issue(
                            &issue_key(&doc),
                            Op::RegisterClear {
                                path: "milestone".into(),
                            },
                        );
                        "none".into()
                    }
                };
                if issue.milestone == milestone {
                    return Ok(unchanged_effect(Some(doc)));
                }
                let mut ev = event("milestoned", &device, ts);
                ev.x = label;
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::CycleSet {
                project_id,
                id,
                name,
                start,
                end,
                tombstone,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_space_any("project.configure"));
                if !catalog.projects.contains_key(&project_id) || id.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = catalog
                    .cycles
                    .get(&project_id)
                    .and_then(|c| c.get(&id))
                    .cloned();
                let mut record = match current.clone() {
                    Some(c) => c,
                    None => {
                        let name = name.clone().unwrap_or_default();
                        if name.trim().is_empty() {
                            return Err(Rejection::InvalidRequest);
                        }
                        crate::views::Cycle {
                            id: id.clone(),
                            project_id: project_id.clone(),
                            name: name.trim().to_string(),
                            start: 0,
                            end: 0,
                            tombstone: false,
                        }
                    }
                };
                if current.is_some() {
                    if let Some(name) = &name {
                        if name.trim().is_empty() {
                            return Err(Rejection::InvalidRequest);
                        }
                        record.name = name.trim().to_string();
                    }
                }
                if let Some(start) = start {
                    record.start = start.unwrap_or(0);
                }
                if let Some(end) = end {
                    record.end = end.unwrap_or(0);
                }
                if record.start != 0 && record.end != 0 && record.end < record.start {
                    return Err(Rejection::InvalidRequest);
                }
                if let Some(tombstone) = tombstone {
                    record.tombstone = tombstone;
                }
                if current.as_ref() == Some(&record) {
                    return Ok(staging.into_effect(None));
                }
                staging.catalog(map_set(
                    "cycles",
                    format!("{project_id}/{id}"),
                    serde_json::to_vec(&record).expect("cycle json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::IssueCycle {
                doc,
                cycle,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let label = match &cycle {
                    Some(c) => {
                        let record = catalog
                            .cycles
                            .get(&issue.project)
                            .and_then(|cs| cs.get(c))
                            .filter(|r| !r.tombstone)
                            .ok_or(Rejection::InvalidRequest)?;
                        staging.issue(&issue_key(&doc), reg("cycle", c.as_bytes().to_vec()));
                        record.name.clone()
                    }
                    None => {
                        staging.issue(
                            &issue_key(&doc),
                            Op::RegisterClear {
                                path: "cycle".into(),
                            },
                        );
                        "none".into()
                    }
                };
                if issue.cycle == cycle {
                    return Ok(unchanged_effect(Some(doc)));
                }
                let mut ev = event("cycled", &device, ts);
                ev.x = label;
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::InitiativeSet {
                id,
                name,
                description,
                owner,
                health,
                target_date,
                projects,
                tombstone,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_space_any("project.create"));
                if id.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = catalog.initiatives.get(&id).cloned();
                let mut record = match current.clone() {
                    Some(i) => i,
                    None => {
                        let name = name.clone().unwrap_or_default();
                        if name.trim().is_empty() {
                            return Err(Rejection::InvalidRequest);
                        }
                        crate::views::Initiative {
                            id: id.clone(),
                            name: name.trim().to_string(),
                            ..Default::default()
                        }
                    }
                };
                if current.is_some() {
                    if let Some(name) = &name {
                        if name.trim().is_empty() {
                            return Err(Rejection::InvalidRequest);
                        }
                        record.name = name.trim().to_string();
                    }
                }
                if let Some(description) = description {
                    record.description = description;
                }
                if let Some(owner) = owner {
                    if !owner.is_empty() && ActorId::parse(&owner).is_none() {
                        return Err(Rejection::InvalidRequest);
                    }
                    record.owner = owner;
                }
                if let Some(health) = health {
                    if !health.is_empty() && !contract::HEALTH_LABELS.contains(&health.as_str()) {
                        return Err(Rejection::InvalidRequest);
                    }
                    record.health = health;
                }
                if let Some(target) = target_date {
                    record.target_date = target;
                }
                if let Some(projects) = projects {
                    for project in &projects {
                        if !catalog.projects.contains_key(project) {
                            return Err(Rejection::InvalidRequest);
                        }
                    }
                    record.projects = projects;
                }
                if let Some(tombstone) = tombstone {
                    record.tombstone = tombstone;
                }
                if current.as_ref() == Some(&record) {
                    return Ok(staging.into_effect(None));
                }
                staging.catalog(map_set(
                    "initiatives",
                    id.clone(),
                    serde_json::to_vec(&record).expect("initiative json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::TeamSet {
                id,
                name,
                key,
                icon,
                lead,
                members,
                tombstone,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_admin());
                if id.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                let current = catalog.teams.get(&id).cloned();
                let mut record = match current.clone() {
                    Some(t) => t,
                    None => {
                        let name = name.clone().unwrap_or_default();
                        let key = key.clone().unwrap_or_default().to_ascii_uppercase();
                        if name.trim().is_empty()
                            || key.is_empty()
                            || key.len() > 8
                            || !key.bytes().all(|b| b.is_ascii_alphabetic())
                        {
                            return Err(Rejection::InvalidRequest);
                        }
                        if catalog.teams.values().any(|t| !t.tombstone && t.key == key) {
                            return Err(Rejection::Conflict);
                        }
                        crate::views::Team {
                            id: id.clone(),
                            name: name.trim().to_string(),
                            key,
                            ..Default::default()
                        }
                    }
                };
                if current.is_some() {
                    // The key binds at creation, like a project key.
                    if key.is_some_and(|k| k.to_ascii_uppercase() != record.key) {
                        return Err(Rejection::InvalidRequest);
                    }
                    if let Some(name) = &name {
                        if name.trim().is_empty() {
                            return Err(Rejection::InvalidRequest);
                        }
                        record.name = name.trim().to_string();
                    }
                }
                if let Some(icon) = icon {
                    record.icon = icon;
                }
                if let Some(lead) = lead {
                    if !lead.is_empty() && ActorId::parse(&lead).is_none() {
                        return Err(Rejection::InvalidRequest);
                    }
                    record.lead = lead;
                }
                if let Some(mut members) = members {
                    for member in &members {
                        if ActorId::parse(member).is_none() {
                            return Err(Rejection::InvalidRequest);
                        }
                    }
                    members.sort();
                    members.dedup();
                    record.members = members;
                }
                if let Some(tombstone) = tombstone {
                    record.tombstone = tombstone;
                }
                if current.as_ref() == Some(&record) {
                    return Ok(staging.into_effect(None));
                }
                staging.catalog(map_set(
                    "teams",
                    id.clone(),
                    serde_json::to_vec(&record).expect("team json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::TriageSubmit {
                id,
                title,
                body,
                source,
                actor,
                device: _,
                ts,
            } => {
                if title.trim().is_empty()
                    || id.is_empty()
                    || ActorId::parse(&actor).is_none()
                    || catalog.triage.contains_key(&id)
                {
                    return Err(Rejection::InvalidRequest);
                }
                let item = crate::views::TriageItem {
                    id: id.clone(),
                    title: title.trim().to_string(),
                    body,
                    source,
                    submitted_by: actor,
                    ts,
                    ..Default::default()
                };
                staging.catalog(map_set(
                    "triage",
                    id,
                    serde_json::to_vec(&item).expect("triage json"),
                ));
                Ok(staging.into_effect(None))
            }
            IssueIntent::TriageDecide {
                id,
                outcome,
                project,
                doc,
                note,
                actor,
                device,
                ts,
            } => {
                staging.require(contract::demand_space_any("project.create"));
                if !contract::TRIAGE_OUTCOMES.contains(&outcome.as_str())
                    || ActorId::parse(&actor).is_none()
                {
                    return Err(Rejection::InvalidRequest);
                }
                let item = catalog.triage.get(&id).ok_or(Rejection::InvalidRequest)?;
                // Decided exactly once.
                if !item.outcome.is_empty() {
                    return Err(Rejection::Conflict);
                }
                let mut decided = item.clone();
                decided.outcome = outcome.clone();
                decided.decided_by = actor.clone();
                decided.decided_ts = ts;
                decided.note = note;
                match outcome.as_str() {
                    "accepted" => {
                        // Atomically create the issue in the same transaction
                        // that stamps the outcome — an accept can never half
                        // happen.
                        let project = project.ok_or(Rejection::InvalidRequest)?;
                        let doc = doc.ok_or(Rejection::InvalidRequest)?;
                        if !catalog.projects.contains_key(&project) || DocId::parse(&doc).is_none()
                        {
                            return Err(Rejection::InvalidRequest);
                        }
                        let key = issue_key(&doc);
                        staging.issue(&key, Op::Create);
                        staging.issue(&key, reg("projectid", project.as_bytes().to_vec()));
                        staging.issue(&key, reg("title", item.title.as_bytes().to_vec()));
                        staging.issue(&key, reg("status", DEFAULT_STATUS.as_bytes().to_vec()));
                        staging.issue(&key, reg("priority", "none".as_bytes().to_vec()));
                        staging.issue(
                            &key,
                            reg("createdby", item.submitted_by.as_bytes().to_vec()),
                        );
                        staging.issue(&key, reg("createdat", ts.to_string().into_bytes()));
                        if !item.body.is_empty() {
                            staging.issue(
                                &key,
                                Op::TextSplice {
                                    path: "description".into(),
                                    index: 0,
                                    delete: 0,
                                    insert: item.body.clone(),
                                },
                            );
                        }
                        let next = catalog.aliases.get(&project).copied().unwrap_or(0) + 1;
                        staging.catalog(map_set("aliases", project.clone(), next.to_string()));
                        staging.catalog(map_set("seqs", doc.clone(), next.to_string()));
                        board_insert_top(&mut staging, &catalog, &project, &doc);
                        push_event(&mut staging, ctx, &doc, &event("created", &device, ts));
                        decided.doc = doc;
                    }
                    "duplicate" => {
                        let doc = doc.ok_or(Rejection::InvalidRequest)?;
                        let _target = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                        decided.doc = doc;
                    }
                    _ => {}
                }
                staging.catalog(map_set(
                    "triage",
                    id,
                    serde_json::to_vec(&decided).expect("triage json"),
                ));
                let doc = (!decided.doc.is_empty() && decided.outcome == "accepted")
                    .then(|| decided.doc.clone());
                Ok(staging.into_effect(doc))
            }
            IssueIntent::Attach {
                doc,
                id,
                name,
                mime,
                content,
                size,
                comment,
                actor,
                device,
                ts,
            } => {
                // The name is refused here rather than repaired, because the
                // party proposing it is a local actor holding write authority
                // who can simply pick another. Repair belongs at the far end,
                // where the proposer is remote and refusing would let them make
                // their own attachment unsaveable.
                let name = name.trim();
                if ActorId::parse(&actor).is_none()
                    || !id.starts_with("att_")
                    || name.is_empty()
                    || name.len() > contract::MAX_ATTACHMENT_NAME_BYTES
                    || name.chars().any(|c| c.is_control())
                {
                    return Err(Rejection::InvalidRequest);
                }
                let Some(content_ref) = parse_content_ref(&content) else {
                    return Err(Rejection::InvalidRequest);
                };
                if size == 0 {
                    return Err(Rejection::InvalidRequest);
                }
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let existing = raw_attachments(ctx, &doc);
                if existing.contains_key(&id) {
                    return Err(Rejection::InvalidRequest);
                }
                // Counted against the raw map rather than the decoded list. A
                // record this build cannot read still occupies a slot, and a cap
                // that skipped it would let a corrupt record raise the ceiling.
                if existing.len() >= contract::MAX_ATTACHMENTS_PER_ISSUE {
                    return Err(Rejection::LimitExceeded);
                }
                if let Some(comment) = &comment {
                    if !issue
                        .comments
                        .iter()
                        .any(|c| c.id.as_deref() == Some(comment.as_str()))
                    {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                let name = name.to_string();
                let record = serde_json::json!({
                    "id": id,
                    "name": name,
                    "mime": mime,
                    "size": size,
                    "by": actor,
                    "ts": ts,
                    "comment": comment.unwrap_or_default(),
                    "content": content,
                });
                let key = issue_key(&doc);
                staging.issue(
                    &key,
                    Op::MapSet {
                        path: "attachments".into(),
                        key: id.clone(),
                        value: serde_json::to_vec(&record).expect("attachment json"),
                    },
                );
                // The Body's whole content set, re-derived over the map this
                // operation is about to change.
                //
                // Whole rather than incremental, because a declaration replaces
                // rather than adds: an entry naming only the new file would
                // silently detach every earlier one. Fail-closed for the same
                // reason the cap counts raw records — a record that does not
                // decode is content this Body still references, and leaving it
                // out of the declaration would make those bytes collectable.
                let mut refs = content_of(&existing)?;
                if !refs.contains(&content_ref) {
                    refs.push(content_ref);
                }
                staging.declare(&key, refs);
                let mut ev = event("attached", &device, ts);
                ev.x = name;
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Detach {
                doc,
                id,
                device,
                ts,
            } => {
                let issue = issue_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let Some(meta) = issue.attachments.iter().find(|a| a.id == id) else {
                    return Err(Rejection::InvalidRequest);
                };
                let name = meta.name.clone();
                staging.issue(
                    &issue_key(&doc),
                    Op::MapRemove {
                        path: "attachments".into(),
                        key: id,
                    },
                );
                let mut ev = event("detached", &device, ts);
                ev.x = name;
                push_event(&mut staging, ctx, &doc, &ev);
                Ok(staging.into_effect(Some(doc)))
            }
        }
    }

    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        let query = IssueQuery::from_json(&query.payload).ok_or(Rejection::InvalidRequest)?;
        // ONE derived read model per Manifest root; every arm below reads the
        // same immutable snapshot (see [`IssuesWorld::derived_snapshot`]).
        let snap = self.derived_snapshot(ctx)?;
        let catalog: &CatalogState = &snap.catalog;
        let aliases: &DerivedAliases = &snap.aliases;
        let projection = |bytes: Vec<u8>| Projection {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            bytes,
            frontier: replica::frontier::ReplicaFrontier::EMPTY, // stamped by Runtime
            demand: contract::demand_read(),
        };
        match query {
            IssueQuery::Snapshot => {
                let value = serde_json::json!({
                    "catalog": catalog,
                    "aliases": {
                        "by_doc": aliases.by_doc,
                        "by_alias": aliases.by_alias,
                        "canonical": aliases.canonical,
                    },
                });
                Ok(projection(serde_json::to_vec(&value).expect("snapshot")))
            }
            IssueQuery::StructureStatus => Ok(projection(
                serde_json::to_vec(&structure_report(ctx, &snap)).expect("structure report JSON"),
            )),
            IssueQuery::View { doc, me } => {
                let me = me.and_then(|m| ActorId::parse(&m));
                let issue = snap.issues.get(&doc);
                let view = match issue {
                    Some(issue) => {
                        // The space id rides in the projection consumer; the
                        // World does not know it — stamp a placeholder the
                        // daemon replaces? No: the daemon supplies it in the
                        // query. Provisional views come from the row path.
                        let resolve = |comment: &StoredComment| {
                            resolve_comment_anchor(ctx, &doc, issue, comment)
                        };
                        issue_view(
                            catalog,
                            aliases,
                            &space_placeholder(),
                            &doc,
                            issue,
                            &resolve,
                        )
                    }
                    None => provisional_view(catalog, aliases, &doc),
                };
                let _ = me;
                Ok(projection(serde_json::to_vec(&view).expect("view json")))
            }
            IssueQuery::List {
                project,
                label,
                status,
                milestone,
                mine,
                all,
                me,
            } => {
                let me = me.and_then(|m| ActorId::parse(&m));
                let mine = mine.and_then(|m| ActorId::parse(&m));
                let mut rows: Vec<(String, Row2)> = Vec::new();
                for (doc, issue) in &snap.issues {
                    if let Some(project) = &project {
                        if &issue.project != project {
                            continue;
                        }
                    } else if catalog
                        .projects
                        .get(&issue.project)
                        .is_some_and(|m| m.archived)
                    {
                        // No explicit project: an archived project's issues stay
                        // out of the all-project list (CUSTOM-9). Opening the
                        // project by ref passes `project` and bypasses this.
                        continue;
                    }
                    let tomb = catalog.tombstones.contains(doc);
                    let done = catalog.status_category(&issue.status) == StatusCategory::Done;
                    if !all && (tomb || done) {
                        continue;
                    }
                    if let Some(status) = &status {
                        if &issue.status != status {
                            continue;
                        }
                    }
                    if let Some(label) = &label {
                        if !issue.labels.contains(label) {
                            continue;
                        }
                    }
                    if let Some(milestone) = &milestone {
                        if issue.milestone.as_deref() != Some(milestone.as_str()) {
                            continue;
                        }
                    }
                    if let Some(mine) = &mine {
                        if !issue.assignees.contains(mine) {
                            continue;
                        }
                    }
                    rows.push((
                        doc.clone(),
                        Row2 {
                            row: project_row(catalog, aliases, doc, Some(issue), me.as_ref()),
                            priority: issue.priority,
                        },
                    ));
                }
                rows.sort_by(|(da, a), (db, b)| {
                    b.priority.cmp(&a.priority).then_with(|| da.cmp(db))
                });
                let rows: Vec<crate::dto::Row> = rows.into_iter().map(|(_, r)| r.row).collect();
                Ok(projection(serde_json::to_vec(&rows).expect("rows json")))
            }
            IssueQuery::Board { project, me } => {
                let me = me.and_then(|m| ActorId::parse(&m));
                let view = board_view(catalog, aliases, &project, &snap.issues, me.as_ref())
                    .ok_or(Rejection::InvalidRequest)?;
                Ok(projection(serde_json::to_vec(&view).expect("board json")))
            }
            IssueQuery::Graph { doc, me } => {
                let me = me.and_then(|m| ActorId::parse(&m));
                let view = graph_view(catalog, aliases, &doc, &snap.issues, me.as_ref());
                Ok(projection(serde_json::to_vec(&view).expect("graph json")))
            }
            IssueQuery::History { doc } => {
                let issue = snap.issues.get(&doc).ok_or(Rejection::InvalidRequest)?;
                let reff = canonical_for(aliases, &doc);
                // The ordinal counts from where the retained history begins, so
                // a trimmed issue's rows keep the numbers they had rather than
                // restarting at one. `events_recorded` is the total ever; the
                // rows in hand are its tail.
                let trimmed = issue
                    .events_recorded
                    .saturating_sub(issue.events.len() as u64);
                let events: Vec<ActivityEvent> = issue
                    .events
                    .iter()
                    .enumerate()
                    .map(|(i, e)| ActivityEvent {
                        seq: trimmed.saturating_add(i as u64).saturating_add(1),
                        cursor: activity_cursor(e, &doc, trimmed.saturating_add(i as u64)),
                        doc_id: DocId::parse(&doc),
                        reff: reff.clone(),
                        kind: e.k.clone(),
                        changes: e
                            .c
                            .iter()
                            .map(|c| FieldChange {
                                field: c.f.clone(),
                                from: c.from.clone(),
                                to: c.to.clone(),
                            })
                            .collect(),
                        actor: actor_of(e),
                        actor_nick: String::new(),
                        text: e.x.clone(),
                        ts: e.t,
                        collision: false,
                    })
                    .collect();
                let last = events.last().map(|e| e.cursor.clone()).unwrap_or_default();
                let value = serde_json::json!({ "events": events, "last": last });
                Ok(projection(serde_json::to_vec(&value).expect("history")))
            }
            IssueQuery::Activity { since } => {
                // The whole-space feed: every event of every issue (tombstoned
                // issues keep their history — the rows already happened),
                // ordered deterministically by `(ts, doc, per-doc index)` so
                // every converged replica derives the identical sequence. The
                // cursor is a position in that total order: `since = last`
                // resumes exactly after the previously served tail.
                // Sorted by the cursor itself, so the order rows are served in
                // and the order `since` compares in are the same order by
                // construction rather than by two definitions agreeing.
                let mut feed: Vec<(String, u64, &String, &IssueEvent)> = Vec::new();
                for (doc, issue) in &snap.issues {
                    let trimmed = issue
                        .events_recorded
                        .saturating_sub(issue.events.len() as u64);
                    for (i, e) in issue.events.iter().enumerate() {
                        let ordinal = trimmed.saturating_add(i as u64);
                        feed.push((activity_cursor(e, doc, ordinal), ordinal, doc, e));
                    }
                }
                feed.sort_by(|a, b| a.0.cmp(&b.0));
                let events: Vec<ActivityEvent> = feed
                    .into_iter()
                    .map(|(cursor, ordinal, doc, e)| ActivityEvent {
                        seq: ordinal.saturating_add(1),
                        cursor,
                        doc_id: DocId::parse(doc),
                        reff: canonical_for(aliases, doc),
                        kind: e.k.clone(),
                        changes: e
                            .c
                            .iter()
                            .map(|c| FieldChange {
                                field: c.f.clone(),
                                from: c.from.clone(),
                                to: c.to.clone(),
                            })
                            .collect(),
                        actor: actor_of(e),
                        actor_nick: String::new(),
                        text: e.x.clone(),
                        ts: e.t,
                        collision: false,
                    })
                    // Strictly after the named row, in the feed's own order.
                    // Comparing the token rather than a count is what makes a
                    // resume safe across a trim: the row the caller names keeps
                    // its identity even when the rows in front of it are gone.
                    .filter(|e| {
                        since
                            .as_deref()
                            .is_none_or(|since| e.cursor.as_str() > since)
                    })
                    .collect();
                // A pull that found nothing hands back the cursor it was given.
                // Returning an empty token there would tell a polling caller it
                // had reached the start of the feed, and its next pull would
                // replay the entire history — the failure mode is silent, and
                // it happens on the most common pull there is.
                let last = events
                    .last()
                    .map(|e| e.cursor.clone())
                    .or(since)
                    .unwrap_or_default();
                let value = serde_json::json!({ "events": events, "last": last });
                Ok(projection(serde_json::to_vec(&value).expect("activity")))
            }
            IssueQuery::Inbox {
                actor,
                exclude_device,
            } => {
                let actor = ActorId::parse(&actor).ok_or(Rejection::InvalidRequest)?;
                let mut entries: Vec<serde_json::Value> = Vec::new();
                for (doc, issue) in &snap.issues {
                    // Addressed-to-you: assigned, or subscribed (INBOX-9) —
                    // followers receive the same event kinds without holding
                    // the assignment.
                    if !issue.assignees.contains(&actor) && !issue.followers.contains(&actor) {
                        continue;
                    }
                    let reff = canonical_for(aliases, doc);
                    for e in &issue.events {
                        let kind = match e.k.as_str() {
                            "assigned" => "assigned",
                            "commented" => "comment",
                            "started" | "finished" | "stopped" => "status",
                            "edited" if e.c.iter().any(|c| c.f == "status") => "status",
                            _ => continue,
                        };
                        if exclude_device.as_deref() == Some(e.d.as_str()) {
                            continue;
                        }
                        entries.push(serde_json::json!({
                            "ts": e.t,
                            "kind": kind,
                            "reff": reff,
                            "doc_id": doc,
                            "title": issue.title,
                            "detail": e.x,
                            "actor": e.d,
                        }));
                    }
                }
                entries.sort_by(|a, b| b["ts"].as_u64().cmp(&a["ts"].as_u64()));
                entries.truncate(500);
                Ok(projection(serde_json::to_vec(&entries).expect("inbox")))
            }
            IssueQuery::RingDigest => Ok(projection(
                serde_json::to_vec(&ring_digest(ctx, catalog)).expect("ring digest json"),
            )),
            IssueQuery::Projects => {
                let projects: Vec<crate::dto::ProjectDto> = catalog
                    .projects
                    .iter()
                    .filter_map(|(id, meta)| project_dto(id, meta))
                    .collect();
                let mut projects = projects;
                projects.sort_by(|a, b| a.key.cmp(&b.key));
                Ok(projection(
                    serde_json::to_vec(&projects).expect("projects json"),
                ))
            }
            IssueQuery::ProjectUpdates { project } => {
                let mut updates: Vec<crate::dto::ProjectUpdateDto> = catalog
                    .project_updates
                    .get(&project)
                    .into_iter()
                    .flatten()
                    .map(|u| crate::dto::ProjectUpdateDto {
                        id: u.id.clone(),
                        author: u.author.clone(),
                        ts: u.ts,
                        body: u.body.clone(),
                        health: u.health.clone(),
                    })
                    .collect();
                // Newest first; ids are ULIDs so id order is time order, a stable
                // tiebreak when two updates share a second.
                updates.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| b.id.cmp(&a.id)));
                Ok(projection(
                    serde_json::to_vec(&updates).expect("project updates json"),
                ))
            }
            IssueQuery::Labels => {
                let labels: Vec<crate::dto::LabelDto> = catalog
                    .labels
                    .iter()
                    .filter_map(|(id, meta)| label_dto(id, meta))
                    .collect();
                let mut labels = labels;
                labels.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(projection(
                    serde_json::to_vec(&labels).expect("labels json"),
                ))
            }
            IssueQuery::Roles => {
                let mut roles: Vec<serde_json::Value> = Vec::new();
                for (id, rev) in &catalog.roles {
                    roles.push(serde_json::json!({
                        "role_id": id,
                        "built_in": true,
                        "revision": rev,
                        "conflict_heads": [],
                    }));
                }
                for id in catalog.role_revisions.keys() {
                    let heads = catalog.role_heads(id);
                    let head = catalog.role_head(id);
                    roles.push(serde_json::json!({
                        "role_id": id,
                        "built_in": false,
                        "revision": head,
                        "conflict_heads": if head.is_some() {
                            Vec::new()
                        } else {
                            heads.iter().map(|h| h.revision_id.clone()).collect()
                        },
                    }));
                }
                roles.sort_by(|a, b| a["role_id"].as_str().cmp(&b["role_id"].as_str()));
                Ok(projection(serde_json::to_vec(&roles).expect("roles json")))
            }
            IssueQuery::RoleShow { role } => {
                let heads = catalog.role_heads(&role);
                let head = catalog.role_head(&role);
                if head.is_none() && heads.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                let view = serde_json::json!({
                    "role_id": role,
                    "built_in": catalog.roles.contains_key(&role),
                    "revision": head,
                    "conflict_heads": if head.is_some() {
                        Vec::new()
                    } else {
                        heads.iter().map(|h| h.revision_id.clone()).collect()
                    },
                });
                Ok(projection(serde_json::to_vec(&view).expect("role json")))
            }
            IssueQuery::Workflow { project } => {
                if !catalog.projects.contains_key(&project) {
                    return Err(Rejection::InvalidRequest);
                }
                let heads = catalog.workflow_heads(&project);
                let head = catalog.workflow_head(&project);
                let view = serde_json::json!({
                    "project_id": project,
                    "revision": head,
                    "conflict_heads": if head.is_some() {
                        Vec::new()
                    } else {
                        heads.iter().map(|h| h.revision_id.clone()).collect()
                    },
                });
                Ok(projection(
                    serde_json::to_vec(&view).expect("workflow json"),
                ))
            }
            IssueQuery::Specs { project } => {
                let mut specs: Vec<crate::spec::SpecView> = all_specs(ctx)
                    .into_iter()
                    .filter_map(|spec| spec_view(&spec))
                    .filter(|spec| {
                        project
                            .as_ref()
                            .is_none_or(|project| &spec.project == project)
                    })
                    .collect();
                specs.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.spec.cmp(&b.spec)));
                Ok(projection(serde_json::to_vec(&specs).expect("specs json")))
            }
            IssueQuery::Spec { spec } => {
                let spec = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let view = spec_view(&spec).ok_or(Rejection::InvalidRequest)?;
                Ok(projection(serde_json::to_vec(&view).expect("spec json")))
            }
            IssueQuery::SpecHistory { spec } => {
                let spec = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                // Oldest first. `Spec::from_view` sorts by revision id, which is
                // a stable order but not a readable one; predecessors give the
                // real one, and a client that wants the DAG still has it.
                let revisions = crate::spec::ordered(&spec.revisions, |revision| {
                    (&revision.revision, &revision.predecessors)
                });
                Ok(projection(
                    serde_json::to_vec(&revisions).expect("spec history json"),
                ))
            }
            IssueQuery::SpecReferences { project } => {
                let mut references: Vec<crate::spec::SpecReference> = Vec::new();
                for spec in all_specs(ctx) {
                    let heads: std::collections::BTreeSet<&str> = spec
                        .heads()
                        .into_iter()
                        .map(|revision| revision.revision.as_str())
                        .collect();
                    let issued: std::collections::BTreeSet<&str> = match spec.issued() {
                        crate::spec::Issued::One(revision) => {
                            [revision.revision.as_str()].into_iter().collect()
                        }
                        // A conflict has no effective revision, so none of the
                        // candidates may claim to be it.
                        crate::spec::Issued::Conflict(_) | crate::spec::Issued::None => {
                            std::collections::BTreeSet::new()
                        }
                    };
                    for revision in &spec.revisions {
                        if project
                            .as_ref()
                            .is_some_and(|project| &revision.body.project != project)
                        {
                            continue;
                        }
                        for link in &revision.body.links {
                            references.push(crate::spec::SpecReference {
                                spec: revision.body.spec.clone(),
                                revision: revision.revision.clone(),
                                kind: revision.body.kind,
                                title: revision.body.title.clone(),
                                link: link.clone(),
                                head: heads.contains(revision.revision.as_str()),
                                issued: issued.contains(revision.revision.as_str()),
                            });
                        }
                    }
                }
                Ok(projection(
                    serde_json::to_vec(&references).expect("spec references json"),
                ))
            }
            IssueQuery::SpecObservations { project } => {
                let mut observations: Vec<crate::spec::Observation> = Vec::new();
                for spec in all_specs(ctx) {
                    // The project is a fact about the document the note is filed
                    // against, since an Observation carries no project of its
                    // own — it is a note, not a document.
                    let Some(first) = spec.revisions.first() else {
                        continue;
                    };
                    if project
                        .as_ref()
                        .is_some_and(|project| &first.body.project != project)
                    {
                        continue;
                    }
                    observations.extend(spec.observations.iter().cloned());
                }
                Ok(projection(
                    serde_json::to_vec(&observations).expect("spec observations json"),
                ))
            }
            IssueQuery::BaselineHistory { baseline } => {
                let baseline = baseline_state(ctx, &baseline).ok_or(Rejection::InvalidRequest)?;
                let revisions = crate::spec::ordered(&baseline.revisions, |revision| {
                    (&revision.revision, &revision.predecessors)
                });
                Ok(projection(
                    serde_json::to_vec(&revisions).expect("baseline history json"),
                ))
            }
            IssueQuery::Baselines { project } => {
                let mut baselines: Vec<crate::spec::BaselineView> = all_baselines(ctx)
                    .into_iter()
                    .filter_map(|baseline| baseline_view(&baseline))
                    .filter(|baseline| {
                        project
                            .as_ref()
                            .is_none_or(|project| &baseline.project == project)
                    })
                    .collect();
                baselines.sort_by(|a, b| {
                    a.name
                        .cmp(&b.name)
                        .then_with(|| a.baseline.cmp(&b.baseline))
                });
                Ok(projection(
                    serde_json::to_vec(&baselines).expect("baselines json"),
                ))
            }
            IssueQuery::Baseline { baseline } => {
                let baseline = baseline_state(ctx, &baseline).ok_or(Rejection::InvalidRequest)?;
                let view = baseline_view(&baseline).ok_or(Rejection::InvalidRequest)?;
                Ok(projection(
                    serde_json::to_vec(&view).expect("baseline json"),
                ))
            }
            IssueQuery::Packet { doc } => {
                let packet = packet(ctx, &doc)?;
                Ok(projection(
                    serde_json::to_vec(&packet).expect("packet json"),
                ))
            }
            IssueQuery::Milestones { project } => {
                if !catalog.projects.contains_key(&project) {
                    return Err(Rejection::InvalidRequest);
                }
                // Derived progress: live issues of the project targeting each
                // milestone, done = a Done-category status.
                let counts = |mid: &str| -> (u32, u32) {
                    let mut total = 0;
                    let mut done = 0;
                    for (doc, issue) in &snap.issues {
                        if issue.project != project
                            || issue.milestone.as_deref() != Some(mid)
                            || catalog.tombstones.contains(doc)
                        {
                            continue;
                        }
                        total += 1;
                        if catalog.status_category(&issue.status) == StatusCategory::Done {
                            done += 1;
                        }
                    }
                    (done, total)
                };
                // Sorted as records, then projected: the DTO carries no rank —
                // a client renders the order it is given and has no business
                // re-deriving it — so the ordering has to happen while the field
                // still exists.
                let mut records: Vec<&Milestone> = catalog
                    .milestones
                    .get(&project)
                    .into_iter()
                    .flat_map(|m| m.values())
                    .filter(|m| !m.tombstone)
                    .collect();
                records.sort_by(|a, b| milestone_order(a, b));
                let rows: Vec<crate::dto::MilestoneDto> = records
                    .into_iter()
                    .map(|m| {
                        let (done, total) = counts(&m.id);
                        crate::dto::MilestoneDto {
                            id: m.id.clone(),
                            name: m.name.clone(),
                            description: m.description.clone(),
                            target_date: m.target_date,
                            total,
                            done,
                        }
                    })
                    .collect();
                Ok(projection(serde_json::to_vec(&rows).expect("milestones")))
            }
            IssueQuery::ProjectGraph { project } => {
                if !catalog.projects.contains_key(&project) {
                    return Err(Rejection::InvalidRequest);
                }
                // "Live issue of this project" is the whole filter, applied to
                // both ends of every edge. An edge to a tombstoned issue, or one
                // reaching into another project, cannot be drawn on this
                // project's chart — and shipping it would only make the client
                // re-derive a fact the catalog already holds.
                let mine = |doc: &String| -> bool {
                    !catalog.tombstones.contains(doc)
                        && snap.issues.get(doc).is_some_and(|i| i.project == project)
                };
                let edges: Vec<crate::dto::GraphEdgeDto> = catalog
                    .edges
                    .iter()
                    .filter(|(from, _, to)| mine(from) && mine(to))
                    .map(|(from, kind, to)| crate::dto::GraphEdgeDto {
                        from: from.clone(),
                        kind: kind.clone(),
                        to: to.clone(),
                    })
                    .collect();
                let parents: Vec<(String, String)> = catalog
                    .parents
                    .iter()
                    .filter(|(child, parent)| mine(child) && mine(parent))
                    .map(|(child, parent)| (child.clone(), parent.clone()))
                    .collect();
                // `catalog.edges` is a BTreeSet and `parents` a BTreeMap, so both
                // arrive sorted and this projection is deterministic without a
                // sort of its own — which matters, because an identical graph
                // must serialize identically or every client re-renders on a
                // poll that changed nothing.
                let view = crate::dto::ProjectGraphView {
                    schema_version: VIEW_SCHEMA_VERSION,
                    project: project.clone(),
                    edges,
                    parents,
                };
                Ok(projection(
                    serde_json::to_vec(&view).expect("project graph"),
                ))
            }
            IssueQuery::Geometry { project, roots } => {
                if !catalog.projects.contains_key(&project)
                    || roots.len() > crate::spec::MAX_PLAN_ROOTS
                    || roots.iter().any(|root| DocId::parse(root).is_none())
                {
                    return Err(Rejection::InvalidRequest);
                }
                let mut canonical_roots = roots;
                canonical_roots.sort();
                canonical_roots.dedup();
                let view = self.geometry_view(ctx, &snap, &project, &canonical_roots);
                Ok(projection(
                    serde_json::to_vec(view.as_ref()).expect("Issue geometry"),
                ))
            }
            IssueQuery::Cycles { project } => {
                if !catalog.projects.contains_key(&project) {
                    return Err(Rejection::InvalidRequest);
                }
                let counts = |cid: &str| -> (u32, u32) {
                    let mut total = 0;
                    let mut done = 0;
                    for (doc, issue) in &snap.issues {
                        if issue.project != project
                            || issue.cycle.as_deref() != Some(cid)
                            || catalog.tombstones.contains(doc)
                        {
                            continue;
                        }
                        total += 1;
                        if catalog.status_category(&issue.status) == StatusCategory::Done {
                            done += 1;
                        }
                    }
                    (done, total)
                };
                let mut rows: Vec<crate::dto::CycleDto> = catalog
                    .cycles
                    .get(&project)
                    .into_iter()
                    .flat_map(|c| c.values())
                    .filter(|c| !c.tombstone)
                    .map(|c| {
                        let (done, total) = counts(&c.id);
                        crate::dto::CycleDto {
                            id: c.id.clone(),
                            name: c.name.clone(),
                            start: c.start,
                            end: c.end,
                            total,
                            done,
                        }
                    })
                    .collect();
                rows.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.name.cmp(&b.name)));
                Ok(projection(serde_json::to_vec(&rows).expect("cycles")))
            }
            IssueQuery::Initiatives => {
                let mut rows: Vec<crate::dto::InitiativeDto> = catalog
                    .initiatives
                    .values()
                    .filter(|i| !i.tombstone)
                    .map(|i| {
                        let mut total = 0;
                        let mut done = 0;
                        for (doc, issue) in &snap.issues {
                            if !i.projects.contains(&issue.project)
                                || catalog.tombstones.contains(doc)
                            {
                                continue;
                            }
                            total += 1;
                            if catalog.status_category(&issue.status) == StatusCategory::Done {
                                done += 1;
                            }
                        }
                        crate::dto::InitiativeDto {
                            id: i.id.clone(),
                            name: i.name.clone(),
                            description: i.description.clone(),
                            owner: i.owner.clone(),
                            health: i.health.clone(),
                            target_date: i.target_date,
                            projects: i
                                .projects
                                .iter()
                                .filter_map(|p| catalog.projects.get(p).map(|m| m.key.clone()))
                                .collect(),
                            total,
                            done,
                        }
                    })
                    .collect();
                rows.sort_by(|a, b| a.name.cmp(&b.name));
                Ok(projection(serde_json::to_vec(&rows).expect("initiatives")))
            }
            IssueQuery::Teams => {
                let mut rows: Vec<crate::dto::TeamDto> = catalog
                    .teams
                    .values()
                    .filter(|t| !t.tombstone)
                    .map(|t| crate::dto::TeamDto {
                        id: t.id.clone(),
                        name: t.name.clone(),
                        key: t.key.clone(),
                        icon: t.icon.clone(),
                        lead: t.lead.clone(),
                        members: t.members.clone(),
                        projects: catalog
                            .projects
                            .values()
                            .filter(|p| p.team == t.id)
                            .map(|p| p.key.clone())
                            .collect(),
                    })
                    .collect();
                rows.sort_by(|a, b| a.key.cmp(&b.key));
                Ok(projection(serde_json::to_vec(&rows).expect("teams")))
            }
            IssueQuery::Triage => {
                let reff_of = |doc: &str| -> String {
                    if doc.is_empty() {
                        String::new()
                    } else {
                        aliases
                            .by_doc
                            .get(doc)
                            .cloned()
                            .unwrap_or_else(|| canonical_for(aliases, doc))
                    }
                };
                let mut rows: Vec<crate::dto::TriageDto> = catalog
                    .triage
                    .values()
                    .map(|t| crate::dto::TriageDto {
                        id: t.id.clone(),
                        title: t.title.clone(),
                        body: t.body.clone(),
                        source: t.source.clone(),
                        submitted_by: t.submitted_by.clone(),
                        ts: t.ts,
                        outcome: t.outcome.clone(),
                        reff: reff_of(&t.doc),
                        decided_by: t.decided_by.clone(),
                        note: t.note.clone(),
                    })
                    .collect();
                // Pending first (newest first); decided after (newest first).
                rows.sort_by(|a, b| {
                    (!a.outcome.is_empty())
                        .cmp(&(!b.outcome.is_empty()))
                        .then_with(|| b.ts.cmp(&a.ts))
                        .then_with(|| a.id.cmp(&b.id))
                });
                Ok(projection(serde_json::to_vec(&rows).expect("triage")))
            }
            IssueQuery::Attachment { doc, id } => {
                // The one read that serves file bytes: straight off the Body
                // map, bypassing the metadata-only snapshot cache.
                let view = ctx
                    .read_collaborative(&issue_key(&doc))
                    .map_err(|_| Rejection::InvalidRequest)?;
                let raw = view
                    .maps
                    .get("attachments")
                    .and_then(|m| m.get(&id))
                    .ok_or(Rejection::InvalidRequest)?;
                let record: serde_json::Value =
                    serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
                Ok(projection(serde_json::to_vec(&record).expect("attachment")))
            }
        }
    }
}

struct Row2 {
    row: crate::dto::Row,
    priority: Priority,
}

fn space_placeholder() -> crate::ids::SpaceId {
    // IssueView carries the SpaceId; the daemon-side adapter overwrites it
    // with the Station's Space before returning the view to a client.
    crate::ids::SpaceId::from_digest([0u8; 16])
}

fn provisional_view(
    catalog: &CatalogState,
    aliases: &DerivedAliases,
    doc: &str,
) -> crate::dto::IssueView {
    let row = project_row(catalog, aliases, doc, None, None);
    crate::dto::IssueView {
        schema_version: VIEW_SCHEMA_VERSION,
        reff: row.reff,
        doc_id: row.doc_id,
        space_id: space_placeholder(),
        project_id: row.project_id,
        project_key: None,
        key_alias: row.key_alias,
        title: row.title,
        description: String::new(),
        document_schema: DOCUMENT_SCHEMA_VERSION,
        status: row.status,
        priority: row.priority,
        assignees: vec![],
        labels: vec![],
        label_names: vec![],
        comments: vec![],
        created_by: ActorId::from_incept_hash(&"0".repeat(64)),
        created_at: 0,
        due_date: None,
        estimate: None,
        followers: vec![],
        milestone: None,
        cycle: None,
        baseline: None,
        attachments: vec![],
        provisional: true,
        corrupt_records: vec![],
    }
}

fn graph_view(
    catalog: &CatalogState,
    aliases: &DerivedAliases,
    doc: &str,
    issues: &BTreeMap<String, Arc<IssueState>>,
    me: Option<&ActorId>,
) -> crate::dto::GraphView {
    let live = |d: &str| issues.contains_key(d) && !catalog.tombstones.contains(d);
    let row = |d: &str| project_row(catalog, aliases, d, issues.get(d).map(|i| i.as_ref()), me);
    let parent = catalog.parents.get(doc).filter(|p| live(p)).map(|p| row(p));
    let mut children: Vec<crate::dto::Row> = catalog
        .parents
        .iter()
        .filter(|(c, p)| p.as_str() == doc && live(c))
        .map(|(c, _)| row(c))
        .collect();
    children.sort_by(|a, b| a.doc_id.cmp(&b.doc_id));
    let mut links = Vec::new();
    for (from, kind, to) in &catalog.edges {
        if from == doc && live(to) {
            links.push(crate::dto::LinkDto {
                kind: kind.clone(),
                direction: "out".into(),
                row: row(to),
            });
        } else if to == doc && live(from) {
            links.push(crate::dto::LinkDto {
                kind: kind.clone(),
                direction: "in".into(),
                row: row(from),
            });
        }
    }
    // Transitive open blockers via BFS backward over `blocks` edges.
    let mut blocked_by = Vec::new();
    let mut visited = std::collections::BTreeSet::new();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(doc.to_string());
    visited.insert(doc.to_string());
    while let Some(cursor) = queue.pop_front() {
        for (from, kind, to) in &catalog.edges {
            if kind == "blocks" && to == &cursor && visited.insert(from.clone()) {
                let open = issues
                    .get(from)
                    .is_some_and(|i| catalog.status_category(&i.status) != StatusCategory::Done);
                if live(from) && open {
                    blocked_by.push(row(from));
                    queue.push_back(from.clone());
                }
            }
        }
    }
    crate::dto::GraphView {
        schema_version: VIEW_SCHEMA_VERSION,
        reff: canonical_for(aliases, doc),
        doc_id: DocId::parse(doc).expect("doc id"),
        parent,
        children,
        links,
        blocked_by,
    }
}

/// The doorbell's view of one committed state: a digest per catalog plane, plus
/// the doc→project index the dirty-set is keyed by.
///
/// The plane taxonomy lives here because the catalog's schema does. Every plane
/// is a distinct region of `CatalogState`, and the ones whose data is grouped by
/// project get one digest *per project* — so editing ENG's milestones does not
/// invalidate DSN's. The daemon compares these between rings; it never decodes
/// them, and it learns nothing about what a milestone is.
///
/// Digest inputs are `BTreeMap`/`BTreeSet` serializations, which are ordered, so
/// equal state always digests equal. A plane absent from the map digests as its
/// empty form rather than being omitted — "emptied" has to be distinguishable
/// from "unchanged".
fn ring_digest(ctx: &Context<'_>, catalog: &CatalogState) -> contract::RingDigestView {
    let mut planes: Vec<contract::PlaneDigest> = Vec::new();
    let mut plane = |plane: CatalogScope, value: serde_json::Value| {
        let bytes = serde_json::to_vec(&value).expect("plane json");
        planes.push(contract::PlaneDigest {
            plane,
            digest: blake3::hash(&bytes).to_hex().to_string(),
        });
    };

    plane(
        CatalogScope::Space,
        serde_json::json!([&catalog.name, &catalog.description]),
    );
    plane(CatalogScope::Projects, serde_json::json!(&catalog.projects));
    plane(CatalogScope::Labels, serde_json::json!(&catalog.labels));
    plane(
        CatalogScope::Workflow,
        serde_json::json!([
            serde_json::json!(&catalog.workflow),
            serde_json::json!(&catalog.workflow_revisions)
        ]),
    );
    plane(
        CatalogScope::Initiatives,
        serde_json::json!(&catalog.initiatives),
    );
    plane(CatalogScope::Teams, serde_json::json!(&catalog.teams));
    plane(CatalogScope::Triage, serde_json::json!(&catalog.triage));
    plane(
        CatalogScope::Roles,
        serde_json::json!([
            serde_json::json!(&catalog.roles),
            serde_json::json!(&catalog.role_revisions)
        ]),
    );
    // The row index: which docs exist, what they are numbered, what is deleted.
    plane(
        CatalogScope::Docs,
        serde_json::json!([
            serde_json::json!(&catalog.aliases),
            serde_json::json!(&catalog.seqs),
            serde_json::json!(&catalog.tombstones)
        ]),
    );
    plane(
        CatalogScope::Relations,
        serde_json::json!([
            serde_json::json!(&catalog.edges),
            serde_json::json!(&catalog.parents)
        ]),
    );
    // The one plane whose contents are not in the catalog. Specs and Baselines
    // are Bodies of their own, so there is no region here to hash — but a Body's
    // version stamp moves exactly when the Body does, and reading stamps costs
    // no decode. Digesting `(key, stamp)` pairs gets the same "did this plane
    // move" answer for the price of an enumeration.
    let mut stamps: Vec<(String, Option<Vec<u8>>)> = Vec::new();
    for schema in [contract::spec_schema(), contract::baseline_schema()] {
        for key in ctx.bodies_with_schema(&contract::world_id(), &schema) {
            stamps.push((key.body.render(), ctx.body_stamp(&key)));
        }
    }
    stamps.sort();
    plane(CatalogScope::Specs, serde_json::json!(stamps));

    // Per-project planes, and the doc index, from the same pinned catalog: one
    // pass, one root. Each names the project by its stable id AND its display
    // key — a dependency matches on the id, which a rename cannot move.
    let mut docs: Vec<contract::RingDoc> = Vec::new();
    for (id, meta) in &catalog.projects {
        let (project_id, project_key) = (id.clone(), meta.key.clone());
        plane(
            CatalogScope::Boards {
                project_id: project_id.clone(),
                project_key: project_key.clone(),
            },
            serde_json::json!(catalog.boards.get(id)),
        );
        plane(
            CatalogScope::Milestones {
                project_id: project_id.clone(),
                project_key: project_key.clone(),
            },
            serde_json::json!(catalog.milestones.get(id)),
        );
        plane(
            CatalogScope::Cycles {
                project_id: project_id.clone(),
                project_key: project_key.clone(),
            },
            serde_json::json!(catalog.cycles.get(id)),
        );
        plane(
            CatalogScope::Updates {
                project_id: project_id.clone(),
                project_key: project_key.clone(),
            },
            serde_json::json!(catalog.project_updates.get(id)),
        );
        for (_element, doc) in catalog.boards.get(id).into_iter().flatten() {
            docs.push(contract::RingDoc {
                doc: doc.clone(),
                project_id: project_id.clone(),
                project_key: project_key.clone(),
            });
        }
    }
    contract::RingDigestView { planes, docs }
}

#[cfg(test)]
mod structure_migration_tests {
    use super::*;

    fn legacy_plan(state: crate::spec::State) -> crate::spec::Revision {
        crate::spec::Revision {
            revision: "11".repeat(32),
            predecessors: Vec::new(),
            body: crate::spec::Body {
                spec: "spc_01k1k8q6c6t0g0000000000000".into(),
                project: "prj_01k1k8q6c6t0g0000000000000".into(),
                kind: crate::spec::Kind::Plan,
                generation: String::new(),
                title: "Plan".into(),
                text: format!("{}Plan", contract::DOCUMENT_PREFIX),
                state,
                links: Vec::new(),
                plan: None,
                author: "act_86a32a40c88b66b026bd7567542e228bd727e0488feaf4d8b528a7a79aa1ee30"
                    .into(),
                ts: 1,
            },
        }
    }

    #[test]
    fn migration_coordinates_a_plan_without_changing_issued_truth() {
        let head = legacy_plan(crate::spec::State::Issued);
        let actor = "act_12a03af5f8de402e33baffbe9a1dfd8321cdebb63af195bd94d7c169325f31fb";
        let body = migrated_spec_body(&head, &"ab".repeat(32), actor, 9).unwrap();

        assert_eq!(body.state, crate::spec::State::Issued);
        assert_eq!(body.generation, "ab".repeat(32));
        assert_eq!(body.plan.unwrap().roots, Vec::<String>::new());
        assert_eq!(body.author, actor);
        assert_eq!(body.ts, 9);
    }

    #[test]
    fn migration_is_a_no_op_once_the_head_is_native() {
        let mut head = legacy_plan(crate::spec::State::Draft);
        head.body.generation = "cd".repeat(32);
        head.body.plan = Some(crate::spec::PlanData { roots: Vec::new() });

        assert!(migrated_spec_body(&head, &"ef".repeat(32), &head.body.author, 9).is_none());
    }
}

#[cfg(test)]
mod milestone_order_tests {
    use super::*;

    fn milestone(id: &str, name: &str, rank: &str, target: Option<u64>) -> Milestone {
        Milestone {
            id: id.into(),
            project_id: "prj_1".into(),
            name: name.into(),
            description: String::new(),
            target_date: target,
            rank: rank.into(),
            tombstone: false,
        }
    }

    fn order(mut list: Vec<Milestone>) -> Vec<String> {
        list.sort_by(milestone_order);
        list.into_iter().map(|m| m.name).collect()
    }

    #[test]
    fn ranked_milestones_ignore_the_target_date() {
        // The whole point: an undated first stage must not sink below a dated
        // later one. `M0` has no target and still leads.
        let list = vec![
            milestone("mls_b", "M1", "2", Some(1_000)),
            milestone("mls_a", "M0", "1", None),
            milestone("mls_c", "M2", "3", Some(500)),
        ];
        assert_eq!(order(list), ["M0", "M1", "M2"]);
    }

    #[test]
    fn unranked_milestones_keep_the_old_date_order() {
        // A project nobody has reordered since ranks existed reads exactly as it
        // did before: by target date, undated last, name breaking ties.
        let list = vec![
            milestone("mls_c", "Later", "", Some(2_000)),
            milestone("mls_a", "Someday", "", None),
            milestone("mls_b", "Soon", "", Some(1_000)),
        ];
        assert_eq!(order(list), ["Soon", "Later", "Someday"]);
    }

    #[test]
    fn an_unranked_stray_sorts_last_rather_than_first() {
        // `""` is below every rank, so the naive comparison would put a legacy
        // record at the head of a list somebody has deliberately ordered. The
        // backfill normally prevents the mix; if one slips through — a concurrent
        // write from an older peer — it lands at the end, where it is visible and
        // harmless, not on top of the first stage.
        let list = vec![
            milestone("mls_x", "Stray", "", Some(1)),
            milestone("mls_b", "M1", "2", None),
            milestone("mls_a", "M0", "1", None),
        ];
        assert_eq!(order(list), ["M0", "M1", "Stray"]);
    }

    #[test]
    fn equal_ranks_break_on_id_so_replicas_agree() {
        // Two peers can place a milestone at the same rank concurrently. Agreeing
        // on *an* order matters more than agreeing on whose move won.
        let list = vec![
            milestone("mls_b", "Second", "5", None),
            milestone("mls_a", "First", "5", None),
        ];
        assert_eq!(order(list), ["First", "Second"]);
    }
}

#[cfg(test)]
mod comment_anchor_tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::dto::CommentAnchorState;

    /// A reader that answers a scripted resolution per anchor offset, and
    /// counts how often it was asked.
    ///
    /// [`Context::new`] carries no reader and answers `Drifted` for every
    /// anchor, which puts the resolved arms of [`resolve_comment_anchor`] out
    /// of reach — a module built on it passes unchanged when the resolver is
    /// replaced by a constant. The offsets of a stored span's two ends differ,
    /// so a script keyed on them drives each arm, including the ones a live
    /// replica reaches only after one specific edit. The count is what proves
    /// the guards ahead of the reader stop before it.
    #[derive(Default)]
    struct ScriptedReader {
        by_offset: BTreeMap<u64, fabric::AnchorResolution>,
        asked: AtomicUsize,
    }

    impl runtime::world::BodyReader for ScriptedReader {
        fn read_body(&self, _key: &replica::body::BodyKey) -> Option<Vec<u8>> {
            None
        }
        fn read_collaborative_body(
            &self,
            _key: &replica::body::BodyKey,
        ) -> Result<fabric::CollaborativeView, fabric::projection::Failure> {
            Err(fabric::projection::Failure::NotCollaborative)
        }
        fn bodies_with_schema(
            &self,
            _world: &replica::body::WorldId,
            _schema: &replica::body::SchemaId,
        ) -> Vec<replica::body::BodyKey> {
            Vec::new()
        }
        fn body_version(&self, _key: &replica::body::BodyKey) -> Option<fabric::Version> {
            None
        }
        fn anchor_in_body(
            &self,
            _key: &replica::body::BodyKey,
            _path: &str,
            _position: u64,
        ) -> Option<fabric::Anchor> {
            None
        }
        fn resolve_anchor(
            &self,
            _key: &replica::body::BodyKey,
            anchor: &fabric::Anchor,
        ) -> fabric::AnchorResolution {
            self.asked.fetch_add(1, Ordering::SeqCst);
            self.by_offset
                .get(&anchor.offset)
                .copied()
                .unwrap_or(fabric::AnchorResolution::Drifted)
        }
        fn content_status(
            &self,
            _content: &replica::content::ContentRef,
        ) -> Option<runtime::world::ContentStatus> {
            None
        }
    }

    fn scripted<const N: usize>(script: [(u64, fabric::AnchorResolution); N]) -> ScriptedReader {
        ScriptedReader {
            by_offset: script.into_iter().collect(),
            asked: AtomicUsize::new(0),
        }
    }

    fn facts() -> runtime::world::PrincipalFacts {
        let device = mechanics::actor::device_from_seed(&[3u8; 32]);
        runtime::world::PrincipalFacts {
            actor: ActorId::from_incept_hash(&"cd".repeat(32)),
            station: mechanics::station::Key::from_device(&device).unwrap(),
            device,
            space: mechanics::ids::SpaceId::from_digest([5u8; 16]),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
        }
    }

    fn issue(description: &str) -> IssueState {
        IssueState {
            description: description.into(),
            ..Default::default()
        }
    }

    fn comment(at: Option<contract::StoredAnchor>) -> StoredComment {
        StoredComment {
            a: ActorId::from_incept_hash(&"cd".repeat(32)).as_str().into(),
            t: 1,
            b: "body".into(),
            id: Some("cmt_00000000000000000000000000".into()),
            parent: None,
            at,
            node: None,
            parent_node: None,
        }
    }

    /// Bytes with the shape a real stored anchor has: canonical, naming a path,
    /// and carrying the offset the script keys on.
    fn anchor_hex(path: &str, offset: u64) -> String {
        let anchor = fabric::Anchor {
            format_version: fabric::CAUSAL_FORMAT_VERSION,
            body: [9u8; 32],
            path: path.into(),
            anchored_to: None,
            offset,
            after: true,
            taken_at: fabric::Version::empty(),
        };
        data_encoding::HEXLOWER.encode(&anchor.encode())
    }

    /// A stored attachment whose ends the script addresses by `head`/`tail`.
    fn stored(field: &str, head: u64, tail: Option<u64>) -> contract::StoredAnchor {
        contract::StoredAnchor {
            field: field.into(),
            start: anchor_hex(field, head),
            end: tail.map(|t| anchor_hex(field, t)),
        }
    }

    fn resolve(
        reader: &ScriptedReader,
        issue: &IssueState,
        at: Option<contract::StoredAnchor>,
    ) -> Option<crate::dto::CommentAnchorDto> {
        let facts = facts();
        let ctx = Context::with_reads(&facts, reader, [0u8; 32]);
        resolve_comment_anchor(&ctx, "iss_x", issue, &comment(at))
    }

    /// An unattached comment has no anchor to report, which is not a state of
    /// an anchor.
    #[test]
    fn an_unattached_comment_resolves_to_nothing() {
        let reader = ScriptedReader::default();
        assert!(resolve(&reader, &issue("the quick brown fox"), None).is_none());
    }

    /// A span's ends resolve one past the characters they bound to, and the
    /// head's one is taken back off.
    ///
    /// The only test in this module that reaches the reader's answer, and the
    /// one that fails if [`resolve_comment_anchor`] stops resolving: the script
    /// answers with positions eight characters along from where the anchors
    /// were taken, as an insertion in front of the span would.
    #[test]
    fn a_resolved_span_reports_the_material_its_ends_bound_to() {
        let reader = scripted([
            (5, fabric::AnchorResolution::Resolved(13)),
            (9, fabric::AnchorResolution::Resolved(17)),
        ]);
        let resolved = resolve(
            &reader,
            &issue("PRE ther quick brown fox"),
            Some(stored("description", 5, Some(9))),
        )
        .unwrap();
        assert_eq!(resolved.field, "description");
        assert_eq!(
            resolved.state,
            CommentAnchorState::At { start: 12, end: 17 },
            "the head bound to the span's first character, so the span starts one back"
        );
    }

    /// A caret bound to the character in front of it already resolves to
    /// itself, so nothing is taken off.
    #[test]
    fn a_resolved_caret_reports_the_position_it_bound_to() {
        let reader = scripted([(4, fabric::AnchorResolution::Resolved(12))]);
        let resolved = resolve(
            &reader,
            &issue("PRE the quick brown fox"),
            Some(stored("description", 4, None)),
        )
        .unwrap();
        assert_eq!(
            resolved.state,
            CommentAnchorState::At { start: 12, end: 12 }
        );
    }

    /// Either end lost is a lost span. Half a span is the guess the algebra
    /// forbids.
    #[test]
    fn either_end_lost_drifts_the_whole_span() {
        for script in [
            [
                (5, fabric::AnchorResolution::Drifted),
                (9, fabric::AnchorResolution::Resolved(17)),
            ],
            [
                (5, fabric::AnchorResolution::Resolved(13)),
                (9, fabric::AnchorResolution::Drifted),
            ],
        ] {
            let reader = scripted(script);
            let resolved = resolve(
                &reader,
                &issue("the quick brown fox"),
                Some(stored("description", 5, Some(9))),
            )
            .unwrap();
            assert_eq!(resolved.state, CommentAnchorState::Drifted);
        }
    }

    /// Ends that resolve out of order no longer describe a span.
    #[test]
    fn ends_that_resolve_out_of_order_are_not_a_span() {
        let reader = scripted([
            (5, fabric::AnchorResolution::Resolved(13)),
            (9, fabric::AnchorResolution::Resolved(3)),
        ]);
        let resolved = resolve(
            &reader,
            &issue("the quick brown fox"),
            Some(stored("description", 5, Some(9))),
        )
        .unwrap();
        assert_eq!(resolved.state, CommentAnchorState::Drifted);
    }

    /// Stored bytes that are not a canonical anchor are `Unresolved`, never
    /// `Drifted`.
    ///
    /// The distinction is the whole reason both states exist: `Drifted` says
    /// the span has no place in the text, and telling someone that because a
    /// decode failed would be a claim about their document made from a bug in
    /// ours.
    #[test]
    fn undecodable_bytes_are_unresolved_and_not_drifted() {
        let reader = scripted([(4, fabric::AnchorResolution::Resolved(4))]);
        for bad in ["", "zz", "00"] {
            let at = contract::StoredAnchor {
                field: "description".into(),
                start: bad.into(),
                end: None,
            };
            let resolved = resolve(&reader, &issue("the quick brown fox"), Some(at)).unwrap();
            assert_eq!(
                resolved.state,
                CommentAnchorState::Unresolved,
                "`{bad}` is not an anchor, so there is no answer — not a lost one"
            );
        }

        // One end decodable and the other not is still no answer.
        let at = contract::StoredAnchor {
            field: "description".into(),
            start: anchor_hex("description", 4),
            end: Some("zz".into()),
        };
        let resolved = resolve(&reader, &issue("the quick brown fox"), Some(at)).unwrap();
        assert_eq!(resolved.state, CommentAnchorState::Unresolved);
    }

    /// A record whose field disagrees with its own anchor's path resolves to
    /// nothing, without asking the reader.
    ///
    /// Both name the value the span is inside. Trusting either one over the
    /// other would report a position in a field the writer may not have meant,
    /// which is a wrong index wearing the right shape.
    #[test]
    fn a_record_that_disagrees_with_its_own_anchor_is_unresolved() {
        let reader = scripted([(4, fabric::AnchorResolution::Resolved(4))]);
        let at = contract::StoredAnchor {
            field: "description".into(),
            start: anchor_hex("title", 4),
            end: None,
        };
        let resolved = resolve(&reader, &issue("the quick brown fox"), Some(at)).unwrap();
        assert_eq!(resolved.state, CommentAnchorState::Unresolved);
        assert_eq!(reader.asked.load(Ordering::SeqCst), 0);
    }

    /// A record naming a field with no positions in it is `Unresolved`, and the
    /// reader is never asked.
    ///
    /// The write seam refuses to mint such a record; a peer on another build
    /// can still put one in the shared Body, and `anchor_in_body` validates no
    /// path — so the reader would answer position zero, forever, for a register.
    /// [`IssueState::anchorable_text`] is the list, and it binds both seams.
    #[test]
    fn a_record_naming_a_field_with_no_positions_is_unresolved() {
        let reader = scripted([(4, fabric::AnchorResolution::Resolved(4))]);
        let resolved = resolve(
            &reader,
            &issue("the quick brown fox"),
            Some(stored("title", 4, None)),
        )
        .unwrap();
        assert_eq!(resolved.field, "title");
        assert_eq!(resolved.state, CommentAnchorState::Unresolved);
        assert_eq!(reader.asked.load(Ordering::SeqCst), 0);
    }

    /// A field that has been emptied has drifted, whatever the reader says.
    ///
    /// An anchor at offset zero binds to no operation, so the algebra keeps
    /// answering zero for it after the last character is deleted. Zero is a
    /// position, and there are no positions in an empty text.
    #[test]
    fn a_record_in_an_emptied_field_has_drifted() {
        let reader = scripted([(0, fabric::AnchorResolution::Resolved(0))]);
        let resolved = resolve(&reader, &issue(""), Some(stored("description", 0, None))).unwrap();
        assert_eq!(resolved.state, CommentAnchorState::Drifted);
        assert_eq!(reader.asked.load(Ordering::SeqCst), 0);
    }
}
