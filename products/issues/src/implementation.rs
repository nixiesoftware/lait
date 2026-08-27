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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use replica::body::BodyKey;
use replica::body::{CollaborativeSchema, MutationModel, Op, Schema};
use runtime::{
    world::BodyDeclaration, world::Context, world::Effect, world::Intent, world::Projection,
    world::Query, world::Rejection, world::World,
};

use crate::dto::{ActivityEvent, FieldChange, Priority, StatusCategory};
use crate::ids::{ActorId, DocId, ProjectId};
use crate::records::CanonicalRecord as _;

use super::contract::{
    self, catalog_key, issue_key, EventChange, IssueEffect, IssueEvent, IssueIntent, IssueQuery,
    NewLabel, Pos, StoredComment, WorkAction, DEFAULT_STATUS, DOCUMENT_SCHEMA_VERSION, LINK_KINDS,
    VIEW_SCHEMA_VERSION,
};
use super::rank;
use super::views::{
    board_view, canonical_for, issue_view, label_dto, project_dto, project_row, CatalogState,
    DerivedAliases, Initiative, IssueState, LabelMeta, Milestone, ProjectMeta, RelationState, Team,
    TriageItem,
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

/// Rows a neighbour probe asks for: the adjacent milestone, plus one in case
/// the first is the milestone being moved.
const NEIGHBOR_PAGE: u32 = 2;

fn milestone_neighbor(
    ctx: &Context<'_>,
    project: &str,
    moving: &str,
    descending: bool,
    pivot: Vec<u8>,
) -> Result<Option<(String, String)>, Rejection> {
    use runtime::find as find_api;
    // Two rows are wanted and at most one of them is the milestone being
    // moved, but the ordered scan that finds them walks whatever the index
    // holds between the pivot and the answer. The hand-written budget here
    // used to allow eight postings, which happened to cover a project with
    // two milestones and refused the third with `LimitExceeded` -- reported
    // to the operator as a corrupt catalog, because this call site also
    // discarded the typed failure. Derive it the way every other paged
    // helper in this file does instead, so it scales with the page rather
    // than with the number of milestones that existed when it was written.
    let candidates = u64::from(NEIGHBOR_PAGE).saturating_mul(8).max(64);
    let bound = find_api::Bound {
        decoded_bodies: 4,
        postings_read: candidates.saturating_mul(8),
        edges_visited: 1,
        nodes_visited: candidates,
        paths_retained: 1,
        candidates_per_branch: candidates,
        score_evaluations: 1,
        projected_bytes: u64::from(NEIGHBOR_PAGE).saturating_mul(16 * 1_024),
        packed_tokens: u64::from(NEIGHBOR_PAGE).saturating_mul(4_096),
        wall_millis: 1_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let index = if descending {
        crate::find::field::KIND_PROJECT_POSITION_DESC
    } else {
        crate::find::field::KIND_PROJECT_POSITION
    };
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::TITLE,
        crate::find::field::PROJECT,
        crate::find::field::POSITION,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                        field: crate::find::field_ref(index),
                        test: find_api::Test::Greater,
                        value: find_api::Atom::Bytes(pivot),
                    })),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![seek],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: NEIGHBOR_PAGE,
            cursor: None,
        })
        .map_err(find_rejection)?;
    for row in answer.rows() {
        if result_text(row, crate::find::field::KIND).as_deref() != Some("milestone")
            || result_text(row, crate::find::field::PROJECT).as_deref() != Some(project)
        {
            return Ok(None);
        }
        let id = result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
        if id == moving {
            continue;
        }
        let position =
            result_text(row, crate::find::field::POSITION).ok_or(Rejection::StateCorrupt)?;
        return Ok(Some((id, position)));
    }
    Ok(None)
}

fn milestone_position(
    ctx: &Context<'_>,
    project: &str,
    moving: &str,
    pos: &Pos,
) -> Result<String, Rejection> {
    let (lower, upper) = match pos {
        Pos::Top => {
            let upper = milestone_neighbor(
                ctx,
                project,
                moving,
                false,
                crate::find::composite_key(["milestone", project]),
            )?;
            (String::new(), upper.map(|(_, rank)| rank))
        }
        Pos::Bottom => {
            let lower = milestone_neighbor(
                ctx,
                project,
                moving,
                true,
                crate::find::composite_key(["milestone", project]),
            )?;
            (lower.map_or_else(String::new, |(_, rank)| rank), None)
        }
        Pos::Before { doc } | Pos::After { doc } => {
            if doc == moving {
                return Err(Rejection::InvalidRequest);
            }
            let mut catalog = CatalogState::default();
            crate::record_store::apply_schedule_record(ctx, &mut catalog, project, doc)?;
            let target = catalog
                .milestones
                .get(project)
                .and_then(|records| records.get(doc))
                .filter(|record| !record.tombstone && !record.rank.is_empty())
                .ok_or(Rejection::InvalidRequest)?;
            let rank = target.rank.clone();
            match pos {
                Pos::Before { .. } => {
                    let lower = milestone_neighbor(
                        ctx,
                        project,
                        moving,
                        true,
                        crate::find::entity_position_desc_key("milestone", project, &rank, doc),
                    )?;
                    (lower.map_or_else(String::new, |(_, rank)| rank), Some(rank))
                }
                Pos::After { .. } => {
                    let upper = milestone_neighbor(
                        ctx,
                        project,
                        moving,
                        false,
                        crate::find::entity_position_key("milestone", project, &rank, doc),
                    )?;
                    (rank, upper.map(|(_, rank)| rank))
                }
                _ => return Err(Rejection::InvalidRequest),
            }
        }
    };
    rank::try_between(&lower, upper.as_deref()).ok_or(Rejection::LimitExceeded)
}

/// The registered product World.
pub struct IssuesWorld {
    id: replica::body::WorldId,
    schemas: Vec<Schema>,
    find_schemas: Vec<runtime::find::Schema>,
    find_extractors: Vec<runtime::find::Extractor>,
    exec_specs: Vec<runtime::exec::Spec>,
    geometry: crate::geometry::GeometryRegistry,
    /// Owned rather than built on demand, because the trait hands back a slice
    /// and the registry compares it against the registration byte for byte —
    /// two constructions of "the same" list is how they come to differ.
    signal_schemas: Vec<runtime::world::SignalSchema>,
    package: IssuesPackage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IssuesPackage {
    /// The preferred implementation installed after the one-time rewrite.
    /// It has no authority to decode aggregate Catalog/Spec/Baseline state or
    /// to advance migration markers.
    Preferred,
    /// Historical implementation used only by the resumable migration job.
    Migrator,
}

/// A request-local working set selected from Runtime's immutable publication
/// corpus. Runtime owns generation sharing; Issues never caches a second
/// root-keyed projection truth.
struct IssueReadSet {
    aliases: DerivedAliases,
    issues: BTreeMap<String, Arc<IssueState>>,
}

impl IssuesWorld {
    fn portable_publication(
        &self,
        ctx: &Context<'_>,
    ) -> Result<runtime::publication::PublicationId, Rejection> {
        ctx.world_publication_id()
            .map(|publication| publication.publication)
            .ok_or(Rejection::ContractViolation)
    }

    fn geometry_projection(
        &self,
        ctx: &Context<'_>,
        project: &str,
        roots: &[String],
        page: Option<crate::geometry::GeometryPageRequest>,
    ) -> Result<contract::GeometryProjection, Rejection> {
        let source = ctx
            .world_publication_id()
            .ok_or(Rejection::ContractViolation)?;
        let request = crate::geometry::GeometryRequest::new(
            source,
            project,
            roots.to_vec(),
            crate::geometry::GeometryBudget::default(),
        );
        let artifact = if let Some(artifact) = self.geometry.get(&request.key()) {
            artifact
        } else {
            let find = ctx.deferred_find()?.ok_or(Rejection::ContractViolation)?;
            let worker_request = request.clone();
            self.geometry.materialize_cached_with_memory(
                &request,
                crate::geometry::GeometryEstimate::default(),
                &find,
                {
                    let find = find.clone();
                    move || crate::geometry::facts_from_find(&find, &worker_request)
                },
            )
        };
        let key = artifact.key();
        let summary = artifact.summary(&key).ok();
        let page = match page {
            Some(page)
                if matches!(
                    artifact.readiness(),
                    crate::geometry::GeometryReadiness::Ready
                ) =>
            {
                Some(
                    artifact
                        .page(&key, page)
                        .map_err(|_| Rejection::InvalidRequest)?,
                )
            }
            _ => None,
        };
        Ok(contract::GeometryProjection {
            key,
            source: artifact.source(),
            estimate: artifact.estimate(),
            readiness: artifact.readiness().clone(),
            summary,
            page,
        })
    }
}

impl Default for IssuesWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl IssuesWorld {
    /// Product-owned read-only planner for one exact frozen migration window.
    /// The lifecycle host invokes this through `Session::with_lifecycle_source`
    /// before it signs or admits a mutation.
    pub fn prepare_v4_migration_plan(
        ctx: &Context<'_>,
        previous_batch: u64,
        previous_cursor: String,
        timestamp: u64,
    ) -> Result<contract::V4MigrationPlan, Rejection> {
        prepare_v4_migration_plan(ctx, previous_batch, previous_cursor, timestamp)
    }

    pub fn new() -> Self {
        Self::preferred()
    }

    /// The package activated for ordinary human and agent work after the
    /// migration audit succeeds.  Its descriptor is intentionally unable to
    /// open the old aggregate schemas.
    pub fn preferred() -> Self {
        Self {
            id: contract::world_id(),
            signal_schemas: contract::signal_schemas(),
            find_schemas: crate::find::preferred_schemas(),
            find_extractors: crate::find::preferred_extractors(),
            exec_specs: vec![contract::verify_spec()],
            geometry: crate::geometry::GeometryRegistry::default(),
            schemas: {
                // The anchored description Body is the only non-v4 physical
                // binding retained by the preferred package.  It is the
                // current content store, has no readable predecessors, and its
                // extractor requires the v4 issue coordinate root.
                let mut schemas = vec![Schema {
                    id: contract::issue_schema(),
                    version: contract::ISSUE_SCHEMA_VERSION,
                    encoding: contract::issue_encoding(),
                    mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
                    readable_predecessors: vec![],
                }];
                schemas.extend(crate::records::preferred_schemas());
                schemas
            },
            package: IssuesPackage::Preferred,
        }
    }

    /// Historical package kept installable solely for the offline migration
    /// worker.  It is a separate implementation identity and therefore cannot
    /// silently lend its decoders to the preferred v4 publication.
    pub fn migrator() -> Self {
        let mut world = Self::preferred();
        world.package = IssuesPackage::Migrator;
        world.find_schemas = crate::find::schemas();
        world.find_extractors = crate::find::extractors();
        world.schemas = legacy_and_v4_schemas();
        world
    }

    /// The reviewed implementation descriptor this build ships. Its canonical
    /// id is the authority identity the founder activates and every product
    /// transaction pins.
    pub fn implementation_descriptor() -> runtime::world::Implementation {
        let world = Self::preferred();
        runtime::world::Implementation::from_registration(
            &world.descriptor(),
            4,
            *blake3::hash(b"lait.issues.policy-table.v4").as_bytes(),
            *blake3::hash(b"lait.issues.physical-records.v4").as_bytes(),
        )
    }

    /// The reviewed coordinate the migrator presents: the exact historical v3
    /// implementation — the one coordinate a live pre-v4 Space actually
    /// activated, whose digest every transaction in its store already attests.
    ///
    /// This is a pinned historical fact, not a projection of current source.
    /// The id was minted by `Implementation::from_registration` over the v3
    /// descriptor before the query/publication rebuild (#122) renumbered
    /// `MutationModel`'s canonical encoding (`Collaborative` moved from byte 1
    /// to byte 2 when `ImmutableAtomic` was inserted), so no current code path
    /// can re-derive it — and re-deriving it was the defect: a migrator
    /// minting a fresh coordinate is an implementation no Space has ever
    /// activated, and the lifecycle's read-continuity gate correctly refuses
    /// to migrate from an identity that is not installed. An implementation id
    /// names what a Space activated; the constant changes only if that history
    /// does, which is never.
    pub const MIGRATOR_IMPLEMENTATION_ID: [u8; 32] = [
        0xe4, 0x05, 0xd9, 0xb5, 0x2b, 0xa7, 0xa3, 0xac, 0xa4, 0xa1, 0xdb, 0x28, 0xf8, 0x02, 0xc4,
        0x56, 0x68, 0x90, 0x33, 0x8e, 0xa2, 0x41, 0x2f, 0xa0, 0xa7, 0x0e, 0x83, 0x2e, 0x80, 0xd0,
        0x4b, 0x56,
    ];

    /// The implementation version of [`Self::MIGRATOR_IMPLEMENTATION_ID`],
    /// which the migrator package's descriptor must also declare — the
    /// (id, version) pair is what a Space's authority compares.
    pub const MIGRATOR_IMPLEMENTATION_VERSION: u32 = 3;
}

fn legacy_and_v4_schemas() -> Vec<Schema> {
    let mut schemas = vec![
        Schema {
            id: contract::issue_schema(),
            version: contract::ISSUE_SCHEMA_VERSION,
            encoding: contract::issue_encoding(),
            mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
            // The preferred and migrator packages share this exact current
            // content binding. Historical v1/v2 are declared independently
            // below; claiming them as implicit predecessors here would make
            // the same `(World,Schema,v3)` coordinate mean two different
            // contracts across installed implementations.
            readable_predecessors: vec![],
        },
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
            readable_predecessors: vec![1],
        },
        Schema {
            id: contract::catalog_schema(),
            version: 1,
            encoding: contract::catalog_encoding(),
            mutation: MutationModel::Collaborative(CollaborativeSchema::default()),
            readable_predecessors: vec![],
        },
    ];
    schemas.extend(crate::records::schemas());
    // The migrator intentionally combines the historical package with every
    // physical v4 declaration. Keep that union explicit and canonical even if
    // a current content schema is also represented in the physical catalog;
    // Runtime admits one declaration per exact schema/version coordinate.
    schemas.sort_by(|left, right| {
        left.id
            .as_str()
            .cmp(right.id.as_str())
            .then_with(|| left.version.cmp(&right.version))
    });
    schemas.dedup_by(|left, right| left.id == right.id && left.version == right.version);
    schemas
}

#[cfg(test)]
mod package_descriptor_tests {
    use super::*;

    #[test]
    fn preferred_descriptor_cannot_open_aggregate_or_predecessor_schemas() {
        let descriptor = IssuesWorld::preferred().descriptor();
        assert_eq!(
            descriptor.limits.max_payload_bytes,
            contract::MAX_PAYLOAD_BYTES
        );
        assert_eq!(contract::MAX_PAYLOAD_BYTES, 1_572_864);
        assert!(descriptor.schemas.iter().all(|schema| {
            schema.id != contract::catalog_schema()
                && schema.id != contract::spec_schema()
                && schema.id != contract::baseline_schema()
                && schema.id != contract::relation_schema()
        }));
        let issue: Vec<_> = descriptor
            .schemas
            .iter()
            .filter(|schema| schema.id == contract::issue_schema())
            .collect();
        assert_eq!(issue.len(), 1);
        assert_eq!(issue[0].version, contract::ISSUE_SCHEMA_VERSION);
        assert!(issue[0].readable_predecessors.is_empty());
        assert!(descriptor.find_extractors.iter().all(|extractor| {
            extractor.source.name != contract::spec_schema()
                && extractor.source.name != contract::baseline_schema()
                && !(extractor.source.name == contract::issue_schema()
                    && extractor.source.version != contract::ISSUE_SCHEMA_VERSION)
        }));
    }

    /// The preferred implementation is pinned, and both places that declare
    /// its version agree.
    ///
    /// Authority compares the `(implementation id, implementation_version)`
    /// pair, and the id is a digest over the descriptor -- which includes
    /// `find_schemas`. So changing a Find schema changes the implementation
    /// WITHOUT changing the version that names it, and a Space that already
    /// activated the old pair refuses the new one. Activation is idempotent by
    /// World id, so nothing re-activates it: the World answers "unavailable" to
    /// every call, forever, with no log line naming why.
    ///
    /// That is not hypothetical. Adding `edge::MEMBER` did exactly this to a
    /// registered local World, and the only route back was removing it from the
    /// Library and adding it again under a fresh handle. The migrator's id was
    /// pinned and caught nothing, because the migrator is not what changed.
    ///
    /// **If this fails, you changed the implementation.** Bump
    /// `implementation_version` in BOTH declarations -- the descriptor below
    /// and `products/issues-runner/world.json.template` -- and update this pin.
    /// Shipping a changed implementation under an unchanged version is what
    /// strands an activated Space.
    #[test]
    fn the_preferred_implementation_is_pinned_to_the_version_that_names_it() {
        let preferred = IssuesWorld::implementation_descriptor();
        assert_eq!(
            data_encoding::HEXLOWER.encode(preferred.id().unwrap().as_ref()),
            "57d034261c84c80cf5173f0a44798c572f1d7577dd4ab2afb81e663cc114ae5e"
        );

        // The runner's manifest and the served descriptor are two declarations
        // of one number. A Space reads the manifest; authority compares the
        // descriptor. They must not drift.
        let template = include_str!("../../issues-runner/world.json.template");
        let declared: serde_json::Value = serde_json::from_str(
            &template
                .replace("${VERSION}", "0.0.0")
                .replace("${EXE}", ""),
        )
        .expect("world.json.template is JSON");
        assert_eq!(
            declared["implementation_version"].as_u64(),
            Some(u64::from(
                IssuesWorld::preferred()
                    .descriptor()
                    .implementation_version
                    .0
            )),
            "world.json.template and the served descriptor disagree about the version"
        );
    }

    #[test]
    fn migrator_has_a_distinct_identity_and_the_historical_decoders() {
        // The identity is the historical one: the exact id the live pre-v4
        // Space activated, pinned as hex because the canonical encoding that
        // minted it no longer exists in this tree. A drifted constant here
        // strands every Space that activated the real coordinate.
        assert_eq!(
            data_encoding::HEXLOWER.encode(&IssuesWorld::MIGRATOR_IMPLEMENTATION_ID),
            "e405d9b52ba7a3aca4a1db28f802c4566890338ea2412fa0a70e832e80d04b56"
        );
        assert_eq!(IssuesWorld::MIGRATOR_IMPLEMENTATION_VERSION, 3);
        let preferred = IssuesWorld::implementation_descriptor();
        assert_ne!(
            preferred.id().unwrap(),
            IssuesWorld::MIGRATOR_IMPLEMENTATION_ID
        );
        // The (id, version) pair is what authority compares, so the served
        // descriptor must declare the same version the pinned id carries.
        let descriptor = IssuesWorld::migrator().descriptor();
        assert_eq!(
            descriptor.implementation_version.0,
            IssuesWorld::MIGRATOR_IMPLEMENTATION_VERSION
        );
        assert!(descriptor
            .schemas
            .iter()
            .any(|schema| schema.id == contract::catalog_schema()));
        assert!(descriptor
            .schemas
            .iter()
            .any(|schema| schema.id == contract::spec_schema()));
        assert!(crate::record_store::migration_source_coverage_complete());
    }
}

#[cfg(test)]
mod migration_window_order_tests {
    use super::*;

    fn enumerate(phase: &str, view: &fabric::CollaborativeView) -> Vec<String> {
        let mut out = Vec::new();
        let mut after = String::new();
        while let Some(next) =
            next_lifecycle_subitem(phase, view, &after).expect("bounded migration subitem")
        {
            assert!(next > after, "migration cursor must advance monotonically");
            after = next.clone();
            out.push(next);
            assert!(out.len() < 64, "fixture must terminate");
        }
        out
    }

    #[test]
    fn every_revision_record_precedes_its_final_head_projection() {
        // The child ids sort before their parents. Record order is therefore
        // deliberately non-causal; the separate final head subitem is what
        // makes the resulting projection causal.
        let mut catalog = fabric::CollaborativeView::default();
        catalog
            .maps
            .entry("role_revisions".into())
            .or_default()
            .extend([
                ("role.example/00".into(), Vec::new()),
                ("role.example/ff".into(), Vec::new()),
            ]);
        catalog
            .maps
            .entry("workflow_revisions".into())
            .or_default()
            .extend([
                ("prj_example/00".into(), Vec::new()),
                ("prj_example/ff".into(), Vec::new()),
            ]);
        let catalog_items = enumerate("catalog", &catalog);
        let governance_head = catalog_items
            .iter()
            .position(|item| item == "07:governance-heads:role.example")
            .expect("governance head projection");
        let workflow_head = catalog_items
            .iter()
            .position(|item| item == "11z:workflow-heads:prj_example")
            .expect("workflow head projection");
        assert!(catalog_items[..governance_head]
            .iter()
            .any(|item| item == "06:governance:role.example/ff"));
        assert!(catalog_items[..workflow_head]
            .iter()
            .any(|item| item == "11:workflow:prj_example/ff"));

        let mut document = fabric::CollaborativeView::default();
        document
            .maps
            .entry("revisions".into())
            .or_default()
            .extend([("00".into(), Vec::new()), ("ff".into(), Vec::new())]);
        let spec_items = enumerate("spec", &document);
        let spec_head = spec_items
            .iter()
            .position(|item| item == "11c:heads")
            .expect("Spec head projection");
        assert!(spec_items[..spec_head]
            .iter()
            .any(|item| item == "11a:revision:ff"));
        assert_eq!(spec_items.last().map(String::as_str), Some("11d:issued"));

        let baseline_items = enumerate("baseline", &document);
        let baseline_head = baseline_items
            .iter()
            .position(|item| item == "11e:heads")
            .expect("Baseline head projection");
        assert!(baseline_items[..baseline_head]
            .iter()
            .any(|item| item == "11d:revision:ff"));
        assert_eq!(
            baseline_items.last().map(String::as_str),
            Some("11f:issued")
        );
    }

    #[test]
    fn every_phase_has_an_explicit_terminal_subitem_or_exhausts() {
        let empty = fabric::CollaborativeView::default();
        assert_eq!(enumerate("catalog", &empty), ["00:space"]);
        assert_eq!(enumerate("issue", &empty), ["20:base"]);
        assert!(enumerate("coordinates", &empty).is_empty());
        assert_eq!(enumerate("spec", &empty), ["11c:heads", "11d:issued"]);
        assert_eq!(enumerate("baseline", &empty), ["11e:heads", "11f:issued"]);
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
    /// Runtime-owned lifecycle commands committed beside the product writes.
    exec: Vec<runtime::exec::Cmd>,
    /// Stable product target binding returned to the application adapter.
    run: Option<String>,
    results: Vec<contract::ChangeResult>,
}

impl Staging {
    fn absorb_records(&mut self, batch: crate::record_store::Batch) {
        let already_declared: std::collections::BTreeSet<BodyKey> = self
            .declarations
            .iter()
            .map(|declaration| declaration.key.clone())
            .collect();
        for declaration in batch.declarations {
            if !self
                .declarations
                .iter()
                .any(|existing| existing.key == declaration.key)
            {
                self.declarations.push(declaration);
            }
        }
        for body in batch.bodies {
            if !self.bodies.contains(&body) {
                self.bodies.push(body);
            }
        }
        self.ops.extend(batch.operations.into_iter().filter(|(body, op)| {
            !(already_declared.contains(body)
                && (matches!(op, Op::Create)
                    || matches!(op, Op::RegisterSet { path, .. } if path == crate::records::roots::IDENTITY)))
        }));
        self.declared.extend(batch.content_refs);
    }

    fn for_space(space: mechanics::ids::SpaceId, declare_catalog_on_use: bool) -> Self {
        Self {
            space,
            ops: Vec::new(),
            bodies: Vec::new(),
            declarations: Vec::new(),
            declared: std::collections::BTreeMap::new(),
            declare_catalog_on_use,
            demand: None,
            exec: Vec::new(),
            run: None,
            results: Vec::new(),
        }
    }
}

impl Staging {
    /// Declarations ride ONLY the transaction that may create a Body.
    ///
    /// A Body's `(schema, version)` binding is immutable once recorded, and a
    /// later declaration must equal it exactly — so declaring the release's
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

    /// Bind one product target to its Runtime command in this same Effect.
    fn bind_run(&mut self, run: String, command: runtime::exec::Cmd) {
        self.run = Some(run);
        self.exec.push(command);
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
            exec: self.exec,
            operations: self.ops,
            bodies: self.bodies,
            effect: IssueEffect {
                doc,
                run: self.run,
                unchanged: false,
                results: self.results,
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

fn parse_run_id(raw: &str) -> Option<runtime::exec::RunId> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    Some(runtime::exec::RunId::from_bytes(
        <[u8; 16]>::try_from(bytes.as_slice()).ok()?,
    ))
}

fn parse_attempt_id(raw: &str) -> Option<runtime::exec::AttemptId> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    Some(runtime::exec::AttemptId::from_bytes(
        <[u8; 16]>::try_from(bytes.as_slice()).ok()?,
    ))
}

fn parse_build_id(raw: &str) -> Option<runtime::exec::BuildId> {
    let bytes = data_encoding::HEXLOWER.decode(raw.as_bytes()).ok()?;
    Some(runtime::exec::BuildId::from_bytes(
        <[u8; 32]>::try_from(bytes.as_slice()).ok()?,
    ))
}

/// Require two independently meaningful product decisions in one mutation.
///
/// Staging's ordinary `require` is replacement-shaped because most intents
/// choose one policy route. Check acceptance can also move workflow state, and
/// neither the verification authority nor the transition gate may lend its
/// standing to the other.
fn require_both(left: Vec<u8>, right: Vec<u8>) -> Result<Vec<u8>, Rejection> {
    if left == right {
        return Ok(left);
    }
    let left = mechanics::authorization::AuthorizationDemand::decode_canonical(&left)
        .map_err(|_| Rejection::ContractViolation)?;
    let right = mechanics::authorization::AuthorizationDemand::decode_canonical(&right)
        .map_err(|_| Rejection::ContractViolation)?;
    mechanics::authorization::AuthorizationDemand::All(vec![left, right])
        .encode_canonical()
        .map_err(|_| Rejection::LimitExceeded)
}

/// The attachment records exactly as they sit in the Body, undecoded.
///
/// The decoded list is the wrong input for anything that has to be complete: it
/// silently drops a record this build cannot read, and a dropped record is one
/// that does not count toward the cap, cannot be detached, and — worst — is
/// missing from a declaration that is supposed to name everything this Body
/// references.
fn raw_attachments(ctx: &Context<'_>, doc: &str) -> Result<BTreeMap<String, Vec<u8>>, Rejection> {
    let mut records = BTreeMap::new();
    let answer = find_source_created_page(
        ctx,
        "issue_attachment",
        doc,
        false,
        &contract::PageRequest {
            limit: u32::try_from(contract::MAX_ATTACHMENTS_PER_ISSUE + 1)
                .map_err(|_| Rejection::ContractViolation)?,
            cursor: None,
        },
        Vec::new(),
        Vec::new(),
    )?;
    if answer.next_cursor().is_some() || answer.rows().len() > contract::MAX_ATTACHMENTS_PER_ISSUE {
        return Err(Rejection::StateCorrupt);
    }
    for row in answer.rows() {
        let raw = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
        let record = crate::records::IssueAttachmentRecord::decode_canonical(&raw)
            .map_err(|_| Rejection::StateCorrupt)?;
        if record.issue != doc {
            return Err(Rejection::StateCorrupt);
        }
        if !record.tombstone
            && records
                .insert(
                    record.id.clone(),
                    serde_json::to_vec(&record).map_err(|_| Rejection::StateCorrupt)?,
                )
                .is_some()
        {
            return Err(Rejection::StateCorrupt);
        }
    }
    Ok(records)
}

fn raw_checks(ctx: &Context<'_>, doc: &str) -> Result<BTreeMap<String, Vec<u8>>, Rejection> {
    let mut records = BTreeMap::new();
    let answer = find_source_created_page(
        ctx,
        "issue_check",
        doc,
        false,
        &contract::PageRequest {
            limit: u32::try_from(contract::MAX_CHECKS_PER_ISSUE + 1)
                .map_err(|_| Rejection::ContractViolation)?,
            cursor: None,
        },
        Vec::new(),
        Vec::new(),
    )?;
    if answer.next_cursor().is_some() || answer.rows().len() > contract::MAX_CHECKS_PER_ISSUE {
        return Err(Rejection::StateCorrupt);
    }
    for row in answer.rows() {
        let raw = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
        let record = crate::records::IssueCheckRecord::decode_canonical(&raw)
            .map_err(|_| Rejection::StateCorrupt)?;
        if record.issue != doc
            || records
                .insert(
                    record.run,
                    serde_json::to_vec(&record.check).map_err(|_| Rejection::StateCorrupt)?,
                )
                .is_some()
        {
            return Err(Rejection::StateCorrupt);
        }
    }
    Ok(records)
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
        exec: Vec::new(),
        operations: vec![],
        bodies: vec![],
        effect: IssueEffect {
            doc,
            run: None,
            unchanged: true,
            results: Vec::new(),
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
fn checked_catalog_view(
    ctx: &Context<'_>,
) -> Result<Option<runtime::world::CollaborativeBody>, Rejection> {
    let expected = catalog_key(&ctx.principal().space);
    let catalogs = ctx.bodies_with_schema(&contract::world_id(), &contract::catalog_schema());
    match catalogs.as_slice() {
        [] => Ok(None),
        [one] if one == &expected => ctx.read_collaborative(&expected).map_err(Into::into),
        _ => Err(Rejection::StateCorrupt),
    }
}

/// Load the catalog state from the committed snapshot (integrity-checked).
fn catalog_state(ctx: &Context<'_>) -> Result<CatalogState, Rejection> {
    let view = checked_catalog_view(ctx)?;
    Ok(CatalogState::from_view(view.as_deref()))
}

fn migration_cursor_body(cursor: &str) -> Option<(String, Option<BodyKey>, String)> {
    if cursor.is_empty() {
        return Some((String::new(), None, String::new()));
    }
    let (phase, body, subitem) = contract::V4MigrationWindow::parse_cursor(cursor)?;
    Some((
        phase,
        body.map(|body| BodyKey::new(contract::world_id(), body)),
        subitem,
    ))
}

fn lifecycle_window_digest(
    ctx: &Context<'_>,
    phase: &str,
    body: Option<&BodyKey>,
    subitem: &str,
) -> Result<[u8; 32], Rejection> {
    let source = ctx.lifecycle_source().ok_or(Rejection::ContractViolation)?;
    let mut hasher = blake3::Hasher::new_derive_key("lait.issues.v4-migration-window.v1");
    hasher.update(
        &postcard::to_stdvec(&source.publication.publication)
            .map_err(|_| Rejection::StateCorrupt)?,
    );
    hasher.update(&postcard::to_stdvec(&source.frontier).map_err(|_| Rejection::StateCorrupt)?);
    hasher.update(phase.as_bytes());
    hasher.update(subitem.as_bytes());
    if let Some(body) = body {
        hasher.update(&body.body.as_bytes());
        let view = ctx
            .read_lifecycle_source_collaborative(body)
            .map_err(Rejection::BodyRead)?
            .ok_or(Rejection::StateCorrupt)?;
        let encoded = postcard::to_stdvec(&*view).map_err(|_| Rejection::StateCorrupt)?;
        if encoded.len() > contract::MAX_PAYLOAD_BYTES as usize {
            return Err(Rejection::LimitExceeded);
        }
        hasher.update(&encoded);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn select_subitem(best: &mut Option<String>, after: &str, candidate: String) {
    if candidate.as_str() > after
        && best
            .as_ref()
            .is_none_or(|current| candidate.as_str() < current.as_str())
    {
        *best = Some(candidate);
    }
}

fn next_lifecycle_subitem(
    phase: &str,
    view: &fabric::CollaborativeView,
    after: &str,
) -> Result<Option<String>, Rejection> {
    let mut best = None;
    match phase {
        "catalog" => {
            select_subitem(&mut best, after, "00:space".into());
            for (path, prefix) in [
                ("labels", "02:label"),
                ("roles", "05:governance"),
                ("role_revisions", "06:governance"),
                ("projects", "10:project"),
                ("workflow_revisions", "11:workflow"),
                ("project_milestones", "12:milestone"),
                ("cycles", "13:cycle"),
                ("project_updates", "40:update"),
                ("initiatives", "50:initiative"),
                ("teams", "60:team"),
                ("triage", "70:triage"),
            ] {
                if let Some(entries) = view.maps.get(path) {
                    for key in entries.keys() {
                        select_subitem(&mut best, after, format!("{prefix}:{key}"));
                    }
                }
            }
            if let Some(entries) = view.maps.get("initiatives") {
                for (id, raw) in entries {
                    let initiative: Initiative =
                        serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
                    if initiative.id != *id {
                        return Err(Rejection::StateCorrupt);
                    }
                    for project in &initiative.projects {
                        select_subitem(
                            &mut best,
                            after,
                            format!("51:initiative-project:{id}:{project}"),
                        );
                    }
                }
            }
            if let Some(entries) = view.maps.get("teams") {
                for (id, raw) in entries {
                    let team: Team =
                        serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
                    if team.id != *id {
                        return Err(Rejection::StateCorrupt);
                    }
                    for member in &team.members {
                        select_subitem(&mut best, after, format!("61:team-member:{id}:{member}"));
                    }
                }
            }
            let mut governance = BTreeSet::new();
            if let Some(entries) = view.maps.get("roles") {
                governance.extend(entries.keys().cloned());
            }
            if let Some(entries) = view.maps.get("role_revisions") {
                for key in entries.keys() {
                    let (role, _) = key.rsplit_once('/').ok_or(Rejection::StateCorrupt)?;
                    governance.insert(role.into());
                }
            }
            for role in governance {
                select_subitem(&mut best, after, format!("07:governance-heads:{role}"));
            }
            if let Some(entries) = view.maps.get("workflow_revisions") {
                let mut projects = BTreeSet::new();
                for key in entries.keys() {
                    let (project, _) = key.rsplit_once('/').ok_or(Rejection::StateCorrupt)?;
                    projects.insert(project);
                }
                for project in projects {
                    select_subitem(&mut best, after, format!("11z:workflow-heads:{project}"));
                }
            }
        }
        "coordinates" => {
            for (path, prefix) in [("seqs", "14:identity"), ("tombstones", "15:tombstone")] {
                if let Some(entries) = view.maps.get(path) {
                    for key in entries.keys() {
                        select_subitem(&mut best, after, format!("{prefix}:{key}"));
                    }
                }
            }
            for (path, entries) in &view.lists {
                if path.starts_with("board/") {
                    for (ordinal, _entry) in entries.iter().enumerate() {
                        select_subitem(&mut best, after, format!("16:board:{path}:{ordinal:020}"));
                    }
                }
            }
            for (ordinal, _node) in view
                .trees
                .get(contract::HIERARCHY_PATH)
                .into_iter()
                .flatten()
                .enumerate()
            {
                select_subitem(&mut best, after, format!("30:tree:{ordinal:020}"));
            }
            if let Some(parents) = view.maps.get("parents") {
                for child in parents.keys() {
                    select_subitem(&mut best, after, format!("30:map:{child}"));
                }
            }
            if let Some(edges) = view.maps.get("edges") {
                for key in edges.keys() {
                    select_subitem(&mut best, after, format!("31:link:{key}"));
                }
            }
            if let Some(triage) = view.maps.get("triage") {
                for (id, raw) in triage {
                    let item: TriageItem =
                        serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
                    if item.id != *id {
                        return Err(Rejection::StateCorrupt);
                    }
                    if !item.outcome.is_empty() {
                        select_subitem(&mut best, after, format!("71:triage-decision:{id}"));
                    }
                }
            }
        }
        "issue" => {
            select_subitem(&mut best, after, "20:base".into());
            for (path, prefix) in [("attachments", "24:attachment"), ("checks", "25:check")] {
                if let Some(entries) = view.maps.get(path) {
                    for key in entries.keys() {
                        select_subitem(&mut best, after, format!("{prefix}:{key}"));
                    }
                }
            }
            for (path, values) in &view.sets {
                let prefix = if path == "assignees" {
                    Some("23:relation:assignee".to_string())
                } else if path == "followers" {
                    Some("23:relation:follower".to_string())
                } else if path == "labels" {
                    Some("23:relation:label".to_string())
                } else if path == contract::REACTIONS_PATH {
                    Some("22:reaction:current".to_string())
                } else {
                    path.strip_prefix("reactions/").map(return_reaction_prefix)
                };
                if let Some(prefix) = prefix {
                    for (ordinal, _value) in values.iter().enumerate() {
                        select_subitem(&mut best, after, format!("{prefix}:{ordinal:020}"));
                    }
                }
            }
            for (kind, value) in [
                ("milestone", view.registers.get("milestone")),
                ("cycle", view.registers.get("cycle")),
                ("baseline", view.registers.get("baseline")),
            ] {
                if value.is_some_and(|value| !value.is_empty()) {
                    select_subitem(&mut best, after, format!("23:relation:{kind}:single"));
                }
            }
            for (path, prefix) in [("comments", "21:comment"), ("events", "26:activity")] {
                for (ordinal, _entry) in view.lists.get(path).into_iter().flatten().enumerate() {
                    select_subitem(&mut best, after, format!("{prefix}:list:{ordinal:020}"));
                }
            }
            for (ordinal, _node) in view.trees.get("comments").into_iter().flatten().enumerate() {
                select_subitem(&mut best, after, format!("21:comment:tree:{ordinal:020}"));
            }
            for (ordinal, _entry) in view
                .logs
                .get(contract::EVENTS_PATH)
                .into_iter()
                .flat_map(|log| &log.entries)
                .enumerate()
            {
                select_subitem(&mut best, after, format!("26:activity:log:{ordinal:020}"));
            }
        }
        "spec" => {
            if let Some(revisions) = view.maps.get("revisions") {
                for key in revisions.keys() {
                    select_subitem(&mut best, after, format!("11a:revision:{key}"));
                }
            }
            for (ordinal, _value) in view
                .sets
                .get("observations")
                .into_iter()
                .flatten()
                .enumerate()
            {
                select_subitem(&mut best, after, format!("11b:observation:{ordinal:020}"));
            }
            select_subitem(&mut best, after, "11c:heads".into());
            select_subitem(&mut best, after, "11d:issued".into());
        }
        "baseline" => {
            if let Some(revisions) = view.maps.get("revisions") {
                for key in revisions.keys() {
                    select_subitem(&mut best, after, format!("11d:revision:{key}"));
                }
            }
            select_subitem(&mut best, after, "11e:heads".into());
            select_subitem(&mut best, after, "11f:issued".into());
        }
        _ => return Err(Rejection::InvalidRequest),
    }
    Ok(best)
}

fn return_reaction_prefix(comment: &str) -> String {
    format!("22:reaction:{comment}")
}

fn first_lifecycle_body(
    ctx: &Context<'_>,
    schema: &replica::body::SchemaId,
    after: Option<&BodyKey>,
) -> Result<Option<BodyKey>, Rejection> {
    let page = ctx
        .lifecycle_source_body_keys_page_with_schema(&contract::world_id(), schema, after, 1)
        .map_err(Rejection::BodyRead)?;
    Ok(page.into_iter().next())
}

/// Select one compact exact-source migration window outside mutation-lane
/// admission. Exactly one logical source fact is selected; a later callback
/// reopens this one Body and recomputes the digest before staging it.
pub fn prepare_v4_migration_plan(
    ctx: &Context<'_>,
    previous_batch: u64,
    previous_cursor: String,
    timestamp: u64,
) -> Result<contract::V4MigrationPlan, Rejection> {
    let source = ctx.lifecycle_source().ok_or(Rejection::ContractViolation)?;
    if timestamp == 0 || previous_cursor.len() > 512 {
        return Err(Rejection::InvalidRequest);
    }
    let (previous_phase, previous_body, previous_subitem) =
        migration_cursor_body(&previous_cursor).ok_or(Rejection::InvalidRequest)?;
    if previous_phase == contract::V4MigrationWindow::TERMINAL_PHASE {
        return Err(Rejection::Conflict);
    }
    let phases = [
        ("catalog", contract::catalog_schema()),
        ("issue", contract::issue_schema()),
        ("coordinates", contract::catalog_schema()),
        ("spec", contract::spec_schema()),
        ("baseline", contract::baseline_schema()),
    ];
    let start = phases
        .iter()
        .position(|(phase, _)| *phase == previous_phase)
        .unwrap_or(0);
    if !previous_phase.is_empty()
        && phases
            .get(start)
            .is_none_or(|(phase, _)| *phase != previous_phase)
    {
        return Err(Rejection::InvalidRequest);
    }
    let mut selected = None;
    for (index, (candidate_phase, schema)) in phases.iter().enumerate().skip(start) {
        if previous_phase == *candidate_phase {
            let body = previous_body.as_ref().ok_or(Rejection::InvalidRequest)?;
            let view = ctx
                .read_lifecycle_source_collaborative(body)
                .map_err(Rejection::BodyRead)?
                .ok_or(Rejection::StateCorrupt)?;
            if let Some(subitem) =
                next_lifecycle_subitem(candidate_phase, &view, &previous_subitem)?
            {
                selected = Some((*candidate_phase, Some(body.clone()), subitem));
                break;
            }
        }
        let after = (previous_phase == *candidate_phase)
            .then_some(previous_body.as_ref())
            .flatten();
        if let Some(body) = first_lifecycle_body(ctx, schema, after)? {
            if matches!(*candidate_phase, "catalog" | "coordinates") {
                let expected = contract::catalog_key(&ctx.principal().space);
                if body != expected || first_lifecycle_body(ctx, schema, Some(&body))?.is_some() {
                    return Err(Rejection::StateCorrupt);
                }
            }
            let view = ctx
                .read_lifecycle_source_collaborative(&body)
                .map_err(Rejection::BodyRead)?
                .ok_or(Rejection::StateCorrupt)?;
            let subitem = next_lifecycle_subitem(candidate_phase, &view, "")?
                .unwrap_or_else(|| "$empty".into());
            selected = Some((*candidate_phase, Some(body), subitem));
            break;
        }
        if index == start && previous_phase == *candidate_phase && previous_body.is_none() {
            return Err(Rejection::InvalidRequest);
        }
    }
    let (phase, body, subitem) = selected.unwrap_or((
        contract::V4MigrationWindow::TERMINAL_PHASE,
        None,
        String::new(),
    ));
    let cursor = contract::V4MigrationWindow::render_cursor(&phase, body.as_ref(), &subitem)
        .ok_or(Rejection::StateCorrupt)?;
    let digest = lifecycle_window_digest(ctx, phase, body.as_ref(), &subitem)?;
    let plan = contract::V4MigrationPlan {
        version: contract::V4MigrationPlan::VERSION,
        source: source.publication.publication,
        source_frontier: source.frontier,
        previous_batch,
        previous_cursor,
        window: contract::V4MigrationWindow {
            phase: phase.into(),
            body,
            subitem,
            digest,
            cursor,
        },
        timestamp,
    };
    plan.valid().then_some(plan).ok_or(Rejection::StateCorrupt)
}

/// The v4 catalog projection. Record Bodies are authoritative; the legacy
/// singleton only supplies fields not yet represented by the migration while
/// its completion marker is false.
fn live_catalog(ctx: &Context<'_>) -> Result<CatalogState, Rejection> {
    let mut catalog = catalog_state(ctx)?;
    crate::record_store::apply_catalog(ctx, &mut catalog)?;
    Ok(catalog)
}

fn issue_read_set(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    project: Option<&str>,
) -> Result<IssueReadSet, Rejection> {
    // Whether the read is scoped to a project or not, the row wanted is the
    // issue node: it carries both the project and the id. The project-scoped
    // branch used to look for `issue_placement`, which no extractor in this
    // package emits, so scoping this read to a project answered with nothing.
    // Only the unscoped caller exists today, which is the only reason that
    // never showed.
    let rows = find_rows_equal(
        ctx,
        project.map_or(crate::find::field::KIND, |_| crate::find::field::PROJECT),
        project.unwrap_or("issue"),
    )?;
    let docs = rows
        .into_iter()
        .filter(|row| result_text(row, crate::find::field::KIND).as_deref() == Some("issue"))
        .map(|row| result_text(&row, crate::find::field::ID).ok_or(Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    issue_read_docs(ctx, catalog, docs)
}

fn issue_read_docs(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    docs: impl IntoIterator<Item = String>,
) -> Result<IssueReadSet, Rejection> {
    let mut issues = BTreeMap::new();
    let mut aliases = DerivedAliases::default();
    let mut coordinates = BTreeMap::new();
    for doc in docs {
        let mut issue = issue_state(ctx, &doc).ok_or(Rejection::StateCorrupt)?;
        if let Some(coordinate) = crate::record_store::issue_coordinate_for(ctx, &doc)? {
            crate::record_store::apply_issue_coordinate(&mut issue, &coordinate);
            let project = catalog
                .projects
                .get(&coordinate.placement.project)
                .ok_or(Rejection::StateCorrupt)?;
            let rendered = coordinate
                .identity
                .alias
                .render(&project.key)
                .map_err(|_| Rejection::StateCorrupt)?;
            if aliases
                .by_alias
                .insert(rendered.to_ascii_lowercase(), doc.clone())
                .is_some()
            {
                return Err(Rejection::StateCorrupt);
            }
            aliases.by_doc.insert(doc.clone(), rendered);
            coordinates.insert(doc.clone(), coordinate);
        }
        aliases.canonical.insert(
            doc.clone(),
            DocId::parse(&doc)
                .map(|id| id.short(7))
                .unwrap_or_else(|| doc.clone()),
        );
        issues.insert(doc, Arc::new(issue));
    }
    crate::record_store::apply_issue_catalog(catalog, &coordinates);
    Ok(IssueReadSet { aliases, issues })
}

fn issue_detail_read(
    ctx: &Context<'_>,
    doc: &str,
) -> Result<(CatalogState, IssueReadSet), Rejection> {
    let coordinate =
        crate::record_store::issue_coordinate_for(ctx, doc)?.ok_or(Rejection::InvalidRequest)?;
    let mut catalog = CatalogState::default();
    crate::record_store::apply_project(ctx, &mut catalog, &coordinate.placement.project)?;
    let read = issue_read_docs(ctx, &mut catalog, [doc.to_owned()])?;
    let labels = read
        .issues
        .get(doc)
        .map(|issue| issue.labels.clone())
        .unwrap_or_default();
    for label in labels {
        crate::record_store::apply_label(ctx, &mut catalog, &label)?;
    }
    Ok((catalog, read))
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

fn load_labels_for_write(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    ids: impl IntoIterator<Item = String>,
    names: impl IntoIterator<Item = String>,
) -> Result<(), Rejection> {
    for id in ids {
        crate::record_store::apply_label(ctx, catalog, &id)?;
    }
    for name in names {
        let canonical = name.trim().to_ascii_lowercase();
        if let Some(row) = unique_find_row(
            ctx,
            crate::find::field::EXACT_NAME,
            &canonical,
            "label",
            None,
        )? {
            let id = result_text(&row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
            crate::record_store::apply_label(ctx, catalog, &id)?;
        }
    }
    Ok(())
}

fn project_key_exists(ctx: &Context<'_>, key: &str) -> Result<bool, Rejection> {
    Ok(unique_find_row(
        ctx,
        crate::find::field::ENTITY_KEY,
        &key.trim().to_ascii_uppercase(),
        "project",
        None,
    )?
    .is_some())
}

fn find_exists_bytes(ctx: &Context<'_>, field: &str, value: Vec<u8>) -> Result<bool, Rejection> {
    use runtime::find as find_api;
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: 2,
        edges_visited: 1,
        nodes_visited: 2,
        paths_retained: 1,
        candidates_per_branch: 1,
        score_evaluations: 1,
        projected_bytes: 1_024,
        packed_tokens: 4,
        wall_millis: 250,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![find_api::Step {
                id: seek,
                input: Vec::new(),
                op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                    field: crate::find::field_ref(field),
                    test: find_api::Test::Equal,
                    value: find_api::Atom::Bytes(value),
                })),
                bound,
            }],
            output: seek,
            bound,
            page_size: 1,
            cursor: None,
        })
        .map_err(find_rejection)?;
    Ok(answer.matched_total().unwrap_or(0) != 0)
}

fn find_rows_equal(
    ctx: &Context<'_>,
    field: &str,
    value: &str,
) -> Result<Vec<runtime::find::ResultRow>, Rejection> {
    use runtime::find as find_api;
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: 20_000,
        edges_visited: 1,
        nodes_visited: 20_000,
        paths_retained: 1,
        candidates_per_branch: 10_000,
        score_evaluations: 1,
        projected_bytes: 8 * 1_024 * 1_024,
        packed_tokens: 32_768,
        wall_millis: 5_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::TITLE,
        crate::find::field::RELATION_KIND,
        crate::find::field::SOURCE_ID,
        crate::find::field::TARGET_ID,
        crate::find::field::STATE,
        crate::find::field::AUTHOR,
        crate::find::field::CREATED_AT,
        crate::find::field::EXACT_NAME,
        crate::find::field::ENTITY_KEY,
        crate::find::field::PROJECT,
        crate::find::field::TOMBSTONE,
        crate::find::field::ALIAS_COORDINATE,
        crate::find::field::REVISION,
        crate::find::field::HEAD_REVISIONS,
        crate::find::field::CONFLICTED,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                        field: crate::find::field_ref(field),
                        test: find_api::Test::Equal,
                        value: find_api::Atom::Text(value.into()),
                    })),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![seek],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: 10_000,
            cursor: None,
        })
        // Through the canonical mapping, like every other Find call site. A
        // blanket `StateCorrupt` here said "your stored state is corrupt"
        // for every reason a Find can fail — including the capability simply
        // not being available, which is not a statement about the store at
        // all. Corrupt is the most severe verdict this World can return and
        // the one thing a Space cannot recover from by retrying.
        .map_err(find_rejection)?;
    if answer.next_cursor().is_some() {
        // The legacy Issue DTO has no continuation coordinate. Truncating
        // would silently lie, so oversized enrichment is explicit until the
        // paged Issue-detail adapter replaces it.
        return Err(Rejection::LimitExceeded);
    }
    Ok(answer.rows().to_vec())
}

fn page_cursor(
    request: &contract::PageRequest,
) -> Result<Option<runtime::find::Cursor>, Rejection> {
    if !request.validate() {
        return Err(Rejection::InvalidRequest);
    }
    request
        .cursor
        .as_ref()
        .map(|cursor| {
            let (_, cursor) =
                contract::decode_page_cursor(cursor).ok_or(Rejection::InvalidRequest)?;
            data_encoding::BASE64URL_NOPAD
                .decode(cursor.as_bytes())
                .map_err(|_| Rejection::InvalidRequest)
                .and_then(|bytes| {
                    runtime::find::Cursor::new(bytes).map_err(|_| Rejection::InvalidRequest)
                })
        })
        .transpose()
}

fn page_from_answer<T>(answer: &runtime::find::Answer, items: Vec<T>) -> contract::Page<T> {
    let publication = answer.coordinates().world_publication();
    contract::Page {
        publication: publication.clone(),
        items,
        next_cursor: answer.next_cursor().and_then(|cursor| {
            contract::encode_page_cursor(
                publication,
                data_encoding::BASE64URL_NOPAD.encode(cursor.as_bytes()),
            )
        }),
        exact_total: answer.matched_total(),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct InboxPageCursor {
    filter: [u8; 32],
    find: String,
}

fn inbox_filter_digest(exclude_device: Option<&str>) -> [u8; 32] {
    blake3::derive_key(
        "lait.issues.inbox-filter.v1",
        exclude_device.unwrap_or_default().as_bytes(),
    )
}

fn inbox_find_request(
    request: &contract::PageRequest,
    exclude_device: Option<&str>,
) -> Result<contract::PageRequest, Rejection> {
    if !request.validate() {
        return Err(Rejection::InvalidRequest);
    }
    let cursor = request
        .cursor
        .as_ref()
        .map(|cursor| {
            let (publication, cursor) =
                contract::decode_page_cursor(cursor).ok_or(Rejection::InvalidRequest)?;
            let bytes = data_encoding::BASE64URL_NOPAD
                .decode(cursor.as_bytes())
                .map_err(|_| Rejection::InvalidRequest)?;
            let cursor: InboxPageCursor =
                postcard::from_bytes(&bytes).map_err(|_| Rejection::InvalidRequest)?;
            if cursor.filter != inbox_filter_digest(exclude_device) {
                return Err(Rejection::InvalidRequest);
            }
            contract::encode_page_cursor(publication, cursor.find).ok_or(Rejection::InvalidRequest)
        })
        .transpose()?;
    Ok(contract::PageRequest {
        limit: request.limit,
        cursor,
    })
}

fn inbox_next_cursor(
    answer: &runtime::find::Answer,
    exclude_device: Option<&str>,
) -> Option<String> {
    let find = answer
        .next_cursor()
        .map(|cursor| data_encoding::BASE64URL_NOPAD.encode(cursor.as_bytes()))?;
    let cursor = InboxPageCursor {
        filter: inbox_filter_digest(exclude_device),
        find,
    };
    let filtered = postcard::to_stdvec(&cursor)
        .ok()
        .map(|bytes| data_encoding::BASE64URL_NOPAD.encode(&bytes))?;
    contract::encode_page_cursor(answer.coordinates().world_publication(), filtered)
}

fn find_rejection(failure: runtime::find::Failure) -> Rejection {
    match failure {
        runtime::find::Failure::Invalid(_) => Rejection::InvalidRequest,
        runtime::find::Failure::PrincipalDenied => {
            Rejection::Denied(runtime::world::DeniedCause::ReadRefused)
        }
        runtime::find::Failure::NoActiveImplementation => Rejection::NoActiveImplementation,
        runtime::find::Failure::ImplementationUnavailable => Rejection::ImplementationUnavailable,
        runtime::find::Failure::AuthorityUnavailable(_) => Rejection::ImplementationUnavailable,
        runtime::find::Failure::Interrupted
        | runtime::find::Failure::PolicyExceeded
        | runtime::find::Failure::PublicationUnavailable
        | runtime::find::Failure::PublicationExpired
        | runtime::find::Failure::PaginationUnsupported
        | runtime::find::Failure::CursorCapacityExceeded
        | runtime::find::Failure::Unavailable => Rejection::LimitExceeded,
    }
}

/// Bounded collection primitive used by every ordinary Issues page. It is an
/// adapter over the same publication Corpus as the generic Find invocation;
/// the product contributes only stable kinds, filters and packed fields.
fn find_kind_page(
    ctx: &Context<'_>,
    kind: &str,
    project: Option<&str>,
    request: &contract::PageRequest,
    additional: Vec<runtime::find::Predicate>,
    mut fields: Vec<runtime::find::FieldRef>,
) -> Result<runtime::find::Answer, Rejection> {
    use runtime::find as find_api;
    let cursor = page_cursor(request)?;
    let candidates = u64::from(request.limit).saturating_mul(8).max(64);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: candidates.saturating_mul(8),
        edges_visited: candidates,
        nodes_visited: candidates,
        paths_retained: candidates,
        candidates_per_branch: candidates,
        score_evaluations: candidates,
        projected_bytes: u64::from(request.limit).saturating_mul(64 * 1_024),
        packed_tokens: u64::from(request.limit).saturating_mul(4_096),
        wall_millis: 5_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let mut steps = vec![find_api::Step {
        id: seek,
        input: Vec::new(),
        op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
            field: if project.is_some() {
                crate::find::field_ref(crate::find::field::KIND_PROJECT)
            } else {
                crate::find::field_ref(crate::find::field::KIND)
            },
            test: find_api::Test::Equal,
            value: project.map_or_else(
                || find_api::Atom::Text(kind.into()),
                |project| find_api::Atom::Bytes(crate::find::composite_key([kind, project])),
            ),
        })),
        bound,
    }];
    let mut output = seek;
    let has_additional = !additional.is_empty();
    if has_additional {
        let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
        steps.push(find_api::Step {
            id: keep,
            input: vec![seek],
            op: find_api::Op::Keep(find_api::Keep {
                predicates: additional,
            }),
            bound,
        });
        output = keep;
    }
    let pack =
        find_api::StepId::new(if has_additional { 3 } else { 2 }).ok_or(Rejection::StateCorrupt)?;
    fields.extend([
        crate::find::field_ref(crate::find::field::ID),
        crate::find::field_ref(crate::find::field::KIND),
        crate::find::field_ref(crate::find::field::SOURCE_ID),
    ]);
    fields.sort();
    fields.dedup();
    steps.push(find_api::Step {
        id: pack,
        input: vec![output],
        op: find_api::Op::Pack(find_api::Pack { fields }),
        bound,
    });
    ctx.find(find_api::Query {
        schema: crate::find::entity_schema_ref(),
        publication: ctx.world_publication_id().map(|id| id.publication),
        mode: find_api::Mode::Exact,
        steps,
        output: pack,
        bound,
        page_size: request.limit,
        cursor,
    })
    .map_err(find_rejection)
}

fn find_field_page(
    ctx: &Context<'_>,
    field: &str,
    value: runtime::find::Atom,
    request: &contract::PageRequest,
    additional: Vec<runtime::find::Predicate>,
    fields: Vec<runtime::find::FieldRef>,
) -> Result<runtime::find::Answer, Rejection> {
    find_field_test_page(
        ctx,
        field,
        runtime::find::Test::Equal,
        value,
        request,
        additional,
        fields,
    )
}

fn find_field_test_page(
    ctx: &Context<'_>,
    field: &str,
    test: runtime::find::Test,
    value: runtime::find::Atom,
    request: &contract::PageRequest,
    additional: Vec<runtime::find::Predicate>,
    mut fields: Vec<runtime::find::FieldRef>,
) -> Result<runtime::find::Answer, Rejection> {
    use runtime::find as find_api;
    let cursor = page_cursor(request)?;
    let candidates = u64::from(request.limit).saturating_mul(8).max(64);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: candidates.saturating_mul(8),
        edges_visited: candidates,
        nodes_visited: candidates,
        paths_retained: candidates,
        candidates_per_branch: candidates,
        score_evaluations: candidates,
        // This helper packs only bounded identity/relation coordinates; large
        // text values are hydrated from the returned source Body by the
        // dedicated detail paths. Keep the 129th cap-detection row inside the
        // schema's honest 8 MiB grant without claiming a 64 KiB tuple.
        projected_bytes: u64::from(request.limit).saturating_mul(16 * 1_024),
        packed_tokens: u64::from(request.limit).saturating_mul(4_096),
        wall_millis: 5_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let mut output = seek;
    let mut steps = vec![find_api::Step {
        id: seek,
        input: Vec::new(),
        op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
            field: crate::find::field_ref(field),
            test,
            value,
        })),
        bound,
    }];
    let has_additional = !additional.is_empty();
    if has_additional {
        let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
        steps.push(find_api::Step {
            id: keep,
            input: vec![seek],
            op: find_api::Op::Keep(find_api::Keep {
                predicates: additional,
            }),
            bound,
        });
        output = keep;
    }
    let pack =
        find_api::StepId::new(if has_additional { 3 } else { 2 }).ok_or(Rejection::StateCorrupt)?;
    fields.extend([
        crate::find::field_ref(crate::find::field::ID),
        crate::find::field_ref(crate::find::field::KIND),
        crate::find::field_ref(crate::find::field::SOURCE_ID),
    ]);
    fields.sort();
    fields.dedup();
    steps.push(find_api::Step {
        id: pack,
        input: vec![output],
        op: find_api::Op::Pack(find_api::Pack { fields }),
        bound,
    });
    ctx.find(find_api::Query {
        schema: crate::find::entity_schema_ref(),
        publication: ctx.world_publication_id().map(|id| id.publication),
        mode: find_api::Mode::Exact,
        steps,
        output: pack,
        bound,
        page_size: request.limit,
        cursor,
    })
    .map_err(find_rejection)
}

fn issue_relation_targets(
    ctx: &Context<'_>,
    doc: &str,
    kind: &str,
    maximum: usize,
) -> Result<std::collections::BTreeSet<String>, Rejection> {
    let limit = u32::try_from(maximum.saturating_add(1)).map_err(|_| Rejection::LimitExceeded)?;
    let request = contract::PageRequest {
        limit,
        cursor: None,
    };
    let answer = find_field_page(
        ctx,
        crate::find::field::RELATION_SOURCE_KIND,
        runtime::find::Atom::Bytes(crate::find::composite_key([kind, doc])),
        &request,
        Vec::new(),
        [
            crate::find::field::RELATION_KIND,
            crate::find::field::TARGET_ID,
        ]
        .into_iter()
        .map(crate::find::field_ref)
        .collect(),
    )?;
    if answer.next_cursor().is_some()
        || answer
            .matched_total()
            .is_some_and(|count| count > maximum as u64)
    {
        return Err(Rejection::StateCorrupt);
    }
    let mut targets = std::collections::BTreeSet::new();
    for row in answer.rows() {
        if result_text(row, crate::find::field::KIND).as_deref() != Some("relation")
            || result_text(row, crate::find::field::RELATION_KIND).as_deref() != Some(kind)
        {
            return Err(Rejection::StateCorrupt);
        }
        targets.insert(
            result_text(row, crate::find::field::TARGET_ID).ok_or(Rejection::StateCorrupt)?,
        );
    }
    if targets.len() > maximum {
        return Err(Rejection::StateCorrupt);
    }
    Ok(targets)
}

fn issue_notification_audience(
    ctx: &Context<'_>,
    doc: &str,
) -> Result<std::collections::BTreeSet<String>, Rejection> {
    let mut audience = issue_relation_targets(ctx, doc, "assignee", contract::MAX_ISSUE_ASSIGNEES)?;
    audience.extend(issue_relation_targets(
        ctx,
        doc,
        "follower",
        contract::MAX_ISSUE_FOLLOWERS,
    )?);
    if audience.len() > contract::MAX_ISSUE_AUDIENCE {
        return Err(Rejection::StateCorrupt);
    }
    Ok(audience)
}

/// Newest-first page for record feeds. The ordered composite posting scopes
/// the scan to one kind (and optionally one project) before visiting rows, so
/// a tracker with millions of unrelated activity records does not tax a
/// one-project update or history pull.
fn find_created_page(
    ctx: &Context<'_>,
    kind: &str,
    project: Option<&str>,
    request: &contract::PageRequest,
    mut additional: Vec<runtime::find::Predicate>,
    mut fields: Vec<runtime::find::FieldRef>,
) -> Result<runtime::find::Answer, Rejection> {
    use runtime::find as find_api;
    let cursor = page_cursor(request)?;
    let candidates = u64::from(request.limit).saturating_mul(4).max(64);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: candidates.saturating_mul(8),
        edges_visited: candidates,
        nodes_visited: candidates,
        paths_retained: candidates,
        candidates_per_branch: candidates,
        score_evaluations: candidates,
        projected_bytes: u64::from(request.limit).saturating_mul(64 * 1_024),
        packed_tokens: u64::from(request.limit).saturating_mul(4_096),
        wall_millis: 5_000,
    };
    let ordered_field = project.map_or(crate::find::field::KIND_CREATED_DESC, |_| {
        crate::find::field::KIND_PROJECT_CREATED_DESC
    });
    let lower = project.map_or_else(
        || crate::find::composite_key([kind]),
        |project| crate::find::composite_key([kind, project]),
    );
    let upper =
        crate::find::composite_prefix_upper(lower.clone()).ok_or(Rejection::StateCorrupt)?;
    additional.insert(
        0,
        find_api::Predicate {
            field: crate::find::field_ref(ordered_field),
            test: find_api::Test::Less,
            value: find_api::Atom::Bytes(upper),
        },
    );
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    fields.extend([
        crate::find::field_ref(crate::find::field::ID),
        crate::find::field_ref(crate::find::field::KIND),
        crate::find::field_ref(crate::find::field::SOURCE_ID),
    ]);
    fields.sort();
    fields.dedup();
    ctx.find(find_api::Query {
        schema: crate::find::entity_schema_ref(),
        publication: ctx.world_publication_id().map(|id| id.publication),
        mode: find_api::Mode::Exact,
        steps: vec![
            find_api::Step {
                id: seek,
                input: Vec::new(),
                op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                    field: crate::find::field_ref(ordered_field),
                    test: find_api::Test::GreaterOrEqual,
                    value: find_api::Atom::Bytes(lower),
                })),
                bound,
            },
            find_api::Step {
                id: keep,
                input: vec![seek],
                op: find_api::Op::Keep(find_api::Keep {
                    predicates: additional,
                }),
                bound,
            },
            find_api::Step {
                id: pack,
                input: vec![keep],
                op: find_api::Op::Pack(find_api::Pack { fields }),
                bound,
            },
        ],
        output: pack,
        bound,
        page_size: request.limit,
        cursor,
    })
    .map_err(find_rejection)
}

/// Rank-ordered page of one project's hand-ordered entities.
///
/// `find_kind_page` seeks `(kind, project)` with `Test::Equal`, so every row
/// in a project shares one key and the page comes back in whatever order the
/// index breaks ties in -- which is not the order somebody arranged. A
/// milestone list is an arrangement; drawing it in index order silently
/// discards the arrangement, and the DTO carries no rank for a client to
/// re-derive it from.
///
/// So seek the ordered `(kind, project, position, id)` posting as a range,
/// the way `find_created_page` seeks the ordered time posting. Rank order is
/// then the page order, and the cursor resumes inside it.
///
/// A row whose position is empty sorts first rather than last. That is the
/// pre-backfill legacy shape only: every rank this product writes comes from
/// `rank::between`.
fn find_kind_position_page(
    ctx: &Context<'_>,
    kind: &str,
    project: &str,
    request: &contract::PageRequest,
    mut additional: Vec<runtime::find::Predicate>,
    mut fields: Vec<runtime::find::FieldRef>,
) -> Result<runtime::find::Answer, Rejection> {
    use runtime::find as find_api;
    let cursor = page_cursor(request)?;
    let candidates = u64::from(request.limit).saturating_mul(4).max(64);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: candidates.saturating_mul(8),
        edges_visited: candidates,
        nodes_visited: candidates,
        paths_retained: candidates,
        candidates_per_branch: candidates,
        score_evaluations: candidates,
        projected_bytes: u64::from(request.limit).saturating_mul(64 * 1_024),
        packed_tokens: u64::from(request.limit).saturating_mul(4_096),
        wall_millis: 5_000,
    };
    let lower = crate::find::composite_key([kind, project]);
    let upper =
        crate::find::composite_prefix_upper(lower.clone()).ok_or(Rejection::StateCorrupt)?;
    additional.insert(
        0,
        find_api::Predicate {
            field: crate::find::field_ref(crate::find::field::KIND_PROJECT_POSITION),
            test: find_api::Test::Less,
            value: find_api::Atom::Bytes(upper),
        },
    );
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    fields.extend([
        crate::find::field_ref(crate::find::field::ID),
        crate::find::field_ref(crate::find::field::KIND),
        crate::find::field_ref(crate::find::field::PROJECT),
    ]);
    fields.sort();
    fields.dedup();
    ctx.find(find_api::Query {
        schema: crate::find::entity_schema_ref(),
        publication: ctx.world_publication_id().map(|id| id.publication),
        mode: find_api::Mode::Exact,
        steps: vec![
            find_api::Step {
                id: seek,
                input: Vec::new(),
                op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                    field: crate::find::field_ref(crate::find::field::KIND_PROJECT_POSITION),
                    test: find_api::Test::GreaterOrEqual,
                    value: find_api::Atom::Bytes(lower),
                })),
                bound,
            },
            find_api::Step {
                id: keep,
                input: vec![seek],
                op: find_api::Op::Keep(find_api::Keep {
                    predicates: additional,
                }),
                bound,
            },
            find_api::Step {
                id: pack,
                input: vec![keep],
                op: find_api::Op::Pack(find_api::Pack { fields }),
                bound,
            },
        ],
        output: pack,
        bound,
        page_size: request.limit,
        cursor,
    })
    .map_err(find_rejection)
}

/// Ordered record page owned by one durable entity. This is the detail-plane
/// equivalent of `find_created_page`: it seeks the `(kind, source, timestamp,
/// id)` posting directly, so a busy tracker cannot tax one issue's history or
/// discussion with unrelated records.
fn find_source_created_page(
    ctx: &Context<'_>,
    kind: &str,
    source: &str,
    descending: bool,
    request: &contract::PageRequest,
    mut additional: Vec<runtime::find::Predicate>,
    mut fields: Vec<runtime::find::FieldRef>,
) -> Result<runtime::find::Answer, Rejection> {
    use runtime::find as find_api;
    let cursor = page_cursor(request)?;
    let candidates = u64::from(request.limit).saturating_mul(4).max(64);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: candidates.saturating_mul(4),
        edges_visited: 1,
        nodes_visited: candidates,
        paths_retained: 1,
        candidates_per_branch: candidates,
        score_evaluations: 1,
        projected_bytes: u64::from(request.limit).saturating_mul(64 * 1_024),
        packed_tokens: u64::from(request.limit).saturating_mul(4_096),
        wall_millis: 5_000,
    };
    let field = if descending {
        crate::find::field::KIND_SOURCE_CREATED_DESC
    } else {
        crate::find::field::KIND_SOURCE_CREATED
    };
    let lower = crate::find::composite_key([kind, source]);
    let upper =
        crate::find::composite_prefix_upper(lower.clone()).ok_or(Rejection::StateCorrupt)?;
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    fields.extend([
        crate::find::field_ref(crate::find::field::ID),
        crate::find::field_ref(crate::find::field::KIND),
        crate::find::field_ref(crate::find::field::SOURCE_ID),
        crate::find::field_ref(crate::find::field::CREATED_AT),
    ]);
    fields.sort();
    fields.dedup();
    additional.insert(
        0,
        find_api::Predicate {
            field: crate::find::field_ref(field),
            test: find_api::Test::Less,
            value: find_api::Atom::Bytes(upper),
        },
    );
    ctx.find(find_api::Query {
        schema: crate::find::entity_schema_ref(),
        publication: ctx.world_publication_id().map(|id| id.publication),
        mode: find_api::Mode::Exact,
        steps: vec![
            find_api::Step {
                id: seek,
                input: Vec::new(),
                op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                    field: crate::find::field_ref(field),
                    test: find_api::Test::GreaterOrEqual,
                    value: find_api::Atom::Bytes(lower),
                })),
                bound,
            },
            find_api::Step {
                id: keep,
                input: vec![seek],
                op: find_api::Op::Keep(find_api::Keep {
                    predicates: additional,
                }),
                bound,
            },
            find_api::Step {
                id: pack,
                input: vec![keep],
                op: find_api::Op::Pack(find_api::Pack { fields }),
                bound,
            },
        ],
        output: pack,
        bound,
        page_size: request.limit,
        cursor,
    })
    .map_err(find_rejection)
}

/// Members a roll-up will resolve. `issue_relation_targets` asks for one more
/// than this to detect overflow, so it has to stay under the page ceiling.
const MAX_ROLLUP_MEMBERS: usize = 256;

/// Members one collection row is scanned over before it reports itself
/// unmeasured.
///
/// This bounds MEMBERSHIP RECORDS READ, not live Issues counted, and the
/// difference is the whole point: a soft-deleted Issue keeps its membership
/// so that restoring it restores the membership, so a milestone holding a
/// hundred live Issues and two hundred deleted ones is three hundred records
/// to walk and a hundred to report. Bounding it by the answer's size would
/// call that milestone unmeasurable while it sits comfortably inside what
/// the reader can afford.
///
/// It was `MAX_SEEK_IDS`, which is the largest id set one Find may seek --
/// an implementation limit of the second pass, promoted by mistake into a
/// statement about what this product can count. The resolution chunks now,
/// so that limit bounds a query rather than a collection.
const MEMBERSHIP_SCAN: u64 = 512;

/// Live and Done-category issue counts for a collection — the issues whose
/// `kind` membership relation points at `target`. `None` means the collection
/// was not measured, which is a different fact from measuring zero.
///
/// Two seeks, both linear, because a linear plan is the only shape the
/// runtime answers with an exact `matched_total` and an honest continuation.
///
/// The first is a bare posting count on the reverse membership coordinate. It
/// visits no rows, so asking it is cheap even for a collection far too large
/// to enrich — which is exactly what makes it the right gate.
///
/// The second resolves those members by id, because it needs two facts the
/// relation Body cannot hold. An issue's workflow state and its tombstone
/// both live in the issue's own Bodies, and by design no extractor overlays
/// them onto a relation node; a soft-deleted issue keeps its membership
/// precisely so that restoring it restores the membership too.
///
/// Above the ceiling this answers `None` rather than counting the first page.
/// It is the same rule the row-page handler states where it post-filters:
/// a partial count is not the count.
fn membership_counts(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    kind: &str,
    target: &str,
) -> Result<Option<(u32, u32)>, Rejection> {
    let coordinate = || runtime::find::Atom::Bytes(crate::find::composite_key([kind, target]));
    let counted = find_field_page(
        ctx,
        crate::find::field::RELATION_TARGET_KIND,
        coordinate(),
        &contract::PageRequest {
            limit: 1,
            cursor: None,
        },
        Vec::new(),
        Vec::new(),
    )?;
    // An absent total is the runtime declining to answer, never a zero.
    let Some(members) = counted.matched_total() else {
        return Ok(None);
    };
    if members > MEMBERSHIP_SCAN {
        return Ok(None);
    }
    let limit = u32::try_from(members).map_err(|_| Rejection::StateCorrupt)?;
    if limit == 0 {
        return Ok(Some((0, 0)));
    }
    let answer = find_field_page(
        ctx,
        crate::find::field::RELATION_TARGET_KIND,
        coordinate(),
        &contract::PageRequest {
            limit,
            cursor: None,
        },
        Vec::new(),
        [crate::find::field::SOURCE_ID]
            .into_iter()
            .map(crate::find::field_ref)
            .collect(),
    )?;
    // The count said this many members exist and the page asked for exactly
    // that many. A continuation means the two disagree, so measure nothing.
    if answer.next_cursor().is_some() {
        return Ok(None);
    }
    let mut ids = Vec::with_capacity(answer.rows().len());
    for row in answer.rows() {
        ids.push(result_text(row, crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?);
    }
    let rows = find_issue_rows_by_ids(ctx, ids.clone())?;
    // Every member has to be accounted for, or the count is of something
    // else. A member resolves to no row when its Issue has not converged
    // here yet, or when its heads disagree so there is no single placement
    // to read a state from -- and either way the honest answer is that this
    // collection was not measured. Counting the rest and presenting it as
    // the total is the failure this whole function is shaped to avoid: a
    // number that looks measured, is smaller than the truth, and says
    // nothing about the difference.
    if rows.len() != ids.len() {
        return Ok(None);
    }
    let mut total = 0u32;
    let mut done = 0u32;
    for row in rows.values() {
        if row.tombstone {
            continue;
        }
        total = total.saturating_add(1);
        let project = row.project_id.as_str();
        apply_project_workflow(ctx, catalog, project)?;
        if issue_status_category(catalog, project, &row.status)? == StatusCategory::Done {
            done = done.saturating_add(1);
        }
    }
    Ok(Some((total, done)))
}

/// Ids resolved in one query.
///
/// `MAX_SEEK_IDS` is what the seek admits; this is what the PROJECTION
/// admits, and it is the smaller of the two. A row is declared at 16 KiB
/// against an 8 MiB grant, so 512 would be the ceiling and 256 the seek's --
/// but the walk below re-reads every row to pack it, so this stays at half
/// the seek's limit rather than at the edge of either.
const ID_RESOLUTION_CHUNK: usize = 128;

fn find_issue_rows_by_ids(
    ctx: &Context<'_>,
    ids: impl IntoIterator<Item = String>,
) -> Result<std::collections::BTreeMap<String, crate::dto::Row>, Rejection> {
    use runtime::find as find_api;
    let ids = ids.into_iter().collect::<std::collections::BTreeSet<_>>();
    if ids.is_empty() {
        return Ok(std::collections::BTreeMap::new());
    }
    if ids.len() > usize::try_from(contract::MAX_PAGE_SIZE).unwrap_or(usize::MAX) {
        return Err(Rejection::LimitExceeded);
    }
    // `Seek::Ids` admits `MAX_SEEK_IDS`, which is well under a page. Resolving
    // a full page in one query would be refused outright, so walk it in the
    // runtime's own units rather than declaring a ceiling this cannot honour.
    if ids.len() > ID_RESOLUTION_CHUNK {
        let mut merged = std::collections::BTreeMap::new();
        let ordered = ids.into_iter().collect::<Vec<_>>();
        for chunk in ordered.chunks(ID_RESOLUTION_CHUNK) {
            merged.extend(find_issue_rows_by_ids(ctx, chunk.to_vec())?);
        }
        return Ok(merged);
    }
    let count = u64::try_from(ids.len()).unwrap_or(u64::MAX);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: count.saturating_mul(16),
        edges_visited: 1,
        nodes_visited: count.saturating_mul(2),
        paths_retained: 1,
        candidates_per_branch: count.saturating_mul(2),
        score_evaluations: 1,
        // Sixteen KiB a row, not sixty-four. This packs bounded identity and
        // placement coordinates -- large text is hydrated from the source Body
        // by the detail paths -- and claiming the 64 KiB tuple made the
        // DECLARED budget exceed the Station's 8 MiB grant above 128 rows.
        // Find refuses on the declaration before it evaluates anything, so a
        // request for 129 ids was refused whole, and the chunking below at 256
        // produced chunks that were each refused in turn. What that cost was a
        // milestone with two hundred issues reporting its progress as
        // unreadable rather than as unmeasured, which is the distinction this
        // file spends its bounds to keep.
        projected_bytes: count.saturating_mul(16 * 1_024),
        packed_tokens: count.saturating_mul(4_096),
        wall_millis: 5_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::TITLE,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::PRIORITY,
        crate::find::field::TOMBSTONE,
        crate::find::field::CONFLICTED,
        crate::find::field::DUE_AT,
        crate::find::field::ESTIMATE,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    // Pack takes a canonical set, and a hand-written list is not one: this
    // array reads in the order a Row is built, which is not sorted order, so
    // the query was refused as `InvalidSet("pack fields")` for every caller.
    // Sorting here rather than reordering the literal keeps the list readable
    // and stops the next field appended to it from reintroducing this.
    fields.sort();
    fields.dedup();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Ids(
                        ids.iter()
                            .map(|id| find_api::NodeId::new(id.as_bytes().to_vec()))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| Rejection::InvalidRequest)?,
                    )),
                    bound,
                },
                // An Issue whose transition heads have not converged has no
                // single placement, so `extract_issue_meta` posts no project
                // and no state for it -- only that it is conflicted. A `Row`
                // has nowhere to put that: building one demanded a state and
                // answered `StateCorrupt` when there was none, so a single
                // unconverged Issue failed every read that passed through
                // here -- a label-filtered list, a milestone's progress, an
                // Issue's own links.
                //
                // It is excluded here rather than refused, which is also what
                // the unfiltered list does with the same predicate. That
                // keeps the two spellings of one question agreeing: asking
                // for a label no longer returns rows that asking for
                // everything leaves out.
                find_api::Step {
                    id: keep,
                    input: vec![seek],
                    op: find_api::Op::Keep(find_api::Keep {
                        predicates: vec![find_api::Predicate {
                            field: crate::find::field_ref(crate::find::field::CONFLICTED),
                            test: find_api::Test::Equal,
                            value: find_api::Atom::Bool(false),
                        }],
                    }),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![keep],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: u32::try_from(ids.len()).map_err(|_| Rejection::LimitExceeded)?,
            cursor: None,
        })
        .map_err(find_rejection)?;
    let mut rows = std::collections::BTreeMap::new();
    for result in answer.rows() {
        if result_text(result, crate::find::field::KIND).as_deref() != Some("issue") {
            return Err(Rejection::StateCorrupt);
        }
        let row = issue_page_row(result)?;
        rows.insert(row.doc_id.to_string(), row);
    }
    Ok(rows)
}

const BOARD_PAGE_CURSOR_VERSION: u8 = 1;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BoardPageCursor {
    version: u8,
    filter: [u8; 32],
    state_index: u32,
    block: String,
    block_order: String,
    member_position: String,
    member_issue: String,
}

#[derive(Debug)]
struct BoardPageItem {
    row: crate::dto::Row,
    state_index: usize,
    block: String,
    block_order: String,
    member_position: String,
    member_issue: String,
}

fn board_page_filter(project: &str) -> [u8; 32] {
    blake3::derive_key("lait.issues.board-page-filter.v1", project.as_bytes())
}

fn valid_board_digest(value: &str) -> bool {
    value.len() == 64
        && data_encoding::HEXLOWER
            .decode(value.as_bytes())
            .is_ok_and(|bytes| bytes.len() == 32)
}

fn decode_board_page_cursor(
    ctx: &Context<'_>,
    project: &str,
    request: &contract::PageRequest,
) -> Result<Option<BoardPageCursor>, Rejection> {
    if !request.validate() {
        return Err(Rejection::InvalidRequest);
    }
    let Some(encoded) = request.cursor.as_deref() else {
        return Ok(None);
    };
    let (publication, inner) =
        contract::decode_page_cursor(encoded).ok_or(Rejection::InvalidRequest)?;
    if ctx.world_publication_id().as_ref() != Some(&publication) {
        return Err(Rejection::InvalidRequest);
    }
    let bytes = data_encoding::BASE64URL_NOPAD
        .decode(inner.as_bytes())
        .map_err(|_| Rejection::InvalidRequest)?;
    let cursor: BoardPageCursor =
        postcard::from_bytes(&bytes).map_err(|_| Rejection::InvalidRequest)?;
    if postcard::to_stdvec(&cursor).map_err(|_| Rejection::InvalidRequest)? != bytes
        || cursor.version != BOARD_PAGE_CURSOR_VERSION
        || cursor.filter != board_page_filter(project)
        || !valid_board_digest(&cursor.block)
        || !crate::rank::valid(&cursor.block_order)
        || !crate::rank::valid(&cursor.member_position)
        || crate::ids::DocId::parse(&cursor.member_issue).is_none()
    {
        return Err(Rejection::InvalidRequest);
    }
    Ok(Some(cursor))
}

fn board_find_bound(rows: u32) -> runtime::find::Bound {
    const BOARD_ROW_BYTES: u64 = 8 * 1_024;
    let candidates = u64::from(rows).saturating_mul(4).max(8);
    let output_rows = u64::from(rows).max(1);
    runtime::find::Bound {
        decoded_bodies: 1,
        postings_read: candidates.saturating_mul(8),
        edges_visited: 1,
        nodes_visited: candidates,
        paths_retained: 1,
        candidates_per_branch: candidates,
        score_evaluations: 1,
        // Board rows retain the <=4 KiB title plus bounded scalar and exact
        // topology coordinates. Candidate slack governs visited postings; it
        // must not be multiplied into the bytes returned by Pack.
        projected_bytes: output_rows.saturating_mul(BOARD_ROW_BYTES),
        packed_tokens: output_rows.saturating_mul(BOARD_ROW_BYTES),
        wall_millis: 5_000,
    }
}

fn board_lane_available(ctx: &Context<'_>, project: &str, state: &str) -> Result<bool, Rejection> {
    use runtime::find as find_api;
    let bound = board_find_bound(2);
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::CONFLICTED,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let lane = format!("lane:{project}:{state}");
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Ids(vec![find_api::NodeId::new(
                        lane.as_bytes().to_vec(),
                    )
                    .map_err(|_| Rejection::StateCorrupt)?])),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![seek],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: 2,
            cursor: None,
        })
        .map_err(find_rejection)?;
    let ([] | [_]) = answer.rows() else {
        return Err(Rejection::StateCorrupt);
    };
    let Some(row) = answer.rows().first() else {
        return Ok(false);
    };
    if result_text(row, crate::find::field::KIND).as_deref() != Some("board_lane")
        || result_text(row, crate::find::field::PROJECT).as_deref() != Some(project)
        || result_text(row, crate::find::field::STATE).as_deref() != Some(state)
    {
        return Err(Rejection::StateCorrupt);
    }
    if result_bool(row, crate::find::field::CONFLICTED) != Some(false) {
        return Err(Rejection::Conflict);
    }
    Ok(true)
}

fn next_board_block(
    ctx: &Context<'_>,
    project: &str,
    state: &str,
    after: Option<(&str, &str)>,
) -> Result<Option<(String, String)>, Rejection> {
    use runtime::find as find_api;
    let bound = board_find_bound(2);
    let prefix = crate::find::composite_key([project, state]);
    let upper =
        crate::find::composite_prefix_upper(prefix.clone()).ok_or(Rejection::StateCorrupt)?;
    let lower = after.map_or_else(
        || find_api::RangeEndpoint::Inclusive(find_api::Atom::Bytes(prefix)),
        |(order, block)| {
            find_api::RangeEndpoint::Exclusive(find_api::Atom::Bytes(
                crate::find::board_block_order_key(project, state, order, block),
            ))
        },
    );
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::KIND,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::BLOCK,
        crate::find::field::POSITION,
        crate::find::field::CONFLICTED,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::FieldRange(find_api::FieldRange {
                        field: crate::find::field_ref(
                            crate::find::field::PROJECT_STATE_BLOCK_ORDER,
                        ),
                        lower,
                        upper: find_api::RangeEndpoint::Exclusive(find_api::Atom::Bytes(upper)),
                    })),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![seek],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: 1,
            cursor: None,
        })
        .map_err(find_rejection)?;
    let Some(row) = answer.rows().first() else {
        return Ok(None);
    };
    if result_text(row, crate::find::field::KIND).as_deref() != Some("board_block")
        || result_text(row, crate::find::field::PROJECT).as_deref() != Some(project)
        || result_text(row, crate::find::field::STATE).as_deref() != Some(state)
    {
        return Err(Rejection::StateCorrupt);
    }
    if result_bool(row, crate::find::field::CONFLICTED) != Some(false) {
        return Err(Rejection::Conflict);
    }
    let block = result_text(row, crate::find::field::BLOCK).ok_or(Rejection::StateCorrupt)?;
    let order = result_text(row, crate::find::field::POSITION).ok_or(Rejection::StateCorrupt)?;
    if !valid_board_digest(&block) || !crate::rank::valid(&order) {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some((block, order)))
}

fn exact_board_block(
    ctx: &Context<'_>,
    project: &str,
    state: &str,
    block: &str,
) -> Result<(String, String), Rejection> {
    use runtime::find as find_api;
    let bound = board_find_bound(2);
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::KIND,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::BLOCK,
        crate::find::field::POSITION,
        crate::find::field::CONFLICTED,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Ids(vec![find_api::NodeId::new(
                        block.as_bytes().to_vec(),
                    )
                    .map_err(|_| Rejection::StateCorrupt)?])),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![seek],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: 2,
            cursor: None,
        })
        .map_err(find_rejection)?;
    let [row] = answer.rows() else {
        return Err(Rejection::StateCorrupt);
    };
    if result_text(row, crate::find::field::KIND).as_deref() != Some("board_block")
        || result_text(row, crate::find::field::PROJECT).as_deref() != Some(project)
        || result_text(row, crate::find::field::STATE).as_deref() != Some(state)
        || result_text(row, crate::find::field::BLOCK).as_deref() != Some(block)
    {
        return Err(Rejection::StateCorrupt);
    }
    if result_bool(row, crate::find::field::CONFLICTED) != Some(false) {
        return Err(Rejection::Conflict);
    }
    let order = result_text(row, crate::find::field::POSITION).ok_or(Rejection::StateCorrupt)?;
    if !crate::rank::valid(&order) {
        return Err(Rejection::StateCorrupt);
    }
    Ok((block.into(), order))
}

fn board_member_rows(
    ctx: &Context<'_>,
    project: &str,
    state: &str,
    block: &str,
    after: Option<(&str, &str)>,
    limit: u32,
) -> Result<Vec<BoardPageItem>, Rejection> {
    use runtime::find as find_api;
    let bound = board_find_bound(limit);
    let prefix = crate::find::composite_key([project, state, block]);
    let upper =
        crate::find::composite_prefix_upper(prefix.clone()).ok_or(Rejection::StateCorrupt)?;
    let lower = after.map_or_else(
        || find_api::RangeEndpoint::Inclusive(find_api::Atom::Bytes(prefix)),
        |(position, issue)| {
            find_api::RangeEndpoint::Exclusive(find_api::Atom::Bytes(
                crate::find::board_block_member_key(project, state, block, position, issue),
            ))
        },
    );
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::TITLE,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::BLOCK,
        crate::find::field::POSITION,
        crate::find::field::PLACEMENT_TRANSITION,
        crate::find::field::PRIORITY,
        crate::find::field::TOMBSTONE,
        crate::find::field::CONFLICTED,
        crate::find::field::DUE_AT,
        crate::find::field::ESTIMATE,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let mut predicates = vec![
        find_api::Predicate {
            field: crate::find::field_ref(crate::find::field::KIND),
            test: find_api::Test::Equal,
            value: find_api::Atom::Text("issue".into()),
        },
        find_api::Predicate {
            field: crate::find::field_ref(crate::find::field::TOMBSTONE),
            test: find_api::Test::Equal,
            value: find_api::Atom::Bool(false),
        },
        find_api::Predicate {
            field: crate::find::field_ref(crate::find::field::CONFLICTED),
            test: find_api::Test::Equal,
            value: find_api::Atom::Bool(false),
        },
    ];
    predicates.sort();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::FieldRange(find_api::FieldRange {
                        field: crate::find::field_ref(
                            crate::find::field::PROJECT_STATE_BLOCK_MEMBER,
                        ),
                        lower,
                        upper: find_api::RangeEndpoint::Exclusive(find_api::Atom::Bytes(upper)),
                    })),
                    bound,
                },
                find_api::Step {
                    id: keep,
                    input: vec![seek],
                    op: find_api::Op::Keep(find_api::Keep { predicates }),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![keep],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: limit,
            cursor: None,
        })
        .map_err(find_rejection)?;
    let mut rows = Vec::with_capacity(answer.rows().len());
    for result in answer.rows() {
        let issue = result_text(result, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
        let position =
            result_text(result, crate::find::field::POSITION).ok_or(Rejection::StateCorrupt)?;
        if result_text(result, crate::find::field::KIND).as_deref() != Some("issue")
            || result_text(result, crate::find::field::PROJECT).as_deref() != Some(project)
            || result_text(result, crate::find::field::STATE).as_deref() != Some(state)
            || result_text(result, crate::find::field::BLOCK).as_deref() != Some(block)
            || !crate::rank::valid(&position)
            || crate::ids::DocId::parse(&issue).is_none()
        {
            return Err(Rejection::StateCorrupt);
        }
        rows.push(BoardPageItem {
            row: issue_page_row(result)?,
            state_index: 0,
            block: block.into(),
            block_order: String::new(),
            member_position: position,
            member_issue: issue,
        });
    }
    Ok(rows)
}

fn find_board_page(
    ctx: &Context<'_>,
    project: &str,
    workflow: &[crate::dto::WorkflowState],
    request: &contract::PageRequest,
) -> Result<contract::Page<crate::dto::Row>, Rejection> {
    let cursor = decode_board_page_cursor(ctx, project, request)?;
    let publication = ctx
        .world_publication_id()
        .ok_or(Rejection::NoActiveImplementation)?;
    let mut state_index = cursor.as_ref().map_or(0, |cursor| {
        usize::try_from(cursor.state_index).unwrap_or(usize::MAX)
    });
    if state_index >= workflow.len() && cursor.is_some() {
        return Err(Rejection::InvalidRequest);
    }
    let target = usize::try_from(request.limit)
        .map_err(|_| Rejection::LimitExceeded)?
        .saturating_add(1);
    let mut items = Vec::with_capacity(target);
    while state_index < workflow.len() && items.len() < target {
        let state = workflow[state_index].id.as_str();
        if !board_lane_available(ctx, project, state)? {
            state_index += 1;
            continue;
        }
        let resume = cursor
            .as_ref()
            .filter(|cursor| usize::try_from(cursor.state_index).ok() == Some(state_index));
        let mut block = if let Some(cursor) = resume {
            let current = exact_board_block(ctx, project, state, &cursor.block)?;
            if current.1 != cursor.block_order {
                return Err(Rejection::InvalidRequest);
            }
            Some(current)
        } else {
            next_board_block(ctx, project, state, None)?
        };
        let mut member_after =
            resume.map(|cursor| (cursor.member_position.clone(), cursor.member_issue.clone()));
        while let Some((block_id, block_order)) = block {
            let remaining = target.saturating_sub(items.len());
            if remaining == 0 {
                break;
            }
            let mut members = board_member_rows(
                ctx,
                project,
                state,
                &block_id,
                member_after
                    .as_ref()
                    .map(|(position, issue)| (position.as_str(), issue.as_str())),
                u32::try_from(remaining).map_err(|_| Rejection::LimitExceeded)?,
            )?;
            let returned = members.len();
            for member in &mut members {
                member.state_index = state_index;
                member.block_order.clone_from(&block_order);
            }
            items.extend(members);
            if items.len() >= target {
                break;
            }
            if returned >= remaining {
                return Err(Rejection::StateCorrupt);
            }
            block = next_board_block(ctx, project, state, Some((&block_order, &block_id)))?;
            member_after = None;
        }
        state_index += 1;
    }
    let limit = usize::try_from(request.limit).map_err(|_| Rejection::LimitExceeded)?;
    let has_more = items.len() > limit;
    if has_more {
        items.truncate(limit);
    }
    let next_cursor = if has_more {
        let last = items.last().ok_or(Rejection::StateCorrupt)?;
        let cursor = BoardPageCursor {
            version: BOARD_PAGE_CURSOR_VERSION,
            filter: board_page_filter(project),
            state_index: u32::try_from(last.state_index).map_err(|_| Rejection::StateCorrupt)?,
            block: last.block.clone(),
            block_order: last.block_order.clone(),
            member_position: last.member_position.clone(),
            member_issue: last.member_issue.clone(),
        };
        let inner = data_encoding::BASE64URL_NOPAD
            .encode(&postcard::to_stdvec(&cursor).map_err(|_| Rejection::StateCorrupt)?);
        Some(
            contract::encode_page_cursor(publication.clone(), inner)
                .ok_or(Rejection::StateCorrupt)?,
        )
    } else {
        None
    };
    Ok(contract::Page {
        publication,
        items: items.into_iter().map(|item| item.row).collect(),
        next_cursor,
        exact_total: None,
    })
}

fn load_role_for_write(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    role: &str,
) -> Result<(), Rejection> {
    // A role being created has no head yet, and that absence is this loader's
    // ordinary case rather than an error: it leaves the catalog without an
    // entry, which is exactly what the create path's `contains_key` guard
    // reads as "the id is free". Refusing here made `RoleCreate` impossible
    // for every role -- it minted a fresh id and then demanded Find already
    // know it -- and left the `Conflict` branch below unreachable.
    let Some(projection) = optional_role_projection(ctx, role)? else {
        return Ok(());
    };
    if !projection.summary.conflict_heads.is_empty() {
        return Err(Rejection::Conflict);
    }
    if projection.summary.built_in {
        let revision = crate::roles::built_in(role).ok_or(Rejection::StateCorrupt)?;
        catalog.roles.insert(
            role.to_string(),
            crate::views::StoredRoleRevision {
                revision_id: data_encoding::HEXLOWER.encode(&revision.revision_id),
                predecessor_ids: revision
                    .predecessor_ids
                    .iter()
                    .map(|digest| data_encoding::HEXLOWER.encode(digest))
                    .collect(),
                body: revision.body,
            },
        );
    } else if let Some(revision) = projection.revision {
        catalog
            .role_revisions
            .entry(role.to_string())
            .or_default()
            .push(revision);
    }
    Ok(())
}

fn load_workflow_for_write(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    project: &str,
) -> Result<(), Rejection> {
    crate::record_store::apply_project(ctx, catalog, project)?;
    let projection = workflow_projection(ctx, project)?;
    if !projection.conflict_heads.is_empty() {
        return Err(Rejection::Conflict);
    }
    if let Some(revision) = projection.revision {
        catalog
            .workflow_revisions
            .entry(project.to_string())
            .or_default()
            .push(revision);
    }
    Ok(())
}

fn triage_submission(
    ctx: &Context<'_>,
    id: &str,
) -> Result<Option<crate::views::TriageItem>, Rejection> {
    let Some(row) = unique_find_row(ctx, crate::find::field::ID, id, "triage_fact", None)? else {
        return Ok(None);
    };
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let crate::records::TriageRecord::Submission(record) =
        crate::records::TriageRecord::decode_canonical(&envelope.record)
            .map_err(|_| Rejection::StateCorrupt)?
    else {
        return Err(Rejection::StateCorrupt);
    };
    if record.triage != id || envelope.identity.record != id {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(crate::views::TriageItem {
        id: record.triage,
        title: record.title,
        body: record.body,
        source: record.source,
        submitted_by: record.submitted_by,
        ts: record.timestamp,
        ..Default::default()
    }))
}

fn triage_has_decision(ctx: &Context<'_>, id: &str) -> Result<bool, Rejection> {
    let answer = find_field_page(
        ctx,
        crate::find::field::TARGET_ID,
        runtime::find::Atom::Text(id.into()),
        &contract::PageRequest {
            limit: 1,
            cursor: None,
        },
        vec![runtime::find::Predicate {
            field: crate::find::field_ref(crate::find::field::RELATION_KIND),
            test: runtime::find::Test::Equal,
            value: runtime::find::Atom::Text("triage".into()),
        }],
        Vec::new(),
    )?;
    Ok(!answer.rows().is_empty())
}

fn result_text(row: &runtime::find::ResultRow, name: &str) -> Option<String> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Text(value) => Some(value.to_string()),
                _ => None,
            })
    })
}

fn result_u64(row: &runtime::find::ResultRow, name: &str) -> Option<u64> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Unsigned(value) => Some(*value),
                _ => None,
            })
    })
}

fn result_bool(row: &runtime::find::ResultRow, name: &str) -> Option<bool> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Bool(value) => Some(*value),
                _ => None,
            })
    })
}

fn result_bytes(row: &runtime::find::ResultRow, name: &str) -> Option<Vec<u8>> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Bytes(value) => Some(value.to_vec()),
                _ => None,
            })
    })
}

fn revision_set(row: &runtime::find::ResultRow, name: &str) -> Result<Vec<String>, Rejection> {
    let bytes = result_bytes(row, name).ok_or(Rejection::StateCorrupt)?;
    let revisions: Vec<String> =
        serde_json::from_slice(&bytes).map_err(|_| Rejection::StateCorrupt)?;
    if revisions.len() > crate::records::MAX_CONCURRENT_HEADS
        || revisions.windows(2).any(|pair| pair[0] >= pair[1])
        || revisions
            .iter()
            .any(|revision| crate::spec::decode_revision(revision).is_none())
    {
        return Err(Rejection::StateCorrupt);
    }
    Ok(revisions)
}

fn revision_head_strings(row: &runtime::find::ResultRow) -> Result<Vec<String>, Rejection> {
    let bytes =
        result_bytes(row, crate::find::field::HEAD_REVISIONS).ok_or(Rejection::StateCorrupt)?;
    let heads: Vec<String> = serde_json::from_slice(&bytes).map_err(|_| Rejection::StateCorrupt)?;
    if heads.len() > crate::records::MAX_CONCURRENT_HEADS
        || heads.windows(2).any(|pair| pair[0] >= pair[1])
        || heads.iter().any(|head| head.is_empty() || head.len() > 256)
    {
        return Err(Rejection::StateCorrupt);
    }
    Ok(heads)
}

fn role_summary_row(row: &runtime::find::ResultRow) -> Result<contract::RoleSummary, Rejection> {
    let heads = revision_head_strings(row)?;
    let sole = (heads.len() == 1).then(|| heads[0].clone());
    Ok(contract::RoleSummary {
        role_id: result_text(row, crate::find::field::ENTITY_KEY).ok_or(Rejection::StateCorrupt)?,
        built_in: result_text(row, crate::find::field::STATE).as_deref() == Some("built_in"),
        revision: sole.clone(),
        conflict_heads: if sole.is_some() { Vec::new() } else { heads },
    })
}

/// Read a role that must exist. A reader asking for a role by id is naming
/// one it believes in, so an absent row is a bad request.
fn role_projection(ctx: &Context<'_>, role: &str) -> Result<contract::RoleProjection, Rejection> {
    optional_role_projection(ctx, role)?.ok_or(Rejection::InvalidRequest)
}

/// The same read, for the one caller that must be able to learn the role is
/// not there: creating it.
fn optional_role_projection(
    ctx: &Context<'_>,
    role: &str,
) -> Result<Option<contract::RoleProjection>, Rejection> {
    let Some(row) = unique_find_row(ctx, crate::find::field::ENTITY_KEY, role, "role_head", None)?
    else {
        return Ok(None);
    };
    let summary = role_summary_row(&row)?;
    let revision = if summary.built_in {
        crate::roles::built_in(role).map(|revision| crate::views::StoredRoleRevision {
            revision_id: data_encoding::HEXLOWER.encode(&revision.revision_id),
            predecessor_ids: revision
                .predecessor_ids
                .iter()
                .map(|digest| data_encoding::HEXLOWER.encode(digest))
                .collect(),
            body: revision.body,
        })
    } else if let Some(head) = &summary.revision {
        let row = unique_find_row(
            ctx,
            crate::find::field::REVISION,
            head,
            "governance_revision",
            None,
        )?
        .ok_or(Rejection::StateCorrupt)?;
        if result_text(&row, crate::find::field::SOURCE_ID).as_deref() != Some(role) {
            return Err(Rejection::StateCorrupt);
        }
        let (_, bytes) = immutable_record_bytes(
            ctx,
            &row,
            crate::records::PhysicalSchema::GovernanceRevision,
        )?;
        let record = crate::records::GovernanceRevisionRecord::decode_canonical(&bytes)
            .map_err(|_| Rejection::StateCorrupt)?;
        if record.role != role || record.revision.revision_id != *head {
            return Err(Rejection::StateCorrupt);
        }
        Some(record.revision)
    } else {
        None
    };
    Ok(Some(contract::RoleProjection { summary, revision }))
}

fn workflow_projection(
    ctx: &Context<'_>,
    project: &str,
) -> Result<contract::WorkflowProjection, Rejection> {
    let row = unique_find_row(
        ctx,
        crate::find::field::SOURCE_ID,
        project,
        "workflow_head",
        Some(project),
    )?
    .ok_or(Rejection::InvalidRequest)?;
    let heads = revision_head_strings(&row)?;
    let revision = if heads.len() == 1 {
        let head = &heads[0];
        let revision_node = format!("workflow-revision:{project}:{head}");
        let row = unique_find_row(
            ctx,
            crate::find::field::ID,
            &revision_node,
            "workflow_revision",
            Some(project),
        )?
        .ok_or(Rejection::StateCorrupt)?;
        let (_, bytes) =
            immutable_record_bytes(ctx, &row, crate::records::PhysicalSchema::WorkflowRevision)?;
        let record = crate::records::ProjectWorkflowRevisionRecord::decode_canonical(&bytes)
            .map_err(|_| Rejection::StateCorrupt)?;
        if record.project != project || record.revision.revision_id != *head {
            return Err(Rejection::StateCorrupt);
        }
        Some(record.revision)
    } else {
        None
    };
    Ok(contract::WorkflowProjection {
        project_id: project.into(),
        revision,
        conflict_heads: if heads.len() == 1 { Vec::new() } else { heads },
    })
}

fn spec_summary_row(row: &runtime::find::ResultRow) -> Result<crate::spec::SpecSummary, Rejection> {
    let heads = revision_set(row, crate::find::field::HEAD_REVISIONS)?;
    let issued = revision_set(row, crate::find::field::ISSUED_REVISIONS)?;
    Ok(crate::spec::SpecSummary {
        spec: result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
        project: result_text(row, crate::find::field::PROJECT).ok_or(Rejection::StateCorrupt)?,
        kind: crate::spec::Kind::parse(
            &result_text(row, crate::find::field::ENTITY_KEY).ok_or(Rejection::StateCorrupt)?,
        )
        .ok_or(Rejection::StateCorrupt)?,
        conflicted: result_bool(row, crate::find::field::CONFLICTED)
            .ok_or(Rejection::StateCorrupt)?,
        heads,
        issued,
        head: None,
    })
}

fn baseline_summary_row(
    row: &runtime::find::ResultRow,
) -> Result<crate::spec::BaselineSummary, Rejection> {
    let heads = revision_set(row, crate::find::field::HEAD_REVISIONS)?;
    let issued = revision_set(row, crate::find::field::ISSUED_REVISIONS)?;
    Ok(crate::spec::BaselineSummary {
        baseline: result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
        project: result_text(row, crate::find::field::PROJECT).ok_or(Rejection::StateCorrupt)?,
        conflicted: result_bool(row, crate::find::field::CONFLICTED)
            .ok_or(Rejection::StateCorrupt)?,
        heads,
        issued,
        head: None,
    })
}

/// The corpus row of one exact revision, if it can be read and belongs to
/// the document that claims it.
///
/// Hydration degrades per row and never refuses the page. A register is the
/// page; a row that cannot be read -- a head the corpus has not posted yet, a
/// seek that hit its bound, a heads set naming a revision this document does
/// not own -- is drawn as a row with no head, which the register lists by id.
/// The old assembly had the same posture, and the alternative is one bad
/// revision id in one replicated heads set taking every reader's register
/// down. `None` is an absence, never a default.
fn head_row(
    ctx: &Context<'_>,
    kind: &str,
    source: &str,
    revision: &str,
) -> Option<runtime::find::ResultRow> {
    let row = unique_find_row(ctx, crate::find::field::ID, revision, kind, None)
        .ok()
        .flatten()?;
    (result_text(&row, crate::find::field::SOURCE_ID).as_deref() == Some(source)).then_some(row)
}

/// A register row's head, from the corpus row its revision posts.
///
/// The register used to assemble every row through `spec_state`: the heads
/// Body, a seek per revision, a Body read and a decode each, to show a title
/// and a state the revision's own row already carries packed. One bounded seek
/// on the head's id is the whole read now. Only a row with exactly one head is
/// hydrated: two heads is concurrent intent, and choosing one would invent a
/// current title.
fn spec_head(
    ctx: &Context<'_>,
    summary: &crate::spec::SpecSummary,
) -> Option<crate::spec::SpecHead> {
    let [revision] = summary.heads.as_slice() else {
        return None;
    };
    let row = head_row(ctx, summary.kind.as_str(), &summary.spec, revision)?;
    Some(crate::spec::SpecHead {
        revision: revision.clone(),
        title: result_text(&row, crate::find::field::TITLE)?,
        state: crate::spec::State::parse(&result_text(&row, crate::find::field::STATE)?)?,
        author: result_text(&row, crate::find::field::AUTHOR)?,
        ts: result_u64(&row, crate::find::field::CREATED_AT)?,
    })
}

/// A Baseline register row's head. Same read as [`spec_head`]; a Baseline
/// revision posts its name as the row's title.
fn baseline_head(
    ctx: &Context<'_>,
    summary: &crate::spec::BaselineSummary,
) -> Option<crate::spec::BaselineHead> {
    let [revision] = summary.heads.as_slice() else {
        return None;
    };
    let row = head_row(ctx, "baseline_revision", &summary.baseline, revision)?;
    Some(crate::spec::BaselineHead {
        revision: revision.clone(),
        name: result_text(&row, crate::find::field::TITLE)?,
        state: crate::spec::State::parse(&result_text(&row, crate::find::field::STATE)?)?,
        author: result_text(&row, crate::find::field::AUTHOR)?,
        ts: result_u64(&row, crate::find::field::CREATED_AT)?,
    })
}

/// The Spec revision a row names, or `None` when the row is not one.
///
/// `extract_spec_revision` emits two nodes from one record: the revision
/// itself, and a relation from the Spec to it so the graph can be walked.
/// Both carry the Spec as their source and both are tagged `spec_revision`,
/// so a seek on those two coordinates matches each revision twice. Only the
/// revision node carries [`crate::find::field::REVISION`], and a row without
/// one is not a revision -- it is the edge pointing at it.
fn spec_revision_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<Option<crate::spec::Revision>, Rejection> {
    if result_text(row, crate::find::field::REVISION).is_none() {
        return Ok(None);
    }
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let record = crate::records::SpecRevisionRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    if result_text(row, crate::find::field::REVISION).as_deref()
        != Some(record.revision.revision.as_str())
        || result_text(row, crate::find::field::SOURCE_ID).as_deref()
            != Some(record.revision.body.spec.as_str())
    {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(record.revision))
}

/// The Baseline revision a row names, or `None` when the row is the relation
/// beside it. Same twin-node shape as `spec_revision_page_row`.
fn baseline_revision_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<Option<crate::spec::BaselineRevision>, Rejection> {
    if result_text(row, crate::find::field::REVISION).is_none() {
        return Ok(None);
    }
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let record = crate::records::BaselineRevisionRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    if result_text(row, crate::find::field::REVISION).as_deref()
        != Some(record.revision.revision.as_str())
        || result_text(row, crate::find::field::SOURCE_ID).as_deref()
            != Some(record.revision.body.baseline.as_str())
    {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(record.revision))
}

fn triage_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<crate::records::TriageRecord, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    crate::records::TriageRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)
}

fn spec_reference_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<crate::spec::SpecReferenceFact, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let record = crate::records::SpecRevisionRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    let relation = result_text(row, crate::find::field::RELATION_KIND)
        .and_then(|value| crate::spec::Rel::parse(&value))
        .ok_or(Rejection::StateCorrupt)?;
    let target = result_text(row, crate::find::field::TARGET_ID).ok_or(Rejection::StateCorrupt)?;
    let link = record
        .revision
        .body
        .links
        .iter()
        .find(|link| {
            let rendered = match &link.target {
                crate::spec::Target::Spec { revision, .. }
                | crate::spec::Target::Baseline { revision, .. } => revision.as_str(),
                crate::spec::Target::Issue { issue } => issue.as_str(),
            };
            link.rel == relation && rendered == target
        })
        .cloned()
        .ok_or(Rejection::StateCorrupt)?;
    Ok(crate::spec::SpecReferenceFact {
        spec: record.revision.body.spec,
        revision: record.revision.revision,
        kind: record.revision.body.kind,
        title: record.revision.body.title,
        link,
    })
}

/// The observation a `spec_observation_fact` row asserts.
///
/// The page is a page of observations, not of the records that carry them:
/// projecting the record put its storage tag on the wire and made the answer
/// undecodable as what it claims to be. A retraction is a fact about an
/// observation rather than one itself, and posts its own node kind, so it
/// does not appear here.
fn spec_observation_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<Option<crate::spec::Observation>, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let record = crate::records::SpecObservationRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    let crate::records::SpecObservationRecord::Assert { observation, .. } = record else {
        return Ok(None);
    };
    // A retracted observation is not one. The retraction posts its own node
    // beside the assertion rather than erasing it -- the assertion is an
    // immutable record and stays readable as history -- so the page has to
    // ask, exactly as `spec_observation_state` asks for a single one.
    if unique_find_row(
        ctx,
        crate::find::field::ID,
        &format!("observation-retraction:{}", observation.observation),
        "spec_observation_fact",
        None,
    )?
    .is_some()
    {
        return Ok(None);
    }
    Ok(Some(observation))
}

fn issue_page_row(row: &runtime::find::ResultRow) -> Result<crate::dto::Row, Rejection> {
    let doc = result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
    Ok(crate::dto::Row {
        reff: doc.clone(),
        doc_id: DocId::parse(&doc).ok_or(Rejection::StateCorrupt)?,
        project_id: ProjectId::parse(
            &result_text(row, crate::find::field::PROJECT).ok_or(Rejection::StateCorrupt)?,
        )
        .ok_or(Rejection::StateCorrupt)?,
        key_alias: None,
        title: result_text(row, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?,
        status: result_text(row, crate::find::field::STATE).ok_or(Rejection::StateCorrupt)?,
        priority: Priority::parse(
            &result_text(row, crate::find::field::PRIORITY).ok_or(Rejection::StateCorrupt)?,
        )
        .ok_or(Rejection::StateCorrupt)?,
        assignee_summary: String::new(),
        assignees: Vec::new(),
        enrichment_complete: false,
        tombstone: result_bool(row, crate::find::field::TOMBSTONE)
            .ok_or(Rejection::StateCorrupt)?,
        provisional: false,
        due_date: result_u64(row, crate::find::field::DUE_AT),
        estimate: result_u64(row, crate::find::field::ESTIMATE)
            .map(|value| value.try_into().map_err(|_| Rejection::StateCorrupt))
            .transpose()?,
        label_names: Vec::new(),
        milestone: None,
        child_done: None,
        child_total: None,
    })
}

fn project_page_row(row: &runtime::find::ResultRow) -> Result<crate::dto::ProjectDto, Rejection> {
    let id = result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
    Ok(crate::dto::ProjectDto {
        id: ProjectId::parse(&id).ok_or(Rejection::StateCorrupt)?,
        name: result_text(row, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?,
        key: result_text(row, crate::find::field::ENTITY_KEY).ok_or(Rejection::StateCorrupt)?,
        color: result_text(row, crate::find::field::HEALTH).unwrap_or_default(),
        description: String::new(),
        lead: result_text(row, crate::find::field::AUTHOR).unwrap_or_default(),
        start_date: result_u64(row, crate::find::field::CREATED_AT),
        target_date: result_u64(row, crate::find::field::TARGET_DATE),
        archived: result_bool(row, crate::find::field::ARCHIVED).unwrap_or(false),
        team: result_text(row, crate::find::field::SOURCE_ID).unwrap_or_default(),
        enrichment_complete: false,
    })
}

fn label_page_row(row: &runtime::find::ResultRow) -> Result<crate::dto::LabelDto, Rejection> {
    let id = result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
    Ok(crate::dto::LabelDto {
        id: crate::ids::LabelId::parse(&id).ok_or(Rejection::StateCorrupt)?,
        name: result_text(row, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?,
        color: result_text(row, crate::find::field::HEALTH).unwrap_or_default(),
    })
}

fn update_page_row(
    row: &runtime::find::ResultRow,
) -> Result<crate::dto::ProjectUpdateDto, Rejection> {
    Ok(crate::dto::ProjectUpdateDto {
        id: result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
        author: result_text(row, crate::find::field::AUTHOR).ok_or(Rejection::StateCorrupt)?,
        ts: result_u64(row, crate::find::field::CREATED_AT).ok_or(Rejection::StateCorrupt)?,
        body: result_text(row, crate::find::field::TEXT).ok_or(Rejection::StateCorrupt)?,
        health: result_text(row, crate::find::field::HEALTH).unwrap_or_default(),
    })
}

fn activity_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<ActivityEvent, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let record = crate::records::ActivityRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    let id = result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
    let doc = result_text(row, crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?;
    if record.issue != doc || id != format!("activity:{doc}:{}", envelope.identity.record) {
        return Err(Rejection::StateCorrupt);
    }
    let event = record.event;
    Ok(ActivityEvent {
        // Sequence numbers belonged to the retired per-Issue append log. The
        // immutable record id and the publication cursor are the stable
        // coordinates now; zero prevents callers from mistaking a local count
        // for a resumable offset.
        seq: 0,
        cursor: id,
        doc_id: DocId::parse(&doc),
        reff: doc,
        kind: event.k,
        changes: event
            .c
            .into_iter()
            .map(|change| FieldChange {
                field: change.f,
                from: change.from,
                to: change.to,
            })
            .collect(),
        actor: ActorId::parse(&event.a),
        actor_nick: String::new(),
        text: event.x,
        ts: event.t,
        collision: false,
    })
}

/// One page of inbox entries, with the per-row lookups hoisted out of the row.
///
/// This was three storage round trips and a digest per entry. `inbox_page_row`
/// called `unique_find_row` for the Issue, `issue_coordinate_for` for the
/// placement and alias -- itself an identity seek, a placement probe, a
/// transition-head query, a Body read and a re-hash of those bytes -- and
/// `unique_find_row` again for the project. A fifty-row inbox spent about four
/// hundred operations to draw fifty lines.
///
/// None of it needed to be per row. Titles and projects come from one batched
/// `Seek::Ids`, alias ordinals from another, and a page holds a handful of
/// distinct projects however many entries it has. What remains per row is the
/// activity Body read, which is irreducible: the entry *is* that record.
///
/// The one row that still takes the old path is an Issue the batch could not
/// resolve -- an unconverged placement, which `find_issue_rows_by_ids`
/// deliberately excludes. Falling back rather than skipping keeps the old
/// behaviour exactly: such an entry refuses the page instead of vanishing from
/// it, which is the difference between a reader knowing something is wrong and
/// a reader being quietly shown less than there is.
fn inbox_page_rows(
    ctx: &Context<'_>,
    rows: &[&runtime::find::ResultRow],
    recipient: &ActorId,
) -> Result<Vec<crate::dto::InboxEntry>, Rejection> {
    let mut docs = rows
        .iter()
        .filter_map(|row| result_text(row, crate::find::field::SOURCE_ID))
        .collect::<Vec<_>>();
    docs.sort();
    docs.dedup();
    let issues = find_issue_rows_by_ids(ctx, docs.clone())?;
    let ordinals = page_alias_ordinals(ctx, &docs)?;
    let mut catalog = CatalogState::default();
    let mut asked = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        out.push(inbox_page_row(
            ctx,
            row,
            recipient,
            &issues,
            &ordinals,
            &mut catalog,
            &mut asked,
        )?);
    }
    Ok(out)
}

fn inbox_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
    recipient: &ActorId,
    issues: &std::collections::BTreeMap<String, crate::dto::Row>,
    ordinals: &std::collections::BTreeMap<String, u64>,
    catalog: &mut CatalogState,
    asked: &mut std::collections::BTreeSet<String>,
) -> Result<crate::dto::InboxEntry, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let record = crate::records::ActivityRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    let doc = result_text(row, crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?;
    let kind = result_text(row, crate::find::field::STATE).ok_or(Rejection::StateCorrupt)?;
    if record.issue != doc
        || record.event.inbox_kind() != Some(kind.as_str())
        || record
            .recipients
            .binary_search_by(|actor| actor.as_str().cmp(recipient.as_str()))
            .is_err()
        || result_text(row, crate::find::field::DEVICE).as_deref() != Some(record.event.d.as_str())
    {
        return Err(Rejection::StateCorrupt);
    }
    let (title, reff) = match (issues.get(&doc), ordinals.get(&doc)) {
        (Some(issue), Some(&ordinal)) => {
            let key = inbox_project_key(ctx, catalog, asked, issue.project_id.as_str())?;
            // `render_short`, matching the Issue's own page and its rows. The
            // full form names the disambiguator, which resolves an ambiguous
            // reference and is not what a person reads a list by -- and an
            // inbox line beside a list saying `ENG-12` must not say
            // `ENG-12-9f3a1c…` about the same Issue.
            let reff = crate::records::IssueAliasCoordinate::for_issue(ordinal, &issue.doc_id)
                .and_then(|alias| alias.render_short(&key))
                .map_err(|_| Rejection::StateCorrupt)?;
            (issue.title.clone(), reff)
        }
        // The batch could not name it. Resolve this one the long way so the
        // page refuses for the same reasons it always did.
        _ => {
            let issue = unique_find_row(ctx, crate::find::field::ID, &doc, "issue", None)?
                .ok_or(Rejection::StateCorrupt)?;
            let title =
                result_text(&issue, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?;
            let coordinate = crate::record_store::issue_coordinate_for(ctx, &doc)?
                .ok_or(Rejection::StateCorrupt)?;
            let key = inbox_project_key(ctx, catalog, asked, &coordinate.placement.project)?;
            let reff = coordinate
                .identity
                .alias
                .render_short(&key)
                .map_err(|_| Rejection::StateCorrupt)?;
            (title, reff)
        }
    };
    Ok(crate::dto::InboxEntry {
        ts: record.event.t,
        kind,
        reff,
        doc_id: doc,
        title,
        detail: record.event.x,
        actor: Some(record.event.a),
        actor_nick: None,
    })
}

/// One project's KEY, read once per page rather than once per entry.
fn inbox_project_key(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    asked: &mut std::collections::BTreeSet<String>,
    project: &str,
) -> Result<String, Rejection> {
    if asked.insert(project.to_string()) {
        crate::record_store::apply_project(ctx, catalog, project)?;
    }
    catalog
        .projects
        .get(project)
        .map(|meta| meta.key.clone())
        .ok_or(Rejection::StateCorrupt)
}

fn immutable_record_bytes(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
    schema: crate::records::PhysicalSchema,
) -> Result<(crate::records::RecordBodyIdentityRecord, Vec<u8>), Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    if crate::records::immutable_record_key(schema, &bytes) != row.source {
        return Err(Rejection::StateCorrupt);
    }
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    Ok((envelope.identity, envelope.record))
}

fn comment_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
    doc: &str,
    issue: &IssueState,
) -> Result<crate::dto::CommentDto, Rejection> {
    let (identity, bytes) =
        immutable_record_bytes(ctx, row, crate::records::PhysicalSchema::IssueComment)?;
    let crate::records::DiscussionRecord::Comment(comment) =
        crate::records::DiscussionRecord::decode_canonical(&bytes)
            .map_err(|_| Rejection::StateCorrupt)?
    else {
        return Err(Rejection::StateCorrupt);
    };
    let id = comment.id.clone().ok_or(Rejection::StateCorrupt)?;
    if identity.owner != doc
        || identity.record != id
        || result_text(row, crate::find::field::SOURCE_ID).as_deref() != Some(doc)
    {
        return Err(Rejection::StateCorrupt);
    }
    Ok(crate::dto::CommentDto {
        author: ActorId::parse(&comment.a).ok_or(Rejection::StateCorrupt)?,
        author_nick: None,
        ts: comment.t,
        body: comment.b.clone(),
        id: Some(id),
        parent: comment.parent.clone(),
        reactions: Vec::new(),
        anchor: resolve_comment_anchor(ctx, doc, issue, &comment)?,
    })
}

fn reaction_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<crate::records::ReactionRecord, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let crate::records::DiscussionRecord::Reaction(record) =
        crate::records::DiscussionRecord::decode_canonical(&bytes)
            .map_err(|_| Rejection::StateCorrupt)?
    else {
        return Err(Rejection::StateCorrupt);
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    Ok(record)
}

fn attachment_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<crate::dto::AttachmentMetaDto, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let record = crate::records::IssueAttachmentRecord::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    Ok(crate::dto::AttachmentMetaDto {
        id: record.id,
        name: record.name,
        mime: record.mime,
        size: record.size,
        by: record.by,
        ts: record.timestamp,
        comment: record.comment.unwrap_or_default(),
    })
}

fn check_page_row(
    ctx: &Context<'_>,
    row: &runtime::find::ResultRow,
) -> Result<crate::dto::CheckDto, Rejection> {
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let record = crate::records::IssueCheckRecord::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    let check = record.check;
    Ok(crate::dto::CheckDto {
        run: record.run,
        spec: check.spec,
        version: check.v,
        build: check.build,
        package_filled: check.package_filled,
        source: check.source,
        state: check.state,
        by: check.by,
        ts: check.ts,
        attempt: check.attempt,
        report: check.report,
        verdict: check.verdict,
    })
}

fn issue_relations_page(
    ctx: &Context<'_>,
    doc: &str,
    direction: crate::dto::RelationDirection,
    page: &contract::PageRequest,
) -> Result<contract::Page<crate::dto::IssueRelationDto>, Rejection> {
    let endpoint = match direction {
        crate::dto::RelationDirection::Out => crate::find::field::GRAPH_SOURCE_ID,
        crate::dto::RelationDirection::In => crate::find::field::GRAPH_TARGET_ID,
    };
    let answer = find_field_page(
        ctx,
        endpoint,
        runtime::find::Atom::Text(doc.into()),
        page,
        Vec::new(),
        [
            crate::find::field::RELATION_KIND,
            crate::find::field::SOURCE_ID,
            crate::find::field::TARGET_ID,
            crate::find::field::PROJECT,
        ]
        .into_iter()
        .map(crate::find::field_ref)
        .collect(),
    )?;
    let other_field = match direction {
        crate::dto::RelationDirection::Out => crate::find::field::TARGET_ID,
        crate::dto::RelationDirection::In => crate::find::field::SOURCE_ID,
    };
    let other_ids = answer
        .rows()
        .iter()
        .map(|row| result_text(row, other_field).ok_or(Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    let rows_by_id = find_issue_rows_by_ids(ctx, other_ids)?;
    let items = answer
        .rows()
        .iter()
        .map(|relation| {
            let other = result_text(relation, other_field).ok_or(Rejection::StateCorrupt)?;
            Ok(crate::dto::IssueRelationDto {
                kind: result_text(relation, crate::find::field::RELATION_KIND)
                    .ok_or(Rejection::StateCorrupt)?,
                direction,
                row: rows_by_id
                    .get(&other)
                    .cloned()
                    .ok_or(Rejection::StateCorrupt)?,
            })
        })
        .collect::<Result<Vec<_>, Rejection>>()?;
    Ok(page_from_answer(&answer, items))
}

fn issue_comments_page(
    ctx: &Context<'_>,
    doc: &str,
    page: &contract::PageRequest,
) -> Result<contract::Page<crate::dto::CommentDto>, Rejection> {
    let issue = issue_core_state(ctx, doc).ok_or(Rejection::InvalidRequest)?;
    let answer = find_source_created_page(
        ctx,
        "comment",
        doc,
        false,
        page,
        Vec::new(),
        [crate::find::field::TARGET_ID]
            .into_iter()
            .map(crate::find::field_ref)
            .collect(),
    )?;
    let items = answer
        .rows()
        .iter()
        .map(|row| comment_page_row(ctx, row, doc, &issue))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(page_from_answer(&answer, items))
}

fn issue_reactions_page(
    ctx: &Context<'_>,
    doc: &str,
    page: &contract::PageRequest,
) -> Result<contract::Page<crate::records::ReactionRecord>, Rejection> {
    let answer = find_field_page(
        ctx,
        crate::find::field::TARGET_ID,
        runtime::find::Atom::Text(doc.into()),
        page,
        vec![runtime::find::Predicate {
            field: crate::find::field_ref(crate::find::field::KIND),
            test: runtime::find::Test::Equal,
            value: runtime::find::Atom::Text("reaction".into()),
        }],
        [
            crate::find::field::STATE,
            crate::find::field::AUTHOR,
            crate::find::field::RELATION_KIND,
            crate::find::field::SOURCE_ID,
        ]
        .into_iter()
        .map(crate::find::field_ref)
        .collect(),
    )?;
    let items = answer
        .rows()
        .iter()
        .map(|row| reaction_page_row(ctx, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(page_from_answer(&answer, items))
}

fn issue_attachments_page(
    ctx: &Context<'_>,
    doc: &str,
    page: &contract::PageRequest,
) -> Result<contract::Page<crate::dto::AttachmentMetaDto>, Rejection> {
    let answer = find_source_created_page(
        ctx,
        "issue_attachment",
        doc,
        false,
        page,
        vec![runtime::find::Predicate {
            field: crate::find::field_ref(crate::find::field::TOMBSTONE),
            test: runtime::find::Test::Equal,
            value: runtime::find::Atom::Bool(false),
        }],
        [crate::find::field::TITLE, crate::find::field::TOMBSTONE]
            .into_iter()
            .map(crate::find::field_ref)
            .collect(),
    )?;
    let items = answer
        .rows()
        .iter()
        .map(|row| attachment_page_row(ctx, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(page_from_answer(&answer, items))
}

fn issue_checks_page(
    ctx: &Context<'_>,
    doc: &str,
    page: &contract::PageRequest,
) -> Result<contract::Page<crate::dto::CheckDto>, Rejection> {
    let answer = find_source_created_page(
        ctx,
        "issue_check",
        doc,
        true,
        page,
        Vec::new(),
        [crate::find::field::STATE, crate::find::field::AUTHOR]
            .into_iter()
            .map(crate::find::field_ref)
            .collect(),
    )?;
    let items = answer
        .rows()
        .iter()
        .map(|row| check_page_row(ctx, row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(page_from_answer(&answer, items))
}

fn milestone_page_row(
    row: &runtime::find::ResultRow,
) -> Result<crate::dto::MilestoneDto, Rejection> {
    Ok(crate::dto::MilestoneDto {
        id: result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
        name: result_text(row, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?,
        // A milestone's body is its own bounded prose, not the independently
        // stored large description an issue or project carries, so the
        // collection page packs it rather than omitting it.
        description: result_text(row, crate::find::field::TEXT).unwrap_or_default(),
        target_date: result_u64(row, crate::find::field::TARGET_DATE),
        total: 0,
        done: 0,
        enrichment_complete: false,
    })
}

fn cycle_page_row(row: &runtime::find::ResultRow) -> Result<crate::dto::CycleDto, Rejection> {
    Ok(crate::dto::CycleDto {
        id: result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
        name: result_text(row, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?,
        start: result_u64(row, crate::find::field::CREATED_AT).unwrap_or(0),
        end: result_u64(row, crate::find::field::DUE_AT).unwrap_or(0),
        total: 0,
        done: 0,
        enrichment_complete: false,
    })
}

fn initiative_page_row(
    row: &runtime::find::ResultRow,
) -> Result<crate::dto::InitiativeDto, Rejection> {
    Ok(crate::dto::InitiativeDto {
        id: result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
        name: result_text(row, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?,
        description: String::new(),
        owner: result_text(row, crate::find::field::AUTHOR).unwrap_or_default(),
        health: result_text(row, crate::find::field::HEALTH).unwrap_or_default(),
        target_date: result_u64(row, crate::find::field::TARGET_DATE),
        projects: Vec::new(),
        total: 0,
        done: 0,
        enrichment_complete: false,
    })
}

fn team_page_row(row: &runtime::find::ResultRow) -> Result<crate::dto::TeamDto, Rejection> {
    Ok(crate::dto::TeamDto {
        id: result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
        name: result_text(row, crate::find::field::TITLE).ok_or(Rejection::StateCorrupt)?,
        key: result_text(row, crate::find::field::ENTITY_KEY).ok_or(Rejection::StateCorrupt)?,
        icon: result_text(row, crate::find::field::HEALTH).unwrap_or_default(),
        lead: result_text(row, crate::find::field::AUTHOR).unwrap_or_default(),
        members: Vec::new(),
        projects: Vec::new(),
        enrichment_complete: false,
    })
}

fn live_predicate() -> runtime::find::Predicate {
    runtime::find::Predicate {
        field: crate::find::field_ref(crate::find::field::TOMBSTONE),
        test: runtime::find::Test::Equal,
        value: runtime::find::Atom::Bool(false),
    }
}

fn unique_find_row(
    ctx: &Context<'_>,
    field: &str,
    value: &str,
    kind: &str,
    project: Option<&str>,
) -> Result<Option<runtime::find::ResultRow>, Rejection> {
    use runtime::find as find_api;
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: 16,
        edges_visited: 1,
        nodes_visited: 4,
        paths_retained: 1,
        candidates_per_branch: 4,
        score_evaluations: 1,
        projected_bytes: 64 * 1_024,
        // A unique head row can carry the bounded 64-way revision-head set
        // plus project/title coordinates. Runtime counts canonical packed
        // bytes, so this must match the explicit projection byte ceiling.
        packed_tokens: 64 * 1_024,
        wall_millis: 1_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    let (seek_field, seek_value) = if field == crate::find::field::SOURCE_ID
        && project.is_some_and(|project| project == value)
    {
        (
            crate::find::field::KIND_PROJECT,
            find_api::Atom::Bytes(crate::find::composite_key([kind, value])),
        )
    } else {
        (field, find_api::Atom::Text(value.into()))
    };
    let mut predicates = vec![find_api::Predicate {
        field: crate::find::field_ref(crate::find::field::KIND),
        test: find_api::Test::Equal,
        value: find_api::Atom::Text(kind.into()),
    }];
    if let Some(project) = project {
        predicates.push(find_api::Predicate {
            field: crate::find::field_ref(crate::find::field::PROJECT),
            test: find_api::Test::Equal,
            value: find_api::Atom::Text(project.into()),
        });
    }
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::TITLE,
        crate::find::field::RELATION_KIND,
        crate::find::field::SOURCE_ID,
        crate::find::field::TARGET_ID,
        crate::find::field::STATE,
        crate::find::field::AUTHOR,
        crate::find::field::CREATED_AT,
        crate::find::field::EXACT_NAME,
        crate::find::field::ENTITY_KEY,
        crate::find::field::PROJECT,
        crate::find::field::TOMBSTONE,
        crate::find::field::ALIAS_COORDINATE,
        crate::find::field::REVISION,
        crate::find::field::HEAD_REVISIONS,
        crate::find::field::CONFLICTED,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                        field: crate::find::field_ref(seek_field),
                        test: find_api::Test::Equal,
                        value: seek_value,
                    })),
                    bound,
                },
                find_api::Step {
                    id: keep,
                    input: vec![seek],
                    op: find_api::Op::Keep(find_api::Keep { predicates }),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![keep],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: 2,
            cursor: None,
        })
        .map_err(find_rejection)?;
    if answer.next_cursor().is_some() || answer.matched_total().is_some_and(|count| count > 1) {
        return Err(Rejection::Conflict);
    }
    let mut rows = answer.rows().to_vec();
    if rows.len() > 1 {
        return Err(Rejection::Conflict);
    }
    Ok(rows.pop())
}

/// The one Issue in `project` whose alias ordinal is `ordinal`.
///
/// Ordinals are derived rather than counted, so two Issues in a project can
/// share one. That is the trade the short reference buys, and this is where
/// it is paid: a short reference naming more than one Issue is refused, and
/// the refusal is the signal to use the full form, which always resolves.
///
/// One exact seek on the `(project, ordinal)` posting.
///
/// It used to seek the ordinal's own prefix across the whole Space and then
/// filter the rows by project, which was survivable only while ordinals were
/// hashes spread over sixty-three bits: one prefix meant one Issue. Counting
/// made them small and dense, so ordinal one now exists in every project at
/// once and that scan returned a row per project -- reading each one's
/// coordinate to find out which -- until it exceeded its cap and answered
/// that the reference names nothing.
///
/// The pair is what the reference means, so the pair is what is indexed.
fn unique_doc_for_ordinal(
    ctx: &Context<'_>,
    project: &str,
    ordinal: u64,
) -> Result<String, Rejection> {
    let request = contract::PageRequest {
        limit: ALIAS_ORDINAL_SCAN,
        cursor: None,
    };
    let answer = find_field_page(
        ctx,
        crate::find::field::ALIAS_PROJECT_ORDINAL,
        runtime::find::Atom::Bytes(crate::find::composite_key([project, &ordinal.to_string()])),
        &request,
        Vec::new(),
        vec![crate::find::field_ref(crate::find::field::SOURCE_ID)],
    )?;
    // More Issues wearing one number than this admits is not something to
    // settle by taking the first.
    if answer.next_cursor().is_some() {
        return Err(Rejection::InvalidRequest);
    }
    let mut found = None;
    for row in answer.rows() {
        if result_text(row, crate::find::field::KIND).as_deref() != Some("issue_identity") {
            continue;
        }
        let doc = result_text(row, crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?;
        if found.replace(doc).is_some() {
            // Two Issues counted the same number while apart. The short form
            // cannot say which; the full one always can.
            return Err(Rejection::InvalidRequest);
        }
    }
    found.ok_or(Rejection::InvalidRequest)
}

/// The one node of `kind` whose id begins with `prefix`.
fn unique_id_by_prefix(ctx: &Context<'_>, kind: &str, prefix: &str) -> Result<String, Rejection> {
    let request = contract::PageRequest {
        limit: ALIAS_ORDINAL_SCAN,
        cursor: None,
    };
    // Bounded by the prefix itself. The `id` field is posted by every entity
    // node, so a half-open range would scan every kind that sorts after this
    // one -- most of the corpus -- and trip the bound rather than answering.
    let answer = find_field_test_page(
        ctx,
        crate::find::field::ID,
        runtime::find::Test::Prefix,
        runtime::find::Atom::Text(prefix.to_string()),
        &request,
        Vec::new(),
        Vec::new(),
    )?;
    if answer.next_cursor().is_some() {
        return Err(Rejection::InvalidRequest);
    }
    let mut found = None;
    for row in answer.rows() {
        if result_text(row, crate::find::field::KIND).as_deref() != Some(kind) {
            continue;
        }
        let id = result_text(row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
        if found.replace(id).is_some() {
            return Err(Rejection::InvalidRequest);
        }
    }
    found.ok_or(Rejection::InvalidRequest)
}

/// Rows one short reference may touch before it is called unusable.
const ALIAS_ORDINAL_SCAN: u32 = 32;

fn resolve_entity(
    ctx: &Context<'_>,
    entity: contract::ResolveEntity,
    selector: &str,
    project: Option<&str>,
) -> Result<contract::ResolvedEntity, Rejection> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(Rejection::InvalidRequest);
    }
    if entity == contract::ResolveEntity::Issue {
        let (doc, requested_project) = if DocId::parse(selector).is_some() {
            let row = unique_find_row(ctx, crate::find::field::ID, selector, "issue", None)?
                .ok_or(Rejection::InvalidRequest)?;
            (
                result_text(&row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?,
                None,
            )
        } else if selector.starts_with(DocId::PREFIX) {
            // A canonical id somebody stopped typing. Unambiguous prefixes
            // resolve; an ambiguous one is refused rather than guessed.
            (unique_id_by_prefix(ctx, "issue", selector)?, None)
        } else {
            // `KEY-ORDINAL`, the reference a person is shown, or
            // `KEY-ORDINAL-SUFFIX`, the full form that names the
            // collision-proof component outright.
            let (key, rest) = selector.split_once('-').ok_or(Rejection::InvalidRequest)?;
            let (ordinal, suffix) = match rest.split_once('-') {
                Some((ordinal, suffix)) => (ordinal, Some(suffix)),
                None => (rest, None),
            };
            // Reparse rather than carrying the typed text through: a padded
            // `ENG-000483102` validates and then matches no posting, because
            // the coordinate is written from the number.
            let Some(canonical_ordinal) = ordinal.parse::<u64>().ok().filter(|value| *value > 0)
            else {
                return Err(Rejection::InvalidRequest);
            };
            if suffix.is_some_and(|suffix| {
                suffix.len() != 32 || data_encoding::HEXLOWER.decode(suffix.as_bytes()).is_err()
            }) {
                return Err(Rejection::InvalidRequest);
            }
            let project_row = unique_find_row(
                ctx,
                crate::find::field::ENTITY_KEY,
                &key.to_ascii_uppercase(),
                "project",
                None,
            )?
            .ok_or(Rejection::InvalidRequest)?;
            let project =
                result_text(&project_row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
            let doc = match suffix {
                Some(suffix) => {
                    let alias = format!("{ordinal}-{}", suffix.to_ascii_lowercase());
                    let identity = unique_find_row(
                        ctx,
                        crate::find::field::ALIAS_COORDINATE,
                        &alias,
                        "issue_identity",
                        None,
                    )?
                    .ok_or(Rejection::InvalidRequest)?;
                    result_text(&identity, crate::find::field::SOURCE_ID)
                        .ok_or(Rejection::StateCorrupt)?
                }
                None => unique_doc_for_ordinal(ctx, &project, canonical_ordinal)?,
            };
            (doc, Some(project))
        };
        let coordinate = crate::record_store::issue_coordinate_for(ctx, &doc)?
            .ok_or(Rejection::InvalidRequest)?;
        if requested_project
            .as_ref()
            .is_some_and(|project| project != &coordinate.placement.project)
        {
            return Err(Rejection::InvalidRequest);
        }
        let mut catalog = CatalogState::default();
        crate::record_store::apply_project(ctx, &mut catalog, &coordinate.placement.project)?;
        let project_meta = catalog
            .projects
            .get(&coordinate.placement.project)
            .ok_or(Rejection::StateCorrupt)?;
        let display = coordinate
            .identity
            .alias
            .render_short(&project_meta.key)
            .map_err(|_| Rejection::StateCorrupt)?;
        return Ok(contract::ResolvedEntity {
            id: doc,
            display,
            record: serde_json::Value::Null,
        });
    }

    let (kind, prefix, name_field) = match entity {
        contract::ResolveEntity::Project => (
            "project",
            crate::ids::ProjectId::PREFIX,
            crate::find::field::ENTITY_KEY,
        ),
        contract::ResolveEntity::Label => (
            "label",
            crate::ids::LabelId::PREFIX,
            crate::find::field::EXACT_NAME,
        ),
        contract::ResolveEntity::Milestone => (
            "milestone",
            crate::ids::MilestoneId::PREFIX,
            crate::find::field::EXACT_NAME,
        ),
        contract::ResolveEntity::Cycle => (
            "cycle",
            crate::ids::CycleId::PREFIX,
            crate::find::field::EXACT_NAME,
        ),
        contract::ResolveEntity::Initiative => (
            "initiative",
            crate::ids::InitiativeId::PREFIX,
            crate::find::field::EXACT_NAME,
        ),
        contract::ResolveEntity::Team => (
            "team",
            crate::ids::TeamId::PREFIX,
            crate::find::field::EXACT_NAME,
        ),
        contract::ResolveEntity::Issue => return Err(Rejection::InvalidRequest),
    };
    let row = if selector.starts_with(prefix) {
        unique_find_row(ctx, crate::find::field::ID, selector, kind, project)?
    } else {
        let lookup = if entity == contract::ResolveEntity::Project
            || entity == contract::ResolveEntity::Team
        {
            selector.to_ascii_uppercase()
        } else {
            selector.to_ascii_lowercase()
        };
        unique_find_row(ctx, name_field, &lookup, kind, project)?.or_else(|| {
            (entity == contract::ResolveEntity::Team)
                .then(|| {
                    unique_find_row(ctx, crate::find::field::ENTITY_KEY, &lookup, kind, project)
                })
                .transpose()
                .ok()
                .flatten()
                .flatten()
        })
    }
    .ok_or(Rejection::InvalidRequest)?;
    let id = result_text(&row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
    Ok(contract::ResolvedEntity {
        display: result_text(&row, crate::find::field::ENTITY_KEY)
            .or_else(|| result_text(&row, crate::find::field::TITLE))
            .unwrap_or_else(|| id.clone()),
        id,
        record: serde_json::Value::Null,
    })
}

fn hydrate_issue_enrichment(
    ctx: &Context<'_>,
    doc: &str,
    issue: &mut IssueState,
) -> Result<(), Rejection> {
    issue.assignees.clear();
    issue.followers.clear();
    issue.labels.clear();
    issue.milestone = None;
    issue.cycle = None;
    issue.baseline = None;
    issue.comments.clear();
    issue.reactions.clear();
    issue.events.clear();
    issue.events_recorded = 0;
    issue.attachments.clear();
    issue.checks.clear();
    issue.check_corrupt_records.clear();

    for row in find_rows_equal(ctx, crate::find::field::SOURCE_ID, doc)? {
        match result_text(&row, crate::find::field::KIND).as_deref() {
            Some("relation") => {
                let Some(kind) = result_text(&row, crate::find::field::RELATION_KIND) else {
                    continue;
                };
                if !matches!(
                    kind.as_str(),
                    "assignee" | "follower" | "label" | "milestone" | "cycle" | "baseline"
                ) {
                    continue;
                }
                let raw = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
                let relation = crate::records::IssueRelationRecord::decode_canonical(&raw)
                    .map_err(|_| Rejection::StateCorrupt)?;
                if relation.issue != doc || relation.kind != kind || !relation.present {
                    return Err(Rejection::StateCorrupt);
                }
                match relation.kind.as_str() {
                    "assignee" => issue
                        .assignees
                        .push(ActorId::parse(&relation.target).ok_or(Rejection::StateCorrupt)?),
                    "follower" => issue
                        .followers
                        .push(ActorId::parse(&relation.target).ok_or(Rejection::StateCorrupt)?),
                    "label" => issue.labels.push(relation.target),
                    "milestone" => issue.milestone = Some(relation.target),
                    "cycle" => issue.cycle = Some(relation.target),
                    "baseline" => {
                        issue.baseline = serde_json::from_str(&relation.target)
                            .map(Some)
                            .map_err(|_| Rejection::StateCorrupt)?
                    }
                    _ => return Err(Rejection::StateCorrupt),
                }
            }
            Some("comment") => {
                let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
                let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
                    .map_err(|_| Rejection::StateCorrupt)?;
                let crate::records::DiscussionRecord::Comment(comment) =
                    crate::records::DiscussionRecord::decode_canonical(&envelope.record)
                        .map_err(|_| Rejection::StateCorrupt)?
                else {
                    return Err(Rejection::StateCorrupt);
                };
                issue.comments.push(comment);
            }
            Some("activity") => {
                let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
                let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
                    .map_err(|_| Rejection::StateCorrupt)?;
                let mut event = crate::records::ActivityRecord::decode_canonical(&envelope.record)
                    .map_err(|_| Rejection::StateCorrupt)?
                    .event;
                event.entry =
                    result_text(&row, crate::find::field::ID).ok_or(Rejection::StateCorrupt)?;
                issue.events.push(event);
            }
            Some("issue_attachment") => {
                let raw = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
                let record = crate::records::IssueAttachmentRecord::decode_canonical(&raw)
                    .map_err(|_| Rejection::StateCorrupt)?;
                if record.issue != doc {
                    return Err(Rejection::StateCorrupt);
                }
                if !record.tombstone {
                    issue.attachments.push(crate::views::AttachmentMeta {
                        id: record.id,
                        name: record.name,
                        mime: record.mime,
                        size: record.size,
                        by: record.by,
                        ts: record.timestamp,
                        comment: record.comment.unwrap_or_default(),
                    });
                }
            }
            Some("issue_check") => {
                let raw = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
                let record = crate::records::IssueCheckRecord::decode_canonical(&raw)
                    .map_err(|_| Rejection::StateCorrupt)?;
                if record.issue != doc {
                    return Err(Rejection::StateCorrupt);
                }
                issue.checks.push((record.run, record.check));
            }
            _ => {}
        }
    }
    for row in find_rows_equal(ctx, crate::find::field::TARGET_ID, doc)? {
        if result_text(&row, crate::find::field::KIND).as_deref() != Some("reaction")
            || result_text(&row, crate::find::field::STATE).as_deref() != Some("on")
        {
            continue;
        }
        let comment =
            result_text(&row, crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?;
        let emoji =
            result_text(&row, crate::find::field::RELATION_KIND).ok_or(Rejection::StateCorrupt)?;
        let actor = result_text(&row, crate::find::field::AUTHOR).ok_or(Rejection::StateCorrupt)?;
        issue
            .reactions
            .entry(comment)
            .or_default()
            .push((emoji, actor));
    }
    issue.assignees.sort();
    issue.assignees.dedup();
    issue.followers.sort();
    issue.followers.dedup();
    issue.labels.sort();
    issue.labels.dedup();
    issue
        .attachments
        .sort_by(|left, right| left.ts.cmp(&right.ts).then_with(|| left.id.cmp(&right.id)));
    issue.checks.sort_by(|left, right| left.0.cmp(&right.0));
    issue
        .comments
        .sort_by(|left, right| left.t.cmp(&right.t).then_with(|| left.id.cmp(&right.id)));
    for reactions in issue.reactions.values_mut() {
        reactions.sort();
        reactions.dedup();
    }
    issue.events.sort_by(|left, right| {
        left.t
            .cmp(&right.t)
            .then_with(|| left.entry.cmp(&right.entry))
    });
    issue.events_recorded = u64::try_from(issue.events.len()).unwrap_or(u64::MAX);
    Ok(())
}

fn issue_state(ctx: &Context<'_>, doc: &str) -> Option<IssueState> {
    let mut issue = issue_core_state(ctx, doc)?;
    hydrate_issue_enrichment(ctx, doc, &mut issue).ok()?;
    Some(issue)
}

/// The bounded Issue anchor used by action validation. Enrichment is stored in
/// independently addressed record Bodies and must never be hydrated merely to
/// edit a title, move a card, or authorize a workflow transition.
fn issue_core_state(ctx: &Context<'_>, doc: &str) -> Option<IssueState> {
    let view = ctx.read_collaborative(&issue_key(doc)).ok()??;
    let mut issue = IssueState::from_view(&view);
    let coordinate = crate::record_store::issue_coordinate_for(ctx, doc).ok()??;
    crate::record_store::apply_issue_coordinate(&mut issue, &coordinate);
    crate::record_store::apply_issue_meta(ctx, &mut issue, doc).ok()?;
    // V3 aggregate roots are migration input, never live enrichment truth.
    issue.assignees.clear();
    issue.followers.clear();
    issue.labels.clear();
    issue.milestone = None;
    issue.cycle = None;
    issue.baseline = None;
    issue.comments.clear();
    issue.reactions.clear();
    issue.events.clear();
    issue.events_recorded = 0;
    Some(issue)
}

/// Put an Issue's relation-held facts back onto the state a view is drawn
/// from.
///
/// `issue_core_state` clears these deliberately: what it decodes are the v3
/// aggregate roots, which are migration input and not live truth. The live
/// truth is one `issue_relation` Body per fact, and until something reads
/// those back an `IssueView` reports an issue with no assignees, no labels,
/// no milestone, no cycle and no baseline -- which reads as an issue that
/// has none, rather than as one nobody asked about.
///
/// Set-valued kinds come from the bounded membership posting. The three
/// singleton kinds are read as records instead, for two reasons: their
/// physical identity ignores the target, so one probe finds the current one
/// whatever it points at; and a baseline is stored as a `BaselineRef`, whose
/// revision the relation node does not carry -- it indexes the baseline id
/// alone.
fn enrich_issue_relations(
    ctx: &Context<'_>,
    issue: &mut IssueState,
    doc: &str,
) -> Result<(), Rejection> {
    let actors = |targets: std::collections::BTreeSet<String>| {
        targets
            .into_iter()
            .map(|target| ActorId::parse(&target).ok_or(Rejection::StateCorrupt))
            .collect::<Result<Vec<_>, _>>()
    };
    issue.assignees = actors(issue_relation_targets(
        ctx,
        doc,
        "assignee",
        contract::MAX_ISSUE_ASSIGNEES,
    )?)?;
    issue.followers = actors(issue_relation_targets(
        ctx,
        doc,
        "follower",
        contract::MAX_ISSUE_FOLLOWERS,
    )?)?;
    issue.labels = issue_relation_targets(ctx, doc, "label", contract::MAX_ISSUE_LABELS)?
        .into_iter()
        .collect();
    let singleton = |kind: &str| -> Result<Option<String>, Rejection> {
        Ok(
            crate::record_store::read_issue_relation(ctx, doc, kind, "")?
                .filter(|record| record.present)
                .map(|record| record.target),
        )
    };
    issue.milestone = singleton("milestone")?;
    issue.cycle = singleton("cycle")?;
    issue.baseline = singleton("baseline")?
        .map(|raw| {
            serde_json::from_str::<crate::spec::BaselineRef>(&raw)
                .map_err(|_| Rejection::StateCorrupt)
        })
        .transpose()?;
    Ok(())
}

/// The alias table for exactly one Issue.
///
/// The view paths built an empty one, so every Issue answered with no human
/// reference at all -- `key_alias` absent and `reff` a bare id prefix, which
/// is not a reference somebody can use or type back. One Issue needs one
/// entry, and both halves of it are already at hand: the coordinate the
/// resolver reads, and the project key the handler has loaded to draw the
/// row.
fn aliases_for_issue(
    ctx: &Context<'_>,
    catalog: &CatalogState,
    doc: &str,
) -> Result<crate::views::DerivedAliases, Rejection> {
    let mut aliases = crate::views::DerivedAliases::default();
    let Some(coordinate) = crate::record_store::issue_coordinate_for(ctx, doc)? else {
        return Ok(aliases);
    };
    let Some(project) = catalog.projects.get(&coordinate.placement.project) else {
        return Ok(aliases);
    };
    let Ok(alias) = coordinate.identity.alias.render_short(&project.key) else {
        return Ok(aliases);
    };
    aliases
        .by_alias
        .insert(alias.to_ascii_lowercase(), doc.to_string());
    aliases.by_doc.insert(doc.to_string(), alias.clone());
    aliases.canonical.insert(doc.to_string(), alias);
    Ok(aliases)
}

/// Live and Done-category Issue counts for one project.
///
/// Exact, and bounded by the project's workflow rather than by its size.
/// Every live Issue posts `(kind, project, state)` and a tombstoned one does
/// not, so one direct posting count per state is the whole answer: sum them
/// for the total, sum the Done-category ones for the rest. A count that
/// visits no rows has nothing to cap, so a project of ten thousand Issues
/// answers as readily as one of ten.
///
/// It used to resolve every member row to read its state and its tombstone,
/// which is why it had a ceiling and why it reported a project past that
/// ceiling as unmeasured -- a regression against the plane this replaced,
/// which counted exactly because it held the whole catalog in memory.
fn project_issue_counts(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    project: &str,
) -> Result<Option<(u32, u32)>, Rejection> {
    apply_project_workflow(ctx, catalog, project)?;
    let Some(workflow) = catalog.workflow_head(project) else {
        return Ok(None);
    };
    let states = workflow.body.states.clone();
    let mut total = 0u64;
    let mut done = 0u64;
    for state in &states {
        let counted = find_field_page(
            ctx,
            crate::find::field::KIND_PROJECT_STATE_LIVE,
            runtime::find::Atom::Bytes(crate::find::composite_key([
                "issue",
                project,
                state.state_id.as_str(),
            ])),
            &contract::PageRequest {
                limit: 1,
                cursor: None,
            },
            Vec::new(),
            Vec::new(),
        )?;
        // An absent total is the runtime declining to answer. One state it
        // will not count makes the sum a different number, not a smaller
        // one, so the whole roll-up is unmeasured.
        let Some(held) = counted.matched_total() else {
            return Ok(None);
        };
        total = total.saturating_add(held);
        if crate::dto::StatusCategory::parse(&state.category) == Some(StatusCategory::Done) {
            done = done.saturating_add(held);
        }
    }
    Ok(Some((
        u32::try_from(total).unwrap_or(u32::MAX),
        u32::try_from(done).unwrap_or(u32::MAX),
    )))
}

/// Every page row's alias ordinal, in one query.
///
/// The ordinal is already indexed. `extract_issue_identity` posts
/// `ALIAS_ORDINAL` on a node whose id is `issue-identity:<doc>`, so a whole
/// page of them is one `Seek::Ids` over ids this function can spell without
/// reading anything first.
///
/// The alternative was `aliases_for_issue` per row, which is what the single
/// Issue paths call. That reaches `issue_coordinate_for`: an exact record
/// seek, a placement probe, a transition-head query, a Body read, a re-hash
/// of those bytes to verify the key, and a meta lookup -- five storage
/// operations and a digest, per row, to end up using one field. This packs
/// that field directly.
///
/// An Issue with no identity record is simply absent from the answer. That is
/// the v3 store before its migration, and the correct degradation is the one
/// `canonical_for` already performs: no `key_alias`, and a short `reff` a
/// person can still type.
fn page_alias_ordinals(
    ctx: &Context<'_>,
    docs: &[String],
) -> Result<std::collections::BTreeMap<String, u64>, Rejection> {
    use runtime::find as find_api;
    let mut out = std::collections::BTreeMap::new();
    if docs.is_empty() {
        return Ok(out);
    }
    if docs.len() > ID_RESOLUTION_CHUNK {
        for chunk in docs.chunks(ID_RESOLUTION_CHUNK) {
            out.extend(page_alias_ordinals(ctx, chunk)?);
        }
        return Ok(out);
    }
    let count = u64::try_from(docs.len()).unwrap_or(u64::MAX);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: count.saturating_mul(16),
        edges_visited: 1,
        nodes_visited: count.saturating_mul(2),
        paths_retained: 1,
        candidates_per_branch: count.saturating_mul(2),
        score_evaluations: 1,
        // An ordinal and the id it belongs to. Nothing here is text somebody
        // wrote, so this claims a kilobyte a row rather than the sixteen
        // `find_issue_rows_by_ids` needs for one carrying a title -- and the
        // declared budget is what Find refuses on, before it evaluates.
        projected_bytes: count.saturating_mul(1_024),
        packed_tokens: count.saturating_mul(64),
        wall_millis: 5_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    // Pack takes a canonical set; see `find_issue_rows_by_ids` for what an
    // unsorted literal costs.
    let mut fields = [
        crate::find::field::SOURCE_ID,
        crate::find::field::ALIAS_ORDINAL,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    fields.dedup();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Ids(
                        docs.iter()
                            .map(|doc| {
                                find_api::NodeId::new(format!("issue-identity:{doc}").into_bytes())
                            })
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| Rejection::InvalidRequest)?,
                    )),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![seek],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: u32::try_from(docs.len()).map_err(|_| Rejection::LimitExceeded)?,
            cursor: None,
        })
        .map_err(find_rejection)?;
    for row in answer.rows() {
        let (Some(doc), Some(ordinal)) = (
            result_text(row, crate::find::field::SOURCE_ID),
            result_u64(row, crate::find::field::ALIAS_ORDINAL),
        ) else {
            return Err(Rejection::StateCorrupt);
        };
        out.insert(doc, ordinal);
    }
    Ok(out)
}

/// Give a page of rows the reference a person reads and types.
///
/// `issue_page_row` leaves `key_alias` absent and `reff` a bare 26-character
/// doc id, and nothing downstream filled either in -- so every list and board
/// row said `iss_02CHGHRS442UPH0SM62KRP894N` where the Issue's own detail page
/// said `ENG-12`. The two spellings of one Issue were both wrong at once: the
/// long one is not a reference somebody can use, and it is not even the
/// canonical short handle `canonical_for` renders.
///
/// `render_short` rather than `render`, matching `aliases_for_issue`: the
/// disambiguator is what settles an ambiguous lookup, not what a row shows.
fn apply_page_aliases(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    rows: &mut [crate::dto::Row],
) -> Result<(), Rejection> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut docs = rows
        .iter()
        .map(|row| row.doc_id.to_string())
        .collect::<Vec<_>>();
    docs.sort();
    docs.dedup();
    let ordinals = page_alias_ordinals(ctx, &docs)?;
    let mut asked = std::collections::BTreeSet::new();
    for row in rows.iter_mut() {
        let doc = row.doc_id.to_string();
        // `reff` is deliberately left whole.
        //
        // It reads like the other half of this fix -- `views::canonical_for`
        // renders `iss_02CHGHR`, so why not here. Because that function does
        // not render a fixed width: it takes the shortest prefix at or above
        // `CANONICAL_MIN` that is *unshared with its neighbours*, and the
        // neighbours are the whole catalog, which a page does not hold.
        //
        // A fixed seven would be ambiguous constantly. `mint_ulid` puts a
        // 48-bit millisecond timestamp in the high bits, and seven characters
        // reach only the top 32 of them -- so two Issues created within about
        // sixty-five seconds of each other share that prefix. The resolver
        // refuses an ambiguous short reference rather than guessing, so the
        // rows would have looked tidier and stopped opening.
        //
        // `key_alias` is what a person reads; `reff` is what resolves. Making
        // the second one prettier at the cost of the first one working is the
        // trade this deliberately does not make.
        let project = row.project_id.as_str();
        // Asked once per project, not once per row, and tracked separately
        // from the catalog because a *tombstoned* project is removed from it
        // rather than stored -- so `contains_key` stays false however many
        // times it is loaded, and a page of rows from a deleted project would
        // read its Body once each.
        if asked.insert(project.to_string()) && !catalog.projects.contains_key(project) {
            crate::record_store::apply_project(ctx, catalog, project)?;
        }
        let (Some(&ordinal), Some(meta)) = (ordinals.get(&doc), catalog.projects.get(project))
        else {
            continue;
        };
        let Some(alias) = crate::records::IssueAliasCoordinate::for_issue(ordinal, &row.doc_id)
            .ok()
            .and_then(|alias| alias.render_short(&meta.key).ok())
        else {
            continue;
        };
        row.key_alias = Some(alias);
    }
    Ok(())
}

/// Facet branches one query may carry, and matches one branch may hold.
///
/// These are the two numbers that keep a faceted query inside Find's ceilings,
/// and they are chosen together. `Merge` demands `Flow::Ranked` inputs, and
/// `Rank` reads every candidate in its branch -- so unlike a linear seek, a
/// merged query materialises each branch WHOLE rather than streaming a page out
/// of it. Its cost is the size of what matches, not the size of what is asked
/// for.
///
/// `nodes_visited` is charged once per candidate ranked and once more when
/// `Pack` reads a survivor back, so the binding arithmetic is
/// `branches x matches x 2 <= 100_000`. Twelve and four thousand sit just
/// inside it, and leave `paths_retained` and `candidates_per_branch` (10_000
/// each) with room.
///
/// A thousand rather than more, because `projected_bytes` binds before any of
/// the work dimensions do: a title may be `MAX_TITLE_BYTES` (4 KiB), so eight
/// KiB a row against the 8 MiB ceiling is a thousand rows and no more. It is
/// the same number as the product's own `MAX_PAGE_SIZE`, which is the right
/// coincidence -- a filtered answer that will not fit one page is one the
/// caller should narrow.
///
/// The cliff is deliberate: a facet matching more than that is REFUSED, not
/// truncated. A filter that quietly dropped matches past a limit would be the
/// "3 of 100" defect wearing a server-side coat.
const MAX_FACET_BRANCHES: usize = 12;
const MAX_FACET_MATCHES: u64 = 1_000;

/// One facet axis, as the seek that answers it.
enum FacetSeek {
    /// An exact field on the Issue node itself.
    Direct {
        field: &'static str,
        value: runtime::find::Atom,
    },
    /// A membership posting, which answers with RELATION nodes and therefore
    /// needs one hop along `edge::MEMBER` to become the Issues it is about.
    /// That is the same edge row enrichment walks inbound; here it is walked
    /// outbound, relation to Issue.
    Membership { kind: &'static str, target: String },
}

/// Turn the facets into seeks, one per value.
fn facet_seeks(
    project: Option<&str>,
    facets: &contract::IssueFacets,
) -> Result<Vec<Vec<FacetSeek>>, Rejection> {
    use runtime::find as find_api;
    let mut axes: Vec<Vec<FacetSeek>> = Vec::new();
    if !facets.statuses.is_empty() {
        axes.push(
            facets
                .statuses
                .iter()
                .map(|state| match project {
                    // Scoped by project the posting is one exact tuple; without
                    // a project it is the bare state across the Space, which is
                    // what the caller asked for.
                    Some(project) => FacetSeek::Direct {
                        field: crate::find::field::KIND_PROJECT_STATE,
                        value: find_api::Atom::Bytes(crate::find::composite_key([
                            "issue", project, state,
                        ])),
                    },
                    None => FacetSeek::Direct {
                        field: crate::find::field::STATE,
                        value: find_api::Atom::Text(state.clone()),
                    },
                })
                .collect(),
        );
    }
    if !facets.priorities.is_empty() {
        axes.push(
            facets
                .priorities
                .iter()
                .map(|priority| FacetSeek::Direct {
                    field: crate::find::field::PRIORITY,
                    value: find_api::Atom::Text(priority.clone()),
                })
                .collect(),
        );
    }
    for (kind, values) in [
        ("label", &facets.labels),
        ("assignee", &facets.assignees),
        ("milestone", &facets.milestones),
    ] {
        if values.is_empty() {
            continue;
        }
        axes.push(
            values
                .iter()
                .map(|target| FacetSeek::Membership {
                    kind,
                    target: target.clone(),
                })
                .collect(),
        );
    }
    let branches = axes.iter().map(Vec::len).sum::<usize>();
    if branches > MAX_FACET_BRANCHES {
        return Err(Rejection::LimitExceeded);
    }
    Ok(axes)
}

/// The work one faceted query declares, for `branches` ranked inputs.
///
/// Lifted out of the planner so it can be checked against the ceiling it has
/// to fit inside. The first version of this claimed 16 KiB a row against an
/// 8 MiB projection and every faceted query was refused as `InvalidRequest` --
/// a failure that names the request and not the number in it. A test now
/// proves the declaration fits before anybody has to read that error.
fn facet_bound(branches: u64) -> runtime::find::Bound {
    let reach = branches.saturating_mul(MAX_FACET_MATCHES);
    runtime::find::Bound {
        decoded_bodies: 1,
        postings_read: reach.saturating_mul(4),
        edges_visited: reach,
        // Ranked once per branch candidate, read again by Pack for survivors.
        nodes_visited: reach.saturating_mul(2),
        paths_retained: MAX_FACET_MATCHES,
        candidates_per_branch: MAX_FACET_MATCHES,
        score_evaluations: reach.saturating_mul(2),
        // A title may be MAX_TITLE_BYTES; eight KiB a row is the ceiling
        // divided by MAX_FACET_MATCHES, which is what sets that constant.
        projected_bytes: MAX_FACET_MATCHES.saturating_mul(8 * 1_024),
        packed_tokens: MAX_FACET_MATCHES.saturating_mul(4_096),
        wall_millis: 5_000,
    }
}

/// One faceted page: the whole filtered set, or a refusal.
///
/// A Query carrying `Merge` is not a linear plan, so Find hands back no
/// continuation for it. That sounds like a limitation and is the property this
/// wanted: a faceted answer is COMPLETE or it is refused, never a partial page
/// that a caller might count. `exact_total` is then the row count itself rather
/// than a posting estimate, and "3 of 12" is finally a true sentence.
///
/// The unfaceted path is untouched and still streams through `find_kind_page`
/// with a real cursor. Only a filter pays merge's price, and only a filter
/// needs merge's exactness.
fn find_faceted_issue_page(
    ctx: &Context<'_>,
    project: Option<&str>,
    axes: &[Vec<FacetSeek>],
    all: bool,
) -> Result<runtime::find::Answer, Rejection> {
    use runtime::find as find_api;
    let branches = u64::try_from(axes.iter().map(Vec::len).sum::<usize>())
        .map_err(|_| Rejection::LimitExceeded)?
        .saturating_add(1);
    let bound = facet_bound(branches);
    let mut steps: Vec<find_api::Step> = Vec::new();
    let mut next_id = 1u32;
    let mut alloc = || -> Result<find_api::StepId, Rejection> {
        let id = find_api::StepId::new(next_id).ok_or(Rejection::StateCorrupt)?;
        next_id = next_id.saturating_add(1);
        Ok(id)
    };
    let position = crate::find::field_ref(crate::find::field::KIND_PROJECT_POSITION);
    let rank_by_position = || {
        find_api::Op::Rank(find_api::Rank {
            by: vec![find_api::RankBy::Field(position.clone())],
        })
    };

    // The base: every live Issue, scoped to the project when there is one.
    let base_seek = alloc()?;
    steps.push(find_api::Step {
        id: base_seek,
        input: Vec::new(),
        op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
            field: crate::find::field_ref(if project.is_some() {
                crate::find::field::KIND_PROJECT
            } else {
                crate::find::field::KIND
            }),
            test: find_api::Test::Equal,
            value: project.map_or_else(
                || find_api::Atom::Text("issue".into()),
                |project| find_api::Atom::Bytes(crate::find::composite_key(["issue", project])),
            ),
        })),
        bound,
    });
    let base_ranked = alloc()?;
    steps.push(find_api::Step {
        id: base_ranked,
        input: vec![base_seek],
        op: rank_by_position(),
        bound,
    });

    let mut axis_outputs = vec![base_ranked];
    for axis in axes {
        let mut ranked = Vec::new();
        for facet in axis {
            let seek_id = alloc()?;
            let (field, value) = match facet {
                FacetSeek::Direct { field, value } => (*field, value.clone()),
                FacetSeek::Membership { kind, target } => (
                    crate::find::field::RELATION_TARGET_KIND,
                    find_api::Atom::Bytes(crate::find::composite_key([kind, target.as_str()])),
                ),
            };
            steps.push(find_api::Step {
                id: seek_id,
                input: Vec::new(),
                op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                    field: crate::find::field_ref(field),
                    test: find_api::Test::Equal,
                    value,
                })),
                bound,
            });
            // A membership posting answers with relation nodes. One hop
            // outbound along `edge::MEMBER` turns them into the Issues they are
            // about -- the same edge row enrichment walks inbound, and for the
            // same reason: it reaches memberships and never board history.
            let issues = match facet {
                FacetSeek::Direct { .. } => seek_id,
                FacetSeek::Membership { .. } => {
                    let hop = alloc()?;
                    steps.push(find_api::Step {
                        id: hop,
                        input: vec![seek_id],
                        op: find_api::Op::Walk(find_api::Walk {
                            edges: vec![crate::find::edge_ref(crate::find::edge::MEMBER)],
                            direction: find_api::Direction::Out,
                            min_hops: 1,
                            max_hops: 1,
                            unique: find_api::Unique::Walk,
                            order: find_api::WalkOrder::Breadth,
                            emit: find_api::Emit::Nodes,
                            gate: crate::find::gate_ref(),
                        }),
                        bound,
                    });
                    hop
                }
            };
            // Merge takes ranked branches only; Seek and Walk both answer with
            // plain nodes, so each branch is ranked before it can be combined.
            let ranked_id = alloc()?;
            steps.push(find_api::Step {
                id: ranked_id,
                input: vec![issues],
                op: rank_by_position(),
                bound,
            });
            ranked.push(ranked_id);
        }
        // Within one axis the values union: two labels means either.
        let axis_out = if ranked.len() == 1 {
            ranked[0]
        } else {
            let union = alloc()?;
            steps.push(find_api::Step {
                id: union,
                input: ranked,
                op: find_api::Op::Merge(find_api::Merge {
                    method: find_api::MergeMethod::Union,
                }),
                bound,
            });
            union
        };
        axis_outputs.push(axis_out);
    }

    // Across axes they intersect: a status and a label means both.
    let combined = if axis_outputs.len() == 1 {
        axis_outputs[0]
    } else {
        let intersect = alloc()?;
        steps.push(find_api::Step {
            id: intersect,
            input: axis_outputs,
            op: find_api::Op::Merge(find_api::Merge {
                method: find_api::MergeMethod::Intersection,
            }),
            bound,
        });
        intersect
    };

    let mut predicates = vec![find_api::Predicate {
        field: crate::find::field_ref(crate::find::field::CONFLICTED),
        test: find_api::Test::Equal,
        value: find_api::Atom::Bool(false),
    }];
    if !all {
        predicates.push(find_api::Predicate {
            field: crate::find::field_ref(crate::find::field::TOMBSTONE),
            test: find_api::Test::Equal,
            value: find_api::Atom::Bool(false),
        });
    }
    let keep = alloc()?;
    steps.push(find_api::Step {
        id: keep,
        input: vec![combined],
        op: find_api::Op::Keep(find_api::Keep { predicates }),
        bound,
    });
    // Merge sorts by its own score and then by node key, which is doc-id order
    // and not the order anybody dragged these into. Put the board's own
    // ordering back before packing.
    let ordered = alloc()?;
    steps.push(find_api::Step {
        id: ordered,
        input: vec![keep],
        op: rank_by_position(),
        bound,
    });
    let mut fields = [
        crate::find::field::TITLE,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::PRIORITY,
        crate::find::field::TOMBSTONE,
        crate::find::field::DUE_AT,
        crate::find::field::ESTIMATE,
        crate::find::field::ID,
        crate::find::field::KIND,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    fields.dedup();
    let pack = alloc()?;
    steps.push(find_api::Step {
        id: pack,
        input: vec![ordered],
        op: find_api::Op::Pack(find_api::Pack { fields }),
        bound,
    });
    if steps.len() > find_api::MAX_QUERY_STEPS {
        return Err(Rejection::LimitExceeded);
    }
    ctx.find(find_api::Query {
        schema: crate::find::entity_schema_ref(),
        publication: ctx.world_publication_id().map(|id| id.publication),
        mode: find_api::Mode::Exact,
        steps,
        output: pack,
        bound,
        page_size: u32::try_from(MAX_FACET_MATCHES).unwrap_or(find_api::MAX_PAGE_SIZE),
        cursor: None,
    })
    .map_err(find_rejection)
}

/// Issues whose memberships one Walk resolves.
///
/// Sixteen, and the binding ceiling is `paths_retained`, not the projection.
/// Find's policy allows 100,000 nodes visited and 8 MiB projected but only
/// 10,000 paths retained and 10,000 candidates per branch, so the arithmetic
/// that matters is `chunk x MAX_MEMBERSHIPS_PER_ISSUE <= 10,000` -- which caps
/// the chunk at 25. Sixteen leaves room rather than sitting on the edge.
///
/// The previous attempt at this declared against `projected_bytes` alone and
/// was wrong twice over: it walked `edge::SOURCE`, which reaches every
/// relation including the board history, and it under-declared
/// `nodes_visited`, which Find charges TWICE per emitted node -- once for the
/// incoming posting during the walk, once again when `Pack` reads the node
/// back. Both are accounted for below.
const MEMBERSHIP_WALK_CHUNK: usize = 16;

/// The memberships one Issue may hold, from the caps the write path enforces.
///
/// Derived rather than written down, because a bound that restates a constant
/// is a bound that stops agreeing with it. The three singletons are milestone,
/// cycle and baseline -- `IssueRelationRecord::identity` omits the target for
/// exactly those, so an Issue holds at most one of each.
const MAX_MEMBERSHIPS_PER_ISSUE: u64 = (contract::MAX_ISSUE_ASSIGNEES
    + contract::MAX_ISSUE_FOLLOWERS
    + contract::MAX_ISSUE_LABELS
    + 3) as u64;

/// Every page row's memberships, in one traversal per chunk.
///
/// A membership relation carries `edge::MEMBER` to the Issue it is about, so a
/// page's memberships are the nodes one inbound hop away along that edge. It
/// is deliberately not `edge::SOURCE`: that one is carried by every relation
/// kind, `issue_transition` included, and an Issue's transitions accumulate
/// forever as its card is dragged. `edge::MEMBER` is posted for
/// `MEMBERSHIP_KINDS` and nothing else, so what this reaches is bounded by
/// what the write path caps rather than by how much the board has been used.
///
/// This replaces three exact seeks per row. What it cannot do is ask for only
/// the three kinds a row draws -- `Keep` predicates conjoin and Find has no set
/// test -- so followers, cycle and baseline come back too and are discarded.
/// That waste is now bounded and small; it was the unbounded version that made
/// the traversal a hazard, not the waste itself.
fn page_memberships(
    ctx: &Context<'_>,
    docs: &[String],
) -> Result<std::collections::BTreeMap<String, Vec<(String, String)>>, Rejection> {
    use runtime::find as find_api;
    let mut out: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    if docs.is_empty() {
        return Ok(out);
    }
    if docs.len() > MEMBERSHIP_WALK_CHUNK {
        for chunk in docs.chunks(MEMBERSHIP_WALK_CHUNK) {
            for (doc, found) in page_memberships(ctx, chunk)? {
                out.entry(doc).or_default().extend(found);
            }
        }
        return Ok(out);
    }
    let count = u64::try_from(docs.len()).unwrap_or(u64::MAX);
    let reachable = count.saturating_mul(MAX_MEMBERSHIPS_PER_ISSUE);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: reachable.saturating_mul(8),
        edges_visited: reachable,
        paths_retained: reachable,
        candidates_per_branch: reachable,
        // Charged once per incoming posting in the walk and once more when
        // `Pack` reads each emitted node back, plus one per seed.
        nodes_visited: reachable.saturating_mul(2).saturating_add(count),
        score_evaluations: reachable,
        // A kind and two ids. Half a kilobyte a row is generous for that.
        projected_bytes: reachable.saturating_mul(512),
        packed_tokens: reachable.saturating_mul(64),
        wall_millis: 5_000,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let walk = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::RELATION_KIND,
        crate::find::field::SOURCE_ID,
        crate::find::field::TARGET_ID,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect::<Vec<_>>();
    fields.sort();
    fields.dedup();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Ids(
                        docs.iter()
                            .map(|doc| find_api::NodeId::new(doc.as_bytes().to_vec()))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(|_| Rejection::InvalidRequest)?,
                    )),
                    bound,
                },
                find_api::Step {
                    id: walk,
                    input: vec![seek],
                    op: find_api::Op::Walk(find_api::Walk {
                        edges: vec![crate::find::edge_ref(crate::find::edge::MEMBER)],
                        // The edge points membership -> Issue, so the Issues
                        // are where it lands and the memberships are the catch.
                        direction: find_api::Direction::In,
                        min_hops: 1,
                        max_hops: 1,
                        unique: find_api::Unique::Walk,
                        order: find_api::WalkOrder::Breadth,
                        emit: find_api::Emit::Nodes,
                        gate: crate::find::gate_ref(),
                    }),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![walk],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: u32::try_from(reachable).unwrap_or(find_api::MAX_PAGE_SIZE),
            cursor: None,
        })
        .map_err(find_rejection)?;
    // No continuation guard here on purpose. A Query carrying a Walk is not a
    // linear plan, so Find hard-codes its next position to none and overflow
    // arrives as a refusal rather than as a short answer -- the runtime will
    // not hand back a truncated page for this shape. A guard on `next_cursor`
    // would read as the safety and be dead code.
    for row in answer.rows() {
        let (Some(kind), Some(source), Some(target)) = (
            result_text(row, crate::find::field::RELATION_KIND),
            result_text(row, crate::find::field::SOURCE_ID),
            result_text(row, crate::find::field::TARGET_ID),
        ) else {
            return Err(Rejection::StateCorrupt);
        };
        out.entry(source).or_default().push((kind, target));
    }
    Ok(out)
}

/// Put the relation-held facts a list row shows onto one row.
///
/// `issue_page_row` builds from one Find row, which carries the Issue's own
/// coordinates and none of its memberships -- so assignees, labels and
/// milestone came back empty on every list and board. A tracker list that
/// does not say who an Issue is assigned to is not a list of Issues, so this
/// is enrichment the collection page owes rather than one it may omit.
///
/// Three bounded exact seeks per row, on the membership posting that exists
/// for exactly this. They are index lookups returning a handful of rows each,
/// not scans, and the page that calls this is already bounded.
fn enrich_issue_page(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    rows: &mut [crate::dto::Row],
    me: Option<&ActorId>,
) -> Result<(), Rejection> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut docs = rows
        .iter()
        .map(|row| row.doc_id.to_string())
        .collect::<Vec<_>>();
    docs.sort();
    docs.dedup();
    let memberships = page_memberships(ctx, &docs)?;
    for row in rows.iter_mut() {
        // Sets, because the seeks this replaced answered from `BTreeSet`s: a
        // row's assignees were sorted and deduplicated, and taking whatever
        // order the index returned would silently reorder every facepile.
        let mut assignees = std::collections::BTreeSet::new();
        let mut labels = std::collections::BTreeSet::new();
        // Singleton, and provably so: `IssueRelationRecord::identity` omits
        // the target for `milestone`, `cycle` and `baseline`, so one Issue
        // holds exactly one Body -- and therefore one node -- per kind.
        let mut milestone = None;
        for (kind, target) in memberships
            .get(&row.doc_id.to_string())
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            match kind.as_str() {
                "assignee" => {
                    assignees.insert(target.clone());
                }
                "label" => {
                    labels.insert(target.clone());
                }
                "milestone" => milestone = Some(target.clone()),
                // follower, cycle and baseline ride the same edge and are not
                // drawn on a row. Bounded by the write caps, so the cost of
                // carrying them is small and known.
                _ => {}
            }
        }
        // The seeks refused a set larger than its cap rather than truncating
        // it. Keep refusing: a row that quietly showed the first hundred and
        // twenty-eight of more is wrong without saying so.
        if assignees.len() > contract::MAX_ISSUE_ASSIGNEES
            || labels.len() > contract::MAX_ISSUE_LABELS
        {
            return Err(Rejection::StateCorrupt);
        }
        row.assignees = assignees
            .iter()
            .map(|target| ActorId::parse(target).ok_or(Rejection::StateCorrupt))
            .collect::<Result<Vec<_>, _>>()?;
        row.assignee_summary = crate::views::assignee_summary(&row.assignees, me);
        // A row carries label NAMES, so an id that has no registry entry renders
        // as itself rather than disappearing -- the same rule the assembled view
        // applies.
        let mut names = Vec::new();
        for label in &labels {
            crate::record_store::apply_label(ctx, catalog, label)?;
            names.push(
                catalog
                    .labels
                    .get(label)
                    .map_or_else(|| label.clone(), |meta| meta.name.clone()),
            );
        }
        row.label_names = names;
        // `extract_issue_relation` posts no node for a cleared relation, so
        // absence here is the same fact the `present` flag used to carry -- and
        // it now comes from the pinned publication like the rest of the row,
        // rather than from a live Body read beside it.
        row.milestone = milestone;
        row.enrichment_complete = true;
    }
    Ok(())
}

/// The KEYs of the projects a team owns.
///
/// A project records its team as its own source coordinate, so this is the
/// `(kind, source)` posting read in the one direction it already answers --
/// no reverse coordinate and no scan over projects.
fn team_project_keys(ctx: &Context<'_>, team: &str) -> Result<Vec<String>, Rejection> {
    let limit = u32::try_from(MAX_ROLLUP_MEMBERS.saturating_add(1))
        .map_err(|_| Rejection::LimitExceeded)?;
    let answer = find_field_page(
        ctx,
        crate::find::field::KIND_SOURCE,
        runtime::find::Atom::Bytes(crate::find::composite_key(["project", team])),
        &contract::PageRequest {
            limit,
            cursor: None,
        },
        Vec::new(),
        vec![crate::find::field_ref(crate::find::field::ENTITY_KEY)],
    )?;
    if answer.next_cursor().is_some() {
        return Err(Rejection::LimitExceeded);
    }
    let mut keys = Vec::new();
    for row in answer.rows() {
        if result_text(row, crate::find::field::KIND).as_deref() != Some("project") {
            continue;
        }
        if let Some(key) = result_text(row, crate::find::field::ENTITY_KEY) {
            keys.push(key);
        }
    }
    keys.sort();
    Ok(keys)
}

/// The number the next Issue in this project answers to.
///
/// A person says "ENG-4", not "ENG-4611686018427387904-a1b2...". The number
/// has to be small and it has to go up, and neither is a property a hash can
/// have -- so it is counted rather than derived.
///
/// Counted, not held in a register: the count of Issues already in the
/// project is one bounded posting count, and reading it contends with
/// nothing. Two devices creating offline can therefore both take the same
/// number, and that is the trade this makes deliberately. The number is a
/// LABEL, not the identity: the alias coordinate pairs it with an
/// independent 128-bit disambiguator derived from the Issue id, so two
/// Issues wearing "ENG-4" remain distinct records, resolve distinctly by
/// their full form, and never overwrite one another. What a collision costs
/// is that the short form is ambiguous for those two until somebody says
/// which -- not that anything is lost.
fn next_project_ordinal(
    ctx: &Context<'_>,
    project: &str,
    run: &mut BTreeMap<String, u64>,
) -> Result<u64, Rejection> {
    // What this submission has already handed out, if anything. One action
    // can create many Issues, and it reads one pinned publication throughout
    // — which by construction does not contain the Issues it is itself
    // staging. Counting afresh per Issue therefore returned the same number
    // every time, so a change set creating sixty-four Issues gave all
    // sixty-four the same one. That is not the offline collision this design
    // accepts; it is a collision inside a single operation, every time.
    if let Some(next) = run.get(project) {
        let ordinal = *next;
        run.insert(project.to_string(), ordinal.saturating_add(1));
        return Ok(ordinal);
    }
    let counted = find_kind_page(
        ctx,
        "issue",
        Some(project),
        &contract::PageRequest {
            limit: 1,
            cursor: None,
        },
        Vec::new(),
        Vec::new(),
    )?;
    // An absent total is the runtime declining to answer, and a number
    // guessed from nothing would collide with every Issue already here.
    let held = counted.matched_total().ok_or(Rejection::LimitExceeded)?;
    let ordinal = held.saturating_add(1);
    run.insert(project.to_string(), ordinal.saturating_add(1));
    Ok(ordinal)
}

fn apply_project_workflow(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    project: &str,
) -> Result<(), Rejection> {
    let projection = workflow_projection(ctx, project)?;
    if !projection.conflict_heads.is_empty() {
        return Err(Rejection::Conflict);
    }
    let revision = projection.revision.ok_or(Rejection::Conflict)?;
    let revisions = catalog
        .workflow_revisions
        .entry(project.to_string())
        .or_default();
    if !revisions
        .iter()
        .any(|current| current.revision_id == revision.revision_id)
    {
        revisions.push(revision);
    }
    Ok(())
}

fn issue_write_state(
    ctx: &Context<'_>,
    doc: &str,
    workflow: bool,
) -> Result<(CatalogState, IssueState), Rejection> {
    let issue = issue_core_state(ctx, doc).ok_or(Rejection::InvalidRequest)?;
    let mut catalog = CatalogState::default();
    crate::record_store::apply_project(ctx, &mut catalog, &issue.project)?;
    if !catalog.projects.contains_key(&issue.project) {
        return Err(Rejection::StateCorrupt);
    }
    let coordinate =
        crate::record_store::issue_coordinate_for(ctx, doc)?.ok_or(Rejection::StateCorrupt)?;
    crate::record_store::apply_issue_catalog(
        &mut catalog,
        &BTreeMap::from([(doc.to_owned(), coordinate)]),
    );
    if workflow {
        apply_project_workflow(ctx, &mut catalog, &issue.project)?;
    }
    Ok((catalog, issue))
}

/// A Spec's heads Body, if it exists and names this Spec.
fn spec_heads(ctx: &Context<'_>, spec: &str) -> Option<runtime::world::CollaborativeBody> {
    let spec_id = crate::ids::SpecId::parse(spec)?;
    let heads = ctx
        .read_collaborative(&crate::records::spec_heads_key(&spec_id))
        .ok()??;
    heads
        .registers
        .get(crate::records::roots::IDENTITY)
        .is_some_and(|identity| identity.as_slice() == spec.as_bytes())
        .then_some(heads)
}

/// The kind a Spec was created as, from its heads Body.
///
/// A revision is posted to the corpus under its Spec's kind
/// (`find::extract_spec_revision`), and the kind is invariant across a
/// Spec's revisions: `SpecRevise` clones the head's body and `SpecResolve`
/// refuses a body whose kind differs from the first revision's. The heads
/// Body records it beside the head sets, so it names the exact seek. This
/// replaced a hunt across a hand-written kind list that had stopped
/// agreeing with `Kind::ALL` -- six kinds could be created and never read.
fn spec_kind(heads: &runtime::world::CollaborativeBody) -> Option<crate::spec::Kind> {
    heads
        .registers
        .get(crate::records::roots::KIND)
        .and_then(|value| std::str::from_utf8(value).ok())
        .and_then(crate::spec::Kind::parse)
}

/// One exact revision of a Spec, whether or not it is still a head or issued.
///
/// A revision is immutable and stays readable after a successor supersedes
/// it; a Baseline pins one, and an incorporation names one, precisely so that
/// later drafting cannot move what governs. So this reads by id rather than
/// through the head sets, and answers `None` only for a revision this replica
/// has not received or one that does not belong to `spec`.
fn spec_revision_at(
    ctx: &Context<'_>,
    spec: &str,
    kind: crate::spec::Kind,
    revision: &str,
) -> Option<crate::spec::Revision> {
    let row =
        unique_find_row(ctx, crate::find::field::ID, revision, kind.as_str(), None).ok()??;
    if result_text(&row, crate::find::field::SOURCE_ID).as_deref() != Some(spec) {
        return None;
    }
    let bytes = ctx.read_body(&row.source).ok()??;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes).ok()?;
    let record = crate::records::SpecRevisionRecord::decode_canonical(&envelope.record).ok()?;
    let found = record.revision;
    (found.revision == revision && found.body.spec == spec && found.body.kind == kind)
        .then_some(found)
}

fn decode_head_set(heads: &runtime::world::CollaborativeBody, path: &str) -> Option<Vec<String>> {
    let mut values = heads
        .sets
        .get(path)
        .into_iter()
        .flatten()
        .map(|value| String::from_utf8(value.clone()).ok())
        .collect::<Option<Vec<_>>>()?;
    values.sort();
    values.dedup();
    Some(values)
}

fn spec_state(ctx: &Context<'_>, spec: &str) -> Option<crate::spec::Spec> {
    let heads = spec_heads(ctx, spec)?;
    let explicit_heads = decode_head_set(&heads, crate::records::roots::HEADS)?;
    let explicit_issued = decode_head_set(&heads, crate::records::roots::ISSUED_HEADS)?;
    if explicit_heads.is_empty() {
        return None;
    }
    let mut wanted = explicit_heads
        .iter()
        .chain(&explicit_issued)
        .cloned()
        .collect::<Vec<_>>();
    wanted.sort();
    wanted.dedup();
    let kind = spec_kind(&heads)?;
    let mut revisions = Vec::with_capacity(wanted.len());
    for revision in wanted {
        revisions.push(spec_revision_at(ctx, spec, kind, &revision)?);
    }
    revisions.sort_by(|left, right| left.revision.cmp(&right.revision));
    Some(crate::spec::Spec {
        revisions,
        observations: Vec::new(),
        explicit_heads,
        explicit_issued,
    })
}

/// A Baseline's heads Body, if it exists and names this Baseline.
fn baseline_heads(ctx: &Context<'_>, baseline: &str) -> Option<runtime::world::CollaborativeBody> {
    let baseline_id = crate::ids::BaselineId::parse(baseline)?;
    let heads = ctx
        .read_collaborative(&crate::records::baseline_heads_key(&baseline_id))
        .ok()??;
    heads
        .registers
        .get(crate::records::roots::IDENTITY)
        .is_some_and(|identity| identity.as_slice() == baseline.as_bytes())
        .then_some(heads)
}

/// One exact revision of a Baseline, by id. Same reasoning as
/// [`spec_revision_at`]: an Issue binds to an exact revision, and the binding
/// must survive the Baseline being revised.
fn baseline_revision_at(
    ctx: &Context<'_>,
    baseline: &str,
    revision: &str,
) -> Option<crate::spec::BaselineRevision> {
    let row = unique_find_row(
        ctx,
        crate::find::field::ID,
        revision,
        "baseline_revision",
        None,
    )
    .ok()??;
    if result_text(&row, crate::find::field::SOURCE_ID).as_deref() != Some(baseline) {
        return None;
    }
    let bytes = ctx.read_body(&row.source).ok()??;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes).ok()?;
    let record = crate::records::BaselineRevisionRecord::decode_canonical(&envelope.record).ok()?;
    let found = record.revision;
    (found.revision == revision && found.body.baseline == baseline).then_some(found)
}

fn baseline_state(ctx: &Context<'_>, baseline: &str) -> Option<crate::spec::Baseline> {
    let heads = baseline_heads(ctx, baseline)?;
    let explicit_heads = decode_head_set(&heads, crate::records::roots::HEADS)?;
    let explicit_issued = decode_head_set(&heads, crate::records::roots::ISSUED_HEADS)?;
    if explicit_heads.is_empty() {
        return None;
    }
    let mut wanted = explicit_heads
        .iter()
        .chain(&explicit_issued)
        .cloned()
        .collect::<Vec<_>>();
    wanted.sort();
    wanted.dedup();
    let mut revisions = Vec::with_capacity(wanted.len());
    for revision in wanted {
        revisions.push(baseline_revision_at(ctx, baseline, &revision)?);
    }
    revisions.sort_by(|left, right| left.revision.cmp(&right.revision));
    Some(crate::spec::Baseline {
        revisions,
        explicit_heads,
        explicit_issued,
    })
}

fn spec_observation_state(
    ctx: &Context<'_>,
    spec: &str,
    observation: &str,
) -> Result<Option<crate::spec::Observation>, Rejection> {
    let Some(row) = unique_find_row(
        ctx,
        crate::find::field::ID,
        observation,
        "spec_observation_fact",
        None,
    )?
    else {
        return Ok(None);
    };
    if result_text(&row, crate::find::field::SOURCE_ID).as_deref() != Some(spec) {
        return Err(Rejection::InvalidRequest);
    }
    if unique_find_row(
        ctx,
        crate::find::field::ID,
        &format!("observation-retraction:{observation}"),
        "spec_observation_fact",
        None,
    )?
    .is_some()
    {
        return Ok(None);
    }
    let bytes = ctx.read_body(&row.source)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let crate::records::SpecObservationRecord::Assert {
        project: _,
        observation: record,
    } = crate::records::SpecObservationRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?
    else {
        return Err(Rejection::StateCorrupt);
    };
    Ok(Some(record))
}

/// Every Spec in the Space.
///
/// Enumerating `spec_schema()` Bodies answers nothing: no such Body is
/// written any more. A Spec is a heads Body plus its revision records, which
/// is what `spec_state` reads and what `baseline_state` reads for its own
/// kind -- so enumerate the heads and assemble each the same way. Reading the
/// retired kind is why a Packet reported every governing Spec as missing.
fn all_specs(ctx: &Context<'_>) -> Vec<crate::spec::Spec> {
    let mut specs: Vec<_> = ctx
        .bodies_with_schema(
            &contract::world_id(),
            &crate::records::PhysicalSchema::SpecHeads.declaration().id,
        )
        .iter()
        .filter_map(|key| ctx.read_collaborative(key).ok().flatten())
        .filter_map(|view| {
            let id = view.registers.get(crate::records::roots::IDENTITY)?;
            String::from_utf8(id.clone()).ok()
        })
        .filter_map(|spec| spec_state(ctx, &spec))
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

/// Every Baseline in the Space, read the way `baseline_state` reads one.
fn all_baselines(ctx: &Context<'_>) -> Vec<crate::spec::Baseline> {
    let mut baselines: Vec<_> = ctx
        .bodies_with_schema(
            &contract::world_id(),
            &crate::records::PhysicalSchema::BaselineHeads
                .declaration()
                .id,
        )
        .iter()
        .filter_map(|key| ctx.read_collaborative(key).ok().flatten())
        .filter_map(|view| {
            let id = view.registers.get(crate::records::roots::IDENTITY)?;
            String::from_utf8(id.clone()).ok()
        })
        .filter_map(|baseline| baseline_state(ctx, &baseline))
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
        .flatten()
        .map(|view| RelationState::from_view(&view))
}

fn count(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// V3-only decoder used exclusively by the one-time v4 migrator. The
/// canonical [`crate::spec::Body`] has no root-only coordinate and therefore
/// cannot accidentally serialize this shape back into durable truth.
#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySpecBody {
    spec: String,
    project: String,
    kind: crate::spec::Kind,
    #[serde(default)]
    generation: String,
    title: String,
    #[serde(default)]
    text: String,
    state: crate::spec::State,
    #[serde(default)]
    links: Vec<crate::spec::Link>,
    #[serde(default)]
    plan: Option<crate::spec::PlanData>,
    author: String,
    ts: u64,
}

#[derive(Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacySpecRevision {
    revision: String,
    #[serde(default)]
    predecessors: Vec<String>,
    body: LegacySpecBody,
}

fn ordered_spec_revisions(
    revisions: BTreeMap<String, crate::spec::Revision>,
) -> Result<Vec<crate::spec::Revision>, Rejection> {
    let ids = revisions.keys().cloned().collect::<BTreeSet<_>>();
    let mut pending = revisions;
    let mut ordered = Vec::new();
    while !pending.is_empty() {
        let ready = pending.iter().find_map(|(id, revision)| {
            revision
                .predecessors
                .iter()
                .all(|predecessor| ids.contains(predecessor) && !pending.contains_key(predecessor))
                .then(|| id.clone())
        });
        let Some(ready) = ready else {
            return Err(Rejection::StateCorrupt);
        };
        ordered.push(pending.remove(&ready).ok_or(Rejection::StateCorrupt)?);
    }
    Ok(ordered)
}

fn ordered_baseline_revisions(
    revisions: BTreeMap<String, crate::spec::BaselineRevision>,
) -> Result<Vec<crate::spec::BaselineRevision>, Rejection> {
    let ids = revisions.keys().cloned().collect::<BTreeSet<_>>();
    let mut pending = revisions;
    let mut ordered = Vec::new();
    while !pending.is_empty() {
        let ready = pending.iter().find_map(|(id, revision)| {
            revision
                .predecessors
                .iter()
                .all(|predecessor| ids.contains(predecessor) && !pending.contains_key(predecessor))
                .then(|| id.clone())
        });
        let Some(ready) = ready else {
            return Err(Rejection::StateCorrupt);
        };
        ordered.push(pending.remove(&ready).ok_or(Rejection::StateCorrupt)?);
    }
    Ok(ordered)
}

fn spec_issued_ids(spec: &crate::spec::Spec) -> Vec<String> {
    match spec.issued() {
        crate::spec::Issued::None => Vec::new(),
        crate::spec::Issued::One(revision) => vec![revision.revision.clone()],
        crate::spec::Issued::Conflict(revisions) => revisions
            .into_iter()
            .map(|revision| revision.revision.clone())
            .collect(),
    }
}

fn baseline_issued_ids(baseline: &crate::spec::Baseline) -> Vec<String> {
    match baseline.issued() {
        crate::spec::BaselineIssued::None => Vec::new(),
        crate::spec::BaselineIssued::One(revision) => vec![revision.revision.clone()],
        crate::spec::BaselineIssued::Conflict(revisions) => revisions
            .into_iter()
            .map(|revision| revision.revision.clone())
            .collect(),
    }
}

fn migration_spec_revisions(
    ctx: &Context<'_>,
    body_key: &BodyKey,
    view: &fabric::CollaborativeView,
    publication: runtime::publication::PublicationId,
) -> Result<(BTreeMap<String, crate::spec::Revision>, bool), Rejection> {
    let records = view.maps.get("revisions").ok_or(Rejection::StateCorrupt)?;
    let mut canonical = BTreeMap::<String, crate::spec::Revision>::new();
    let mut legacy = BTreeMap::<String, LegacySpecRevision>::new();
    for (stored_id, raw) in records {
        if let Ok(revision) = serde_json::from_slice::<crate::spec::Revision>(raw) {
            if stored_id != &revision.revision
                || contract::spec_key(&revision.body.spec) != *body_key
                || canonical.insert(stored_id.clone(), revision).is_some()
            {
                return Err(Rejection::StateCorrupt);
            }
        } else {
            let revision: LegacySpecRevision =
                serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
            if stored_id != &revision.revision
                || contract::spec_key(&revision.body.spec) != *body_key
                || legacy.insert(stored_id.clone(), revision).is_some()
            {
                return Err(Rejection::StateCorrupt);
            }
        }
    }
    if !canonical.is_empty() && !legacy.is_empty() {
        return Err(Rejection::StateCorrupt);
    }
    if !canonical.is_empty() {
        let ordered = ordered_spec_revisions(canonical)?;
        return Ok((
            ordered
                .into_iter()
                .map(|revision| (revision.revision.clone(), revision))
                .collect(),
            false,
        ));
    }
    let legacy_ids = legacy.keys().cloned().collect::<BTreeSet<_>>();
    let mut mapped = BTreeMap::<String, crate::spec::Revision>::new();
    while !legacy.is_empty() {
        let ready = legacy.iter().find_map(|(id, revision)| {
            revision
                .predecessors
                .iter()
                .all(|predecessor| {
                    !legacy_ids.contains(predecessor) || mapped.contains_key(predecessor)
                })
                .then(|| id.clone())
        });
        let old = ready.ok_or(Rejection::StateCorrupt)?;
        let revision = legacy.remove(&old).ok_or(Rejection::StateCorrupt)?;
        if !revision.body.generation.is_empty()
            && (revision.body.generation.len() != 64
                || data_encoding::HEXLOWER
                    .decode(revision.body.generation.as_bytes())
                    .is_err())
        {
            return Err(Rejection::StateCorrupt);
        }
        let plan = if revision.body.kind == crate::spec::Kind::Plan {
            Some(
                revision
                    .body
                    .plan
                    .unwrap_or(crate::spec::PlanData { roots: Vec::new() }),
            )
        } else {
            revision.body.plan
        };
        // A migrated revision is history, not a new authoring: its plan roots
        // are what they were, including Issues since deleted or abandoned.
        // Only the plan's structure is checked here; the live rule that every
        // root must name an existing Issue in the same project belongs to
        // authoring, and reads already surface a dangling root as a Packet
        // conflict rather than an error.
        if let Some(plan) = plan.as_ref() {
            plan.validate().map_err(|_| Rejection::StateCorrupt)?;
        }
        let body = crate::spec::Body {
            spec: revision.body.spec,
            project: revision.body.project,
            kind: revision.body.kind,
            publication,
            title: revision.body.title,
            text: revision.body.text,
            state: revision.body.state,
            links: revision.body.links,
            plan,
            author: revision.body.author,
            ts: revision.body.ts,
        };
        let predecessors = revision
            .predecessors
            .iter()
            .map(|predecessor| {
                let canonical = mapped
                    .get(predecessor)
                    .map_or(predecessor.as_str(), |revision| revision.revision.as_str());
                crate::spec::decode_revision(canonical).ok_or(Rejection::StateCorrupt)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let migrated =
            crate::spec::build_revision(body, predecessors).map_err(|_| Rejection::StateCorrupt)?;
        mapped.insert(old, migrated);
    }
    Ok((mapped, true))
}

fn migration_spec_window(
    ctx: &Context<'_>,
    body_key: &BodyKey,
    subitem: &str,
    view: &fabric::CollaborativeView,
    publication: runtime::publication::PublicationId,
) -> Result<crate::record_store::Batch, Rejection> {
    let (revisions, legacy) = migration_spec_revisions(ctx, body_key, view, publication)?;
    if subitem == "$empty" {
        return Ok(crate::record_store::Batch::default());
    }
    if let Some(stored_id) = subitem.strip_prefix("11a:revision:") {
        let revision = revisions.get(stored_id).ok_or(Rejection::StateCorrupt)?;
        let stored = crate::records::SpecRevisionRecord {
            revision: revision.clone(),
        };
        let present = crate::record_store::migration_immutable_present(
            ctx,
            crate::records::PhysicalSchema::SpecRevision,
            crate::records::RecordBodyIdentityRecord {
                owner: revision.body.spec.clone(),
                record: revision.revision.clone(),
            },
            stored
                .encode_canonical()
                .map_err(|_| Rejection::StateCorrupt)?,
            crate::find::field::REVISION,
            &revision.revision,
            &[
                (crate::find::field::SOURCE_ID, &revision.body.spec),
                (crate::find::field::RELATION_KIND, "spec_revision"),
            ],
        )?;
        let mut batch = if present {
            crate::record_store::Batch::default()
        } else {
            crate::record_store::write_spec_revision_record(ctx, revision)?
        };
        if legacy {
            let alias = crate::records::RevisionAliasRecord {
                spec: revision.body.spec.clone(),
                legacy_revision: stored_id.into(),
                canonical_revision: revision.revision.clone(),
            };
            if !crate::record_store::migration_immutable_present(
                ctx,
                crate::records::PhysicalSchema::RevisionAlias,
                crate::records::RecordBodyIdentityRecord {
                    owner: alias.spec.clone(),
                    record: alias.legacy_revision.clone(),
                },
                alias
                    .encode_canonical()
                    .map_err(|_| Rejection::StateCorrupt)?,
                crate::find::field::SOURCE_ID,
                &alias.legacy_revision,
                &[
                    (crate::find::field::KIND, "relation"),
                    (crate::find::field::RELATION_KIND, "revision_alias"),
                ],
            )? {
                batch.absorb(crate::record_store::write_revision_alias(ctx, &alias)?);
            }
        }
        return Ok(batch);
    }
    let first = revisions.values().next().ok_or(Rejection::StateCorrupt)?;
    if let Some(raw) = subitem.strip_prefix("11b:observation:") {
        let ordinal = raw.parse::<usize>().map_err(|_| Rejection::StateCorrupt)?;
        let observation: crate::spec::Observation = serde_json::from_slice(
            view.sets
                .get("observations")
                .and_then(|records| records.get(ordinal))
                .ok_or(Rejection::StateCorrupt)?,
        )
        .map_err(|_| Rejection::StateCorrupt)?;
        if observation.spec != first.body.spec {
            return Err(Rejection::StateCorrupt);
        }
        let semantic_id = observation.observation.clone();
        let record = crate::records::SpecObservationRecord::Assert {
            project: first.body.project.clone(),
            observation,
        };
        let identity = record.identity();
        if crate::record_store::migration_immutable_present(
            ctx,
            crate::records::PhysicalSchema::SpecObservation,
            crate::records::RecordBodyIdentityRecord {
                owner: record.spec().into(),
                record: identity,
            },
            record
                .encode_canonical()
                .map_err(|_| Rejection::StateCorrupt)?,
            crate::find::field::ID,
            &semantic_id,
            &[
                (crate::find::field::KIND, "spec_observation_fact"),
                (crate::find::field::SOURCE_ID, record.spec()),
                (crate::find::field::STATE, "assert"),
            ],
        )? {
            return Ok(crate::record_store::Batch::default());
        }
        return crate::record_store::write_spec_observation(ctx, &record);
    }
    if subitem == "11c:heads" {
        let ids = revisions
            .values()
            .map(|revision| revision.revision.clone())
            .collect::<BTreeSet<_>>();
        let predecessors = revisions
            .values()
            .flat_map(|revision| revision.predecessors.iter().cloned())
            .collect::<BTreeSet<_>>();
        if !predecessors.is_subset(&ids) {
            return Err(Rejection::StateCorrupt);
        }
        let heads = ids.difference(&predecessors).cloned().collect();
        let spec = crate::ids::SpecId::parse(&first.body.spec).ok_or(Rejection::StateCorrupt)?;
        return crate::record_store::migration_exact_set(
            ctx,
            crate::records::PhysicalSchema::SpecHeads,
            &crate::records::spec_heads_key(&spec),
            &first.body.spec,
            &[
                (crate::records::roots::PROJECT, first.body.project.as_str()),
                (crate::records::roots::KIND, first.body.kind.as_str()),
            ],
            crate::records::roots::HEADS,
            &heads,
            true,
        );
    }
    if subitem == "11d:issued" {
        let state = crate::spec::Spec {
            revisions: revisions.values().cloned().collect(),
            observations: Vec::new(),
            explicit_heads: Vec::new(),
            explicit_issued: Vec::new(),
        };
        let issued = spec_issued_ids(&state);
        if issued.len() > crate::records::MAX_CONCURRENT_HEADS {
            return Err(Rejection::Conflict);
        }
        let issued = issued.into_iter().collect::<BTreeSet<_>>();
        let spec = crate::ids::SpecId::parse(&first.body.spec).ok_or(Rejection::StateCorrupt)?;
        return crate::record_store::migration_exact_set(
            ctx,
            crate::records::PhysicalSchema::SpecHeads,
            &crate::records::spec_heads_key(&spec),
            &first.body.spec,
            &[
                (crate::records::roots::PROJECT, first.body.project.as_str()),
                (crate::records::roots::KIND, first.body.kind.as_str()),
            ],
            crate::records::roots::ISSUED_HEADS,
            &issued,
            false,
        );
    }
    Err(Rejection::ContractViolation)
}

fn migration_baseline_window(
    ctx: &Context<'_>,
    body_key: &BodyKey,
    subitem: &str,
    view: &fabric::CollaborativeView,
) -> Result<crate::record_store::Batch, Rejection> {
    let records = view.maps.get("revisions").ok_or(Rejection::StateCorrupt)?;
    let mut revisions = BTreeMap::<String, crate::spec::BaselineRevision>::new();
    for (stored_id, raw) in records {
        let revision: crate::spec::BaselineRevision =
            serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
        if stored_id != &revision.revision
            || contract::baseline_key(&revision.body.baseline) != *body_key
            || revisions.insert(stored_id.clone(), revision).is_some()
        {
            return Err(Rejection::StateCorrupt);
        }
    }
    let ordered = ordered_baseline_revisions(revisions)?;
    if subitem == "$empty" {
        return Ok(crate::record_store::Batch::default());
    }
    if let Some(stored_id) = subitem.strip_prefix("11d:revision:") {
        let revision = ordered
            .iter()
            .find(|revision| revision.revision == stored_id)
            .ok_or(Rejection::StateCorrupt)?;
        let stored = crate::records::BaselineRevisionRecord {
            revision: revision.clone(),
        };
        return if crate::record_store::migration_immutable_present(
            ctx,
            crate::records::PhysicalSchema::BaselineRevision,
            crate::records::RecordBodyIdentityRecord {
                owner: revision.body.baseline.clone(),
                record: revision.revision.clone(),
            },
            stored
                .encode_canonical()
                .map_err(|_| Rejection::StateCorrupt)?,
            crate::find::field::REVISION,
            &revision.revision,
            &[
                (crate::find::field::KIND, "baseline_revision"),
                (crate::find::field::SOURCE_ID, &revision.body.baseline),
                (crate::find::field::RELATION_KIND, "baseline_revision"),
            ],
        )? {
            Ok(crate::record_store::Batch::default())
        } else {
            crate::record_store::write_baseline_revision_record(ctx, revision)
        };
    }
    let baseline = ordered
        .first()
        .map(|revision| revision.body.baseline.clone())
        .ok_or(Rejection::StateCorrupt)?;
    if subitem == "11e:heads" {
        let ids = ordered
            .iter()
            .map(|revision| revision.revision.clone())
            .collect::<BTreeSet<_>>();
        let predecessors = ordered
            .iter()
            .flat_map(|revision| revision.predecessors.iter().cloned())
            .collect::<BTreeSet<_>>();
        if !predecessors.is_subset(&ids) {
            return Err(Rejection::StateCorrupt);
        }
        let heads = ids.difference(&predecessors).cloned().collect();
        let first = ordered.first().ok_or(Rejection::StateCorrupt)?;
        let baseline_id =
            crate::ids::BaselineId::parse(&baseline).ok_or(Rejection::StateCorrupt)?;
        return crate::record_store::migration_exact_set(
            ctx,
            crate::records::PhysicalSchema::BaselineHeads,
            &crate::records::baseline_heads_key(&baseline_id),
            &baseline,
            &[
                (crate::records::roots::PROJECT, first.body.project.as_str()),
                (crate::records::roots::KIND, "baseline"),
            ],
            crate::records::roots::HEADS,
            &heads,
            true,
        );
    }
    if subitem == "11f:issued" {
        let state = crate::spec::Baseline {
            revisions: ordered,
            explicit_heads: Vec::new(),
            explicit_issued: Vec::new(),
        };
        let issued = baseline_issued_ids(&state);
        if issued.len() > crate::records::MAX_CONCURRENT_HEADS {
            return Err(Rejection::Conflict);
        }
        let issued = issued.into_iter().collect::<BTreeSet<_>>();
        let first = state.revisions.first().ok_or(Rejection::StateCorrupt)?;
        let baseline_id =
            crate::ids::BaselineId::parse(&baseline).ok_or(Rejection::StateCorrupt)?;
        return crate::record_store::migration_exact_set(
            ctx,
            crate::records::PhysicalSchema::BaselineHeads,
            &crate::records::baseline_heads_key(&baseline_id),
            &baseline,
            &[
                (crate::records::roots::PROJECT, first.body.project.as_str()),
                (crate::records::roots::KIND, "baseline"),
            ],
            crate::records::roots::ISSUED_HEADS,
            &issued,
            false,
        );
    }
    Err(Rejection::ContractViolation)
}

/// Audit the current representation without treating immutable history as work
/// still to do. The derived catalog is the visible truth; relation Bodies are
/// checked against it to find facts still supplied only by the compatibility
/// overlay.
fn structure_report(
    ctx: &Context<'_>,
    catalog: &CatalogState,
    read: &IssueReadSet,
) -> Result<contract::StructureReport, Rejection> {
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
            let belongs = read
                .issues
                .get(&edge.0)
                .is_some_and(|issue| &issue.project == project);
            if belongs && state.as_ref().and_then(|state| state.edges.get(edge)) != Some(&true) {
                relation_edges_pending = relation_edges_pending.saturating_add(1);
                project_pending = true;
            }
        }
        for (child, parent) in &catalog.parents {
            let belongs = read
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
    let spec_heads_pending = 0u64;
    let mut spec_conflicts = 0u64;
    let plans_without_roots = 0u64;
    for spec in &specs {
        let heads = spec.heads();
        if heads.len() != 1 {
            spec_conflicts = spec_conflicts.saturating_add(1);
        }
    }
    let issue_documents_pending = count(
        read.issues
            .values()
            .filter(|issue| issue.document_schema != DOCUMENT_SCHEMA_VERSION)
            .count(),
    );
    let complete = relation_edges_pending == 0
        && relation_parents_pending == 0
        && spec_heads_pending == 0
        && issue_documents_pending == 0;

    Ok(contract::StructureReport {
        generation: data_encoding::HEXLOWER.encode(&ctx.manifest_root()),
        projects: count(catalog.projects.len()),
        issues: count(read.issues.len()),
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
        migration: crate::record_store::migration_verification(ctx)?,
        complete,
    })
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

fn canonical_spec_revision(
    ctx: &Context<'_>,
    spec: &str,
    revision: &str,
) -> Result<String, Rejection> {
    let key = crate::records::revision_alias_key(spec, revision);
    if ctx.body_version(&key).is_none() {
        return Ok(revision.into());
    }
    let bytes = ctx.read_body(&key)?.ok_or(Rejection::StateCorrupt)?;
    let envelope = crate::records::ImmutableRecordEnvelope::decode_canonical(&bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let identity = envelope.identity;
    if identity.owner != spec || identity.record != revision {
        return Err(Rejection::StateCorrupt);
    }
    let alias = crate::records::RevisionAliasRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    if alias.spec != spec || alias.legacy_revision != revision {
        return Err(Rejection::StateCorrupt);
    }
    Ok(alias.canonical_revision)
}

fn validate_spec_ref(
    ctx: &Context<'_>,
    member: &crate::spec::SpecRef,
    project: &str,
) -> Result<(), Rejection> {
    let spec = spec_state(ctx, &member.spec).ok_or(Rejection::InvalidRequest)?;
    let revision_id = canonical_spec_revision(ctx, &member.spec, &member.revision)?;
    let revision = spec
        .revision(&revision_id)
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
                let revision = canonical_spec_revision(ctx, spec, revision)?;
                if target.revision(&revision).is_none() {
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
                if issue_core_state(ctx, issue).is_none() {
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
        let target = issue_core_state(ctx, issue).ok_or(Rejection::InvalidRequest)?;
        if target.project != project {
            return Err(Rejection::InvalidRequest);
        }
    }
    Ok(())
}

/// How much history a Packet will read to find what governs one Issue.
///
/// Every link a revision asserts is a `spec_reference` relation keyed by its
/// target, so a seek on the Issue finds every revision that has *ever*
/// governed it -- a set that grows with revisions of governing Specs, not
/// with the Space. Paged and capped rather than read whole: past the cap the
/// Packet is refused, never quietly short.
const PACKET_REFERENCE_PAGE: u32 = 128;
const PACKET_REFERENCE_PAGES: u32 = 32;

/// The Spec a revision id belongs to, from the revision's own corpus row.
fn revision_owner(ctx: &Context<'_>, revision: &str) -> Result<Option<String>, Rejection> {
    let answer = find_field_page(
        ctx,
        crate::find::field::ID,
        runtime::find::Atom::Text(revision.into()),
        &contract::PageRequest {
            limit: 1,
            cursor: None,
        },
        vec![runtime::find::Predicate {
            field: crate::find::field_ref(crate::find::field::RELATION_KIND),
            test: runtime::find::Test::Equal,
            value: runtime::find::Atom::Text("spec_revision".into()),
        }],
        vec![crate::find::field_ref(crate::find::field::SOURCE_ID)],
    )?;
    Ok(answer
        .rows()
        .first()
        .and_then(|row| result_text(row, crate::find::field::SOURCE_ID)))
}

/// The Specs that may govern `doc` directly: every Spec one of whose
/// revisions asserts `governs` on it.
///
/// This is what `all_specs` stood in for. Which of them govern *now* is the
/// issued revision's question, and `spec_state` answers it per candidate --
/// a set bounded by what has ever named this Issue rather than by the Space.
fn governing_candidates(ctx: &Context<'_>, doc: &str) -> Result<BTreeSet<String>, Rejection> {
    let mut revisions = BTreeSet::new();
    let mut request = contract::PageRequest {
        limit: PACKET_REFERENCE_PAGE,
        cursor: None,
    };
    let mut pages = 0u32;
    loop {
        // The composite posting, not the bare target: `TARGET_ID` is shared by
        // every comment, reaction and child that names the Issue, and Find
        // charges every posting it scans before a Keep can drop it. A busy
        // Issue would have its Packet refused for being busy.
        let answer = find_field_page(
            ctx,
            crate::find::field::RELATION_TARGET_KIND,
            runtime::find::Atom::Bytes(crate::find::composite_key([
                crate::spec::Rel::Governs.as_str(),
                doc,
            ])),
            &request,
            vec![runtime::find::Predicate {
                field: crate::find::field_ref(crate::find::field::ENTITY_KEY),
                test: runtime::find::Test::Equal,
                value: runtime::find::Atom::Text("spec_reference".into()),
            }],
            vec![crate::find::field_ref(crate::find::field::SOURCE_ID)],
        )?;
        revisions.extend(
            answer
                .rows()
                .iter()
                .filter_map(|row| result_text(row, crate::find::field::SOURCE_ID)),
        );
        let Some(cursor) = page_from_answer(&answer, Vec::<()>::new()).next_cursor else {
            break;
        };
        pages += 1;
        if pages >= PACKET_REFERENCE_PAGES {
            return Err(Rejection::LimitExceeded);
        }
        request.cursor = Some(cursor);
    }
    let mut specs = BTreeSet::new();
    for revision in revisions {
        if let Some(spec) = revision_owner(ctx, &revision)? {
            specs.insert(spec);
        }
    }
    Ok(specs)
}

fn packet(ctx: &Context<'_>, doc: &str) -> Result<crate::spec::Packet, Rejection> {
    let mut issue = issue_core_state(ctx, doc).ok_or(Rejection::InvalidRequest)?;
    // A Packet is built around the issue's baseline binding, which is one of
    // the relation-held facts the core state clears. `issue_state` would put
    // it back, but by scanning every record that names this doc; this needs
    // one bounded read of one singleton relation.
    enrich_issue_relations(ctx, &mut issue, doc)?;
    let mut exact: BTreeMap<
        (String, String),
        (crate::spec::Revision, crate::spec::PacketSource, bool),
    > = BTreeMap::new();
    let mut conflicts = Vec::new();
    let governs = |revision: &crate::spec::Revision| {
        revision.body.links.iter().any(|link| {
            link.rel == crate::spec::Rel::Governs
                && matches!(&link.target, crate::spec::Target::Issue { issue } if issue == doc)
        })
    };

    if let Some(binding) = &issue.baseline {
        if baseline_heads(ctx, &binding.baseline).is_none() {
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
        }
        // The exact pinned revision, whether or not the Baseline has moved on
        // since: the pin is the agreement, and a successor draft or issuance
        // does not unmake it.
        let Some(revision) = baseline_revision_at(ctx, &binding.baseline, &binding.revision) else {
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
            let Some(kind) = spec_heads(ctx, &member.spec).as_ref().and_then(spec_kind) else {
                conflicts.push(crate::spec::PacketConflict::MissingSpec {
                    spec: member.spec.clone(),
                });
                continue;
            };
            let canonical = canonical_spec_revision(ctx, &member.spec, &member.revision)?;
            let Some(revision) = spec_revision_at(ctx, &member.spec, kind, &canonical) else {
                conflicts.push(crate::spec::PacketConflict::MissingSpecRevision {
                    spec: member.spec.clone(),
                    revision: member.revision.clone(),
                });
                continue;
            };
            exact.insert(
                (member.spec.clone(), canonical),
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
    for spec in governing_candidates(ctx, doc)? {
        let Some(state) = spec_state(ctx, &spec) else {
            continue;
        };
        match state.issued() {
            crate::spec::Issued::One(revision) => {
                if governs(revision) {
                    exact.insert(
                        (spec.clone(), revision.revision.clone()),
                        (revision.clone(), crate::spec::PacketSource::Direct, false),
                    );
                }
            }
            crate::spec::Issued::Conflict(revisions) => {
                if revisions.iter().any(|revision| governs(revision)) {
                    conflicts.push(crate::spec::PacketConflict::IssuedSpecConflict { spec });
                }
            }
            crate::spec::Issued::None => {}
        }
    }

    // Incorporation, unlike reference, pulls the exact target into the
    // governing set. Traverse to a fixed point over exact revisions.
    let mut missing = BTreeSet::new();
    loop {
        let mut added = false;
        let snapshot = exact
            .values()
            .map(|(revision, _, _)| {
                (
                    revision.body.spec.clone(),
                    revision.revision.clone(),
                    revision.body.links.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (from_spec, from_revision, links) in snapshot {
            for link in links {
                if link.rel != crate::spec::Rel::Incorporates {
                    continue;
                }
                let crate::spec::Target::Spec {
                    spec,
                    revision: target_revision,
                } = link.target
                else {
                    continue;
                };
                let canonical = canonical_spec_revision(ctx, &spec, &target_revision)?;
                if exact.contains_key(&(spec.clone(), canonical.clone())) {
                    continue;
                }
                let target = spec_heads(ctx, &spec)
                    .as_ref()
                    .and_then(spec_kind)
                    .and_then(|kind| spec_revision_at(ctx, &spec, kind, &canonical));
                let Some(target) = target else {
                    if missing.insert((spec.clone(), target_revision.clone())) {
                        conflicts.push(crate::spec::PacketConflict::MissingIncorporated {
                            spec,
                            revision: target_revision,
                        });
                    }
                    continue;
                };
                exact.insert(
                    (spec, canonical),
                    (
                        target,
                        crate::spec::PacketSource::Incorporated {
                            spec: from_spec.clone(),
                            revision: from_revision.clone(),
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
        let kind = revision.body.kind;
        let item = crate::spec::PacketSpec {
            spec: revision.body.spec,
            revision: revision.revision,
            kind,
            title: revision.body.title,
            state: revision.body.state,
            source,
            links: revision.body.links,
        };
        if incorporated || kind.governs() {
            governing.push(item);
        } else {
            match kind {
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
    id: Option<&str>,
    parent: Option<&str>,
) -> Result<(IssueState, Option<String>), Rejection> {
    if body.is_empty() || body.len() > 1024 * 1024 {
        return Err(Rejection::InvalidRequest);
    }
    let issue = issue_core_state(ctx, doc).ok_or(Rejection::InvalidRequest)?;
    if let Some(id) = id {
        if !contract::is_comment_id(id) || find_comment(ctx, id)?.is_some() {
            return Err(Rejection::InvalidRequest);
        }
    }
    let Some(parent) = parent else {
        return Ok((issue, None));
    };
    // A reply needs an addressable target: an existing comment that carries
    // an id (pre-identity comments cannot anchor threads) and is itself a
    // root — one level, no ladders.
    let target = find_comment(ctx, parent)?
        .filter(|target| target.issue == doc)
        .ok_or(Rejection::InvalidRequest)?;
    // A root is a comment that answers nothing by either account. Both are
    // checked, not just the hierarchy, because they can legitimately disagree:
    // a reply to a comment that predates the hierarchy has no parent edge to
    // hang from and threads through its `parent` field alone. Trusting the
    // edge there would read that reply as a root and let a reply hang off it —
    // the ladder the one-level rule exists to refuse, rebuilt through the one
    // case the cutover creates.
    if id.is_none() || target.parent.is_some() {
        return Err(Rejection::InvalidRequest);
    }
    Ok((issue, None))
}

struct FoundComment {
    issue: String,
    parent: Option<String>,
}

/// Resolve one comment through the publication corpus. This is used on the
/// write path as well as reads: uniqueness and one-level reply validation are
/// exact indexed seeks, never a decode of the entire thread.
fn find_comment(ctx: &Context<'_>, id: &str) -> Result<Option<FoundComment>, Rejection> {
    use runtime::find as find_api;
    let bound = find_api::Bound {
        decoded_bodies: 2,
        postings_read: 8,
        edges_visited: 1,
        nodes_visited: 8,
        paths_retained: 1,
        candidates_per_branch: 2,
        score_evaluations: 1,
        projected_bytes: 16 * 1024,
        packed_tokens: 256,
        wall_millis: 250,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    let fields = [
        crate::find::field::KIND,
        crate::find::field::SOURCE_ID,
        crate::find::field::TARGET_ID,
    ]
    .into_iter()
    .map(crate::find::field_ref)
    .collect();
    let answer = ctx
        .find(find_api::Query {
            schema: crate::find::entity_schema_ref(),
            publication: ctx.world_publication_id().map(|id| id.publication),
            mode: find_api::Mode::Exact,
            steps: vec![
                find_api::Step {
                    id: seek,
                    input: Vec::new(),
                    op: find_api::Op::Seek(find_api::Seek::Field(find_api::Predicate {
                        field: crate::find::field_ref(crate::find::field::ID),
                        test: find_api::Test::Equal,
                        value: find_api::Atom::Text(id.into()),
                    })),
                    bound,
                },
                find_api::Step {
                    id: keep,
                    input: vec![seek],
                    op: find_api::Op::Keep(find_api::Keep {
                        predicates: vec![find_api::Predicate {
                            field: crate::find::field_ref(crate::find::field::KIND),
                            test: find_api::Test::Equal,
                            value: find_api::Atom::Text("comment".into()),
                        }],
                    }),
                    bound,
                },
                find_api::Step {
                    id: pack,
                    input: vec![keep],
                    op: find_api::Op::Pack(find_api::Pack { fields }),
                    bound,
                },
            ],
            output: pack,
            bound,
            page_size: 2,
            cursor: None,
        })
        .map_err(find_rejection)?;
    if answer.rows().len() > 1 {
        return Err(Rejection::StateCorrupt);
    }
    let Some(row) = answer.rows().first() else {
        return Ok(None);
    };
    let text = |name: &str| {
        row.fields.iter().find_map(|field| {
            (field.reference == crate::find::field_ref(name))
                .then_some(&field.value)
                .and_then(|value| match value {
                    find_api::Value::Text(value) => Some(value.to_string()),
                    _ => None,
                })
        })
    };
    if text(crate::find::field::KIND).as_deref() != Some("comment") {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(FoundComment {
        issue: text(crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?,
        parent: text(crate::find::field::TARGET_ID),
    }))
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
    _parent_node: Option<String>,
    record: StoredComment,
    device: &str,
    ts: u64,
) -> Result<String, Rejection> {
    let mut ev = event("commented", device, ts);
    ev.x = record.b.clone();
    let (batch, id) = crate::record_store::write_comment(ctx, doc, record)?;
    staging.absorb_records(batch);
    push_event(staging, ctx, doc, &ev)?;
    Ok(id)
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
            .map_err(Rejection::BodyRead)?
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
) -> Result<Option<crate::dto::CommentAnchorDto>, Rejection> {
    use crate::dto::CommentAnchorState;
    let Some(at) = comment.at.as_ref() else {
        return Ok(None);
    };
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
        None => return Ok(dto(CommentAnchorState::Unresolved)),
        // The mint side's rule, applied to the material as it stands rather
        // than as it stood: a span of an empty text names nothing.
        Some("") => return Ok(dto(CommentAnchorState::Drifted)),
        Some(_) => {}
    }
    let key = issue_key(doc);
    let one = |hex: &str| -> Result<Option<fabric::AnchorResolution>, Rejection> {
        let Some(raw) = data_encoding::HEXLOWER.decode(hex.as_bytes()).ok() else {
            return Ok(None);
        };
        let Some(anchor) = fabric::Anchor::decode_canonical(&raw).ok() else {
            return Ok(None);
        };
        // The record names a field and so does the anchor inside it. This
        // build writes them together and they always agree; a record from
        // anywhere else that disagrees cannot say which one its writer meant,
        // and resolving the anchor while reporting the record's field would
        // hand back the right offset of the wrong value.
        if anchor.path != at.field {
            return Ok(None);
        }
        Ok(Some(
            ctx.resolve_anchor(&key, &anchor)
                .map_err(Rejection::BodyRead)?,
        ))
    };
    let Some(head) = one(&at.start)? else {
        return Ok(dto(CommentAnchorState::Unresolved));
    };
    let tail = match &at.end {
        None => Some(head),
        Some(hex) => one(hex)?,
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
    Ok(dto(state))
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
fn push_event(
    staging: &mut Staging,
    ctx: &Context<'_>,
    doc: &str,
    event: &IssueEvent,
) -> Result<(), Rejection> {
    // Both attribution coordinates come from the authenticated outer action.
    // Adapter JSON may carry legacy actor/device fields during this cutover,
    // but neither can become durable authorship by construction.
    let event = &IssueEvent {
        a: ctx.principal().actor.as_str().to_string(),
        d: ctx.principal().device.as_str().to_string(),
        ..event.clone()
    };
    let mut recipients = if event.inbox_kind().is_some() {
        issue_notification_audience(ctx, doc)?
    } else {
        std::collections::BTreeSet::new()
    };
    // An assignment's target relation is staged in this same transaction and
    // therefore absent from the pinned pre-action Corpus. Include the explicit
    // successor actors so the assignment notification is causal and does not
    // require a second publication/read loop.
    if event.k == "assigned" {
        recipients.extend(
            event
                .c
                .iter()
                .filter(|change| change.f == "assignees")
                .filter_map(|change| change.to.clone()),
        );
    }
    if recipients.len() > contract::MAX_ISSUE_AUDIENCE
        || recipients
            .iter()
            .any(|actor| ActorId::parse(actor).is_none())
    {
        return Err(Rejection::InvalidRequest);
    }
    staging.absorb_records(crate::record_store::write_activity(
        ctx,
        doc,
        event,
        &recipients.into_iter().collect::<Vec<_>>(),
    )?);
    Ok(())
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

fn issue_transition_successor(
    ctx: &Context<'_>,
    doc: &str,
    placement: crate::records::BoardPlacement,
    evidence: &str,
    timestamp: u64,
) -> Result<crate::record_store::Batch, Rejection> {
    let heads = crate::record_store::issue_transition_heads(ctx, doc)?;
    let predecessors = match heads.as_slice() {
        [(head, _)] => vec![head.clone()],
        [] => return Err(Rejection::StateCorrupt),
        _ => return Err(Rejection::Conflict),
    };
    crate::record_store::write_issue_transition(
        ctx,
        doc,
        &predecessors,
        &placement,
        evidence,
        timestamp,
    )
    .map(|(batch, _)| batch)
}

fn workflow_rejection(error: super::views::Failure) -> Rejection {
    match error {
        super::views::Failure::Missing => Rejection::StateCorrupt,
        super::views::Failure::Conflicted => Rejection::Conflict,
    }
}

/// Resolve completion semantics from the issue's project workflow.  Unknown
/// stored states are corrupt rather than silently reclassified as backlog.
fn issue_status_category(
    catalog: &CatalogState,
    project: &str,
    status: &str,
) -> Result<StatusCategory, Rejection> {
    catalog
        .status_category(project, status)
        .map_err(workflow_rejection)?
        .ok_or(Rejection::StateCorrupt)
}

/// Whether every capability id is registered for the declared scope kind
/// (sorted, unique, non-empty).
fn validate_role_caps(caps: &[String], scope: crate::roles::ScopeKind) -> Result<(), Rejection> {
    if caps.is_empty() || caps.len() > crate::roles::MAX_CAPABILITIES {
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

/// Stage one immutable role revision. The shared corpus is the revision log;
/// there is no aggregate governance map on the user action path.
fn stage_role_revision(
    staging: &mut Staging,
    ctx: &Context<'_>,
    revision: &crate::roles::RoleRevision,
) -> Result<(), Rejection> {
    let stored = crate::views::StoredRoleRevision {
        revision_id: data_encoding::HEXLOWER.encode(&revision.revision_id),
        predecessor_ids: revision
            .predecessor_ids
            .iter()
            .map(|p| data_encoding::HEXLOWER.encode(p))
            .collect(),
        body: revision.body.clone(),
    };
    staging.absorb_records(crate::record_store::write_governance_revision(
        ctx, &stored,
    )?);
    Ok(())
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
fn is_ancestor_live(
    ctx: &Context<'_>,
    project: &str,
    start: &str,
    needle: &str,
) -> Result<bool, Rejection> {
    const MAX_PARENT_WALK: usize = 4_096;
    let mut seen = std::collections::BTreeSet::new();
    let mut cursor = start.to_string();
    for _ in 0..MAX_PARENT_WALK {
        let Some(record) = crate::record_store::read_parent(ctx, project, &cursor)? else {
            return Ok(false);
        };
        let Some(parent) = record.parent else {
            return Ok(false);
        };
        if !seen.insert(parent.clone()) {
            return Err(Rejection::StateCorrupt);
        }
        if parent == needle {
            return Ok(true);
        }
        cursor = parent;
    }
    Err(Rejection::LimitExceeded)
}

fn change_identity(ctx: &Context<'_>, operation: u16, kind: &str) -> Result<[u8; 16], Rejection> {
    let request = ctx.request_id().ok_or(Rejection::ContractViolation)?;
    let mut material = Vec::with_capacity(18 + kind.len());
    material.extend_from_slice(&request.as_bytes());
    material.extend_from_slice(&operation.to_be_bytes());
    material.extend_from_slice(kind.as_bytes());
    let digest = blake3::derive_key("lait.issues.change-identity.v1", &material);
    digest[..16]
        .try_into()
        .map_err(|_| Rejection::ContractViolation)
}

fn stage_project_create(
    staging: &mut Staging,
    ctx: &Context<'_>,
    catalog: &CatalogState,
    id: &str,
    name: &str,
    key: &str,
    color: &str,
) -> Result<ProjectMeta, Rejection> {
    let key = key.trim().to_ascii_uppercase();
    if crate::ids::ProjectId::parse(id).is_none()
        || !contract::valid_name(name)
        || color.len() > contract::MAX_PRESENTATION_TOKEN_BYTES
        || key.is_empty()
        || key.len() > 8
        || !key.bytes().all(|byte| byte.is_ascii_alphabetic())
        || catalog.projects.values().any(|project| project.key == key)
        || project_key_exists(ctx, &key)?
    {
        return Err(Rejection::InvalidRequest);
    }
    let meta = ProjectMeta {
        name: name.trim().into(),
        key,
        color: color.into(),
        ..ProjectMeta::default()
    };
    staging.absorb_records(crate::record_store::write_project(
        ctx,
        catalog,
        id,
        &meta,
        false,
        Some(&meta.description),
    )?);
    let workflow = crate::workflow::default_workflow_revision(id);
    staging.absorb_records(crate::record_store::write_workflow_revision(
        ctx, id, &workflow,
    )?);
    Ok(meta)
}

fn stage_label_create(
    staging: &mut Staging,
    ctx: &Context<'_>,
    id: &str,
    name: String,
    color: String,
) -> Result<Vec<u8>, Rejection> {
    let mut catalog = CatalogState::default();
    load_labels_for_write(ctx, &mut catalog, Vec::new(), [name.clone()])?;
    if crate::ids::LabelId::parse(id).is_none()
        || !contract::valid_name(&name)
        || color.len() > contract::MAX_PRESENTATION_TOKEN_BYTES
    {
        return Err(Rejection::InvalidRequest);
    }
    if catalog
        .labels
        .values()
        .any(|label| label.name.eq_ignore_ascii_case(&name))
    {
        return Err(Rejection::Conflict);
    }
    staging.absorb_records(crate::record_store::write_label(
        ctx,
        &catalog,
        id,
        &LabelMeta { name, color },
        false,
    )?);
    Ok(contract::demand_space_any("catalog.label.configure"))
}

fn stage_label_edit(
    staging: &mut Staging,
    ctx: &Context<'_>,
    label: &str,
    name: Option<String>,
    color: Option<String>,
) -> Result<(String, Vec<u8>, bool), Rejection> {
    if name.is_none() && color.is_none() {
        return Err(Rejection::InvalidRequest);
    }
    let id = resolve_entity(ctx, contract::ResolveEntity::Label, label, None)?.id;
    let mut catalog = CatalogState::default();
    crate::record_store::apply_label(ctx, &mut catalog, &id)?;
    if let Some(name) = &name {
        load_labels_for_write(ctx, &mut catalog, Vec::new(), [name.clone()])?;
    }
    let current = catalog
        .labels
        .get(&id)
        .cloned()
        .ok_or(Rejection::InvalidRequest)?;
    let mut meta = current.clone();
    if let Some(name) = name {
        let name = name.trim().to_string();
        if !contract::valid_name(&name) {
            return Err(Rejection::InvalidRequest);
        }
        if catalog
            .labels
            .iter()
            .any(|(other, label)| other != &id && label.name.eq_ignore_ascii_case(&name))
        {
            return Err(Rejection::Conflict);
        }
        meta.name = name;
    }
    if let Some(color) = color {
        if color.len() > contract::MAX_PRESENTATION_TOKEN_BYTES {
            return Err(Rejection::InvalidRequest);
        }
        meta.color = color;
    }
    if meta == current {
        return Ok((
            id,
            contract::demand_space_any("catalog.label.configure"),
            false,
        ));
    }
    staging.absorb_records(crate::record_store::write_label(
        ctx, &catalog, &id, &meta, false,
    )?);
    Ok((
        id,
        contract::demand_space_any("catalog.label.configure"),
        true,
    ))
}

fn stage_label_delete(
    staging: &mut Staging,
    ctx: &Context<'_>,
    label: &str,
) -> Result<(String, Vec<u8>), Rejection> {
    let id = resolve_entity(ctx, contract::ResolveEntity::Label, label, None)?.id;
    let mut catalog = CatalogState::default();
    crate::record_store::apply_label(ctx, &mut catalog, &id)?;
    let meta = catalog
        .labels
        .get(&id)
        .cloned()
        .ok_or(Rejection::InvalidRequest)?;
    staging.absorb_records(crate::record_store::write_label(
        ctx, &catalog, &id, &meta, true,
    )?);
    Ok((id, contract::demand_space_any("catalog.label.configure")))
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn stage_issue_create(
    staging: &mut Staging,
    ctx: &Context<'_>,
    ordinals: &mut BTreeMap<String, u64>,
    doc: &str,
    project: &str,
    title: String,
    priority: String,
    requested_status: Option<String>,
    parent: Option<String>,
    assignees: Vec<String>,
    labels: Vec<String>,
    created_labels: &BTreeSet<String>,
    body: Option<String>,
    due: Option<u64>,
    estimate: Option<u32>,
    ts: u64,
) -> Result<Vec<u8>, Rejection> {
    let parent = parent
        .map(|parent| {
            resolve_entity(ctx, contract::ResolveEntity::Issue, &parent, None).map(|row| row.id)
        })
        .transpose()?;
    if let Some(parent) = &parent {
        let state = issue_core_state(ctx, parent).ok_or(Rejection::InvalidRequest)?;
        if state.project != project {
            return Err(Rejection::InvalidRequest);
        }
    }
    let mut catalog = CatalogState::default();
    load_workflow_for_write(ctx, &mut catalog, project)?;
    load_labels_for_write(
        ctx,
        &mut catalog,
        labels
            .iter()
            .filter(|label| !created_labels.contains(*label))
            .cloned(),
        Vec::<String>::new(),
    )?;
    if crate::ids::DocId::parse(doc).is_none()
        || !contract::valid_title(&title)
        || body
            .as_deref()
            .is_some_and(|body| !contract::valid_text(body))
        || !catalog.projects.contains_key(project)
        || due == Some(0)
        || estimate.is_some_and(|value| value > contract::MAX_ESTIMATE)
        || assignees.len() > contract::MAX_ISSUE_ASSIGNEES
        || assignees
            .iter()
            .any(|actor| ActorId::parse(actor).is_none())
        || labels.len() > contract::MAX_ISSUE_LABELS
    {
        return Err(Rejection::InvalidRequest);
    }
    let priority = Priority::parse(&priority).ok_or(Rejection::InvalidRequest)?;
    for label in &labels {
        if !catalog.labels.contains_key(label) && !created_labels.contains(label) {
            return Err(Rejection::InvalidRequest);
        }
    }
    if let Some(parent) = &parent {
        staging.absorb_records(crate::record_store::write_parent(
            ctx,
            project,
            doc,
            Some(parent.clone()),
        )?);
    }
    let status = match requested_status {
        Some(status) => {
            if catalog
                .workflow_state(project, &status)
                .map_err(workflow_rejection)?
                .is_none()
            {
                return Err(Rejection::InvalidRequest);
            }
            status
        }
        None => catalog
            .first_state_in(project, StatusCategory::Backlog)
            .map_err(workflow_rejection)?
            .ok_or(Rejection::Conflict)?
            .state_id
            .clone(),
    };
    let key = issue_key(doc);
    if ctx.body_version(&key).is_some() {
        return Err(Rejection::Conflict);
    }
    staging.issue(&key, Op::Create);
    staging.issue(
        &key,
        reg(crate::records::roots::ISSUE_ID, doc.as_bytes().to_vec()),
    );
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
    if let Some(body) = body.filter(|body| !body.is_empty()) {
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
    for actor in &assignees {
        staging.absorb_records(crate::record_store::write_issue_relation(
            ctx, doc, project, "assignee", actor, true,
        )?);
    }
    for label in &labels {
        staging.absorb_records(crate::record_store::write_issue_relation(
            ctx, doc, project, "label", label, true,
        )?);
    }
    let ordinal = next_project_ordinal(ctx, project, ordinals)?;
    let placement_plan =
        crate::record_store::board_placement(ctx, project, &status, doc, Some(&Pos::Top))?;
    let placement = placement_plan.placement.ok_or(Rejection::StateCorrupt)?;
    staging.absorb_records(placement_plan.maintenance);
    let mut batch = crate::record_store::Batch::default();
    crate::record_store::write_issue_identity(ctx, &mut batch, doc, project, ordinal)?;
    staging.absorb_records(batch);
    let meta = IssueState {
        project: project.into(),
        title,
        status,
        priority,
        created_by: Some(ctx.principal().actor.clone()),
        created_at: ts,
        duedate: due,
        estimate,
        ..IssueState::default()
    };
    staging.absorb_records(crate::record_store::write_issue_meta(
        ctx, doc, &meta, false,
    )?);
    let (transition, _) =
        crate::record_store::write_issue_transition(ctx, doc, &[], &placement, "", ts)?;
    staging.absorb_records(transition);
    push_event(staging, ctx, doc, &event("created", "", ts))?;
    let create = contract::demand_project_work("issue.create", project);
    if parent.is_some() {
        require_both(
            create,
            contract::demand_project_work("issue.parent", project),
        )
    } else {
        Ok(create)
    }
}

#[allow(clippy::too_many_arguments)]
fn stage_spec_create(
    staging: &mut Staging,
    ctx: &Context<'_>,
    catalog: &CatalogState,
    publication: runtime::publication::PublicationId,
    spec: &str,
    project: &str,
    kind: crate::spec::Kind,
    title: String,
    text: String,
    links: Vec<crate::spec::Link>,
    ts: u64,
) -> Result<(), Rejection> {
    if crate::ids::SpecId::parse(spec).is_none()
        || !catalog.projects.contains_key(project)
        || spec_state(ctx, spec).is_some()
    {
        return Err(Rejection::InvalidRequest);
    }
    validate_spec_links(ctx, &links)?;
    let plan =
        (kind == crate::spec::Kind::Plan).then(|| crate::spec::PlanData { roots: Vec::new() });
    let revision = crate::spec::build_revision(
        crate::spec::Body {
            spec: spec.into(),
            project: project.into(),
            kind,
            publication,
            title,
            text,
            state: crate::spec::State::Draft,
            links,
            plan,
            author: ctx.principal().actor.to_string(),
            ts,
        },
        vec![],
    )
    .map_err(|_| Rejection::InvalidRequest)?;
    staging.absorb_records(crate::record_store::write_spec_revision(ctx, &revision)?);
    Ok(())
}

fn change_position(
    ctx: &Context<'_>,
    position: contract::ChangePosition,
) -> Result<Pos, Rejection> {
    Ok(match position {
        contract::ChangePosition::Top => Pos::Top,
        contract::ChangePosition::Bottom => Pos::Bottom,
        contract::ChangePosition::Before { issue } => Pos::Before {
            doc: resolve_entity(ctx, contract::ResolveEntity::Issue, &issue, None)?.id,
        },
        contract::ChangePosition::After { issue } => Pos::After {
            doc: resolve_entity(ctx, contract::ResolveEntity::Issue, &issue, None)?.id,
        },
    })
}

/// Stage one atomic Board change over the exact publication pinned by this
/// ChangeSet. State and rank become one predecessor-bound transition, so a
/// cross-column drag cannot expose the adapter's former half-committed state.
fn stage_issue_board_change(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    status: Option<String>,
    position: Option<contract::ChangePosition>,
    ts: u64,
) -> Result<(String, String, Vec<u8>), Rejection> {
    if status.is_none() && position.is_none() {
        return Err(Rejection::InvalidRequest);
    }
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let (catalog, held) = issue_write_state(ctx, &doc, status.is_some())?;
    let target_status = status.unwrap_or_else(|| held.status.clone());
    if catalog
        .workflow_state(&held.project, &target_status)
        .map_err(workflow_rejection)?
        .is_none()
    {
        return Err(Rejection::InvalidRequest);
    }
    let changed_status = target_status != held.status;
    let position = position
        .map(|position| change_position(ctx, position))
        .transpose()?;
    let placement = crate::record_store::board_placement(
        ctx,
        &held.project,
        &target_status,
        &doc,
        position.as_ref(),
    )?;
    let mut evidence = String::new();
    let mut demand = contract::demand_project_work("issue.write", &held.project);
    if changed_status {
        let (transition_demand, transition_evidence) =
            transition_gate(&catalog, &held.project, &held.status, &target_status)?;
        demand = require_both(demand, transition_demand)?;
        evidence = serde_json::to_string(&transition_evidence)
            .map_err(|_| Rejection::ContractViolation)?;
    }
    let mut batch = placement.maintenance;
    batch.absorb(issue_transition_successor(
        ctx,
        &doc,
        placement.placement.ok_or(Rejection::StateCorrupt)?,
        &evidence,
        ts,
    )?);
    staging.absorb_records(batch);
    let mut change = event("board_changed", "", ts);
    if changed_status {
        change.c.push(EventChange {
            f: "status".into(),
            from: Some(held.status),
            to: Some(target_status),
        });
        change.x = evidence;
    }
    push_event(staging, ctx, &doc, &change)?;
    Ok((doc, held.project, demand))
}

fn stage_issue_work(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    action: WorkAction,
    ts: u64,
) -> Result<(String, Vec<u8>, bool), Rejection> {
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let (catalog, held) = issue_write_state(ctx, &doc, true)?;
    let actor = ctx.principal().actor.to_string();
    let (category, kind) = match action {
        WorkAction::Start => (StatusCategory::Active, "started"),
        WorkAction::Done => (StatusCategory::Done, "finished"),
        WorkAction::Stop => (StatusCategory::Backlog, "stopped"),
    };
    let target = catalog
        .first_state_in(&held.project, category)
        .map_err(workflow_rejection)?
        .ok_or(Rejection::Conflict)?
        .clone();
    let mut demand = contract::demand_project_work("issue.write", &held.project);
    let mut changes = Vec::new();
    let mut transition_evidence = None;
    if held.status != target.state_id {
        let (transition_demand, evidence) =
            transition_gate(&catalog, &held.project, &held.status, &target.state_id)?;
        demand = require_both(demand, transition_demand)?;
        changes.push(EventChange {
            f: "status".into(),
            from: Some(held.status.clone()),
            to: Some(target.state_id.clone()),
        });
        let placement =
            crate::record_store::board_placement(ctx, &held.project, &target.state_id, &doc, None)?;
        let evidence_json =
            serde_json::to_string(&evidence).map_err(|_| Rejection::ContractViolation)?;
        let mut batch = placement.maintenance;
        batch.absorb(issue_transition_successor(
            ctx,
            &doc,
            placement.placement.ok_or(Rejection::StateCorrupt)?,
            &evidence_json,
            ts,
        )?);
        staging.absorb_records(batch);
        transition_evidence = Some(evidence_json);
    }
    let assigned = crate::record_store::read_issue_relation(ctx, &doc, "assignee", &actor)?
        .is_some_and(|relation| relation.present);
    match action {
        WorkAction::Start if !assigned => {
            changes.push(EventChange {
                f: "assignees".into(),
                from: None,
                to: Some("@me".into()),
            });
            staging.absorb_records(crate::record_store::write_issue_relation(
                ctx,
                &doc,
                &held.project,
                "assignee",
                &actor,
                true,
            )?);
        }
        WorkAction::Stop if assigned => {
            changes.push(EventChange {
                f: "assignees".into(),
                from: Some("@me".into()),
                to: None,
            });
            staging.absorb_records(crate::record_store::write_issue_relation(
                ctx,
                &doc,
                &held.project,
                "assignee",
                &actor,
                false,
            )?);
        }
        _ => {}
    }
    if changes.is_empty() {
        return Ok((doc, demand, false));
    }
    let mut event = event(kind, "", ts);
    event.c = changes;
    event.x = transition_evidence.unwrap_or_default();
    push_event(staging, ctx, &doc, &event)?;
    Ok((doc, demand, true))
}

#[allow(clippy::too_many_arguments)]
fn stage_issue_patch(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    title: Option<String>,
    status: Option<String>,
    priority: Option<String>,
    due: Option<u64>,
    clear_due: bool,
    estimate: Option<u32>,
    clear_estimate: bool,
    assignees: Option<Vec<String>>,
    labels: Option<Vec<String>>,
    ts: u64,
) -> Result<(String, Vec<u8>), Rejection> {
    if title.is_none()
        && status.is_none()
        && priority.is_none()
        && due.is_none()
        && !clear_due
        && estimate.is_none()
        && !clear_estimate
        && assignees.is_none()
        && labels.is_none()
    {
        return Err(Rejection::InvalidRequest);
    }
    if due.is_some() && clear_due || estimate.is_some() && clear_estimate {
        return Err(Rejection::InvalidRequest);
    }
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let (catalog, mut held) = issue_write_state(ctx, &doc, status.is_some())?;
    let project = held.project.clone();
    let mut demand = contract::demand_project_work("issue.write", &project);
    if let Some(status) = status {
        let (_, _, transition_demand) =
            stage_issue_board_change(staging, ctx, &doc, Some(status), None, ts)?;
        demand = require_both(demand, transition_demand)?;
    }
    let mut changes = Vec::new();
    let mut meta_changed = false;
    if let Some(title) = title {
        if !contract::valid_title(&title) {
            return Err(Rejection::InvalidRequest);
        }
        changes.push(EventChange {
            f: "title".into(),
            from: Some(held.title),
            to: Some(title.clone()),
        });
        held.title = title;
        meta_changed = true;
    }
    if let Some(priority) = priority {
        let priority = Priority::parse(&priority).ok_or(Rejection::InvalidRequest)?;
        changes.push(EventChange {
            f: "priority".into(),
            from: Some(held.priority.as_str().to_owned()),
            to: Some(priority.as_str().to_owned()),
        });
        held.priority = priority;
        meta_changed = true;
    }
    if due == Some(0) || estimate.is_some_and(|value| value > contract::MAX_ESTIMATE) {
        return Err(Rejection::InvalidRequest);
    }
    if due.is_some() || clear_due {
        let next = if clear_due { None } else { due };
        changes.push(EventChange {
            f: "duedate".into(),
            from: held.duedate.map(|value| value.to_string()),
            to: next.map(|value| value.to_string()),
        });
        held.duedate = next;
        meta_changed = true;
    }
    if estimate.is_some() || clear_estimate {
        let next = if clear_estimate { None } else { estimate };
        changes.push(EventChange {
            f: "estimate".into(),
            from: held.estimate.map(|value| value.to_string()),
            to: next.map(|value| value.to_string()),
        });
        held.estimate = next;
        meta_changed = true;
    }
    if meta_changed {
        staging.absorb_records(crate::record_store::write_issue_meta(
            ctx,
            &doc,
            &held,
            catalog.tombstones.contains(&doc),
        )?);
    }
    if let Some(assignees) = assignees {
        if assignees.len() > contract::MAX_ISSUE_ASSIGNEES
            || assignees
                .iter()
                .any(|actor| ActorId::parse(actor).is_none())
        {
            return Err(Rejection::LimitExceeded);
        }
        let next = assignees.into_iter().collect::<BTreeSet<_>>();
        let current = issue_relation_targets(ctx, &doc, "assignee", contract::MAX_ISSUE_ASSIGNEES)?;
        for actor in current.union(&next) {
            let on = next.contains(actor);
            if current.contains(actor) != on {
                staging.absorb_records(crate::record_store::write_issue_relation(
                    ctx, &doc, &project, "assignee", actor, on,
                )?);
            }
        }
        changes.push(EventChange {
            f: "assignees".into(),
            from: None,
            to: None,
        });
    }
    if let Some(labels) = labels {
        if labels.len() > contract::MAX_ISSUE_LABELS {
            return Err(Rejection::LimitExceeded);
        }
        let mut next = BTreeSet::new();
        for label in labels {
            if crate::ids::LabelId::parse(&label).is_none() {
                return Err(Rejection::InvalidRequest);
            }
            next.insert(label);
        }
        let current = issue_relation_targets(ctx, &doc, "label", contract::MAX_ISSUE_LABELS)?;
        for label in current.union(&next) {
            let on = next.contains(label);
            if current.contains(label) != on {
                staging.absorb_records(crate::record_store::write_issue_relation(
                    ctx, &doc, &project, "label", label, on,
                )?);
            }
        }
        changes.push(EventChange {
            f: "labels".into(),
            from: None,
            to: None,
        });
    }
    if !changes.is_empty() {
        let mut edit = event("edited", "", ts);
        edit.c = changes;
        push_event(staging, ctx, &doc, &edit)?;
    }
    Ok((doc, demand))
}

fn stage_issue_tombstone(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    on: bool,
    ts: u64,
) -> Result<(String, Vec<u8>), Rejection> {
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let held = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
    staging.absorb_records(crate::record_store::write_issue_meta(ctx, &doc, &held, on)?);
    push_event(
        staging,
        ctx,
        &doc,
        &event(if on { "deleted" } else { "restored" }, "", ts),
    )?;
    Ok((
        doc,
        contract::demand_project_work(
            if on { "issue.delete" } else { "issue.restore" },
            &held.project,
        ),
    ))
}

/// Stage one comment, minting its id when the caller did not name one.
///
/// A client is allowed to choose the id -- that is how a retry lands on the
/// same comment rather than a second one -- but it is not required to, and
/// `write_comment` derives one from the request when it is absent, which is
/// equally stable under replay. Refusing an unnamed comment made the
/// optional field in the protocol a lie: every value it could take other
/// than `Some` was rejected.
fn stage_issue_comment(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    id: Option<String>,
    body: String,
    parent: Option<String>,
    ts: u64,
) -> Result<(String, Vec<u8>), Rejection> {
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let (held, parent_node) = check_comment(ctx, &doc, &body, id.as_deref(), parent.as_deref())?;
    let id = stage_comment(
        staging,
        ctx,
        &doc,
        parent_node,
        StoredComment {
            a: ctx.principal().actor.to_string(),
            t: ts,
            b: body,
            id,
            parent,
            at: None,
            node: None,
            parent_node: None,
        },
        "",
        ts,
    )?;
    Ok((
        id,
        contract::demand_project_work("comment.create", &held.project),
    ))
}

#[allow(clippy::too_many_arguments)]
fn stage_issue_comment_at(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    id: String,
    body: String,
    field: String,
    start: u64,
    end: Option<u64>,
    parent: Option<String>,
    source: runtime::publication::WorldPublicationId,
    ts: u64,
) -> Result<(String, Vec<u8>), Rejection> {
    if ctx.world_publication_id().as_ref() != Some(&source) {
        return Err(Rejection::Conflict);
    }
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let (held, parent_node) = check_comment(ctx, &doc, &body, Some(&id), parent.as_deref())?;
    let at = mint_comment_anchor(ctx, &doc, &held, &field, start, end)?;
    stage_comment(
        staging,
        ctx,
        &doc,
        parent_node,
        StoredComment {
            a: ctx.principal().actor.to_string(),
            t: ts,
            b: body,
            id: Some(id.clone()),
            parent,
            at: Some(at),
            node: None,
            parent_node: None,
        },
        "",
        ts,
    )?;
    Ok((
        id,
        contract::demand_project_work("comment.create", &held.project),
    ))
}

fn stage_issue_reaction(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    comment: String,
    emoji: String,
    on: bool,
) -> Result<(String, Vec<u8>), Rejection> {
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let held = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
    if !contract::is_comment_id(&comment)
        || !contract::is_reaction_emoji(&emoji)
        || find_comment(ctx, &comment)?.is_none_or(|found| found.issue != doc)
    {
        return Err(Rejection::InvalidRequest);
    }
    staging.absorb_records(crate::record_store::write_reaction(
        ctx,
        &doc,
        &comment,
        &emoji,
        ctx.principal().actor.as_str(),
        on,
    )?);
    Ok((
        doc,
        contract::demand_project_work("comment.create", &held.project),
    ))
}

fn stage_issue_link(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    kind: String,
    target: &str,
    on: bool,
    ts: u64,
) -> Result<(String, Vec<u8>), Rejection> {
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let target = resolve_entity(ctx, contract::ResolveEntity::Issue, target, None)?.id;
    let kind = kind.to_ascii_lowercase();
    if !LINK_KINDS.contains(&kind.as_str()) || doc == target {
        return Err(Rejection::InvalidRequest);
    }
    let held = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
    let other = issue_core_state(ctx, &target).ok_or(Rejection::InvalidRequest)?;
    let (from, to) = if kind == "relates" && target < doc {
        (target.clone(), doc.clone())
    } else {
        (doc.clone(), target.clone())
    };
    let relation_project = if from == doc {
        &held.project
    } else {
        &other.project
    };
    if !on
        && !crate::record_store::read_link(ctx, relation_project, &from, &kind, &to)?
            .is_some_and(|record| record.present)
    {
        return Err(Rejection::InvalidRequest);
    }
    staging.absorb_records(crate::record_store::write_link(
        ctx,
        relation_project,
        &from,
        &kind,
        &to,
        on,
    )?);
    let mut change = event(if on { "linked" } else { "unlinked" }, "", ts);
    change.x = format!("{kind} {target}");
    push_event(staging, ctx, &doc, &change)?;
    Ok((
        doc,
        contract::demand_project_work("issue.link", &held.project),
    ))
}

fn stage_issue_parent(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    parent: Option<String>,
    ts: u64,
) -> Result<(String, Vec<u8>), Rejection> {
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let held = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
    let parent = parent
        .map(|parent| {
            resolve_entity(ctx, contract::ResolveEntity::Issue, &parent, None).map(|row| row.id)
        })
        .transpose()?;
    if let Some(parent) = &parent {
        if parent == &doc {
            return Err(Rejection::Conflict);
        }
        let parent_issue = issue_core_state(ctx, parent).ok_or(Rejection::InvalidRequest)?;
        if parent_issue.project != held.project {
            return Err(Rejection::InvalidRequest);
        }
        if is_ancestor_live(ctx, &held.project, parent, &doc)? {
            return Err(Rejection::Conflict);
        }
    }
    staging.absorb_records(crate::record_store::write_parent(
        ctx,
        &held.project,
        &doc,
        parent.clone(),
    )?);
    let mut change = event("parented", "", ts);
    change.x = parent.unwrap_or_else(|| "unparented".into());
    push_event(staging, ctx, &doc, &change)?;
    Ok((
        doc,
        contract::demand_project_work("issue.parent", &held.project),
    ))
}

fn stage_issue_move(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    project: Option<String>,
    position: Option<contract::ChangePosition>,
    ts: u64,
) -> Result<(String, Vec<u8>, bool), Rejection> {
    if project.is_none() && position.is_none() {
        return Err(Rejection::InvalidRequest);
    }
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let (mut catalog, mut held) = issue_write_state(ctx, &doc, false)?;
    let mut effective = held.project.clone();
    let mut target_status = held.status.clone();
    let mut demand = contract::demand_project_work("issue.move_out", &held.project);
    let project_changed = project
        .as_ref()
        .is_some_and(|project| project != &held.project);
    if let Some(target) = &project {
        load_workflow_for_write(ctx, &mut catalog, target)?;
        if !catalog.projects.contains_key(target) {
            return Err(Rejection::InvalidRequest);
        }
        if project_changed {
            effective.clone_from(target);
            if catalog
                .workflow_state(target, &target_status)
                .map_err(workflow_rejection)?
                .is_none()
            {
                target_status = catalog
                    .first_state_in(target, StatusCategory::Backlog)
                    .map_err(workflow_rejection)?
                    .ok_or(Rejection::Conflict)?
                    .state_id
                    .clone();
            }
            demand = require_both(
                demand,
                contract::demand_project_work("issue.move_in", target),
            )?;
        }
    }
    let position = position
        .map(|position| change_position(ctx, position))
        .transpose()?
        .or_else(|| project_changed.then_some(Pos::Top));
    let placement = crate::record_store::board_placement(
        ctx,
        &effective,
        &target_status,
        &doc,
        position.as_ref(),
    )?;
    if project_changed {
        held.project.clone_from(&effective);
        held.status.clone_from(&target_status);
        staging.absorb_records(crate::record_store::write_issue_meta(
            ctx,
            &doc,
            &held,
            catalog.tombstones.contains(&doc),
        )?);
    }
    let mut batch = placement.maintenance;
    batch.absorb(issue_transition_successor(
        ctx,
        &doc,
        placement.placement.ok_or(Rejection::StateCorrupt)?,
        "",
        ts,
    )?);
    staging.absorb_records(batch);
    push_event(staging, ctx, &doc, &event("moved", "", ts))?;
    Ok((doc, demand, true))
}

fn stage_issue_milestone(
    staging: &mut Staging,
    ctx: &Context<'_>,
    issue: &str,
    milestone: Option<String>,
    ts: u64,
) -> Result<(String, Vec<u8>, bool), Rejection> {
    let doc = resolve_entity(ctx, contract::ResolveEntity::Issue, issue, None)?.id;
    let held = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
    let milestone = milestone
        .map(|milestone| {
            resolve_entity(
                ctx,
                contract::ResolveEntity::Milestone,
                &milestone,
                Some(&held.project),
            )
            .map(|row| row.id)
        })
        .transpose()?;
    if held.milestone == milestone {
        return Ok((
            doc,
            contract::demand_project_work("issue.bind", &held.project),
            false,
        ));
    }
    let mut catalog = CatalogState::default();
    let label = match &milestone {
        Some(milestone) => {
            crate::record_store::apply_schedule_record(
                ctx,
                &mut catalog,
                &held.project,
                milestone,
            )?;
            let record = catalog
                .milestones
                .get(&held.project)
                .and_then(|records| records.get(milestone))
                .filter(|record| !record.tombstone)
                .ok_or(Rejection::InvalidRequest)?;
            if let Some(previous) = held.milestone.as_deref() {
                staging.absorb_records(crate::record_store::write_issue_relation(
                    ctx,
                    &doc,
                    &held.project,
                    "milestone",
                    previous,
                    false,
                )?);
            }
            staging.absorb_records(crate::record_store::write_issue_relation(
                ctx,
                &doc,
                &held.project,
                "milestone",
                milestone,
                true,
            )?);
            record.name.clone()
        }
        None => {
            staging.absorb_records(crate::record_store::write_issue_relation(
                ctx,
                &doc,
                &held.project,
                "milestone",
                held.milestone.as_deref().ok_or(Rejection::StateCorrupt)?,
                false,
            )?);
            "none".into()
        }
    };
    let mut change = event("milestoned", "", ts);
    change.x = label;
    push_event(staging, ctx, &doc, &change)?;
    Ok((
        doc,
        contract::demand_project_work("issue.bind", &held.project),
        true,
    ))
}

impl World for IssuesWorld {
    fn descriptor(&self) -> runtime::world::Descriptor {
        runtime::world::Descriptor {
            id: self.id.clone(),
            implementation_version: runtime::world::Version(match self.package {
                IssuesPackage::Preferred => 6,
                IssuesPackage::Migrator => Self::MIGRATOR_IMPLEMENTATION_VERSION,
            }),
            schemas: self.schemas.clone(),
            limits: runtime::world::Limits {
                max_payload_bytes: contract::MAX_PAYLOAD_BYTES,
            },
            scope_schemas: Vec::new(),
            signal_schemas: self.signal_schemas.clone(),
            find_schemas: self.find_schemas.clone(),
            find_extractors: self.find_extractors.clone(),
            exec_specs: self.exec_specs.clone(),
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

    fn find_schemas(&self) -> &[runtime::find::Schema] {
        &self.find_schemas
    }

    fn find_extractors(&self) -> &[runtime::find::Extractor] {
        &self.find_extractors
    }

    fn extract(
        &self,
        ctx: &runtime::world::ExtractionContext<'_>,
        extractor: &runtime::find::Extractor,
        body: &replica::body::BodyKey,
    ) -> Result<runtime::find::BodyExtraction, Rejection> {
        crate::find::extract(ctx, extractor, body)
    }

    fn exec_specs(&self) -> &[runtime::exec::Spec] {
        &self.exec_specs
    }

    fn submit(&self, ctx: &mut Context<'_>, intent: Intent) -> Result<Effect, Rejection> {
        let intent = IssueIntent::from_json(&intent.payload).ok_or(Rejection::InvalidRequest)?;
        if self.package == IssuesPackage::Preferred
            && matches!(&intent, IssueIntent::V4Migrate { .. })
        {
            return Err(Rejection::InvalidRequest);
        }
        let mut staging = Staging::for_space(
            ctx.principal().space.clone(),
            ctx.body_version(&catalog_key(&ctx.principal().space))
                .is_none(),
        );
        match intent {
            IssueIntent::ChangeSet { operations, ts } => {
                // A ChangeSet validates against an in-action overlay plus
                // exact v4 records. It never assembles the tracker catalog.
                let mut catalog_storage = CatalogState::default();
                let catalog = &mut catalog_storage;
                if operations.is_empty()
                    || operations.len() > contract::CHANGE_SET_MAX_OPERATIONS
                    || serde_json::to_vec(&operations)
                        .map_err(|_| Rejection::InvalidRequest)?
                        .len()
                        > contract::CHANGE_SET_MAX_BYTES
                    || ts == 0
                {
                    return Err(Rejection::LimitExceeded);
                }
                let publication = self.portable_publication(ctx)?;
                let mut created_projects = BTreeMap::<u16, String>::new();
                let mut created_labels = BTreeMap::<u16, String>::new();
                // Numbers handed out by THIS action, per project. The pinned
                // publication cannot see what the action is staging, so the
                // count is taken once and carried forward from there.
                let mut ordinal_run = BTreeMap::<String, u64>::new();
                let mut demand: Option<Vec<u8>> = None;
                for (index, operation) in operations.into_iter().enumerate() {
                    let ordinal = u16::try_from(index).map_err(|_| Rejection::LimitExceeded)?;
                    match operation {
                        contract::ChangeOperation::ProjectCreate { name, key, color } => {
                            let id = crate::ids::ProjectId::from_digest(change_identity(
                                ctx, ordinal, "project",
                            )?)
                            .as_str()
                            .to_owned();
                            let meta = stage_project_create(
                                &mut staging,
                                ctx,
                                &catalog,
                                &id,
                                &name,
                                &key,
                                &color,
                            )?;
                            catalog.projects.insert(id.clone(), meta);
                            created_projects.insert(ordinal, id.clone());
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "project".into(),
                                id,
                            });
                            let next = contract::demand_space_any("project.create");
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::SpecCreate {
                            project,
                            kind,
                            title,
                            text,
                            links,
                        } => {
                            let project = match project {
                                contract::ChangeProject::Existing { project } => {
                                    let project = resolve_entity(
                                        ctx,
                                        contract::ResolveEntity::Project,
                                        &project,
                                        None,
                                    )?
                                    .id;
                                    crate::record_store::apply_project(ctx, catalog, &project)?;
                                    project
                                }
                                contract::ChangeProject::Created { operation } => {
                                    if operation >= ordinal {
                                        return Err(Rejection::InvalidRequest);
                                    }
                                    created_projects
                                        .get(&operation)
                                        .cloned()
                                        .ok_or(Rejection::InvalidRequest)?
                                }
                            };
                            let spec = crate::ids::SpecId::from_digest(change_identity(
                                ctx, ordinal, "spec",
                            )?)
                            .as_str()
                            .to_owned();
                            stage_spec_create(
                                &mut staging,
                                ctx,
                                &catalog,
                                publication,
                                &spec,
                                &project,
                                kind,
                                title,
                                text,
                                links,
                                ts,
                            )?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "spec".into(),
                                id: spec,
                            });
                            let next = contract::demand_project_work("spec.write", &project);
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueCreate {
                            project,
                            title,
                            priority,
                            status,
                            parent,
                            assignees,
                            labels,
                            body,
                            due,
                            estimate,
                        } => {
                            let project = match project {
                                contract::ChangeProject::Existing { project } => {
                                    resolve_entity(
                                        ctx,
                                        contract::ResolveEntity::Project,
                                        &project,
                                        None,
                                    )?
                                    .id
                                }
                                contract::ChangeProject::Created { operation } => {
                                    if operation >= ordinal {
                                        return Err(Rejection::InvalidRequest);
                                    }
                                    created_projects
                                        .get(&operation)
                                        .cloned()
                                        .ok_or(Rejection::InvalidRequest)?
                                }
                            };
                            let doc = crate::ids::DocId::from_digest(change_identity(
                                ctx, ordinal, "issue",
                            )?)
                            .as_str()
                            .to_owned();
                            let labels = labels
                                .into_iter()
                                .map(|label| match label {
                                    contract::ChangeLabel::Existing { label } => resolve_entity(
                                        ctx,
                                        contract::ResolveEntity::Label,
                                        &label,
                                        None,
                                    )
                                    .map(|row| row.id),
                                    contract::ChangeLabel::Created { operation } => {
                                        if operation >= ordinal {
                                            return Err(Rejection::InvalidRequest);
                                        }
                                        created_labels
                                            .get(&operation)
                                            .cloned()
                                            .ok_or(Rejection::InvalidRequest)
                                    }
                                })
                                .collect::<Result<Vec<_>, _>>()?;
                            let next = stage_issue_create(
                                &mut staging,
                                ctx,
                                &mut ordinal_run,
                                &doc,
                                &project,
                                title,
                                priority,
                                status,
                                parent,
                                assignees,
                                labels,
                                &created_labels.values().cloned().collect(),
                                body,
                                due,
                                estimate,
                                ts,
                            )?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueBoard {
                            issue,
                            status,
                            position,
                        } => {
                            let (doc, _project, next) = stage_issue_board_change(
                                &mut staging,
                                ctx,
                                &issue,
                                status,
                                position,
                                ts,
                            )?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssuePatch {
                            issue,
                            title,
                            status,
                            priority,
                            due,
                            clear_due,
                            estimate,
                            clear_estimate,
                            assignees,
                            labels,
                        } => {
                            let labels = labels
                                .map(|labels| {
                                    labels
                                        .into_iter()
                                        .map(|label| match label {
                                            contract::ChangeLabel::Existing { label } => {
                                                resolve_entity(
                                                    ctx,
                                                    contract::ResolveEntity::Label,
                                                    &label,
                                                    None,
                                                )
                                                .map(|row| row.id)
                                            }
                                            contract::ChangeLabel::Created { operation } => {
                                                if operation >= ordinal {
                                                    return Err(Rejection::InvalidRequest);
                                                }
                                                created_labels
                                                    .get(&operation)
                                                    .cloned()
                                                    .ok_or(Rejection::InvalidRequest)
                                            }
                                        })
                                        .collect::<Result<Vec<_>, _>>()
                                })
                                .transpose()?;
                            let (doc, next) = stage_issue_patch(
                                &mut staging,
                                ctx,
                                &issue,
                                title,
                                status,
                                priority,
                                due,
                                clear_due,
                                estimate,
                                clear_estimate,
                                assignees,
                                labels,
                                ts,
                            )?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueWork { issue, action } => {
                            let (doc, next, _changed) =
                                stage_issue_work(&mut staging, ctx, &issue, action, ts)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueTombstone { issue, on } => {
                            let (doc, next) =
                                stage_issue_tombstone(&mut staging, ctx, &issue, on, ts)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueComment {
                            issue,
                            body,
                            parent,
                        } => {
                            let id = crate::ids::CommentId::from_digest(change_identity(
                                ctx, ordinal, "comment",
                            )?)
                            .as_str()
                            .to_ascii_lowercase();
                            let (id, next) = stage_issue_comment(
                                &mut staging,
                                ctx,
                                &issue,
                                Some(id),
                                body,
                                parent,
                                ts,
                            )?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "comment".into(),
                                id,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueCommentAt {
                            issue,
                            body,
                            field,
                            start,
                            end,
                            parent,
                            source,
                        } => {
                            let id = crate::ids::CommentId::from_digest(change_identity(
                                ctx, ordinal, "comment",
                            )?)
                            .as_str()
                            .to_ascii_lowercase();
                            let (id, next) = stage_issue_comment_at(
                                &mut staging,
                                ctx,
                                &issue,
                                id,
                                body,
                                field,
                                start,
                                end,
                                parent,
                                source,
                                ts,
                            )?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "comment".into(),
                                id,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueReaction {
                            issue,
                            comment,
                            emoji,
                            on,
                        } => {
                            let (doc, next) = stage_issue_reaction(
                                &mut staging,
                                ctx,
                                &issue,
                                comment,
                                emoji,
                                on,
                            )?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueLink {
                            issue,
                            kind,
                            target,
                            on,
                        } => {
                            let (doc, next) =
                                stage_issue_link(&mut staging, ctx, &issue, kind, &target, on, ts)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueParent { issue, parent } => {
                            let (doc, next) =
                                stage_issue_parent(&mut staging, ctx, &issue, parent, ts)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueMove {
                            issue,
                            project,
                            position,
                        } => {
                            let project = project
                                .map(|project| match project {
                                    contract::ChangeProject::Existing { project } => {
                                        resolve_entity(
                                            ctx,
                                            contract::ResolveEntity::Project,
                                            &project,
                                            None,
                                        )
                                        .map(|row| row.id)
                                    }
                                    contract::ChangeProject::Created { operation } => {
                                        if operation >= ordinal {
                                            return Err(Rejection::InvalidRequest);
                                        }
                                        created_projects
                                            .get(&operation)
                                            .cloned()
                                            .ok_or(Rejection::InvalidRequest)
                                    }
                                })
                                .transpose()?;
                            let (doc, next, _changed) =
                                stage_issue_move(&mut staging, ctx, &issue, project, position, ts)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::IssueMilestone { issue, milestone } => {
                            let (doc, next, _changed) =
                                stage_issue_milestone(&mut staging, ctx, &issue, milestone, ts)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "issue".into(),
                                id: doc,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::LabelCreate { name, color } => {
                            let id = crate::ids::LabelId::from_digest(change_identity(
                                ctx, ordinal, "label",
                            )?)
                            .as_str()
                            .to_owned();
                            let next = stage_label_create(&mut staging, ctx, &id, name, color)?;
                            created_labels.insert(ordinal, id.clone());
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "label".into(),
                                id,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::LabelEdit { label, name, color } => {
                            let (id, next, _changed) =
                                stage_label_edit(&mut staging, ctx, &label, name, color)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "label".into(),
                                id,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                        contract::ChangeOperation::LabelDelete { label } => {
                            let (id, next) = stage_label_delete(&mut staging, ctx, &label)?;
                            staging.results.push(contract::ChangeResult {
                                operation: ordinal,
                                kind: "label".into(),
                                id,
                            });
                            demand = Some(match demand {
                                Some(current) => require_both(current, next)?,
                                None => next,
                            });
                        }
                    }
                }
                staging.require(demand.ok_or(Rejection::InvalidRequest)?);
                Ok(staging.into_effect(None))
            }
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
                // arrives in the intent (the lifecycle adapter persisted the
                // signed bytes); the World calls no clock and mints no id.
                let project_key = project_key.trim().to_ascii_uppercase();
                if !contract::valid_name(&name)
                    || !contract::valid_name(&project_name)
                    || project_key.is_empty()
                    || project_key.len() > 8
                    || !project_key.bytes().all(|b| b.is_ascii_alphabetic())
                    || project_id.is_empty()
                    || ts == 0
                {
                    return Err(Rejection::InvalidRequest);
                }
                // The golden commitments must match this implementation's
                // reviewed release definitions exactly.
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
                let initial_project = ProjectMeta {
                    name: project_name.trim().into(),
                    key: project_key.clone(),
                    color: "blue".into(),
                    ..ProjectMeta::default()
                };
                let mut initial_catalog = CatalogState {
                    name: name.clone(),
                    ..CatalogState::default()
                };
                initial_catalog
                    .projects
                    .insert(project_id.clone(), initial_project.clone());
                // The preferred implementation has no aggregate Catalog
                // schema. Initialization therefore commits only the v4
                // entity-sized records below; the historical Catalog is an
                // input to the separately installed migrator, never a shadow
                // truth recreated by a fresh tracker.
                let directory = crate::records::space_directory_key(&ctx.principal().space);
                if ctx.body_version(&directory).is_some() {
                    return Err(Rejection::Conflict);
                }
                let mut batch = crate::record_store::write_space(
                    ctx,
                    &initial_catalog,
                    &initial_catalog.name,
                    Some(&initial_catalog.description),
                )?;
                batch.absorb(crate::record_store::write_project(
                    ctx,
                    &initial_catalog,
                    &project_id,
                    &initial_project,
                    false,
                    Some(&initial_project.description),
                )?);
                batch.absorb(crate::record_store::write_workflow_revision(
                    ctx,
                    &project_id,
                    &workflow_revision,
                )?);
                for id in crate::roles::BUILT_IN_ROLE_IDS {
                    let revision = crate::roles::built_in(id).expect("built-in role");
                    let stored = crate::views::StoredRoleRevision {
                        revision_id: data_encoding::HEXLOWER.encode(&revision.revision_id),
                        predecessor_ids: Vec::new(),
                        body: revision.body,
                    };
                    batch.absorb(crate::record_store::write_governance_revision(
                        ctx, &stored,
                    )?);
                }
                staging.absorb_records(batch);
                // Tracker initialization is a founder-composition admin action.
                staging.require(contract::demand_admin());
                Ok(staging.into_effect(None))
            }
            IssueIntent::V4Migrate { plan } => {
                let Some(source) = ctx.lifecycle_source() else {
                    return Err(Rejection::Denied(
                        runtime::world::DeniedCause::DemandUnsatisfied,
                    ));
                };
                if !plan.valid()
                    || plan.source != source.publication.publication
                    || plan.source_frontier != source.frontier
                {
                    return Err(Rejection::ContractViolation);
                }
                crate::record_store::validate_migration_plan(ctx, &plan)?;
                // The signed plan is only a compact coordinate. Recompute the
                // canonical next window from the same frozen publication and
                // reject any substituted Body/subitem/digest before staging.
                let expected = prepare_v4_migration_plan(
                    ctx,
                    plan.previous_batch,
                    plan.previous_cursor.clone(),
                    plan.timestamp,
                )?;
                if expected != plan {
                    return Err(Rejection::ContractViolation);
                }
                let item = if plan.window.terminal() {
                    crate::record_store::Batch::default()
                } else {
                    let body = plan
                        .window
                        .body
                        .as_ref()
                        .ok_or(Rejection::ContractViolation)?;
                    let view = ctx
                        .read_lifecycle_source_collaborative(body)
                        .map_err(Rejection::BodyRead)?
                        .ok_or(Rejection::StateCorrupt)?;
                    match plan.window.phase.as_str() {
                        "catalog" => crate::record_store::migration_catalog_window(
                            ctx,
                            &plan.window.subitem,
                            view.as_ref(),
                        )?,
                        "issue" => crate::record_store::migration_issue_window(
                            ctx,
                            body,
                            &plan.window.subitem,
                            view.as_ref(),
                        )?,
                        "coordinates" => crate::record_store::migration_coordinate_window(
                            ctx,
                            &plan.window.subitem,
                            view.as_ref(),
                        )?,
                        "spec" => migration_spec_window(
                            ctx,
                            body,
                            &plan.window.subitem,
                            view.as_ref(),
                            plan.source,
                        )?,
                        "baseline" => migration_baseline_window(
                            ctx,
                            body,
                            &plan.window.subitem,
                            view.as_ref(),
                        )?,
                        "terminal" => return Err(Rejection::ContractViolation),
                        _ => return Err(Rejection::ContractViolation),
                    }
                };
                staging.absorb_records(crate::record_store::finalize_migration_window(
                    ctx, &plan, item,
                )?);
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
                actor: _,
                device,
                ts,
            } => {
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &project)?;
                load_labels_for_write(
                    ctx,
                    &mut catalog,
                    labels.iter().cloned(),
                    new_labels.iter().map(|label| label.name.clone()),
                )?;
                if !contract::valid_title(&title)
                    || body
                        .as_deref()
                        .is_some_and(|body| !contract::valid_text(body))
                    || DocId::parse(&doc).is_none()
                    || ts == 0
                {
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
                if new_labels.iter().any(|label| {
                    crate::ids::LabelId::parse(&label.id).is_none()
                        || !contract::valid_name(&label.name)
                        || label.color.len() > contract::MAX_PRESENTATION_TOKEN_BYTES
                }) {
                    return Err(Rejection::InvalidRequest);
                }
                if duedate == Some(0) || estimate.is_some_and(|e| e > contract::MAX_ESTIMATE) {
                    return Err(Rejection::InvalidRequest);
                }
                if assignees.len() > contract::MAX_ISSUE_ASSIGNEES
                    || assignees
                        .iter()
                        .any(|actor| ActorId::parse(actor).is_none())
                {
                    return Err(Rejection::InvalidRequest);
                }
                let key = issue_key(&doc);
                staging.issue(&key, Op::Create);
                staging.issue(
                    &key,
                    reg(crate::records::roots::ISSUE_ID, doc.as_bytes().to_vec()),
                );
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
                    staging.absorb_records(crate::record_store::write_issue_relation(
                        ctx, &doc, &project, "assignee", who, true,
                    )?);
                }
                let (new_labels, label_ids) = reconcile_new_labels(&catalog, &labels, &new_labels);
                for new_label in &new_labels {
                    staging.absorb_records(crate::record_store::write_label(
                        ctx,
                        &catalog,
                        &new_label.id,
                        &LabelMeta {
                            name: new_label.name.clone(),
                            color: new_label.color.clone(),
                        },
                        false,
                    )?);
                }
                for label in &label_ids {
                    staging.absorb_records(crate::record_store::write_issue_relation(
                        ctx, &doc, &project, "label", label, true,
                    )?);
                }
                // The number a person reads, counted from what the project
                // already holds. Creation still contends on nothing: this is
                // a posting count, not a register anybody has to agree on.
                let ordinal = next_project_ordinal(ctx, &project, &mut BTreeMap::new())?;
                let placement_plan = crate::record_store::board_placement(
                    ctx,
                    &project,
                    DEFAULT_STATUS,
                    &doc,
                    Some(&Pos::Top),
                )?;
                let placement = placement_plan.placement.ok_or(Rejection::StateCorrupt)?;
                staging.absorb_records(placement_plan.maintenance);
                let mut batch = crate::record_store::Batch::default();
                crate::record_store::write_issue_identity(
                    ctx, &mut batch, &doc, &project, ordinal,
                )?;
                staging.absorb_records(batch);
                let meta = IssueState {
                    project: project.clone(),
                    title: title.clone(),
                    status: DEFAULT_STATUS.into(),
                    priority: Priority::parse(&priority).ok_or(Rejection::InvalidRequest)?,
                    created_by: Some(ctx.principal().actor.clone()),
                    created_at: ts,
                    duedate,
                    estimate,
                    ..IssueState::default()
                };
                staging.absorb_records(crate::record_store::write_issue_meta(
                    ctx, &doc, &meta, false,
                )?);
                let (transition, _) = crate::record_store::write_issue_transition(
                    ctx,
                    &doc,
                    &[],
                    &placement,
                    "",
                    ts,
                )?;
                staging.absorb_records(transition);
                push_event(&mut staging, ctx, &doc, &event("created", &device, ts))?;
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
                let (catalog, mut issue) = issue_write_state(ctx, &doc, status.is_some())?;
                if title.is_none()
                    && status.is_none()
                    && priority.is_none()
                    && description.is_none()
                    && duedate.is_none()
                    && estimate.is_none()
                {
                    return Err(Rejection::InvalidRequest);
                }
                if title
                    .as_deref()
                    .is_some_and(|title| !contract::valid_title(title))
                    || description
                        .as_deref()
                        .is_some_and(|description| !contract::valid_text(description))
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
                    if catalog
                        .workflow_state(&issue.project, status)
                        .map_err(workflow_rejection)?
                        .is_none()
                    {
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
                let mut meta_changed = false;
                if let Some(title) = &title {
                    changes.push(EventChange {
                        f: "title".into(),
                        from: Some(issue.title.clone()),
                        to: Some(title.clone()),
                    });
                    issue.title.clone_from(title);
                    meta_changed = true;
                }
                let mut transition_evidence = None;
                if let Some(status) = &status {
                    let changed = *status != issue.status;
                    if changed {
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
                    let placement_plan = crate::record_store::board_placement(
                        ctx,
                        &issue.project,
                        status,
                        &doc,
                        None,
                    )?;
                    let placement = placement_plan.placement.ok_or(Rejection::StateCorrupt)?;
                    let mut batch = crate::record_store::Batch::default();
                    batch.absorb(placement_plan.maintenance);
                    if changed {
                        let evidence = transition_evidence
                            .as_ref()
                            .map(|value| {
                                serde_json::to_string(value).expect("transition evidence JSON")
                            })
                            .unwrap_or_default();
                        let transition =
                            issue_transition_successor(ctx, &doc, placement, &evidence, ts)?;
                        batch.absorb(transition);
                    }
                    staging.absorb_records(batch);
                }
                if let Some(priority) = &priority {
                    changes.push(EventChange {
                        f: "priority".into(),
                        from: Some(issue.priority.as_str().to_string()),
                        to: Some(priority.clone()),
                    });
                    issue.priority = Priority::parse(priority).ok_or(Rejection::InvalidRequest)?;
                    meta_changed = true;
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
                        issue.duedate = duedate;
                        meta_changed = true;
                    }
                }
                if let Some(estimate) = estimate {
                    if estimate != issue.estimate {
                        changes.push(EventChange {
                            f: "estimate".into(),
                            from: issue.estimate.map(|e| e.to_string()),
                            to: estimate.map(|e| e.to_string()),
                        });
                        issue.estimate = estimate;
                        meta_changed = true;
                    }
                }
                if meta_changed {
                    staging.absorb_records(crate::record_store::write_issue_meta(
                        ctx,
                        &doc,
                        &issue,
                        catalog.tombstones.contains(&doc),
                    )?);
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
                push_event(&mut staging, ctx, &doc, &ev)?;
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueTextSplice {
                doc,
                index,
                delete,
                insert,
                base_len,
            } => {
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if delete == 0 && insert.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }
                // The fence. A positional splice means nothing without
                // agreement about which document it was measured against, and
                // a caller whose coordinate space has drifted writes over
                // whatever now sits at that offset. Refused as a Conflict
                // rather than an InvalidRequest, because the caller is not
                // malformed — it is working from a version this node no longer
                // holds, and the remedy is to re-read, not to fix the request.
                //
                // Optional so that a client which has not been taught to send
                // it is refused nothing it could already do; a client that does
                // send it gets the guarantee. Once no unfenced client remains,
                // this becomes required.
                if let Some(base_len) = base_len {
                    let held = u64::try_from(issue.description.chars().count())
                        .map_err(|_| Rejection::StateCorrupt)?;
                    if held != base_len {
                        return Err(Rejection::Conflict);
                    }
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
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                // Two jobs, one mechanism. A schema-0 body being moved onto the
                // document schema, and a schema-1 document being rewritten into
                // the form an editor can address positionally — both are
                // "replace this whole source, atomically, only if it is still
                // what I read". The `expected` compare-and-swap is the whole of
                // the safety, and it is the same in both directions.
                //
                // Normalization needs this route rather than a splice because a
                // non-canonical document is precisely one whose offsets cannot
                // be trusted: fixing it with the primitive that the mismatch
                // breaks would be circular.
                if issue.document_schema > DOCUMENT_SCHEMA_VERSION || issue.description != expected
                {
                    return Err(Rejection::Conflict);
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
                )?;
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueTextCheckpoint { doc, device, ts } => {
                issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let mut ev = event("edited", &device, ts);
                ev.c.push(EventChange {
                    f: "description".into(),
                    from: None,
                    to: None,
                });
                push_event(&mut staging, ctx, &doc, &ev)?;
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::IssueMove {
                doc,
                project,
                pos,
                device,
                ts,
            } => {
                let position = pos.map(|position| match position {
                    Pos::Top => contract::ChangePosition::Top,
                    Pos::Bottom => contract::ChangePosition::Bottom,
                    Pos::Before { doc } => contract::ChangePosition::Before { issue: doc },
                    Pos::After { doc } => contract::ChangePosition::After { issue: doc },
                });
                let (_doc, demand, changed) =
                    stage_issue_move(&mut staging, ctx, &doc, project, position, ts)?;
                let _ = device;
                if !changed {
                    return Ok(unchanged_effect(Some(doc)));
                }
                staging.require(demand);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Assign {
                doc,
                who,
                add,
                device,
                ts,
            } => {
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let mut resulting =
                    issue_relation_targets(ctx, &doc, "assignee", contract::MAX_ISSUE_ASSIGNEES)?;
                for actor in &who {
                    if ActorId::parse(actor).is_none() {
                        return Err(Rejection::InvalidRequest);
                    }
                    if add {
                        resulting.insert(actor.clone());
                    } else {
                        resulting.remove(actor);
                    }
                    staging.absorb_records(crate::record_store::write_issue_relation(
                        ctx,
                        &doc,
                        &issue.project,
                        "assignee",
                        actor,
                        add,
                    )?);
                }
                if resulting.len() > contract::MAX_ISSUE_ASSIGNEES {
                    return Err(Rejection::LimitExceeded);
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
                push_event(&mut staging, ctx, &doc, &ev)?;
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
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let mut catalog = CatalogState::default();
                load_labels_for_write(
                    ctx,
                    &mut catalog,
                    add.iter().chain(&remove).cloned(),
                    new_labels.iter().map(|label| label.name.clone()),
                )?;
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
                let (new_labels, label_ids) = reconcile_new_labels(&catalog, &add, &new_labels);
                for new_label in &new_labels {
                    staging.absorb_records(crate::record_store::write_label(
                        ctx,
                        &catalog,
                        &new_label.id,
                        &LabelMeta {
                            name: new_label.name.clone(),
                            color: new_label.color.clone(),
                        },
                        false,
                    )?);
                }
                for label in &label_ids {
                    staging.absorb_records(crate::record_store::write_issue_relation(
                        ctx,
                        &doc,
                        &issue.project,
                        "label",
                        label,
                        true,
                    )?);
                }
                for label in &remove {
                    staging.absorb_records(crate::record_store::write_issue_relation(
                        ctx,
                        &doc,
                        &issue.project,
                        "label",
                        label,
                        false,
                    )?);
                }
                push_event(&mut staging, ctx, &doc, &event("labeled", &device, ts))?;
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Comment {
                doc,
                body,
                id,
                parent,
                actor: _,
                device,
                ts,
            } => {
                let (_id, demand) =
                    stage_issue_comment(&mut staging, ctx, &doc, id, body, parent, ts)?;
                let _ = device;
                staging.require(demand);
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
                source,
                actor: _,
                device,
                ts,
            } => {
                let (_id, demand) = stage_issue_comment_at(
                    &mut staging,
                    ctx,
                    &doc,
                    id,
                    body,
                    field,
                    start,
                    end,
                    parent,
                    source,
                    ts,
                )?;
                let _ = device;
                staging.require(demand);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::React {
                doc,
                comment,
                emoji,
                actor: _,
                on,
                device: _,
                ts: _,
            } => {
                let (_doc, demand) =
                    stage_issue_reaction(&mut staging, ctx, &doc, comment, emoji, on)?;
                staging.require(demand);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::SetTombstone {
                doc,
                on,
                device,
                ts,
            } => {
                let (_doc, demand) = stage_issue_tombstone(&mut staging, ctx, &doc, on, ts)?;
                let _ = device;
                staging.require(demand);
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
                let (_doc, demand) =
                    stage_issue_link(&mut staging, ctx, &doc, kind, &target, add, ts)?;
                let _ = device;
                staging.require(demand);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Parent {
                doc,
                parent,
                device,
                ts,
            } => {
                let (_doc, demand) = stage_issue_parent(&mut staging, ctx, &doc, parent, ts)?;
                let _ = device;
                staging.require(demand);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::WorkState {
                doc,
                action,
                actor: _,
                device: _,
                ts,
            } => {
                let (doc, demand, changed) = stage_issue_work(&mut staging, ctx, &doc, action, ts)?;
                if !changed {
                    // The idempotent no-op: nothing committed, nothing rung.
                    return Ok(unchanged_effect(Some(doc)));
                }
                staging.require(demand);
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Verify {
                doc,
                run,
                source,
                build,
                package_filled,
                actor: _,
                device: _,
                ts,
            } => {
                let (catalog, issue) = issue_write_state(ctx, &doc, true)?;
                if DocId::parse(&doc).is_none() || catalog.tombstones.contains(&doc) || ts == 0 {
                    return Err(Rejection::InvalidRequest);
                }
                let run_id = parse_run_id(&run).ok_or(Rejection::InvalidRequest)?;
                if ctx.run_id(0) != Some(run_id) {
                    return Err(Rejection::InvalidRequest);
                }
                let source_ref = parse_content_ref(&source).ok_or(Rejection::InvalidRequest)?;
                let source_status = ctx
                    .content_status(&source_ref)
                    .ok_or(Rejection::InvalidRequest)?;
                let build_id = parse_build_id(&build).ok_or(Rejection::InvalidRequest)?;
                let workflow = catalog
                    .workflow_head(&issue.project)
                    .ok_or(Rejection::Conflict)?;
                if !workflow
                    .body
                    .states
                    .iter()
                    .any(|state| state.category == "done")
                {
                    return Err(Rejection::InvalidRequest);
                }

                let checks = raw_checks(ctx, &doc)?;
                if checks.contains_key(&run) {
                    return Err(Rejection::Conflict);
                }
                if checks.len() >= contract::MAX_CHECKS_PER_ISSUE {
                    return Err(Rejection::LimitExceeded);
                }
                let record = contract::CheckRecord {
                    spec: contract::VERIFY_SPEC.into(),
                    v: contract::VERIFY_SPEC_VERSION,
                    build: build.clone(),
                    source: source.clone(),
                    state: "started".into(),
                    by: ctx.principal().actor.to_string(),
                    ts,
                    package_filled,
                    attempt: None,
                    report: None,
                    verdict: None,
                };
                let stored = crate::records::IssueCheckRecord {
                    issue: doc.clone(),
                    run: run.clone(),
                    check: record,
                };
                staging.absorb_records(crate::record_store::write_check(ctx, &stored)?);
                staging.declare(
                    &crate::records::issue_check_key(
                        &DocId::parse(&doc).ok_or(Rejection::InvalidRequest)?,
                        &run,
                    ),
                    vec![source_ref],
                );
                let mut ev = event("check_started", ctx.principal().device.as_str(), ts);
                ev.x = run.clone();
                push_event(&mut staging, ctx, &doc, &ev)?;
                staging.require(contract::demand_project_work(
                    "issue.verify",
                    &issue.project,
                ));
                let input = contract::VerifyInput {
                    doc: doc.clone(),
                    source: source.clone(),
                };
                staging.bind_run(
                    run.clone(),
                    runtime::exec::Cmd::Start(runtime::exec::Start {
                        spec: contract::verify_spec_ref(),
                        build: build_id,
                        input: runtime::exec::Input {
                            inline: serde_json::to_vec(&input).expect("verification input JSON"),
                            content: vec![source_ref],
                            content_bytes: source_status.plaintext_len,
                        },
                        parent: None,
                        source: None,
                        service: None,
                        resources: Vec::new(),
                        limits: contract::verify_limits(),
                        queries: Vec::new(),
                    }),
                );
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::AcceptCheck {
                doc,
                run,
                attempt,
                report,
                verdict,
                move_to_done,
                id,
                actor: _,
                device: _,
                ts,
            } => {
                let (catalog, issue) = issue_write_state(ctx, &doc, move_to_done)?;
                if DocId::parse(&doc).is_none()
                    || catalog.tombstones.contains(&doc)
                    || ts == 0
                    || !matches!(verdict.as_str(), "pass" | "fail")
                    || (move_to_done && verdict != "pass")
                    || crate::ids::AttachmentId::parse(&id).is_none()
                    || id != id.to_ascii_lowercase()
                {
                    return Err(Rejection::InvalidRequest);
                }
                let run_id = parse_run_id(&run).ok_or(Rejection::InvalidRequest)?;
                let attempt_id = parse_attempt_id(&attempt).ok_or(Rejection::InvalidRequest)?;
                let report_ref = parse_content_ref(&report).ok_or(Rejection::InvalidRequest)?;
                let report_status = ctx
                    .content_status(&report_ref)
                    .ok_or(Rejection::InvalidRequest)?;
                let facts = ctx
                    .outcome(run_id, attempt_id)
                    .map_err(Rejection::BodyRead)?
                    .ok_or(Rejection::InvalidRequest)?;
                let expected_output =
                    contract::verify_candidate(&verdict, report_ref, report_status.plaintext_len)
                        .ok_or(Rejection::InvalidRequest)?;
                let expected_digest = expected_output
                    .digest()
                    .map_err(|_| Rejection::ContractViolation)?;
                let expected_inline_bytes = u32::try_from(expected_output.inline.len())
                    .map_err(|_| Rejection::ContractViolation)?;
                if !facts.returned_exactly_once
                    || facts.run != run_id
                    || facts.attempt != attempt_id
                    || facts.spec != contract::verify_spec_ref()
                    || facts.output != contract::verify_output_ref()
                    || facts.terminal != expected_output.terminal
                    || facts.output_digest != expected_digest
                    || facts.output_inline_bytes != expected_inline_bytes
                    || facts.output_content.as_slice() != [report_ref]
                    || facts.output_content_bytes != report_status.plaintext_len
                {
                    return Err(Rejection::InvalidRequest);
                }

                let attachments = raw_attachments(ctx, &doc)?;
                if attachments.contains_key(&id) {
                    return Err(Rejection::Conflict);
                }
                if attachments.len() >= contract::MAX_ATTACHMENTS_PER_ISSUE {
                    return Err(Rejection::LimitExceeded);
                }
                let mut check = crate::record_store::read_check(ctx, &doc, &run)?
                    .ok_or(Rejection::InvalidRequest)?
                    .check;
                let actual_build = data_encoding::HEXLOWER.encode(&facts.build.as_bytes());
                if check.spec != contract::VERIFY_SPEC
                    || check.v != contract::VERIFY_SPEC_VERSION
                    || check.build != actual_build
                    || check.state != "started"
                    || check.attempt.is_some()
                    || check.report.is_some()
                    || check.verdict.is_some()
                {
                    return Err(Rejection::InvalidRequest);
                }
                check.state = "accepted".into();
                check.attempt = Some(attempt.clone());
                check.report = Some(report.clone());
                check.verdict = Some(verdict.clone());
                let source_ref = parse_content_ref(&check.source).ok_or(Rejection::StateCorrupt)?;
                let check_record = crate::records::IssueCheckRecord {
                    issue: doc.clone(),
                    run: run.clone(),
                    check,
                };
                staging.absorb_records(crate::record_store::write_check(ctx, &check_record)?);
                let attachment = crate::records::IssueAttachmentRecord {
                    issue: doc.clone(),
                    id: id.clone(),
                    name: format!("verification-{run}.json"),
                    mime: "application/json".into(),
                    size: report_status.plaintext_len,
                    by: ctx.principal().actor.to_string(),
                    timestamp: ts,
                    comment: None,
                    content: report.clone(),
                    tombstone: false,
                };
                staging.absorb_records(crate::record_store::write_attachment(ctx, &attachment)?);
                let mut acceptance_demand =
                    contract::demand_project_work("issue.verify", &issue.project);
                let mut changes = Vec::new();
                let mut transition_evidence = None;
                if move_to_done {
                    let workflow = catalog
                        .workflow_head(&issue.project)
                        .ok_or(Rejection::Conflict)?;
                    let done = workflow
                        .body
                        .states
                        .iter()
                        .find(|state| state.category == "done")
                        .ok_or(Rejection::InvalidRequest)?;
                    if issue.status != done.state_id {
                        let (demand, evidence) = transition_gate(
                            &catalog,
                            &issue.project,
                            &issue.status,
                            &done.state_id,
                        )?;
                        acceptance_demand = require_both(acceptance_demand, demand)?;
                        transition_evidence = Some(evidence);
                        changes.push(EventChange {
                            f: "status".into(),
                            from: Some(issue.status.clone()),
                            to: Some(done.state_id.clone()),
                        });
                        let placement_plan = crate::record_store::board_placement(
                            ctx,
                            &issue.project,
                            &done.state_id,
                            &doc,
                            None,
                        )?;
                        let mut batch = crate::record_store::Batch::default();
                        batch.absorb(placement_plan.maintenance);
                        let evidence = transition_evidence
                            .as_ref()
                            .map(|value| {
                                serde_json::to_string(value).expect("transition evidence JSON")
                            })
                            .unwrap_or_default();
                        batch.absorb(issue_transition_successor(
                            ctx,
                            &doc,
                            placement_plan.placement.ok_or(Rejection::StateCorrupt)?,
                            &evidence,
                            ts,
                        )?);
                        staging.absorb_records(batch);
                    }
                }
                staging.require(acceptance_demand);
                let parsed_doc = DocId::parse(&doc).ok_or(Rejection::InvalidRequest)?;
                staging.declare(
                    &crate::records::issue_check_key(&parsed_doc, &run),
                    vec![source_ref, report_ref],
                );
                staging.declare(
                    &crate::records::issue_attachment_key(&parsed_doc, &id),
                    vec![report_ref],
                );
                let mut ev = event("check_accepted", ctx.principal().device.as_str(), ts);
                ev.c = changes;
                ev.x = transition_evidence.map_or_else(
                    || verdict.clone(),
                    |evidence| serde_json::to_string(&evidence).expect("transition evidence JSON"),
                );
                push_event(&mut staging, ctx, &doc, &ev)?;
                staging.bind_run(
                    run.clone(),
                    runtime::exec::Cmd::Accept {
                        run: run_id,
                        attempt: attempt_id,
                    },
                );
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
                let catalog = CatalogState::default();
                stage_project_create(&mut staging, ctx, &catalog, &id, &name, &key, &color)?;
                staging.require(contract::demand_space_any("project.create"));
                Ok(staging.into_effect(None))
            }
            IssueIntent::LabelNew {
                id,
                name,
                color,
                device: _,
                ts: _,
            } => {
                let demand = stage_label_create(&mut staging, ctx, &id, name, color)?;
                staging.require(demand);
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
                let mut catalog_storage = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog_storage, &id)?;
                if let Some(team) = team.as_deref().filter(|team| !team.is_empty()) {
                    crate::record_store::apply_team(ctx, &mut catalog_storage, team)?;
                }
                let catalog = &mut catalog_storage;
                staging.require(contract::demand_space_any("project.configure"));
                let current = catalog.projects.get(&id).ok_or(Rejection::InvalidRequest)?;
                let mut meta = current.clone();
                let description_changed = description.is_some();
                if let Some(name) = name {
                    let name = name.trim().to_string();
                    if !contract::valid_name(&name) {
                        return Err(Rejection::InvalidRequest);
                    }
                    // No name-uniqueness guard: projects are unique on KEY, not
                    // name (which stays immutable here), so two may share a name.
                    meta.name = name;
                }
                if let Some(color) = color {
                    if color.len() > contract::MAX_PRESENTATION_TOKEN_BYTES {
                        return Err(Rejection::InvalidRequest);
                    }
                    meta.color = color;
                }
                if let Some(description) = description {
                    if !contract::valid_text(&description) {
                        return Err(Rejection::InvalidRequest);
                    }
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
                staging.absorb_records(crate::record_store::write_project(
                    ctx,
                    &catalog,
                    &id,
                    &meta,
                    false,
                    description_changed.then_some(meta.description.as_str()),
                )?);
                Ok(staging.into_effect(None))
            }
            IssueIntent::ProjectUpdatePost {
                project_id,
                id,
                author: _,
                body,
                health,
                device: _,
                ts,
            } => {
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &project_id)?;
                staging.require(contract::demand_space_any("project.configure"));
                if !catalog.projects.contains_key(&project_id) {
                    return Err(Rejection::InvalidRequest);
                }
                let body = body.trim();
                if body.is_empty() || !contract::valid_text(body) {
                    return Err(Rejection::InvalidRequest);
                }
                let update = crate::views::ProjectUpdate {
                    id: id.clone(),
                    project_id: project_id.clone(),
                    author: ctx.principal().actor.to_string(),
                    ts,
                    body: body.to_string(),
                    health,
                };
                staging.absorb_records(crate::record_store::write_project_update(ctx, &update)?);
                Ok(staging.into_effect(None))
            }
            IssueIntent::LabelEdit {
                id,
                name,
                color,
                device: _,
                ts: _,
            } => {
                let (_id, demand, changed) = stage_label_edit(&mut staging, ctx, &id, name, color)?;
                staging.require(demand);
                if !changed {
                    return Ok(staging.into_effect(None));
                }
                Ok(staging.into_effect(None))
            }
            IssueIntent::LabelDelete {
                id,
                device: _,
                ts: _,
            } => {
                let (_id, demand) = stage_label_delete(&mut staging, ctx, &id)?;
                staging.require(demand);
                Ok(staging.into_effect(None))
            }
            IssueIntent::SpaceRename {
                name,
                device: _,
                ts: _,
            } => {
                let mut catalog_storage = CatalogState::default();
                crate::record_store::apply_space(ctx, &mut catalog_storage)?;
                let catalog = &mut catalog_storage;
                staging.require(contract::demand_admin());
                let name = name.trim();
                if !contract::valid_name(name) {
                    return Err(Rejection::InvalidRequest);
                }
                if catalog.name == name {
                    return Ok(staging.into_effect(None));
                }
                staging
                    .absorb_records(crate::record_store::write_space(ctx, &catalog, name, None)?);
                Ok(staging.into_effect(None))
            }
            IssueIntent::SpaceDescribe {
                description,
                device: _,
                ts: _,
            } => {
                let mut catalog_storage = CatalogState::default();
                crate::record_store::apply_space(ctx, &mut catalog_storage)?;
                let catalog = &mut catalog_storage;
                staging.require(contract::demand_admin());
                // Empty clears; no trim so intentional leading/trailing prose is
                // preserved. LWW on the catalog `description` register.
                if catalog.description == description {
                    return Ok(staging.into_effect(None));
                }
                if !contract::valid_text(&description) {
                    return Err(Rejection::InvalidRequest);
                }
                staging.absorb_records(crate::record_store::write_space(
                    ctx,
                    &catalog,
                    &catalog.name,
                    Some(&description),
                )?);
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
                let mut catalog_storage = CatalogState::default();
                load_role_for_write(ctx, &mut catalog_storage, &role_id)?;
                if let Some(project) = &scope_project {
                    crate::record_store::apply_project(ctx, &mut catalog_storage, project)?;
                }
                let catalog = &mut catalog_storage;
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
                stage_role_revision(&mut staging, ctx, &revision)?;
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
                let mut catalog_storage = CatalogState::default();
                load_role_for_write(ctx, &mut catalog_storage, &role_id)?;
                let catalog = &mut catalog_storage;
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
                stage_role_revision(&mut staging, ctx, &revision)?;
                staging.require(contract::demand_space_any("policy.configure"));
                Ok(staging.into_effect(None))
            }
            IssueIntent::RoleDelete {
                role_id,
                expected_revision,
                device: _,
                ts: _,
            } => {
                let mut catalog_storage = CatalogState::default();
                load_role_for_write(ctx, &mut catalog_storage, &role_id)?;
                let catalog = &mut catalog_storage;
                if catalog.roles.contains_key(&role_id) {
                    return Err(Rejection::InvalidRequest);
                }
                let head = expect_single_head(&catalog, &role_id, &expected_revision)?;
                let mut body = head.body.clone();
                body.tombstone = true;
                let predecessor = decode_hex32(&expected_revision)?;
                let revision = crate::roles::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                stage_role_revision(&mut staging, ctx, &revision)?;
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
                let mut catalog_storage = CatalogState::default();
                load_role_for_write(ctx, &mut catalog_storage, &role_id)?;
                let catalog = &mut catalog_storage;
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
                stage_role_revision(&mut staging, ctx, &revision)?;
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
                let mut catalog_storage = CatalogState::default();
                load_workflow_for_write(ctx, &mut catalog_storage, &project_id)?;
                let catalog = &mut catalog_storage;
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
                staging.absorb_records(crate::record_store::write_workflow_revision(
                    ctx,
                    &project_id,
                    &revision,
                )?);
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
                actor: _,
                device: _,
                ts,
            } => {
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &project)?;
                stage_spec_create(
                    &mut staging,
                    ctx,
                    &catalog,
                    self.portable_publication(ctx)?,
                    &spec,
                    &project,
                    kind,
                    title,
                    text,
                    links,
                    ts,
                )?;
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
                actor: _,
                device: _,
                ts,
            } => {
                let catalog = CatalogState::default();
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
                body.publication = self.portable_publication(ctx)?;
                body.state = crate::spec::State::Draft;
                body.author = ctx.principal().actor.to_string();
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_spec_revision(ctx, &revision)?);
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
                actor: _,
                device: _,
                ts,
            } => {
                if !text.starts_with(contract::DOCUMENT_PREFIX) {
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
                body.publication = self.portable_publication(ctx)?;
                body.author = ctx.principal().actor.to_string();
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_spec_revision(ctx, &revision)?);
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
                actor: _,
                device: _,
                ts,
            } => {
                if state == crate::spec::State::Draft {
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
                body.publication = self.portable_publication(ctx)?;
                body.author = ctx.principal().actor.to_string();
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_spec_revision(ctx, &revision)?);
                if matches!(
                    state,
                    crate::spec::State::Issued | crate::spec::State::Withdrawn
                ) {
                    let issued = match current.issued() {
                        crate::spec::Issued::None => Vec::new(),
                        crate::spec::Issued::One(current) => vec![current.revision.clone()],
                        crate::spec::Issued::Conflict(current) => current
                            .into_iter()
                            .map(|revision| revision.revision.clone())
                            .collect(),
                    };
                    staging.absorb_records(crate::record_store::write_spec_issued_heads(
                        ctx,
                        &spec,
                        &issued,
                        (state == crate::spec::State::Issued).then_some(revision.revision.as_str()),
                    )?);
                }
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
                actor: _,
                device: _,
                ts,
            } => {
                let catalog = CatalogState::default();
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
                body.publication = self.portable_publication(ctx)?;
                body.state = crate::spec::State::Draft;
                body.author = ctx.principal().actor.to_string();
                body.ts = ts;
                let predecessors = expected
                    .iter()
                    .map(|revision| {
                        crate::spec::decode_revision(revision).ok_or(Rejection::InvalidRequest)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let revision = crate::spec::build_revision(body, predecessors)
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_spec_revision(ctx, &revision)?);
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
                actor: _,
                device: _,
                ts,
            } => {
                if crate::ids::ObservationId::parse(&observation).is_none() {
                    return Err(Rejection::InvalidRequest);
                }
                if spec_observation_state(ctx, &spec, &observation)?.is_some()
                    || unique_find_row(
                        ctx,
                        crate::find::field::ID,
                        &format!("observation-retraction:{observation}"),
                        "spec_observation_fact",
                        None,
                    )?
                    .is_some()
                {
                    return Err(Rejection::Conflict);
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
                    observer: ctx.principal().actor.to_string(),
                    ts,
                    rel,
                    target,
                    note,
                };
                entry.validate().map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_spec_observation(
                    ctx,
                    &crate::records::SpecObservationRecord::Assert {
                        project: first.body.project.clone(),
                        observation: entry,
                    },
                )?);
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
                actor: _,
                device: _,
                ts,
            } => {
                let current = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let first = current.revisions.first().ok_or(Rejection::InvalidRequest)?;
                let entry = spec_observation_state(ctx, &spec, &observation)?
                    .ok_or(Rejection::InvalidRequest)?;
                let project = first.body.project.clone();
                let own = entry.observer == ctx.principal().actor.as_str();
                staging.absorb_records(crate::record_store::write_spec_observation(
                    ctx,
                    &crate::records::SpecObservationRecord::Retract {
                        project: project.clone(),
                        observation,
                        spec: spec.clone(),
                        actor: ctx.principal().actor.to_string(),
                        timestamp: ts,
                    },
                )?);
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
                actor: _,
                device: _,
                ts,
            } => {
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &project)?;
                if crate::ids::BaselineId::parse(&baseline).is_none()
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
                        author: ctx.principal().actor.to_string(),
                        ts,
                    },
                    vec![],
                )
                .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_baseline_revision(
                    ctx, &revision,
                )?);
                staging.require(contract::demand_project_work("baseline.write", &project));
                Ok(staging.into_effect(Some(baseline)))
            }
            IssueIntent::BaselineRevise {
                baseline,
                expected,
                name,
                members,
                actor: _,
                device: _,
                ts,
            } => {
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
                body.author = ctx.principal().actor.to_string();
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_baseline_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_baseline_revision(
                    ctx, &revision,
                )?);
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
                actor: _,
                device: _,
                ts,
            } => {
                if state == crate::spec::State::Draft {
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
                body.author = ctx.principal().actor.to_string();
                body.ts = ts;
                let predecessor =
                    crate::spec::decode_revision(&expected).ok_or(Rejection::InvalidRequest)?;
                let revision = crate::spec::build_baseline_revision(body, vec![predecessor])
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_baseline_revision(
                    ctx, &revision,
                )?);
                if matches!(
                    state,
                    crate::spec::State::Issued | crate::spec::State::Withdrawn
                ) {
                    let issued = match current.issued() {
                        crate::spec::BaselineIssued::None => Vec::new(),
                        crate::spec::BaselineIssued::One(current) => vec![current.revision.clone()],
                        crate::spec::BaselineIssued::Conflict(current) => current
                            .into_iter()
                            .map(|revision| revision.revision.clone())
                            .collect(),
                    };
                    staging.absorb_records(crate::record_store::write_baseline_issued_heads(
                        ctx,
                        &baseline,
                        &issued,
                        (state == crate::spec::State::Issued).then_some(revision.revision.as_str()),
                    )?);
                }
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
                actor: _,
                device: _,
                ts,
            } => {
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
                body.author = ctx.principal().actor.to_string();
                body.ts = ts;
                let predecessors = expected
                    .iter()
                    .map(|revision| {
                        crate::spec::decode_revision(revision).ok_or(Rejection::InvalidRequest)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let revision = crate::spec::build_baseline_revision(body, predecessors)
                    .map_err(|_| Rejection::InvalidRequest)?;
                staging.absorb_records(crate::record_store::write_baseline_revision(
                    ctx, &revision,
                )?);
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
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
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
                let baseline_target = baseline
                    .as_ref()
                    .or(issue.baseline.as_ref())
                    .map(|binding| serde_json::to_string(binding).expect("Baseline binding JSON"))
                    .unwrap_or_else(|| "none".into());
                staging.absorb_records(crate::record_store::write_issue_relation(
                    ctx,
                    &doc,
                    &issue.project,
                    "baseline",
                    &baseline_target,
                    baseline.is_some(),
                )?);
                staging.require(contract::demand_project_any("issue.bind", &issue.project));
                let mut event = event("baseline", &device, ts);
                event.x = baseline
                    .map(|binding| format!("{}@{}", binding.baseline, binding.revision))
                    .unwrap_or_default();
                push_event(&mut staging, ctx, &doc, &event)?;
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::ProjectDelete {
                id,
                device: _,
                ts: _,
            } => {
                staging.require(contract::demand_project_any("project.delete", &id));
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &id)?;
                let Some(meta) = catalog.projects.get(&id).cloned() else {
                    return Err(Rejection::InvalidRequest);
                };
                // The safe v1 (CUSTOM-10): a project still referenced by ANY
                // issue — live or tombstoned — refuses. Every doc's alias keys
                // off its project; deleting under one would orphan it
                // silently. Reassign (`issue move`) or archive instead.
                //
                // This asked for `issue_placement`, a migration-only node no
                // extractor in this package emits, so the guard answered
                // "unreferenced" for every project and deleted one out from
                // under its issues.
                let referenced = find_exists_bytes(
                    ctx,
                    crate::find::field::KIND_PROJECT,
                    crate::find::composite_key(["issue", id.as_str()]),
                )?;
                if referenced {
                    return Err(Rejection::Conflict);
                }
                staging.absorb_records(crate::record_store::write_project(
                    ctx, &catalog, &id, &meta, true, None,
                )?);
                Ok(staging.into_effect(None))
            }
            IssueIntent::Follow {
                doc,
                actor: _,
                on,
                device: _,
                ts: _,
            } => {
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                if on {
                    let mut followers = issue_relation_targets(
                        ctx,
                        &doc,
                        "follower",
                        contract::MAX_ISSUE_FOLLOWERS,
                    )?;
                    followers.insert(ctx.principal().actor.as_str().to_string());
                    if followers.len() > contract::MAX_ISSUE_FOLLOWERS {
                        return Err(Rejection::LimitExceeded);
                    }
                }
                staging.absorb_records(crate::record_store::write_issue_relation(
                    ctx,
                    &doc,
                    &issue.project,
                    "follower",
                    ctx.principal().actor.as_str(),
                    on,
                )?);
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
                let mut catalog_storage = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog_storage, &project_id)?;
                crate::record_store::apply_schedule_record(
                    ctx,
                    &mut catalog_storage,
                    &project_id,
                    &id,
                )?;
                let catalog = &mut catalog_storage;
                staging.require(contract::demand_space_any("project.configure"));
                if !catalog.projects.contains_key(&project_id) || id.is_empty() {
                    return Err(Rejection::InvalidRequest);
                }

                let current = catalog
                    .milestones
                    .get(&project_id)
                    .and_then(|records| records.get(&id))
                    .cloned();
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
                            rank: milestone_position(
                                ctx,
                                &project_id,
                                &id,
                                pos.as_ref().unwrap_or(&Pos::Bottom),
                            )?,
                            tombstone: false,
                        }
                    }
                };
                if let Some(pos) = &pos {
                    record.rank = milestone_position(ctx, &project_id, &id, pos)?;
                } else if record.rank.is_empty() {
                    record.rank = milestone_position(ctx, &project_id, &id, &Pos::Bottom)?;
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
                if current.as_ref() == Some(&record) {
                    return Ok(staging.into_effect(None));
                }
                staging.absorb_records(crate::record_store::write_milestone(ctx, &record)?);
                Ok(staging.into_effect(None))
            }
            IssueIntent::IssueMilestone {
                doc,
                milestone,
                device,
                ts,
            } => {
                let (_doc, demand, changed) =
                    stage_issue_milestone(&mut staging, ctx, &doc, milestone, ts)?;
                let _ = device;
                if !changed {
                    return Ok(unchanged_effect(Some(doc)));
                }
                staging.require(demand);
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
                let mut catalog_storage = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog_storage, &project_id)?;
                crate::record_store::apply_schedule_record(
                    ctx,
                    &mut catalog_storage,
                    &project_id,
                    &id,
                )?;
                let catalog = &mut catalog_storage;
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
                staging.absorb_records(crate::record_store::write_cycle(ctx, &record)?);
                Ok(staging.into_effect(None))
            }
            IssueIntent::IssueCycle {
                doc,
                cycle,
                device,
                ts,
            } => {
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let mut catalog = CatalogState::default();
                if let Some(cycle) = &cycle {
                    crate::record_store::apply_schedule_record(
                        ctx,
                        &mut catalog,
                        &issue.project,
                        cycle,
                    )?;
                }
                let label = match &cycle {
                    Some(c) => {
                        let record = catalog
                            .cycles
                            .get(&issue.project)
                            .and_then(|cs| cs.get(c))
                            .filter(|r| !r.tombstone)
                            .ok_or(Rejection::InvalidRequest)?;
                        staging.absorb_records(crate::record_store::write_issue_relation(
                            ctx,
                            &doc,
                            &issue.project,
                            "cycle",
                            c,
                            true,
                        )?);
                        record.name.clone()
                    }
                    None => {
                        staging.absorb_records(crate::record_store::write_issue_relation(
                            ctx,
                            &doc,
                            &issue.project,
                            "cycle",
                            issue.cycle.as_deref().unwrap_or("none"),
                            false,
                        )?);
                        "none".into()
                    }
                };
                if issue.cycle == cycle {
                    return Ok(unchanged_effect(Some(doc)));
                }
                let mut ev = event("cycled", &device, ts);
                ev.x = label;
                push_event(&mut staging, ctx, &doc, &ev)?;
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::InitiativeSet {
                id,
                name,
                description,
                owner,
                health,
                target_date,
                add_projects,
                remove_projects,
                tombstone,
                device: _,
                ts: _,
            } => {
                let mut catalog_storage = CatalogState::default();
                let catalog = &mut catalog_storage;
                crate::record_store::apply_initiative(ctx, catalog, &id)?;
                staging.require(contract::demand_space_any("project.create"));
                let description_changed = description.is_some();
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
                for project in add_projects.iter().chain(&remove_projects) {
                    crate::record_store::apply_project(ctx, catalog, project)?;
                    if !catalog.projects.contains_key(project) {
                        return Err(Rejection::InvalidRequest);
                    }
                }
                if let Some(tombstone) = tombstone {
                    record.tombstone = tombstone;
                }
                if current.as_ref() == Some(&record)
                    && add_projects.is_empty()
                    && remove_projects.is_empty()
                {
                    return Ok(staging.into_effect(None));
                }
                staging.absorb_records(crate::record_store::write_initiative(
                    ctx,
                    &record,
                    description_changed.then_some(record.description.as_str()),
                )?);
                for project in add_projects {
                    staging.absorb_records(crate::record_store::write_entity_relation(
                        ctx,
                        &record.id,
                        "initiative_project",
                        &project,
                        true,
                    )?);
                }
                for project in remove_projects {
                    staging.absorb_records(crate::record_store::write_entity_relation(
                        ctx,
                        &record.id,
                        "initiative_project",
                        &project,
                        false,
                    )?);
                }
                Ok(staging.into_effect(None))
            }
            IssueIntent::TeamSet {
                id,
                name,
                key,
                icon,
                lead,
                add_members,
                remove_members,
                tombstone,
                device: _,
                ts: _,
            } => {
                let mut catalog_storage = CatalogState::default();
                let catalog = &mut catalog_storage;
                crate::record_store::apply_team(ctx, catalog, &id)?;
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
                        if unique_find_row(ctx, crate::find::field::ENTITY_KEY, &key, "team", None)?
                            .is_some()
                        {
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
                if add_members
                    .iter()
                    .chain(&remove_members)
                    .any(|member| ActorId::parse(member).is_none())
                {
                    return Err(Rejection::InvalidRequest);
                }
                if let Some(tombstone) = tombstone {
                    record.tombstone = tombstone;
                }
                if current.as_ref() == Some(&record)
                    && add_members.is_empty()
                    && remove_members.is_empty()
                {
                    return Ok(staging.into_effect(None));
                }
                staging.absorb_records(crate::record_store::write_team(ctx, &record)?);
                for member in add_members {
                    staging.absorb_records(crate::record_store::write_entity_relation(
                        ctx,
                        &record.id,
                        "team_member",
                        &member,
                        true,
                    )?);
                }
                for member in remove_members {
                    staging.absorb_records(crate::record_store::write_entity_relation(
                        ctx,
                        &record.id,
                        "team_member",
                        &member,
                        false,
                    )?);
                }
                Ok(staging.into_effect(None))
            }
            IssueIntent::TriageSubmit {
                id,
                title,
                body,
                source,
                actor: _,
                device: _,
                ts,
            } => {
                if !contract::valid_name(title.trim())
                    || !contract::valid_text(&body)
                    || id.is_empty()
                    || triage_submission(ctx, &id)?.is_some()
                {
                    return Err(Rejection::InvalidRequest);
                }
                let item = crate::views::TriageItem {
                    id: id.clone(),
                    title: title.trim().to_string(),
                    body,
                    source,
                    submitted_by: ctx.principal().actor.to_string(),
                    ts,
                    ..Default::default()
                };
                staging.absorb_records(crate::record_store::write_triage_submission(ctx, &item)?);
                Ok(staging.into_effect(None))
            }
            IssueIntent::TriageDecide {
                id,
                outcome,
                project,
                doc,
                note,
                actor: _,
                device,
                ts,
            } => {
                staging.require(contract::demand_space_any("project.create"));
                if !contract::TRIAGE_OUTCOMES.contains(&outcome.as_str()) {
                    return Err(Rejection::InvalidRequest);
                }
                let item = triage_submission(ctx, &id)?.ok_or(Rejection::InvalidRequest)?;
                // Decided exactly once.
                if triage_has_decision(ctx, &id)? {
                    return Err(Rejection::Conflict);
                }
                let mut decided = item.clone();
                decided.outcome = outcome.clone();
                decided.decided_by = ctx.principal().actor.to_string();
                decided.decided_ts = ts;
                decided.note = note;
                let mut accepted_project = None;
                match outcome.as_str() {
                    "accepted" => {
                        // Atomically create the issue in the same transaction
                        // that stamps the outcome — an accept can never half
                        // happen.
                        let project = project.ok_or(Rejection::InvalidRequest)?;
                        let doc = doc.ok_or(Rejection::InvalidRequest)?;
                        let mut project_catalog = CatalogState::default();
                        crate::record_store::apply_project(ctx, &mut project_catalog, &project)?;
                        if !project_catalog.projects.contains_key(&project)
                            || DocId::parse(&doc).is_none()
                        {
                            return Err(Rejection::InvalidRequest);
                        }
                        let key = issue_key(&doc);
                        staging.issue(&key, Op::Create);
                        staging.issue(
                            &key,
                            reg(crate::records::roots::ISSUE_ID, doc.as_bytes().to_vec()),
                        );
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
                        let ordinal = next_project_ordinal(ctx, &project, &mut BTreeMap::new())?;
                        let placement_plan = crate::record_store::board_placement(
                            ctx,
                            &project,
                            DEFAULT_STATUS,
                            &doc,
                            Some(&Pos::Top),
                        )?;
                        let placement = placement_plan.placement.ok_or(Rejection::StateCorrupt)?;
                        staging.absorb_records(placement_plan.maintenance);
                        let mut batch = crate::record_store::Batch::default();
                        crate::record_store::write_issue_identity(
                            ctx, &mut batch, &doc, &project, ordinal,
                        )?;
                        staging.absorb_records(batch);
                        let meta = IssueState {
                            project: project.clone(),
                            title: item.title.clone(),
                            status: DEFAULT_STATUS.into(),
                            priority: Priority::None,
                            created_by: ActorId::parse(&item.submitted_by),
                            created_at: ts,
                            ..IssueState::default()
                        };
                        staging.absorb_records(crate::record_store::write_issue_meta(
                            ctx, &doc, &meta, false,
                        )?);
                        let (transition, _) = crate::record_store::write_issue_transition(
                            ctx,
                            &doc,
                            &[],
                            &placement,
                            "",
                            ts,
                        )?;
                        staging.absorb_records(transition);
                        push_event(&mut staging, ctx, &doc, &event("created", &device, ts))?;
                        accepted_project = Some(project);
                        decided.doc = doc;
                    }
                    "duplicate" => {
                        let doc = doc.ok_or(Rejection::InvalidRequest)?;
                        let _target =
                            issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                        decided.doc = doc;
                    }
                    _ => {}
                }
                staging.absorb_records(crate::record_store::write_triage_decision(
                    ctx,
                    &decided,
                    accepted_project.as_deref(),
                )?);
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
                actor: _,
                device,
                ts,
            } => {
                // The name is refused here rather than repaired, because the
                // party proposing it is a local actor holding write authority
                // who can simply pick another. Repair belongs at the far end,
                // where the proposer is remote and refusing would let them make
                // their own attachment unsaveable.
                let name = name.trim();
                if !id.starts_with("att_")
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
                let issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                let existing = raw_attachments(ctx, &doc)?;
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
                let record = crate::records::IssueAttachmentRecord {
                    issue: doc.clone(),
                    id: id.clone(),
                    name: name.clone(),
                    mime,
                    size,
                    by: ctx.principal().actor.to_string(),
                    timestamp: ts,
                    comment,
                    content,
                    tombstone: false,
                };
                staging.absorb_records(crate::record_store::write_attachment(ctx, &record)?);
                staging.declare(
                    &crate::records::issue_attachment_key(
                        &DocId::parse(&doc).ok_or(Rejection::InvalidRequest)?,
                        &id,
                    ),
                    vec![content_ref],
                );
                let mut ev = event("attached", &device, ts);
                ev.x = name;
                push_event(&mut staging, ctx, &doc, &ev)?;
                Ok(staging.into_effect(Some(doc)))
            }
            IssueIntent::Detach {
                doc,
                id,
                device,
                ts,
            } => {
                // The attachment's own record is the one that says whether it
                // exists, and it carries the name too. Asking the Issue's
                // in-memory attachment list first refused every detach: that
                // list is enrichment the core state no longer holds, so it is
                // always empty and the record read below never ran.
                let mut record = crate::record_store::read_attachment(ctx, &doc, &id)?
                    .ok_or(Rejection::InvalidRequest)?;
                if record.tombstone || record.issue != doc {
                    return Err(Rejection::InvalidRequest);
                }
                let name = record.name.clone();
                record.tombstone = true;
                staging.absorb_records(crate::record_store::write_attachment(ctx, &record)?);
                staging.declare(
                    &crate::records::issue_attachment_key(
                        &DocId::parse(&doc).ok_or(Rejection::InvalidRequest)?,
                        &id,
                    ),
                    Vec::new(),
                );
                let mut ev = event("detached", &device, ts);
                ev.x = name;
                push_event(&mut staging, ctx, &doc, &ev)?;
                Ok(staging.into_effect(Some(doc)))
            }
        }
    }

    fn query(&self, ctx: &Context<'_>, query: Query) -> Result<Projection, Rejection> {
        let query = IssueQuery::from_json(&query.payload).ok_or(Rejection::InvalidRequest)?;
        if self.package == IssuesPackage::Preferred
            && matches!(
                &query,
                IssueQuery::V4MigrationStatus | IssueQuery::StructureStatus
            )
        {
            return Err(Rejection::InvalidRequest);
        }
        let load_catalog = || live_catalog(ctx);
        let projection = |bytes: Vec<u8>| Projection {
            schema: contract::issue_schema(),
            schema_version: contract::ISSUE_SCHEMA_VERSION,
            bytes,
            frontier: replica::frontier::ReplicaFrontier::EMPTY, // stamped by Runtime
            publication: None, // stamped by Runtime with the exact immutable read image
            demand: contract::demand_read(),
        };
        match query {
            IssueQuery::V4MigrationStatus => Ok(projection(
                serde_json::to_vec(&crate::record_store::migration_verification(ctx)?)
                    .expect("migration verification JSON"),
            )),
            IssueQuery::Resolve {
                entity,
                selector,
                project,
            } => Ok(projection(
                serde_json::to_vec(&resolve_entity(ctx, entity, &selector, project.as_deref())?)
                    .expect("resolved entity JSON"),
            )),
            IssueQuery::StructureStatus => {
                let mut catalog = load_catalog()?;
                let catalog: &mut CatalogState = &mut catalog;
                let read = issue_read_set(ctx, catalog, None)?;
                Ok(projection(
                    serde_json::to_vec(&structure_report(ctx, catalog, &read)?)
                        .expect("structure report JSON"),
                ))
            }
            IssueQuery::View { doc, me } => {
                let mut issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                enrich_issue_relations(ctx, &mut issue, &doc)?;
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &issue.project)?;
                apply_project_workflow(ctx, &mut catalog, &issue.project)?;
                let aliases = aliases_for_issue(ctx, &catalog, &doc)?;
                // View is the bounded issue/content summary. Discussion,
                // relations, attachments, checks and activity are independent
                // pages and therefore are intentionally empty here.
                let resolve = |_comment: &StoredComment| None;
                let view = issue_view(
                    &catalog,
                    &aliases,
                    &space_placeholder(),
                    &doc,
                    &issue,
                    &resolve,
                );
                let _ = me;
                Ok(projection(serde_json::to_vec(&view).expect("view json")))
            }
            IssueQuery::Detail { doc, me, pages } => {
                let mut issue = issue_core_state(ctx, &doc).ok_or(Rejection::InvalidRequest)?;
                enrich_issue_relations(ctx, &mut issue, &doc)?;
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &issue.project)?;
                apply_project_workflow(ctx, &mut catalog, &issue.project)?;
                let aliases = aliases_for_issue(ctx, &catalog, &doc)?;
                let resolve = |_comment: &StoredComment| None;
                let issue = issue_view(
                    &catalog,
                    &aliases,
                    &space_placeholder(),
                    &doc,
                    &issue,
                    &resolve,
                );
                let detail = contract::IssueDetailProjection {
                    publication: ctx
                        .world_publication_id()
                        .ok_or(Rejection::ImplementationUnavailable)?,
                    issue,
                    comments: issue_comments_page(ctx, &doc, &pages.comments)?,
                    reactions: issue_reactions_page(ctx, &doc, &pages.reactions)?,
                    attachments: issue_attachments_page(ctx, &doc, &pages.attachments)?,
                    checks: issue_checks_page(ctx, &doc, &pages.checks)?,
                    outgoing_relations: issue_relations_page(
                        ctx,
                        &doc,
                        crate::dto::RelationDirection::Out,
                        &pages.outgoing_relations,
                    )?,
                    incoming_relations: issue_relations_page(
                        ctx,
                        &doc,
                        crate::dto::RelationDirection::In,
                        &pages.incoming_relations,
                    )?,
                };
                let _ = me;
                Ok(projection(
                    serde_json::to_vec(&detail).expect("issue detail json"),
                ))
            }
            IssueQuery::List {
                project,
                label,
                status,
                milestone,
                mine,
                all,
                me,
                mut facets,
                page,
            } => {
                // The singular arguments are one spelling of the plural ones.
                // Fold rather than branch: two code paths answering the same
                // question is how they come to disagree about it.
                for (axis, value) in [
                    (&mut facets.labels, &label),
                    (&mut facets.statuses, &status),
                    (&mut facets.milestones, &milestone),
                    (&mut facets.assignees, &mine),
                ] {
                    if let Some(value) = value {
                        axis.push(value.clone());
                    }
                }
                facets
                    .canonicalize()
                    .map_err(|()| Rejection::InvalidRequest)?;
                if !facets.is_empty() {
                    let axes = facet_seeks(project.as_deref(), &facets)?;
                    let answer = find_faceted_issue_page(ctx, project.as_deref(), &axes, all)?;
                    let mut catalog = CatalogState::default();
                    let mut loaded_workflows = std::collections::BTreeSet::new();
                    let mut rows = Vec::new();
                    for result in answer.rows() {
                        let row = issue_page_row(result)?;
                        let project_id = row.project_id.as_str();
                        if !all {
                            if loaded_workflows.insert(project_id.to_string()) {
                                apply_project_workflow(ctx, &mut catalog, project_id)?;
                            }
                            if issue_status_category(&catalog, project_id, &row.status)?
                                == StatusCategory::Done
                            {
                                continue;
                            }
                        }
                        rows.push(row);
                    }
                    let me_actor = me.as_deref().and_then(ActorId::parse);
                    apply_page_aliases(ctx, &mut catalog, &mut rows)?;
                    enrich_issue_page(ctx, &mut catalog, &mut rows, me_actor.as_ref())?;
                    // The merged answer is the WHOLE filtered set -- Find hands
                    // back no continuation for a non-linear plan, so there is no
                    // partial page to miscount. This total is the count itself,
                    // and it is exact.
                    let exact = u64::try_from(rows.len()).ok();
                    let mut page_out = page_from_answer(&answer, rows);
                    page_out.exact_total = exact;
                    page_out.next_cursor = None;
                    return Ok(projection(
                        serde_json::to_vec(&page_out).expect("faceted rows page json"),
                    ));
                }
                // A relation filter asks the reverse membership question --
                // "which issues carry this label" -- so it is seeked on the
                // reverse coordinate rather than post-filtered out of a page
                // of every issue. One coordinate leads and any others narrow
                // the result, because a page can only be bounded by one
                // posting; which one leads changes what the scan costs, never
                // what it answers.
                let lead = label
                    .as_ref()
                    .map(|value| ("label", value.clone()))
                    .or_else(|| milestone.as_ref().map(|value| ("milestone", value.clone())))
                    .or_else(|| mine.as_ref().map(|value| ("assignee", value.clone())));
                let mut narrowing: Vec<(&str, String, usize)> = Vec::new();
                for (kind, value, maximum) in [
                    ("label", label.clone(), contract::MAX_ISSUE_LABELS),
                    ("milestone", milestone.clone(), 1),
                    ("assignee", mine.clone(), contract::MAX_ISSUE_ASSIGNEES),
                ] {
                    let Some(value) = value else { continue };
                    if lead.as_ref().is_some_and(|(led, _)| *led == kind) {
                        continue;
                    }
                    narrowing.push((kind, value, maximum));
                }
                let mut predicates = vec![runtime::find::Predicate {
                    field: crate::find::field_ref(crate::find::field::CONFLICTED),
                    test: runtime::find::Test::Equal,
                    value: runtime::find::Atom::Bool(false),
                }];
                if !all {
                    predicates.push(runtime::find::Predicate {
                        field: crate::find::field_ref(crate::find::field::TOMBSTONE),
                        test: runtime::find::Test::Equal,
                        value: runtime::find::Atom::Bool(false),
                    });
                }
                if let Some(status) = &status {
                    predicates.push(runtime::find::Predicate {
                        field: crate::find::field_ref(crate::find::field::STATE),
                        test: runtime::find::Test::Equal,
                        value: runtime::find::Atom::Text(status.clone()),
                    });
                }
                let (answer, candidates) = match &lead {
                    Some((kind, value)) => {
                        let answer = find_field_page(
                            ctx,
                            crate::find::field::RELATION_TARGET_KIND,
                            runtime::find::Atom::Bytes(crate::find::composite_key([
                                *kind,
                                value.as_str(),
                            ])),
                            &page,
                            Vec::new(),
                            Vec::new(),
                        )?;
                        let ids = answer
                            .rows()
                            .iter()
                            .map(|row| {
                                result_text(row, crate::find::field::SOURCE_ID)
                                    .ok_or(Rejection::StateCorrupt)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let resolved = find_issue_rows_by_ids(ctx, ids.clone())?;
                        // Keep the posting order the page was cut in: the
                        // cursor resumes against that order, so re-sorting
                        // here would make the next page overlap this one.
                        let rows = ids
                            .iter()
                            .filter_map(|id| resolved.get(id).cloned())
                            .collect::<Vec<_>>();
                        (answer, rows)
                    }
                    None => {
                        let answer = find_kind_page(
                            ctx,
                            "issue",
                            project.as_deref(),
                            &page,
                            predicates,
                            [
                                crate::find::field::TITLE,
                                crate::find::field::PROJECT,
                                crate::find::field::STATE,
                                crate::find::field::PRIORITY,
                                crate::find::field::TOMBSTONE,
                                crate::find::field::DUE_AT,
                                crate::find::field::ESTIMATE,
                            ]
                            .into_iter()
                            .map(crate::find::field_ref)
                            .collect(),
                        )?;
                        let rows = answer
                            .rows()
                            .iter()
                            .map(issue_page_row)
                            .collect::<Result<Vec<_>, _>>()?;
                        (answer, rows)
                    }
                };
                let mut catalog = CatalogState::default();
                let mut loaded_projects = std::collections::BTreeSet::new();
                let mut loaded_workflows = std::collections::BTreeSet::new();
                let mut rows = Vec::new();
                for row in candidates {
                    let project_id = row.project_id.as_str();
                    if lead.is_some() {
                        // A relation posting carries membership and nothing
                        // else, so every scalar the other branch expressed as
                        // a predicate is applied here instead.
                        if project.as_ref().is_some_and(|want| want != project_id) {
                            continue;
                        }
                        if status.as_ref().is_some_and(|want| want != &row.status) {
                            continue;
                        }
                        if !all && row.tombstone {
                            continue;
                        }
                        let doc = row.doc_id.to_string();
                        let mut carries_all = true;
                        for (kind, value, maximum) in &narrowing {
                            if !issue_relation_targets(ctx, &doc, kind, *maximum)?.contains(value) {
                                carries_all = false;
                                break;
                            }
                        }
                        if !carries_all {
                            continue;
                        }
                    }
                    // Both of these were per ROW, and neither memoises itself:
                    // `apply_project` re-reads the project Body on every call,
                    // and `apply_project_workflow` re-runs `workflow_projection`
                    // before deduplicating into the catalog it was already in.
                    // A hundred-row cross-project list therefore did a hundred
                    // project reads and a hundred workflow resolutions to learn
                    // the same handful of facts. A page holds few distinct
                    // projects; ask once each.
                    if project.is_none() && loaded_projects.insert(project_id.to_string()) {
                        crate::record_store::apply_project(ctx, &mut catalog, project_id)?;
                    }
                    if project.is_none()
                        && catalog
                            .projects
                            .get(project_id)
                            .is_some_and(|meta| meta.archived)
                    {
                        continue;
                    }
                    if !all {
                        if loaded_workflows.insert(project_id.to_string()) {
                            apply_project_workflow(ctx, &mut catalog, project_id)?;
                        }
                        if issue_status_category(&catalog, project_id, &row.status)?
                            == StatusCategory::Done
                        {
                            continue;
                        }
                    }
                    rows.push(row);
                }
                let me_actor = me.as_deref().and_then(ActorId::parse);
                apply_page_aliases(ctx, &mut catalog, &mut rows)?;
                enrich_issue_page(ctx, &mut catalog, &mut rows, me_actor.as_ref())?;
                let mut page = page_from_answer(&answer, rows);
                if !all || project.is_none() || lead.is_some() {
                    // Post-filtered totals are not the source posting total.
                    // A relation page is always in that position: its total
                    // counts membership, and every scalar above narrows it.
                    page.exact_total = None;
                }
                Ok(projection(
                    serde_json::to_vec(&page).expect("rows page json"),
                ))
            }
            IssueQuery::Board { project, me, page } => {
                let mut catalog = CatalogState::default();
                crate::record_store::apply_project(ctx, &mut catalog, &project)?;
                apply_project_workflow(ctx, &mut catalog, &project)?;
                let project_view = catalog
                    .projects
                    .get(&project)
                    .and_then(|meta| project_dto(&project, meta))
                    .ok_or(Rejection::InvalidRequest)?;
                let revision = catalog
                    .resolved_workflow(&project)
                    .map_err(workflow_rejection)?;
                let workflow = revision
                    .body
                    .states
                    .iter()
                    .map(|state| {
                        Ok(crate::dto::WorkflowState {
                            id: state.state_id.clone(),
                            name: state.name.clone(),
                            category: StatusCategory::parse(&state.category)
                                .ok_or(Rejection::StateCorrupt)?,
                            color: state.color.clone(),
                        })
                    })
                    .collect::<Result<Vec<_>, Rejection>>()?;
                let mut rows = find_board_page(ctx, &project, &workflow, &page)?;
                // The board never enriched at all -- not the alias, and not
                // the memberships the list has been enriching since
                // `enrich_issue_page` landed. A card with no assignee is the
                // same defect as a list row with none, one surface over.
                apply_page_aliases(ctx, &mut catalog, &mut rows.items)?;
                let me_actor = me.as_deref().and_then(ActorId::parse);
                enrich_issue_page(ctx, &mut catalog, &mut rows.items, me_actor.as_ref())?;
                // The board page walks the block posting and cannot count what
                // it did not visit, so it declined to. But the live count per
                // state is a posting that exists for exactly this, and the
                // viewer needs it: it draws every list from this one answer,
                // and without a total it counted the rows it held and called
                // that the project. `None` stays `None` -- an unmeasured
                // state makes the sum unmeasured, never smaller.
                rows.exact_total = project_issue_counts(ctx, &mut catalog, &project)?
                    .map(|(total, _done)| u64::from(total));
                let view = crate::dto::BoardPage {
                    schema_version: VIEW_SCHEMA_VERSION,
                    project: project_view,
                    workflow,
                    rows,
                };
                Ok(projection(
                    serde_json::to_vec(&view).expect("board page json"),
                ))
            }
            IssueQuery::History { doc, page } => {
                let answer = find_source_created_page(
                    ctx,
                    "activity",
                    &doc,
                    // Oldest first. An Issue's history is a trail read from
                    // where it starts -- the first row is the creation and
                    // carries sequence one. The newest-first ordering belongs
                    // to the cross-Issue activity feed, which is a different
                    // question: "what just happened", not "what happened".
                    false,
                    &page,
                    Vec::new(),
                    [
                        crate::find::field::STATE,
                        crate::find::field::AUTHOR,
                        crate::find::field::TEXT,
                        crate::find::field::CREATED_AT,
                        crate::find::field::SOURCE_ID,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| activity_page_row(ctx, row))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("history page json"),
                ))
            }
            IssueQuery::Relations {
                doc,
                direction,
                page,
            } => Ok(projection(
                serde_json::to_vec(&issue_relations_page(ctx, &doc, direction, &page)?)
                    .expect("relation page json"),
            )),
            IssueQuery::Comments { doc, page } => Ok(projection(
                serde_json::to_vec(&issue_comments_page(ctx, &doc, &page)?)
                    .expect("comment page json"),
            )),
            IssueQuery::Reactions { doc, page } => Ok(projection(
                serde_json::to_vec(&issue_reactions_page(ctx, &doc, &page)?)
                    .expect("reaction page json"),
            )),
            IssueQuery::Attachments { doc, page } => Ok(projection(
                serde_json::to_vec(&issue_attachments_page(ctx, &doc, &page)?)
                    .expect("attachment page json"),
            )),
            IssueQuery::Checks { doc, page } => Ok(projection(
                serde_json::to_vec(&issue_checks_page(ctx, &doc, &page)?).expect("check page json"),
            )),
            IssueQuery::Activity { page } => {
                let answer = find_created_page(
                    ctx,
                    "activity",
                    None,
                    &page,
                    Vec::new(),
                    [
                        crate::find::field::STATE,
                        crate::find::field::AUTHOR,
                        crate::find::field::TEXT,
                        crate::find::field::CREATED_AT,
                        crate::find::field::SOURCE_ID,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| activity_page_row(ctx, row))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("activity page json"),
                ))
            }
            IssueQuery::Inbox {
                exclude_device,
                page,
            } => {
                let actor = ctx.principal().actor.clone();
                let find_page = inbox_find_request(&page, exclude_device.as_deref())?;
                let answer = find_field_test_page(
                    ctx,
                    crate::find::field::INBOX_ORDER,
                    runtime::find::Test::Prefix,
                    runtime::find::Atom::Bytes(crate::find::composite_key([actor.as_str()])),
                    &find_page,
                    Vec::new(),
                    [
                        crate::find::field::STATE,
                        crate::find::field::AUTHOR,
                        crate::find::field::DEVICE,
                        crate::find::field::CREATED_AT,
                        crate::find::field::SOURCE_ID,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let visible = answer
                    .rows()
                    .iter()
                    .filter(|row| {
                        exclude_device.as_deref()
                            != result_text(row, crate::find::field::DEVICE).as_deref()
                    })
                    .collect::<Vec<_>>();
                let items = inbox_page_rows(ctx, &visible, &actor)?;
                let page = contract::Page {
                    publication: answer.coordinates().world_publication(),
                    items,
                    next_cursor: inbox_next_cursor(&answer, exclude_device.as_deref()),
                    // Filtering the caller's current device changes the
                    // cardinality after the ordered posting seek. Do not claim
                    // the posting total is an exact visible total.
                    exact_total: exclude_device
                        .is_none()
                        .then(|| answer.matched_total())
                        .flatten(),
                };
                Ok(projection(serde_json::to_vec(&page).expect("inbox page")))
            }
            IssueQuery::Projects { page } => {
                let answer = find_kind_page(
                    ctx,
                    "project",
                    None,
                    &page,
                    vec![live_predicate()],
                    [
                        crate::find::field::TITLE,
                        crate::find::field::ENTITY_KEY,
                        crate::find::field::HEALTH,
                        crate::find::field::AUTHOR,
                        crate::find::field::CREATED_AT,
                        crate::find::field::TARGET_DATE,
                        crate::find::field::ARCHIVED,
                        crate::find::field::SOURCE_ID,
                        crate::find::field::TOMBSTONE,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(project_page_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("projects page json"),
                ))
            }
            IssueQuery::ProjectUpdates { project, page } => {
                let answer = find_created_page(
                    ctx,
                    "project_update",
                    Some(&project),
                    &page,
                    Vec::new(),
                    [
                        crate::find::field::TEXT,
                        crate::find::field::AUTHOR,
                        crate::find::field::CREATED_AT,
                        crate::find::field::HEALTH,
                        crate::find::field::PROJECT,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(update_page_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("project updates page json"),
                ))
            }
            IssueQuery::Labels { page } => {
                let answer = find_kind_page(
                    ctx,
                    "label",
                    None,
                    &page,
                    vec![live_predicate()],
                    [
                        crate::find::field::TITLE,
                        crate::find::field::HEALTH,
                        crate::find::field::TOMBSTONE,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(label_page_row)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("labels page json"),
                ))
            }
            IssueQuery::Label { label } => {
                let row = unique_find_row(ctx, crate::find::field::ID, &label, "label", None)?
                    .ok_or(Rejection::InvalidRequest)?;
                let label = if result_bool(&row, crate::find::field::TOMBSTONE).unwrap_or(false) {
                    None
                } else {
                    Some(label_page_row(&row)?)
                };
                let result = contract::LabelProjection {
                    publication: ctx
                        .world_publication_id()
                        .ok_or(Rejection::ImplementationUnavailable)?,
                    label,
                };
                Ok(projection(
                    serde_json::to_vec(&result).expect("label projection json"),
                ))
            }
            IssueQuery::Roles { page } => {
                let answer = find_kind_page(
                    ctx,
                    "role_head",
                    None,
                    &page,
                    Vec::new(),
                    [
                        crate::find::field::ENTITY_KEY,
                        crate::find::field::STATE,
                        crate::find::field::HEAD_REVISIONS,
                        crate::find::field::CONFLICTED,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| {
                        let summary = role_summary_row(row)?;
                        role_projection(ctx, &summary.role_id)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items)).expect("role page json"),
                ))
            }
            IssueQuery::RoleShow { role } => {
                let view = role_projection(ctx, &role)?;
                Ok(projection(serde_json::to_vec(&view).expect("role json")))
            }
            IssueQuery::Workflow { project } => {
                let view = workflow_projection(ctx, &project)?;
                Ok(projection(
                    serde_json::to_vec(&view).expect("workflow json"),
                ))
            }
            IssueQuery::Specs { project, page } => {
                let answer = find_kind_page(
                    ctx,
                    "spec",
                    project.as_deref(),
                    &page,
                    vec![runtime::find::Predicate {
                        field: crate::find::field_ref(crate::find::field::RELATION_KIND),
                        test: runtime::find::Test::Equal,
                        value: runtime::find::Atom::Text("spec_document".into()),
                    }],
                    [
                        crate::find::field::PROJECT,
                        crate::find::field::ENTITY_KEY,
                        crate::find::field::HEAD_REVISIONS,
                        crate::find::field::ISSUED_REVISIONS,
                        crate::find::field::CONFLICTED,
                        crate::find::field::RELATION_KIND,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let mut items = answer
                    .rows()
                    .iter()
                    .map(spec_summary_row)
                    .collect::<Result<Vec<_>, _>>()?;
                for item in &mut items {
                    item.head = spec_head(ctx, item);
                }
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items)).expect("specs page json"),
                ))
            }
            IssueQuery::Spec { spec } => {
                let spec = spec_state(ctx, &spec).ok_or(Rejection::InvalidRequest)?;
                let view = spec_view(&spec).ok_or(Rejection::InvalidRequest)?;
                Ok(projection(serde_json::to_vec(&view).expect("spec json")))
            }
            IssueQuery::SpecHistory { spec, page } => {
                let answer = find_field_page(
                    ctx,
                    crate::find::field::SOURCE_ID,
                    runtime::find::Atom::Text(spec.into()),
                    &page,
                    vec![runtime::find::Predicate {
                        field: crate::find::field_ref(crate::find::field::RELATION_KIND),
                        test: runtime::find::Test::Equal,
                        value: runtime::find::Atom::Text("spec_revision".into()),
                    }],
                    [
                        crate::find::field::REVISION,
                        crate::find::field::SOURCE_ID,
                        crate::find::field::RELATION_KIND,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| spec_revision_page_row(ctx, row))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("spec history page json"),
                ))
            }
            IssueQuery::SpecReferences { project, page } => {
                let answer = find_kind_page(
                    ctx,
                    "relation",
                    project.as_deref(),
                    &page,
                    vec![runtime::find::Predicate {
                        field: crate::find::field_ref(crate::find::field::ENTITY_KEY),
                        test: runtime::find::Test::Equal,
                        value: runtime::find::Atom::Text("spec_reference".into()),
                    }],
                    [
                        crate::find::field::RELATION_KIND,
                        crate::find::field::SOURCE_ID,
                        crate::find::field::TARGET_ID,
                        crate::find::field::PROJECT,
                        crate::find::field::ENTITY_KEY,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| spec_reference_page_row(ctx, row))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("spec references page json"),
                ))
            }
            IssueQuery::SpecObservations { project, page } => {
                let answer = find_created_page(
                    ctx,
                    "spec_observation_fact",
                    project.as_deref(),
                    &page,
                    Vec::new(),
                    [
                        crate::find::field::STATE,
                        crate::find::field::PROJECT,
                        crate::find::field::SOURCE_ID,
                        crate::find::field::TARGET_ID,
                        crate::find::field::RELATION_KIND,
                        crate::find::field::AUTHOR,
                        crate::find::field::TEXT,
                        crate::find::field::CREATED_AT,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| spec_observation_page_row(ctx, row))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("spec observations page json"),
                ))
            }
            IssueQuery::BaselineHistory { baseline, page } => {
                let answer = find_field_page(
                    ctx,
                    crate::find::field::SOURCE_ID,
                    runtime::find::Atom::Text(baseline.into()),
                    &page,
                    vec![runtime::find::Predicate {
                        field: crate::find::field_ref(crate::find::field::RELATION_KIND),
                        test: runtime::find::Test::Equal,
                        value: runtime::find::Atom::Text("baseline_revision".into()),
                    }],
                    [
                        crate::find::field::REVISION,
                        crate::find::field::SOURCE_ID,
                        crate::find::field::RELATION_KIND,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| baseline_revision_page_row(ctx, row))
                    .collect::<Result<Vec<_>, _>>()?
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("baseline history page json"),
                ))
            }
            IssueQuery::Baselines { project, page } => {
                let answer = find_kind_page(
                    ctx,
                    "baseline",
                    project.as_deref(),
                    &page,
                    vec![runtime::find::Predicate {
                        field: crate::find::field_ref(crate::find::field::RELATION_KIND),
                        test: runtime::find::Test::Equal,
                        value: runtime::find::Atom::Text("baseline_document".into()),
                    }],
                    [
                        crate::find::field::PROJECT,
                        crate::find::field::HEAD_REVISIONS,
                        crate::find::field::ISSUED_REVISIONS,
                        crate::find::field::CONFLICTED,
                        crate::find::field::RELATION_KIND,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let mut items = answer
                    .rows()
                    .iter()
                    .map(baseline_summary_row)
                    .collect::<Result<Vec<_>, _>>()?;
                for item in &mut items {
                    item.head = baseline_head(ctx, item);
                }
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("baselines page json"),
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
            IssueQuery::Milestones { project, page } => {
                let answer = find_kind_position_page(
                    ctx,
                    "milestone",
                    &project,
                    &page,
                    vec![live_predicate()],
                    [
                        crate::find::field::TITLE,
                        crate::find::field::TEXT,
                        crate::find::field::TARGET_DATE,
                        crate::find::field::POSITION,
                        crate::find::field::TOMBSTONE,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let mut catalog = CatalogState::default();
                let mut items = Vec::with_capacity(answer.rows().len());
                for row in answer.rows() {
                    let mut item = milestone_page_row(row)?;
                    // A milestone's progress is the whole reason the row is
                    // drawn, so measure it where it can be measured exactly
                    // and say so where it cannot. Reporting the unmeasured
                    // case as `0 of 0` would render as a complete milestone.
                    if let Some((total, done)) =
                        membership_counts(ctx, &mut catalog, "milestone", &item.id)?
                    {
                        item.total = total;
                        item.done = done;
                        item.enrichment_complete = true;
                    }
                    items.push(item);
                }
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("milestones page json"),
                ))
            }
            IssueQuery::Geometry {
                project,
                roots,
                page,
            } => {
                if crate::ids::ProjectId::parse(&project).is_none()
                    || roots.len() > crate::spec::MAX_PLAN_ROOTS
                    || roots.iter().any(|root| DocId::parse(root).is_none())
                {
                    return Err(Rejection::InvalidRequest);
                }
                let mut canonical_roots = roots;
                canonical_roots.sort();
                canonical_roots.dedup();
                let view = self.geometry_projection(ctx, &project, &canonical_roots, page)?;
                Ok(projection(
                    serde_json::to_vec(&view).expect("Issue geometry"),
                ))
            }
            IssueQuery::Cycles { project, page } => {
                let answer = find_kind_page(
                    ctx,
                    "cycle",
                    Some(&project),
                    &page,
                    vec![live_predicate()],
                    [
                        crate::find::field::TITLE,
                        crate::find::field::CREATED_AT,
                        crate::find::field::DUE_AT,
                        crate::find::field::TOMBSTONE,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let mut catalog = CatalogState::default();
                let mut items = Vec::with_capacity(answer.rows().len());
                for row in answer.rows() {
                    let mut item = cycle_page_row(row)?;
                    // Same as a milestone: the progress is the reason the row
                    // is drawn, and `0 of 0` renders as a finished cycle.
                    if let Some((total, done)) =
                        membership_counts(ctx, &mut catalog, "cycle", &item.id)?
                    {
                        item.total = total;
                        item.done = done;
                        item.enrichment_complete = true;
                    }
                    items.push(item);
                }
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("cycles page json"),
                ))
            }
            IssueQuery::Initiatives { page } => {
                let answer = find_kind_page(
                    ctx,
                    "initiative",
                    None,
                    &page,
                    vec![live_predicate()],
                    [
                        crate::find::field::TITLE,
                        crate::find::field::AUTHOR,
                        crate::find::field::HEALTH,
                        crate::find::field::TARGET_DATE,
                        crate::find::field::TOMBSTONE,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let mut catalog = CatalogState::default();
                // One project's facts, asked once per page rather than once per
                // initiative that names it. Both calls below were per (initiative
                // x project) with no memo: `apply_project` re-reads the Body every
                // time, and `project_issue_counts` re-resolves the workflow and
                // seeks a count per state. A project in three initiatives was
                // read three times and counted three times to say one thing.
                let mut counted: std::collections::BTreeMap<String, Option<(u32, u32)>> =
                    std::collections::BTreeMap::new();
                let mut items = Vec::with_capacity(answer.rows().len());
                for row in answer.rows() {
                    let mut item = initiative_page_row(row)?;
                    // An initiative IS its member projects and what they add
                    // up to; a row without them is a name and nothing else.
                    let members =
                        issue_relation_targets(ctx, &item.id, "project", MAX_ROLLUP_MEMBERS)?;
                    let mut total = 0u32;
                    let mut done = 0u32;
                    let mut measured = true;
                    for project in &members {
                        if !counted.contains_key(project) {
                            crate::record_store::apply_project(ctx, &mut catalog, project)?;
                            let counts = project_issue_counts(ctx, &mut catalog, project)?;
                            counted.insert(project.clone(), counts);
                        }
                        if let Some(meta) = catalog.projects.get(project) {
                            item.projects.push(meta.key.clone());
                        }
                        match counted.get(project).copied().flatten() {
                            Some((project_total, project_done)) => {
                                total = total.saturating_add(project_total);
                                done = done.saturating_add(project_done);
                            }
                            // One unmeasurable member makes the roll-up
                            // unmeasured. A partial sum is not a smaller sum.
                            None => measured = false,
                        }
                    }
                    if measured {
                        item.total = total;
                        item.done = done;
                        item.enrichment_complete = true;
                    }
                    items.push(item);
                }
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("initiatives page json"),
                ))
            }
            IssueQuery::Teams { page } => {
                let answer = find_kind_page(
                    ctx,
                    "team",
                    None,
                    &page,
                    vec![live_predicate()],
                    [
                        crate::find::field::TITLE,
                        crate::find::field::ENTITY_KEY,
                        crate::find::field::HEALTH,
                        crate::find::field::AUTHOR,
                        crate::find::field::TOMBSTONE,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let mut items = Vec::with_capacity(answer.rows().len());
                for row in answer.rows() {
                    let mut item = team_page_row(row)?;
                    // A team is who is on it and what it owns. Both are
                    // bounded exact seeks: membership from the relation
                    // posting, ownership from the project's own source
                    // coordinate, which is where a project records its team.
                    item.members =
                        issue_relation_targets(ctx, &item.id, "member", MAX_ROLLUP_MEMBERS)?
                            .into_iter()
                            .collect();
                    item.projects = team_project_keys(ctx, &item.id)?;
                    item.enrichment_complete = true;
                    items.push(item);
                }
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items)).expect("teams page json"),
                ))
            }
            IssueQuery::Triage { page } => {
                let answer = find_created_page(
                    ctx,
                    "triage_fact",
                    None,
                    &page,
                    Vec::new(),
                    [
                        crate::find::field::STATE,
                        crate::find::field::HEALTH,
                        crate::find::field::CREATED_AT,
                    ]
                    .into_iter()
                    .map(crate::find::field_ref)
                    .collect(),
                )?;
                let items = answer
                    .rows()
                    .iter()
                    .map(|row| triage_page_row(ctx, row))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(projection(
                    serde_json::to_vec(&page_from_answer(&answer, items))
                        .expect("triage fact page json"),
                ))
            }
            IssueQuery::Attachment { doc, id } => {
                let record = crate::record_store::read_attachment(ctx, &doc, &id)?
                    .filter(|record| !record.tombstone)
                    .ok_or(Rejection::InvalidRequest)?;
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
        checks: vec![],
        provisional: true,
        corrupt_records: vec![],
        more_comments: None,
        reactions_complete: true,
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
mod check_demand_tests {
    use super::*;

    #[test]
    fn accepting_into_done_requires_both_verification_and_transition_authority() {
        let verification = contract::demand_project_work("issue.verify", "prj_example");
        let transition = contract::demand_project_any("issue.transition", "prj_example");
        let combined = require_both(verification.clone(), transition.clone()).unwrap();
        let decoded = mechanics::authorization::AuthorizationDemand::decode_canonical(&combined)
            .expect("combined demand is canonical");
        let mechanics::authorization::AuthorizationDemand::All(children) = decoded else {
            panic!("independent product decisions must remain an All demand");
        };
        assert_eq!(children.len(), 2);
        let encoded = children
            .iter()
            .map(|child| child.encode_canonical().unwrap())
            .collect::<Vec<_>>();
        assert!(encoded.contains(&verification));
        assert!(encoded.contains(&transition));
    }
}

#[cfg(test)]
mod facet_bound_tests {
    use super::*;

    /// The declaration fits the ceiling it will be measured against.
    ///
    /// Find refuses on the DECLARATION, before it evaluates anything, and the
    /// refusal is `Invalid` -- which reaches a caller as `InvalidRequest` and
    /// names the request rather than the dimension that overflowed. The first
    /// version of `facet_bound` claimed 16 KiB a row against an 8 MiB
    /// projection and every faceted query in the product failed that way.
    ///
    /// Checked against the World's own declared schema bound rather than a
    /// copy of the numbers, so tightening the schema tightens this too.
    #[test]
    fn a_faceted_declaration_fits_the_schema_it_runs_under() {
        let schemas = crate::find::preferred_schemas();
        let ceiling = schemas.first().expect("the entity schema").bound;
        for branches in 1..=(MAX_FACET_BRANCHES as u64 + 1) {
            let bound = facet_bound(branches);
            for (name, claimed, allowed) in [
                (
                    "decoded_bodies",
                    bound.decoded_bodies,
                    ceiling.decoded_bodies,
                ),
                ("postings_read", bound.postings_read, ceiling.postings_read),
                ("edges_visited", bound.edges_visited, ceiling.edges_visited),
                ("nodes_visited", bound.nodes_visited, ceiling.nodes_visited),
                (
                    "paths_retained",
                    bound.paths_retained,
                    ceiling.paths_retained,
                ),
                (
                    "candidates_per_branch",
                    bound.candidates_per_branch,
                    ceiling.candidates_per_branch,
                ),
                (
                    "score_evaluations",
                    bound.score_evaluations,
                    ceiling.score_evaluations,
                ),
                (
                    "projected_bytes",
                    bound.projected_bytes,
                    ceiling.projected_bytes,
                ),
                ("packed_tokens", bound.packed_tokens, ceiling.packed_tokens),
                ("wall_millis", bound.wall_millis, ceiling.wall_millis),
            ] {
                assert!(
                    claimed <= allowed,
                    "{branches} branches claim {claimed} {name}, ceiling is {allowed}"
                );
            }
        }
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
        fn read_body(
            &self,
            _key: &replica::body::BodyKey,
        ) -> Result<Option<runtime::world::BodyBytes>, runtime::world::BodyReadFailure> {
            Ok(None)
        }
        fn read_collaborative_body(
            &self,
            _key: &replica::body::BodyKey,
        ) -> Result<Option<runtime::world::CollaborativeBody>, runtime::world::BodyReadFailure>
        {
            Ok(None)
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
        ) -> Result<Option<fabric::Anchor>, runtime::world::BodyReadFailure> {
            Ok(None)
        }
        fn resolve_anchor(
            &self,
            _key: &replica::body::BodyKey,
            anchor: &fabric::Anchor,
        ) -> Result<fabric::AnchorResolution, runtime::world::BodyReadFailure> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .by_offset
                .get(&anchor.offset)
                .copied()
                .unwrap_or(fabric::AnchorResolution::Drifted))
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
        resolve_comment_anchor(&ctx, "iss_x", issue, &comment(at)).unwrap()
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
