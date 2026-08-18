//! Transitional v3/v4 projection and v4 write helpers.
//!
//! The durable rule is simple: a v4 record, when present, owns the fact it
//! names. Legacy Catalog state fills only facts which have not been
//! materialized yet. This lets migration advance in bounded transactions while
//! every publication remains readable. New writes use the same helpers as the
//! migrator, so human and agent access paths cannot create different shapes.

// Rank/topology maintenance below operates on bounded, validated windows and
// uses direct index arithmetic to keep each transaction within its declared
// fixed work envelope.
#![allow(clippy::arithmetic_side_effects, clippy::indexing_slicing)]

use std::collections::{BTreeMap, BTreeSet};

use replica::body::{BodyKey, Op};
use runtime::world::{BodyDeclaration, Context, DeniedCause, Rejection};

use crate::{
    contract,
    ids::{DocId, ProjectId},
    v4::{self, CanonicalRecord as _, PhysicalSchema},
    views::{
        CatalogState, Cycle, DerivedAliases, Initiative, IssueState, LabelMeta, Milestone,
        ProjectMeta, ProjectUpdate, Team, TriageItem,
    },
};

/// Operations and declarations for one collection of v4 facts. `absorb` in
/// the World preserves their transaction boundary with any Issue operations.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Batch {
    pub operations: Vec<(BodyKey, Op)>,
    pub bodies: Vec<BodyKey>,
    pub declarations: Vec<BodyDeclaration>,
    pub content_refs: BTreeMap<BodyKey, Vec<replica::content::ContentRef>>,
}

impl Batch {
    pub fn absorb(&mut self, other: Batch) {
        let already_declared: BTreeSet<BodyKey> = self
            .declarations
            .iter()
            .map(|declaration| declaration.key.clone())
            .collect();
        for declaration in other.declarations {
            if !already_declared.contains(&declaration.key)
                && !self
                    .declarations
                    .iter()
                    .any(|existing| existing.key == declaration.key)
            {
                self.declarations.push(declaration);
            }
        }
        for body in other.bodies {
            if !self.bodies.contains(&body) {
                self.bodies.push(body);
            }
        }
        for (body, operation) in other.operations {
            let duplicate_creation = already_declared.contains(&body)
                && (matches!(operation, Op::Create)
                    || matches!(
                        &operation,
                        Op::RegisterSet { path, .. } if path == v4::roots::IDENTITY
                    ));
            if !duplicate_creation {
                self.operations.push((body, operation));
            }
        }
        self.content_refs.extend(other.content_refs);
    }

    fn estimated_bytes(&self) -> usize {
        self.operations.iter().fold(0usize, |total, (body, op)| {
            total
                .saturating_add(body.body.render().len())
                .saturating_add(serde_json::to_vec(op).map_or(usize::MAX, |bytes| bytes.len()))
                .saturating_add(32)
        })
    }

    pub fn operation(&mut self, key: &BodyKey, op: Op) {
        if !self.bodies.contains(key) {
            self.bodies.push(key.clone());
        }
        self.operations.push((key.clone(), op));
    }

    pub fn create(&mut self, schema: PhysicalSchema, key: &BodyKey) {
        if !self
            .declarations
            .iter()
            .any(|declaration| declaration.key == *key)
        {
            self.declarations.push(BodyDeclaration {
                key: key.clone(),
                schema: schema.declaration().id,
                schema_version: v4::SCHEMA_VERSION,
            });
            // Atomic and ImmutableAtomic Bodies are created by their first
            // ReplaceAtomic under the declaration. `Op::Create` is a
            // collaborative operation and Runtime correctly rejects it for
            // those models.
            if !schema.atomic() {
                self.operation(key, Op::Create);
            }
        }
    }

    pub fn ensure_body(
        &mut self,
        ctx: &Context<'_>,
        schema: PhysicalSchema,
        key: &BodyKey,
        identity: Vec<u8>,
    ) {
        if ctx.body_version(key).is_none()
            && !self
                .declarations
                .iter()
                .any(|declaration| declaration.key == *key)
        {
            self.create(schema, key);
            self.operation(
                key,
                Op::RegisterSet {
                    path: v4::roots::IDENTITY.into(),
                    value: identity,
                },
            );
        }
    }

    fn immutable_record(
        &mut self,
        ctx: &Context<'_>,
        schema: PhysicalSchema,
        coordinate_key: &BodyKey,
        identity: v4::RecordBodyIdentityRecord,
        record: Vec<u8>,
    ) -> Result<(), Rejection> {
        if !schema.immutable() {
            return Err(Rejection::ContractViolation);
        }
        let bytes = canonical(&v4::ImmutableRecordEnvelope { identity, record })?;
        let key = v4::immutable_record_key(schema, &bytes);
        if coordinate_key.world != key.world {
            return Err(Rejection::ContractViolation);
        }
        if let Some(existing) = ctx.read_body(&key)? {
            return if existing.as_ref() == bytes.as_slice() {
                Ok(())
            } else {
                Err(Rejection::Conflict)
            };
        }
        if ctx.body_version(&key).is_some() {
            return Err(Rejection::StateCorrupt);
        }
        self.create(schema, &key);
        self.operation(&key, Op::ReplaceAtomic { value: bytes });
        Ok(())
    }

    fn atomic_value(
        &mut self,
        ctx: &Context<'_>,
        schema: PhysicalSchema,
        key: &BodyKey,
        value: Vec<u8>,
    ) -> Result<(), Rejection> {
        if !schema.atomic() || schema.immutable() {
            return Err(Rejection::ContractViolation);
        }
        if ctx.body_version(key).is_none()
            && !self
                .declarations
                .iter()
                .any(|declaration| declaration.key == *key)
        {
            self.create(schema, key);
        }
        self.operation(key, Op::ReplaceAtomic { value });
        Ok(())
    }
}

fn schema_bodies(ctx: &Context<'_>, schema: PhysicalSchema) -> Vec<BodyKey> {
    let declaration = schema.declaration();
    let mut bodies = ctx.bodies_with_schema(&contract::world_id(), &declaration.id);
    bodies.sort();
    bodies
}

/// Resolve one semantic record coordinate through the shared, publication-
/// pinned Corpus. Immutable Body addresses are content-derived, so semantic
/// ids are indexed facts rather than alternate physical addresses.
fn exact_record_source_matching(
    ctx: &Context<'_>,
    field: &str,
    value: &str,
    predicates: &[(&str, &str)],
) -> Result<Option<BodyKey>, Rejection> {
    use runtime::find as find_api;
    let kind_source = field == crate::find::field::SOURCE_ID
        && matches!(predicates, [(predicate, _)] if *predicate == crate::find::field::KIND);
    let (seek_field, seek_value) = if kind_source {
        (
            crate::find::field::KIND_SOURCE,
            find_api::Atom::Bytes(crate::find::composite_key([predicates[0].1, value])),
        )
    } else {
        (field, find_api::Atom::Text(value.into()))
    };
    let bound = find_api::Bound {
        decoded_bodies: 2,
        postings_read: 32,
        edges_visited: 1,
        nodes_visited: 8,
        paths_retained: 2,
        candidates_per_branch: 2,
        score_evaluations: 2,
        projected_bytes: 4 * 1_024,
        packed_tokens: 32,
        wall_millis: 500,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
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
                    op: find_api::Op::Keep(find_api::Keep {
                        predicates: predicates
                            .iter()
                            .map(|(field, value)| find_api::Predicate {
                                field: crate::find::field_ref(field),
                                test: find_api::Test::Equal,
                                value: find_api::Atom::Text((*value).into()),
                            })
                            .collect(),
                    }),
                    bound,
                },
            ],
            output: keep,
            bound,
            page_size: 2,
            cursor: None,
        })
        .map_err(|_| Rejection::StateCorrupt)?;
    if answer.next_cursor().is_some() || answer.rows().len() > 1 {
        // Two different immutable payloads claiming one semantic coordinate
        // are a product-level conflict, never an arbitrary corpus winner.
        return Err(Rejection::Conflict);
    }
    Ok(answer.rows().first().map(|row| row.source.clone()))
}

fn exact_record_source(
    ctx: &Context<'_>,
    field: &str,
    value: &str,
    kind: &str,
) -> Result<Option<BodyKey>, Rejection> {
    exact_record_source_matching(ctx, field, value, &[(crate::find::field::KIND, kind)])
}

fn register(view: &fabric::CollaborativeView, path: &str) -> String {
    view.registers
        .get(path)
        .map(|value| String::from_utf8_lossy(value).into_owned())
        .unwrap_or_default()
}

fn optional_u64(view: &fabric::CollaborativeView, path: &str) -> Option<u64> {
    register(view, path).parse().ok()
}

fn boolean(view: &fabric::CollaborativeView, path: &str) -> bool {
    matches!(register(view, path).as_str(), "1" | "true")
}

fn read_view(
    ctx: &Context<'_>,
    key: &BodyKey,
) -> Result<runtime::world::CollaborativeBody, Rejection> {
    ctx.read_collaborative(key)?.ok_or(Rejection::StateCorrupt)
}

fn read_immutable(
    ctx: &Context<'_>,
    key: &BodyKey,
) -> Result<v4::ImmutableRecordEnvelope, Rejection> {
    let bytes = ctx.read_body(key)?.ok_or(Rejection::StateCorrupt)?;
    v4::ImmutableRecordEnvelope::decode_canonical(&bytes).map_err(|_| Rejection::StateCorrupt)
}

fn require_identity(view: &fabric::CollaborativeView, expected: &str) -> Result<(), Rejection> {
    if register(view, v4::roots::IDENTITY) == expected {
        Ok(())
    } else {
        Err(Rejection::StateCorrupt)
    }
}

/// V4 coordinates read from the small identity/placement/meta Bodies. The
/// anchored Issue content Body is never opened for a board or alias lookup.
#[derive(Debug, Clone)]
pub(crate) struct IssueCoordinate {
    pub identity: v4::IssueIdentityRecord,
    pub placement: v4::BoardPlacement,
    /// Absent only on an incompletely migrated Body. New and migrated Issues
    /// always carry the explicit value so a legacy Catalog tombstone cannot
    /// reappear after restoration.
    pub tombstone: Option<bool>,
}

pub(crate) fn issue_coordinates(
    ctx: &Context<'_>,
) -> Result<BTreeMap<String, IssueCoordinate>, Rejection> {
    let mut out = BTreeMap::new();
    for key in schema_bodies(ctx, PhysicalSchema::IssueIdentity) {
        let envelope = read_immutable(ctx, &key)?;
        if v4::immutable_record_key(
            PhysicalSchema::IssueIdentity,
            &ctx.read_body(&key)?.ok_or(Rejection::StateCorrupt)?,
        ) != key
        {
            return Err(Rejection::StateCorrupt);
        }
        let identity = v4::IssueIdentityRecord::decode_canonical(&envelope.record)
            .map_err(|_| Rejection::StateCorrupt)?;
        DocId::parse(&identity.issue).ok_or(Rejection::StateCorrupt)?;
        if envelope.identity.owner != identity.issue || envelope.identity.record != "identity" {
            return Err(Rejection::StateCorrupt);
        }
        let Some(coordinate) = issue_coordinate_for(ctx, &identity.issue)? else {
            return Err(Rejection::StateCorrupt);
        };
        if out.insert(identity.issue.clone(), coordinate).is_some() {
            return Err(Rejection::StateCorrupt);
        }
    }
    Ok(out)
}

pub(crate) fn issue_coordinate_for(
    ctx: &Context<'_>,
    doc: &str,
) -> Result<Option<IssueCoordinate>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let identity_key =
        exact_record_source(ctx, crate::find::field::SOURCE_ID, doc, "issue_identity")?;
    let placement_key = v4::issue_placement_key(&issue);
    let identity_present = identity_key.is_some();
    let placement_present = ctx.body_version(&placement_key).is_some();
    let transition_heads = issue_transition_heads(ctx, doc)?;
    if !identity_present && !placement_present && transition_heads.is_empty() {
        return Ok(None);
    }
    if !identity_present {
        return Err(Rejection::StateCorrupt);
    }
    let identity_key = identity_key.ok_or(Rejection::StateCorrupt)?;
    let identity_bytes = ctx
        .read_body(&identity_key)?
        .ok_or(Rejection::StateCorrupt)?;
    if v4::immutable_record_key(PhysicalSchema::IssueIdentity, &identity_bytes) != identity_key {
        return Err(Rejection::StateCorrupt);
    }
    let identity_envelope = v4::ImmutableRecordEnvelope::decode_canonical(&identity_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let identity = v4::IssueIdentityRecord::decode_canonical(&identity_envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    let placement = match transition_heads.as_slice() {
        [] => {
            if !placement_present {
                return Err(Rejection::StateCorrupt);
            }
            let placement_bytes = ctx
                .read_body(&placement_key)?
                .ok_or(Rejection::StateCorrupt)?;
            let placement_record = v4::IssuePlacementRecord::decode_canonical(&placement_bytes)
                .map_err(|_| Rejection::StateCorrupt)?;
            if placement_record.issue != doc {
                return Err(Rejection::StateCorrupt);
            }
            placement_record.placement
        }
        [(transition, head)] => effective_issue_placement(ctx, doc, transition, &head.placement)?,
        _ => return Err(Rejection::Conflict),
    };
    if identity.issue != doc
        || identity_envelope.identity.owner != doc
        || identity_envelope.identity.record != "identity"
    {
        return Err(Rejection::StateCorrupt);
    }
    let meta = issue_meta_for(ctx, doc)?;
    Ok(Some(IssueCoordinate {
        identity,
        placement,
        tombstone: meta.map(|record| record.tombstone),
    }))
}

fn issue_meta_for(ctx: &Context<'_>, doc: &str) -> Result<Option<v4::IssueMetaRecord>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let key = v4::issue_meta_key(&issue);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let view = read_view(ctx, &key)?;
    require_identity(&view, doc)?;
    let created_by = register(&view, v4::roots::CREATED_BY);
    let record = v4::IssueMetaRecord {
        issue: doc.into(),
        title: register(&view, v4::roots::TITLE),
        priority: register(&view, v4::roots::PRIORITY),
        created_by: (!created_by.is_empty()).then_some(created_by),
        created_at: optional_u64(&view, v4::roots::CREATED_AT).unwrap_or_default(),
        due_at: optional_u64(&view, v4::roots::DUE_AT),
        estimate: optional_u64(&view, v4::roots::ESTIMATE).and_then(|value| value.try_into().ok()),
        tombstone: boolean(&view, v4::roots::TOMBSTONE),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    Ok(Some(record))
}

pub(crate) fn apply_issue_meta(
    ctx: &Context<'_>,
    issue: &mut IssueState,
    doc: &str,
) -> Result<(), Rejection> {
    let record = issue_meta_for(ctx, doc)?.ok_or(Rejection::StateCorrupt)?;
    issue.title = record.title;
    issue.priority =
        crate::dto::Priority::parse(&record.priority).ok_or(Rejection::StateCorrupt)?;
    issue.created_by = record
        .created_by
        .and_then(|actor| crate::ids::ActorId::parse(&actor));
    issue.created_at = record.created_at;
    issue.duedate = record.due_at;
    issue.estimate = record.estimate;
    Ok(())
}

pub(crate) fn apply_issue_coordinate(issue: &mut IssueState, coordinate: &IssueCoordinate) {
    issue.project.clone_from(&coordinate.placement.project);
    issue
        .status
        .clone_from(&coordinate.placement.workflow_state);
}

pub(crate) fn apply_issue_catalog(
    catalog: &mut CatalogState,
    coordinates: &BTreeMap<String, IssueCoordinate>,
) {
    // V4 placement is the authoritative board index. Preserve legacy entries
    // only for not-yet-migrated Issues, then splice every coordinate into its
    // project lane in rank/id order. No Catalog list mutation is needed on the
    // action path.
    for board in catalog.boards.values_mut() {
        board.retain(|(_, doc)| !coordinates.contains_key(doc));
    }
    let mut lanes = BTreeMap::<String, Vec<(String, String)>>::new();
    for (doc, coordinate) in coordinates {
        lanes
            .entry(coordinate.placement.project.clone())
            .or_default()
            .push((coordinate.placement.position.clone(), doc.clone()));
        match coordinate.tombstone {
            Some(true) => {
                catalog.tombstones.insert(doc.clone());
            }
            Some(false) => {
                catalog.tombstones.remove(doc);
            }
            None => {}
        }
    }
    for (project, mut lane) in lanes {
        lane.sort();
        let board = catalog.boards.entry(project).or_default();
        board.extend(
            lane.into_iter()
                .map(|(position, doc)| (format!("v4:{position}:{doc}"), doc)),
        );
    }
}

/// Add stable v4 aliases after deriving the legacy compatibility table. A v4
/// coordinate always wins for its Issue, including reverse lookup.
pub(crate) fn apply_aliases(
    catalog: &CatalogState,
    coordinates: &BTreeMap<String, IssueCoordinate>,
    aliases: &mut DerivedAliases,
) -> Result<(), Rejection> {
    for (doc, coordinate) in coordinates {
        let key = catalog
            .projects
            .get(&coordinate.placement.project)
            .ok_or(Rejection::StateCorrupt)?
            .key
            .as_str();
        let rendered = coordinate
            .identity
            .alias
            .render(key)
            .map_err(|_| Rejection::StateCorrupt)?;
        if let Some(old) = aliases.by_doc.insert(doc.clone(), rendered.clone()) {
            aliases.by_alias.remove(&old.to_ascii_lowercase());
        }
        let folded = rendered.to_ascii_lowercase();
        if aliases.by_alias.insert(folded, doc.clone()).is_some() {
            // The disambiguator makes this cryptographically implausible; a
            // collision is corruption, never an invitation to rename either.
            return Err(Rejection::StateCorrupt);
        }
    }
    Ok(())
}

/// Overlay every v4-owned catalog fact. This deliberately does not enumerate
/// Issues; they are discovered from Issue schema bindings by
/// [`issue_coordinates`], keeping the Issue Body the unit enrichment builds on.
pub(crate) fn apply_catalog(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
) -> Result<(), Rejection> {
    apply_directory(ctx, catalog)?;
    apply_labels(ctx, catalog)?;
    apply_governance(ctx, catalog)?;
    apply_project_meta(ctx, catalog)?;
    apply_workflows(ctx, catalog)?;
    apply_schedule(ctx, catalog)?;
    apply_hierarchy(ctx, catalog)?;
    apply_updates(ctx, catalog)?;
    apply_initiatives(ctx, catalog)?;
    apply_teams(ctx, catalog)?;
    apply_entity_relations(ctx, catalog)?;
    apply_triage(ctx, catalog)
}

fn apply_governance(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    let mut custom = BTreeMap::<String, Vec<crate::views::StoredRoleRevision>>::new();
    for key in schema_bodies(ctx, PhysicalSchema::GovernanceRevision) {
        let role = apply_governance_revision(ctx, catalog, &key)?;
        if let Some(revisions) = catalog.role_revisions.remove(&role) {
            custom.entry(role).or_default().extend(revisions);
        }
    }
    for revisions in custom.values_mut() {
        revisions.sort_by(|left, right| left.revision_id.cmp(&right.revision_id));
    }
    for (role, revisions) in custom {
        catalog.role_revisions.insert(role, revisions);
    }
    Ok(())
}

pub(crate) fn apply_governance_revision(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    key: &BodyKey,
) -> Result<String, Rejection> {
    let envelope = read_immutable(ctx, key)?;
    let identity = envelope.identity;
    let record = v4::GovernanceRevisionRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    if record.role != identity.owner || record.revision.revision_id != identity.record {
        return Err(Rejection::StateCorrupt);
    }
    let role = record.role.clone();
    if crate::roles::BUILT_IN_ROLE_IDS.contains(&record.role.as_str()) {
        catalog.roles.insert(record.role, record.revision);
    } else {
        let revisions = catalog.role_revisions.entry(record.role).or_default();
        if !revisions
            .iter()
            .any(|current| current.revision_id == record.revision.revision_id)
        {
            revisions.push(record.revision);
            revisions.sort_by(|left, right| left.revision_id.cmp(&right.revision_id));
        }
    }
    Ok(role)
}

fn apply_directory(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    let space = &ctx.principal().space;
    let key = v4::space_directory_key(space);
    if ctx.body_version(&key).is_none() {
        return Ok(());
    }
    let view = read_view(ctx, &key)?;
    require_identity(&view, space.as_str())?;
    let record = v4::SpaceDirectoryRecord {
        name: register(&view, v4::roots::NAME),
        description: read_content(ctx, &v4::space_content_key(space), space.as_str())?,
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    catalog.name = record.name;
    catalog.description = record.description;
    Ok(())
}

/// Hydrate only the bounded space metadata/content singleton. Normal actions
/// use this instead of assembling the tracker-wide catalog.
pub(crate) fn apply_space(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    apply_directory(ctx, catalog)
}

fn read_content(ctx: &Context<'_>, key: &BodyKey, identity: &str) -> Result<String, Rejection> {
    if ctx.body_version(key).is_none() {
        return Ok(String::new());
    }
    let view = read_view(ctx, key)?;
    require_identity(&view, identity)?;
    let description = view
        .texts
        .get(v4::roots::DESCRIPTION)
        .cloned()
        .unwrap_or_default();
    if !contract::valid_text(&description) {
        return Err(Rejection::StateCorrupt);
    }
    Ok(description)
}

fn apply_labels(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::Label) {
        apply_label_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_label_key(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    key: &BodyKey,
) -> Result<(), Rejection> {
    let view = read_view(ctx, key)?;
    let id = register(&view, v4::roots::IDENTITY);
    let label = crate::ids::LabelId::parse(&id).ok_or(Rejection::StateCorrupt)?;
    if v4::label_key(&label) != *key {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record =
        v4::LabelDirectoryEntry::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?;
    if record.label != id {
        return Err(Rejection::StateCorrupt);
    }
    if record.tombstone {
        catalog.labels.remove(&id);
    } else {
        catalog.labels.insert(
            id,
            LabelMeta {
                name: record.name,
                color: record.color,
            },
        );
    }
    Ok(())
}

pub(crate) fn apply_label(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    label: &str,
) -> Result<(), Rejection> {
    let label = crate::ids::LabelId::parse(label).ok_or(Rejection::InvalidRequest)?;
    let key = v4::label_key(&label);
    if ctx.body_version(&key).is_some() {
        apply_label_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_project_meta(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::ProjectMeta) {
        apply_project_meta_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_project_meta_key(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    key: &BodyKey,
) -> Result<(), Rejection> {
    let view = read_view(ctx, key)?;
    let project = register(&view, v4::roots::IDENTITY);
    let project_id = ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
    if v4::project_meta_key(&project_id) != *key {
        return Err(Rejection::StateCorrupt);
    }
    let record = v4::ProjectMetaRecord {
        project: project.clone(),
        name: register(&view, v4::roots::NAME),
        key: register(&view, v4::roots::KEY),
        color: register(&view, v4::roots::COLOR),
        description: read_content(ctx, &v4::project_content_key(&project_id), &project)?,
        lead: register(&view, v4::roots::LEAD),
        start_date: optional_u64(&view, v4::roots::START_DATE),
        target_date: optional_u64(&view, v4::roots::TARGET_DATE),
        archived: boolean(&view, v4::roots::ARCHIVED),
        team: register(&view, v4::roots::TEAM),
        tombstone: boolean(&view, v4::roots::TOMBSTONE),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    if record.tombstone {
        catalog.projects.remove(&project);
        return Ok(());
    }
    catalog.projects.insert(
        project,
        ProjectMeta {
            name: record.name,
            key: record.key,
            color: record.color,
            description: record.description,
            lead: record.lead,
            start_date: record.start_date,
            target_date: record.target_date,
            archived: record.archived,
            team: record.team,
        },
    );
    Ok(())
}

pub(crate) fn apply_project(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    project: &str,
) -> Result<(), Rejection> {
    let project = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let key = v4::project_meta_key(&project);
    if ctx.body_version(&key).is_some() {
        apply_project_meta_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_workflows(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::WorkflowRevision) {
        apply_workflow_revision(ctx, catalog, &key)?;
    }
    for records in catalog.workflow_revisions.values_mut() {
        records.sort_by(|left, right| left.revision_id.cmp(&right.revision_id));
        records.dedup_by(|left, right| left.revision_id == right.revision_id);
    }
    Ok(())
}

/// Apply one exact immutable workflow revision selected by the shared corpus.
/// Normal action validation calls this with `ResultRow::source`; only the
/// offline migration/admin assembler enumerates the whole schema.
pub(crate) fn apply_workflow_revision(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    key: &BodyKey,
) -> Result<(), Rejection> {
    let envelope = read_immutable(ctx, key)?;
    let identity = envelope.identity;
    ProjectId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
    let record = v4::ProjectWorkflowRevisionRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    if record.project != identity.owner || record.revision.revision_id != identity.record {
        return Err(Rejection::StateCorrupt);
    }
    let revisions = catalog
        .workflow_revisions
        .entry(record.project)
        .or_default();
    if !revisions
        .iter()
        .any(|current| current.revision_id == record.revision.revision_id)
    {
        revisions.push(record.revision);
        revisions.sort_by(|left, right| left.revision_id.cmp(&right.revision_id));
    }
    Ok(())
}

fn apply_schedule(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::ProjectSchedule) {
        apply_schedule_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_schedule_key(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    key: &BodyKey,
) -> Result<(), Rejection> {
    let view = read_view(ctx, key)?;
    let identity = view
        .registers
        .get(v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
    let project = identity.owner;
    let project_id = ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
    if v4::project_schedule_key(&project_id, &identity.record) != *key {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    match v4::ScheduleRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)? {
        v4::ScheduleRecord::Milestone {
            milestone,
            project: owner,
            name,
            description,
            target_date,
            position,
            tombstone,
        } => {
            if owner != project || identity.record != milestone {
                return Err(Rejection::StateCorrupt);
            }
            catalog
                .milestones
                .entry(project.clone())
                .or_default()
                .insert(
                    milestone.clone(),
                    Milestone {
                        id: milestone,
                        project_id: project,
                        name,
                        description,
                        target_date,
                        rank: position,
                        tombstone,
                    },
                );
        }
        v4::ScheduleRecord::Cycle {
            cycle,
            project: owner,
            name,
            start,
            end,
            tombstone,
        } => {
            if owner != project || identity.record != cycle {
                return Err(Rejection::StateCorrupt);
            }
            catalog.cycles.entry(project.clone()).or_default().insert(
                cycle.clone(),
                Cycle {
                    id: cycle,
                    project_id: project,
                    name,
                    start,
                    end,
                    tombstone,
                },
            );
        }
    }
    Ok(())
}

pub(crate) fn apply_schedule_record(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    project: &str,
    record: &str,
) -> Result<(), Rejection> {
    let project = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let key = v4::project_schedule_key(&project, record);
    if ctx.body_version(&key).is_some() {
        apply_schedule_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_hierarchy(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::ProjectHierarchy) {
        let raw = ctx.read_body(&key)?.ok_or(Rejection::StateCorrupt)?;
        match v4::TopologyRecord::decode_canonical(&raw).map_err(|_| Rejection::StateCorrupt)? {
            v4::TopologyRecord::Parent(record) => {
                let project = ProjectId::parse(&record.project).ok_or(Rejection::StateCorrupt)?;
                if v4::project_hierarchy_key(&project, &record.child) != key {
                    return Err(Rejection::StateCorrupt);
                }
                match record.parent {
                    Some(parent) => {
                        catalog.parents.insert(record.child, parent);
                    }
                    None => {
                        catalog.parents.remove(&record.child);
                    }
                }
            }
            v4::TopologyRecord::Link(record) => {
                let project = ProjectId::parse(&record.project).ok_or(Rejection::StateCorrupt)?;
                let identity = data_encoding::HEXLOWER.encode(&record.relation_identity());
                if v4::project_hierarchy_key(&project, &identity) != key {
                    return Err(Rejection::StateCorrupt);
                }
                let edge = (record.from, record.kind, record.to);
                if record.present {
                    catalog.edges.insert(edge);
                } else {
                    catalog.edges.remove(&edge);
                }
            }
        }
    }
    Ok(())
}

fn apply_updates(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    let mut updated = BTreeSet::new();
    for key in schema_bodies(ctx, PhysicalSchema::ProjectUpdates) {
        let envelope = read_immutable(ctx, &key)?;
        let identity = envelope.identity;
        let project = identity.owner;
        ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
        if updated.insert(project.clone()) {
            catalog.project_updates.insert(project.clone(), Vec::new());
        }
        {
            let record = v4::ProjectUpdateRecord::decode_canonical(&envelope.record)
                .map_err(|_| Rejection::StateCorrupt)?;
            if record.project != project || record.update != identity.record {
                return Err(Rejection::StateCorrupt);
            }
            catalog
                .project_updates
                .entry(project.clone())
                .or_default()
                .push(ProjectUpdate {
                    id: record.update,
                    project_id: project.clone(),
                    author: record.author,
                    ts: record.timestamp,
                    body: record.body,
                    health: record.health,
                });
        }
    }
    for project in updated {
        if let Some(records) = catalog.project_updates.get_mut(&project) {
            records
                .sort_by(|left, right| left.ts.cmp(&right.ts).then_with(|| left.id.cmp(&right.id)));
            records.dedup_by(|left, right| left.id == right.id);
        }
    }
    Ok(())
}

fn apply_initiatives(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::Initiative) {
        apply_initiative_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_initiative_key(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    key: &BodyKey,
) -> Result<(), Rejection> {
    let view = read_view(ctx, key)?;
    let id = register(&view, v4::roots::IDENTITY);
    let parsed = crate::ids::InitiativeId::parse(&id).ok_or(Rejection::StateCorrupt)?;
    if v4::initiative_key(&parsed) != *key {
        return Err(Rejection::StateCorrupt);
    }
    let record = v4::InitiativeRecord {
        initiative: id.clone(),
        name: register(&view, v4::roots::NAME),
        description: read_content(ctx, &v4::initiative_content_key(&parsed), &id)?,
        owner: register(&view, v4::roots::OWNER),
        health: register(&view, v4::roots::HEALTH),
        target_date: optional_u64(&view, v4::roots::TARGET_DATE),
        tombstone: boolean(&view, v4::roots::TOMBSTONE),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    catalog.initiatives.insert(
        id.clone(),
        Initiative {
            id,
            name: record.name,
            description: record.description,
            owner: record.owner,
            health: record.health,
            target_date: record.target_date,
            projects: Vec::new(),
            tombstone: record.tombstone,
        },
    );
    Ok(())
}

pub(crate) fn apply_initiative(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    initiative: &str,
) -> Result<(), Rejection> {
    let initiative =
        crate::ids::InitiativeId::parse(initiative).ok_or(Rejection::InvalidRequest)?;
    let key = v4::initiative_key(&initiative);
    if ctx.body_version(&key).is_some() {
        apply_initiative_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_teams(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::Team) {
        apply_team_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_team_key(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    key: &BodyKey,
) -> Result<(), Rejection> {
    let view = read_view(ctx, key)?;
    let id = register(&view, v4::roots::IDENTITY);
    let parsed = crate::ids::TeamId::parse(&id).ok_or(Rejection::StateCorrupt)?;
    if v4::team_key(&parsed) != *key {
        return Err(Rejection::StateCorrupt);
    }
    let record = v4::TeamRecord {
        team: id.clone(),
        name: register(&view, v4::roots::NAME),
        key: register(&view, v4::roots::KEY),
        icon: register(&view, v4::roots::ICON),
        lead: register(&view, v4::roots::LEAD),
        tombstone: boolean(&view, v4::roots::TOMBSTONE),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    catalog.teams.insert(
        id.clone(),
        Team {
            id,
            name: record.name,
            key: record.key,
            icon: record.icon,
            lead: record.lead,
            members: Vec::new(),
            tombstone: record.tombstone,
        },
    );
    Ok(())
}

pub(crate) fn apply_team(
    ctx: &Context<'_>,
    catalog: &mut CatalogState,
    team: &str,
) -> Result<(), Rejection> {
    let team = crate::ids::TeamId::parse(team).ok_or(Rejection::InvalidRequest)?;
    let key = v4::team_key(&team);
    if ctx.body_version(&key).is_some() {
        apply_team_key(ctx, catalog, &key)?;
    }
    Ok(())
}

fn apply_entity_relations(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::EntityRelation) {
        let raw = ctx.read_body(&key)?.ok_or(Rejection::StateCorrupt)?;
        let record = v4::EntityRelationRecord::decode_canonical(&raw)
            .map_err(|_| Rejection::StateCorrupt)?;
        let identity = record.identity();
        if record.identity() != identity || v4::entity_relation_key(&record.owner, &identity) != key
        {
            return Err(Rejection::StateCorrupt);
        }
        match record.kind.as_str() {
            "initiative_project" => {
                let Some(initiative) = catalog.initiatives.get_mut(&record.owner) else {
                    return Err(Rejection::StateCorrupt);
                };
                initiative
                    .projects
                    .retain(|project| project != &record.target);
                if record.present {
                    initiative.projects.push(record.target);
                    initiative.projects.sort();
                    initiative.projects.dedup();
                }
            }
            "team_member" => {
                let Some(team) = catalog.teams.get_mut(&record.owner) else {
                    return Err(Rejection::StateCorrupt);
                };
                team.members.retain(|actor| actor != &record.target);
                if record.present {
                    team.members.push(record.target);
                    team.members.sort();
                    team.members.dedup();
                }
            }
            _ => return Err(Rejection::StateCorrupt),
        }
    }
    Ok(())
}

fn apply_triage(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    let mut submissions = BTreeMap::<String, TriageItem>::new();
    let mut decisions = BTreeMap::<String, Vec<v4::TriageDecisionRecord>>::new();
    let mut resolutions = BTreeMap::<String, String>::new();
    for key in schema_bodies(ctx, PhysicalSchema::SpaceTriage) {
        let envelope = read_immutable(ctx, &key)?;
        let identity = envelope.identity;
        let space = identity.owner;
        let space_id = crate::ids::SpaceId::parse(&space).ok_or(Rejection::StateCorrupt)?;
        if space_id != ctx.principal().space {
            return Err(Rejection::StateCorrupt);
        }
        match v4::TriageRecord::decode_canonical(&envelope.record)
            .map_err(|_| Rejection::StateCorrupt)?
        {
            v4::TriageRecord::Submission(record) => {
                if record.triage != identity.record {
                    return Err(Rejection::StateCorrupt);
                }
                submissions.insert(
                    record.triage.clone(),
                    TriageItem {
                        id: record.triage,
                        title: record.title,
                        body: record.body,
                        source: record.source,
                        submitted_by: record.submitted_by,
                        ts: record.timestamp,
                        ..TriageItem::default()
                    },
                );
            }
            v4::TriageRecord::Decision(record) => {
                if record.decision != identity.record {
                    return Err(Rejection::StateCorrupt);
                }
                decisions
                    .entry(record.triage.clone())
                    .or_default()
                    .push(record);
            }
            v4::TriageRecord::Resolution(record) => {
                if record.identity() != identity.record {
                    return Err(Rejection::StateCorrupt);
                }
                resolutions.insert(record.triage, record.decision);
            }
        }
    }
    if submissions.is_empty() {
        return Ok(());
    }
    for (triage, mut item) in submissions {
        let choices = decisions.remove(&triage).unwrap_or_default();
        let selected = resolutions
            .get(&triage)
            .and_then(|decision| choices.iter().find(|choice| &choice.decision == decision))
            .or_else(|| (choices.len() == 1).then(|| choices.first()).flatten());
        if let Some(decision) = selected {
            item.outcome = match decision.outcome {
                v4::TriageOutcome::Accepted => "accepted",
                v4::TriageOutcome::Declined => "declined",
                v4::TriageOutcome::Duplicate => "duplicate",
            }
            .into();
            item.doc = decision.issue.clone().unwrap_or_default();
            item.decided_by.clone_from(&decision.decided_by);
            item.decided_ts = decision.timestamp;
            item.note.clone_from(&decision.note);
        }
        catalog.triage.insert(triage, item);
    }
    Ok(())
}

pub(crate) fn set_register(
    batch: &mut Batch,
    key: &BodyKey,
    path: &str,
    value: impl Into<Vec<u8>>,
) {
    batch.operation(
        key,
        Op::RegisterSet {
            path: path.into(),
            value: value.into(),
        },
    );
}

pub(crate) fn clear_register(batch: &mut Batch, key: &BodyKey, path: &str) {
    batch.operation(key, Op::RegisterClear { path: path.into() });
}

pub(crate) fn set_map(
    batch: &mut Batch,
    key: &BodyKey,
    path: &str,
    map_key: impl Into<String>,
    value: Vec<u8>,
) {
    batch.operation(
        key,
        Op::MapSet {
            path: path.into(),
            key: map_key.into(),
            value,
        },
    );
}

pub(crate) fn canonical<T: v4::CanonicalRecord>(record: &T) -> Result<Vec<u8>, Rejection> {
    record
        .encode_canonical()
        .map_err(|_| Rejection::StateCorrupt)
}

fn crockford_128(mut value: u128) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";
    let mut out = [b'0'; 26];
    for index in (0..26).rev() {
        let digit = usize::try_from(value & 0x1f).unwrap_or(0);
        if let (Some(target), Some(symbol)) = (out.get_mut(index), ALPHABET.get(digit)) {
            *target = *symbol;
        }
        value >>= 5;
    }
    out.into_iter().map(char::from).collect()
}

fn request_record_id(
    ctx: &Context<'_>,
    domain: &str,
    issue: &DocId,
    extra: &[u8],
) -> Result<String, Rejection> {
    let request = ctx.request_id().ok_or(Rejection::InvalidRequest)?;
    let mut material = Vec::with_capacity(16 + issue.as_str().len() + extra.len());
    material.extend_from_slice(&request.as_bytes());
    material.extend_from_slice(issue.as_str().as_bytes());
    material.extend_from_slice(extra);
    Ok(data_encoding::HEXLOWER.encode(&blake3::derive_key(domain, &material)))
}

/// Stage one independently addressable comment. The Issue Body remains the
/// anchor source for spans, but the thread record itself never enlarges or
/// invalidates that core Body.
pub(crate) fn write_comment(
    ctx: &Context<'_>,
    doc: &str,
    mut comment: contract::StoredComment,
) -> Result<(Batch, String), Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    if comment.id.is_none() {
        let identity = request_record_id(ctx, "lait.issues.comment-request.v1", &issue, &[])?;
        let bytes = data_encoding::HEXLOWER
            .decode(identity.as_bytes())
            .map_err(|_| Rejection::StateCorrupt)?;
        let raw = <[u8; 16]>::try_from(bytes.get(..16).ok_or(Rejection::StateCorrupt)?)
            .map_err(|_| Rejection::StateCorrupt)?;
        comment.id = Some(format!("cmt_{}", crockford_128(u128::from_be_bytes(raw))));
    }
    let id = comment.id.clone().ok_or(Rejection::StateCorrupt)?;
    if !contract::is_comment_id(&id)
        || comment
            .parent
            .as_ref()
            .is_some_and(|parent| !contract::is_comment_id(parent))
    {
        return Err(Rejection::InvalidRequest);
    }
    // Engine-local tree node handles never cross the record Body boundary.
    comment.node = None;
    comment.parent_node = None;
    let key = v4::issue_comment_key(&issue, &id);
    let mut batch = Batch::default();
    batch.immutable_record(
        ctx,
        PhysicalSchema::IssueComment,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: doc.into(),
            record: id.clone(),
        },
        canonical(&v4::DiscussionRecord::Comment(comment))?,
    )?;
    Ok((batch, id))
}

/// Stage one LWW reaction toggle in the Body owned by its exact tuple.
pub(crate) fn write_reaction(
    ctx: &Context<'_>,
    doc: &str,
    comment: &str,
    emoji: &str,
    actor: &str,
    on: bool,
) -> Result<Batch, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let reaction = v4::ReactionRecord {
        issue: doc.into(),
        comment: comment.into(),
        emoji: emoji.into(),
        actor: actor.into(),
        on,
    };
    let record = reaction.identity();
    let key = v4::issue_reaction_key(&issue, &record);
    let mut batch = Batch::default();
    batch.atomic_value(
        ctx,
        PhysicalSchema::IssueReaction,
        &key,
        canonical(&v4::DiscussionRecord::Reaction(reaction))?,
    )?;
    Ok(batch)
}

pub(crate) fn write_issue_relation(
    ctx: &Context<'_>,
    doc: &str,
    project: &str,
    kind: &str,
    target: &str,
    present: bool,
) -> Result<Batch, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let record = v4::IssueRelationRecord {
        issue: doc.into(),
        project: project.into(),
        kind: kind.into(),
        target: target.into(),
        present,
    };
    let identity = record.identity();
    let key = v4::issue_relation_key(&issue, &identity);
    let mut batch = Batch::default();
    batch.atomic_value(
        ctx,
        PhysicalSchema::IssueRelation,
        &key,
        canonical(&record)?,
    )?;
    Ok(batch)
}

/// Read one exact enrichment register without enumerating the Issue's other
/// relations. Singleton kinds ignore `target` in their physical identity; set
/// kinds include it, matching [`v4::IssueRelationRecord::identity`].
pub(crate) fn read_issue_relation(
    ctx: &Context<'_>,
    doc: &str,
    kind: &str,
    target: &str,
) -> Result<Option<v4::IssueRelationRecord>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let probe = v4::IssueRelationRecord {
        issue: doc.into(),
        project: String::new(),
        kind: kind.into(),
        target: target.into(),
        present: false,
    };
    let identity = probe.identity();
    let key = v4::issue_relation_key(&issue, &identity);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let raw = ctx.read_body(&key)?.ok_or(Rejection::StateCorrupt)?;
    let record =
        v4::IssueRelationRecord::decode_canonical(&raw).map_err(|_| Rejection::StateCorrupt)?;
    if record.issue != doc
        || record.kind != kind
        || record.identity() != identity
        || v4::issue_relation_key(&issue, &record.identity()) != key
    {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(record))
}

pub(crate) fn write_entity_relation(
    ctx: &Context<'_>,
    owner: &str,
    kind: &str,
    target: &str,
    present: bool,
) -> Result<Batch, Rejection> {
    let record = v4::EntityRelationRecord {
        owner: owner.into(),
        kind: kind.into(),
        target: target.into(),
        present,
    };
    let identity = record.identity();
    let key = v4::entity_relation_key(owner, &identity);
    let mut batch = Batch::default();
    batch.atomic_value(
        ctx,
        PhysicalSchema::EntityRelation,
        &key,
        canonical(&record)?,
    )?;
    Ok(batch)
}

pub(crate) fn write_entity_relation_diff(
    ctx: &Context<'_>,
    owner: &str,
    kind: &str,
    previous: &[String],
    wanted: &[String],
) -> Result<Batch, Rejection> {
    let previous = previous.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let wanted = wanted.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut batch = Batch::default();
    for target in previous.difference(&wanted) {
        batch.absorb(write_entity_relation(ctx, owner, kind, target, false)?);
    }
    for target in wanted.difference(&previous) {
        batch.absorb(write_entity_relation(ctx, owner, kind, target, true)?);
    }
    Ok(batch)
}

/// Stage one immutable activity record at the exact action coordinate.
pub(crate) fn write_activity(
    ctx: &Context<'_>,
    doc: &str,
    event: &contract::IssueEvent,
    recipients: &[String],
) -> Result<Batch, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let payload = serde_json::to_vec(event).map_err(|_| Rejection::StateCorrupt)?;
    let record = request_record_id(ctx, "lait.issues.activity-request.v1", &issue, &payload)?;
    write_activity_record(ctx, doc, &record, event, recipients)
}

fn write_activity_record(
    ctx: &Context<'_>,
    doc: &str,
    record: &str,
    event: &contract::IssueEvent,
    recipients: &[String],
) -> Result<Batch, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let descriptor = v4::SegmentDescriptor {
        issue: doc.into(),
        kind: v4::SegmentKind::Activity,
        record: record.into(),
    };
    let key = v4::issue_activity_key(&issue, &record);
    let mut batch = Batch::default();
    let activity = v4::ActivityRecord {
        issue: doc.into(),
        event: event.clone(),
        recipients: recipients.to_vec(),
    };
    batch.immutable_record(
        ctx,
        PhysicalSchema::IssueActivity,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: descriptor.issue,
            record: descriptor.record,
        },
        canonical(&activity)?,
    )?;
    Ok(batch)
}

pub(crate) fn issue_coordinate(
    doc: &str,
    project: &str,
    workflow_state: &str,
    position: String,
    ordinal: u64,
) -> Result<(v4::IssueIdentityRecord, v4::IssuePlacementRecord), Rejection> {
    let doc = DocId::parse(doc).ok_or(Rejection::StateCorrupt)?;
    let identity = v4::IssueIdentityRecord {
        issue: doc.as_str().into(),
        alias: v4::IssueAliasCoordinate::for_issue(ordinal, &doc)
            .map_err(|_| Rejection::StateCorrupt)?,
    };
    let placement = v4::IssuePlacementRecord {
        issue: doc.as_str().into(),
        placement: v4::BoardPlacement {
            project: project.into(),
            workflow_state: workflow_state.into(),
            block: v4::board_seed_block_id(project, workflow_state),
            position,
        },
    };
    identity.validate().map_err(|_| Rejection::StateCorrupt)?;
    placement.validate().map_err(|_| Rejection::StateCorrupt)?;
    Ok((identity, placement))
}

#[derive(Debug, Clone)]
struct BoardMember {
    issue: String,
    transition: String,
    position: String,
}

#[derive(Debug, Default)]
pub(crate) struct BoardPlacementPlan {
    pub placement: Option<v4::BoardPlacement>,
    pub maintenance: Batch,
}

fn packed_text(row: &runtime::find::ResultRow, name: &str) -> Option<String> {
    row.fields.iter().find_map(|field| {
        (field.reference == crate::find::field_ref(name))
            .then_some(&field.value)
            .and_then(|value| match value {
                runtime::find::Value::Text(value) => Some(value.to_string()),
                _ => None,
            })
    })
}

fn board_find_rejection(failure: runtime::find::Failure) -> Rejection {
    use runtime::find::Failure;

    match failure {
        Failure::Invalid(_) => Rejection::ContractViolation,
        Failure::PrincipalDenied => Rejection::Denied(DeniedCause::ReadRefused),
        Failure::NoActiveImplementation => Rejection::NoActiveImplementation,
        Failure::ImplementationUnavailable | Failure::AuthorityUnavailable(_) => {
            Rejection::ImplementationUnavailable
        }
        Failure::Interrupted
        | Failure::PolicyExceeded
        | Failure::PublicationUnavailable
        | Failure::PublicationExpired
        | Failure::PaginationUnsupported
        | Failure::CursorCapacityExceeded
        | Failure::Unavailable => Rejection::LimitExceeded,
    }
}

fn ordered_block(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    descending: bool,
    pivot: Vec<u8>,
) -> Result<Option<(String, String)>, Rejection> {
    use runtime::find as find_api;
    let bound = find_api::Bound {
        decoded_bodies: 2,
        postings_read: 16,
        edges_visited: 1,
        nodes_visited: 8,
        paths_retained: 1,
        candidates_per_branch: 8,
        score_evaluations: 1,
        projected_bytes: 8 * 1024,
        // Runtime's pack counter is byte-based. One block row carries bounded
        // project/state/block/revision/rank coordinates, so budget 1 KiB for
        // each of the at most two rows inspected here.
        packed_tokens: 2 * 1_024,
        wall_millis: 250,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let index = if descending {
        crate::find::field::PROJECT_STATE_BLOCK_ORDER_DESC
    } else {
        crate::find::field::PROJECT_STATE_BLOCK_ORDER
    };
    let upper =
        crate::find::composite_prefix_upper(crate::find::composite_key([project, workflow_state]))
            .ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::ID,
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
                        field: crate::find::field_ref(index),
                        lower: find_api::RangeEndpoint::Exclusive(find_api::Atom::Bytes(pivot)),
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
            page_size: 2,
            cursor: None,
        })
        .map_err(board_find_rejection)?;
    if let Some(row) = answer.rows().first() {
        if packed_text(row, crate::find::field::KIND).as_deref() != Some("board_block")
            || packed_text(row, crate::find::field::PROJECT).as_deref() != Some(project)
            || packed_text(row, crate::find::field::STATE).as_deref() != Some(workflow_state)
        {
            return Ok(None);
        }
        let block = packed_text(row, crate::find::field::BLOCK).ok_or(Rejection::StateCorrupt)?;
        let order = packed_text(row, crate::find::field::POSITION).ok_or(Rejection::Conflict)?;
        return Ok(Some((block, order)));
    }
    Ok(None)
}

#[derive(Debug, Clone)]
struct BlockOrderMember {
    block: String,
    order: String,
    revision: String,
}

fn ordered_blocks_page(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    descending: bool,
    pivot: Vec<u8>,
    limit: u32,
) -> Result<Vec<BlockOrderMember>, Rejection> {
    use runtime::find as find_api;
    let candidates = u64::from(limit).saturating_mul(2).max(8);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: candidates.saturating_mul(4),
        edges_visited: 1,
        nodes_visited: candidates,
        paths_retained: 1,
        candidates_per_branch: candidates,
        score_evaluations: 1,
        projected_bytes: candidates.saturating_mul(1024),
        packed_tokens: candidates.saturating_mul(1_024),
        wall_millis: 500,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let index = if descending {
        crate::find::field::PROJECT_STATE_BLOCK_ORDER_DESC
    } else {
        crate::find::field::PROJECT_STATE_BLOCK_ORDER
    };
    let upper =
        crate::find::composite_prefix_upper(crate::find::composite_key([project, workflow_state]))
            .ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::KIND,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::BLOCK,
        crate::find::field::POSITION,
        crate::find::field::REVISION,
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
                        field: crate::find::field_ref(index),
                        lower: find_api::RangeEndpoint::Exclusive(find_api::Atom::Bytes(pivot)),
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
            page_size: limit,
            cursor: None,
        })
        .map_err(board_find_rejection)?;
    let mut blocks = Vec::new();
    for row in answer.rows() {
        if packed_text(row, crate::find::field::KIND).as_deref() != Some("board_block")
            || packed_text(row, crate::find::field::PROJECT).as_deref() != Some(project)
            || packed_text(row, crate::find::field::STATE).as_deref() != Some(workflow_state)
        {
            break;
        }
        blocks.push(BlockOrderMember {
            block: packed_text(row, crate::find::field::BLOCK).ok_or(Rejection::StateCorrupt)?,
            order: packed_text(row, crate::find::field::POSITION).ok_or(Rejection::Conflict)?,
            revision: packed_text(row, crate::find::field::REVISION)
                .ok_or(Rejection::StateCorrupt)?,
        });
    }
    Ok(blocks)
}

fn board_block_members(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    block: &str,
    moving: &str,
) -> Result<Vec<BoardMember>, Rejection> {
    use runtime::find as find_api;
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: 2_048,
        edges_visited: 1,
        nodes_visited: 512,
        paths_retained: 1,
        candidates_per_branch: 256,
        score_evaluations: 1,
        projected_bytes: 512 * 1024,
        packed_tokens: u64::try_from(v4::BOARD_BLOCK_CAPACITY + 1)
            .unwrap_or(u64::MAX)
            .saturating_mul(1_024),
        wall_millis: 1_000,
    };
    let prefix = crate::find::composite_key([project, workflow_state, block]);
    let upper =
        crate::find::composite_prefix_upper(prefix.clone()).ok_or(Rejection::StateCorrupt)?;
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let keep = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(3).ok_or(Rejection::StateCorrupt)?;
    let mut fields = [
        crate::find::field::KIND,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::BLOCK,
        crate::find::field::SOURCE_ID,
        crate::find::field::POSITION,
        crate::find::field::PLACEMENT_TRANSITION,
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
                        lower: find_api::RangeEndpoint::Inclusive(find_api::Atom::Bytes(prefix)),
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
            page_size: u32::try_from(v4::BOARD_BLOCK_CAPACITY + 1)
                .map_err(|_| Rejection::ContractViolation)?,
            cursor: None,
        })
        .map_err(board_find_rejection)?;
    if answer.next_cursor().is_some() || answer.rows().len() > v4::BOARD_BLOCK_CAPACITY {
        return Err(Rejection::StateCorrupt);
    }
    let mut members = Vec::with_capacity(answer.rows().len());
    for row in answer.rows() {
        let issue =
            packed_text(row, crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?;
        if issue == moving {
            continue;
        }
        members.push(BoardMember {
            issue,
            transition: packed_text(row, crate::find::field::PLACEMENT_TRANSITION)
                .ok_or(Rejection::StateCorrupt)?,
            position: packed_text(row, crate::find::field::POSITION)
                .ok_or(Rejection::StateCorrupt)?,
        });
    }
    Ok(members)
}

fn maintenance_id(ctx: &Context<'_>) -> Result<String, Rejection> {
    let request = ctx.request_id().ok_or(Rejection::ContractViolation)?;
    Ok(data_encoding::HEXLOWER.encode(&blake3::derive_key(
        "lait.issues.board-maintenance.v1",
        &request.as_bytes(),
    )))
}

fn stage_member_overlay(
    ctx: &Context<'_>,
    batch: &mut Batch,
    member: &BoardMember,
    project: &str,
    workflow_state: &str,
    block: &str,
    position: String,
    maintenance: &str,
) -> Result<(), Rejection> {
    batch.absorb(write_issue_rank_overlay(
        ctx,
        &v4::IssueRankOverlay {
            issue: member.issue.clone(),
            transition: member.transition.clone(),
            project: project.into(),
            workflow_state: workflow_state.into(),
            block: block.into(),
            position,
            maintenance: maintenance.into(),
        },
    )?);
    Ok(())
}

fn stage_block_order_overlay(
    ctx: &Context<'_>,
    batch: &mut Batch,
    project: &str,
    workflow_state: &str,
    block: &BlockOrderMember,
    order: String,
    maintenance: &str,
) -> Result<(), Rejection> {
    let project = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let key = v4::board_block_key(&project, workflow_state, &block.block);
    if ctx.body_version(&key).is_none() {
        return Err(Rejection::StateCorrupt);
    }
    let overlay = v4::BoardBlockOrderOverlay {
        block_revision: block.revision.clone(),
        order,
        maintenance: maintenance.into(),
    };
    overlay.validate().map_err(|_| Rejection::StateCorrupt)?;
    set_register(batch, &key, v4::roots::ORDER_OVERLAY, canonical(&overlay)?);
    Ok(())
}

/// Plan one board insertion over bounded leaf blocks. Dense local labels never
/// reject the user move: the same transaction relabels at most 127 exact-head
/// neighbours, or splits a full 128-member leaf and relabels the two halves.
pub(crate) fn board_placement(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    moving: &str,
    position: Option<&contract::Pos>,
) -> Result<BoardPlacementPlan, Rejection> {
    if position.is_none() {
        if let Some(current) = issue_coordinate_for(ctx, moving)? {
            if current.placement.project == project
                && current.placement.workflow_state == workflow_state
            {
                return Ok(BoardPlacementPlan {
                    placement: Some(current.placement),
                    maintenance: Batch::default(),
                });
            }
        }
    }
    let requested = position.unwrap_or(&contract::Pos::Top);
    let topology = board_topology_heads(ctx, project, workflow_state)?;
    if topology.len() > 1 {
        return Err(Rejection::Conflict);
    }
    if topology.is_empty() {
        return Ok(BoardPlacementPlan {
            placement: Some(v4::BoardPlacement {
                project: project.into(),
                workflow_state: workflow_state.into(),
                block: v4::board_seed_block_id(project, workflow_state),
                position: crate::rank::between("", None),
            }),
            maintenance: Batch::default(),
        });
    }
    let (block, target_issue, after_target) = match requested {
        contract::Pos::Top => {
            let first = ordered_block(
                ctx,
                project,
                workflow_state,
                false,
                crate::find::composite_key([project, workflow_state]),
            )?
            .ok_or(Rejection::StateCorrupt)?;
            (first.0, None, false)
        }
        contract::Pos::Bottom => {
            let last = ordered_block(
                ctx,
                project,
                workflow_state,
                true,
                crate::find::composite_key([project, workflow_state]),
            )?
            .ok_or(Rejection::StateCorrupt)?;
            (last.0, None, true)
        }
        contract::Pos::Before { doc } | contract::Pos::After { doc } => {
            if doc == moving {
                return Err(Rejection::InvalidRequest);
            }
            let target = issue_coordinate_for(ctx, doc)?
                .filter(|coordinate| {
                    coordinate.placement.project == project
                        && coordinate.placement.workflow_state == workflow_state
                })
                .ok_or(Rejection::InvalidRequest)?;
            (
                target.placement.block,
                Some(doc.clone()),
                matches!(requested, contract::Pos::After { .. }),
            )
        }
    };
    let members = board_block_members(ctx, project, workflow_state, &block, moving)?;
    let insertion = match target_issue {
        None if after_target => members.len(),
        None => 0,
        Some(target) => {
            let at = members
                .iter()
                .position(|member| member.issue == target)
                .ok_or(Rejection::InvalidRequest)?;
            at + usize::from(after_target)
        }
    };
    let lower = insertion
        .checked_sub(1)
        .and_then(|index| members.get(index))
        .map_or("", |member| member.position.as_str());
    let upper = members
        .get(insertion)
        .map(|member| member.position.as_str());
    if let Some(local) = crate::rank::try_between(lower, upper) {
        if !crate::rank::under_pressure(&local) {
            return Ok(BoardPlacementPlan {
                placement: Some(v4::BoardPlacement {
                    project: project.into(),
                    workflow_state: workflow_state.into(),
                    block,
                    position: local,
                }),
                maintenance: Batch::default(),
            });
        }
    }
    if members.len() < v4::BOARD_BLOCK_CAPACITY {
        let labels = crate::rank::balanced_between("", None, members.len() + 1)
            .ok_or(Rejection::StateCorrupt)?;
        let maintenance = maintenance_id(ctx)?;
        let mut batch = Batch::default();
        for (index, member) in members.iter().enumerate() {
            let label_index = index + usize::from(index >= insertion);
            stage_member_overlay(
                ctx,
                &mut batch,
                member,
                project,
                workflow_state,
                &block,
                labels[label_index].clone(),
                &maintenance,
            )?;
        }
        return Ok(BoardPlacementPlan {
            placement: Some(v4::BoardPlacement {
                project: project.into(),
                workflow_state: workflow_state.into(),
                block,
                position: labels[insertion].clone(),
            }),
            maintenance: batch,
        });
    }
    split_board_block(ctx, project, workflow_state, &block, members, insertion)
}

fn split_board_block(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    source_block: &str,
    members: Vec<BoardMember>,
    insertion: usize,
) -> Result<BoardPlacementPlan, Rejection> {
    if members.len() != v4::BOARD_BLOCK_CAPACITY || insertion > members.len() {
        return Err(Rejection::StateCorrupt);
    }
    let request = ctx.request_id().ok_or(Rejection::ContractViolation)?;
    let mut material = Vec::new();
    material.extend_from_slice(&request.as_bytes());
    material.extend_from_slice(project.as_bytes());
    material.extend_from_slice(workflow_state.as_bytes());
    material.extend_from_slice(source_block.as_bytes());
    let new_block = data_encoding::HEXLOWER.encode(&blake3::derive_key(
        "lait.issues.board-split-block.v1",
        &material,
    ));
    let source = effective_board_block(ctx, project, workflow_state, source_block)?;
    let next = ordered_block(
        ctx,
        project,
        workflow_state,
        false,
        crate::find::board_block_order_key(project, workflow_state, &source.order, source_block),
    )?;
    let direct_order = crate::rank::try_between(
        &source.order,
        next.as_ref().map(|(_, order)| order.as_str()),
    );
    let maintenance = maintenance_id(ctx)?;
    let mut batch = Batch::default();
    let new_order = if direct_order
        .as_deref()
        .is_some_and(|order| !crate::rank::under_pressure(order))
    {
        direct_order.ok_or(Rejection::StateCorrupt)?
    } else {
        let mut before = ordered_blocks_page(
            ctx,
            project,
            workflow_state,
            true,
            crate::find::board_block_order_desc_key(
                project,
                workflow_state,
                &source.order,
                source_block,
            ),
            64,
        )?;
        let mut after = ordered_blocks_page(
            ctx,
            project,
            workflow_state,
            false,
            crate::find::board_block_order_key(
                project,
                workflow_state,
                &source.order,
                source_block,
            ),
            64,
        )?;
        let lower = (before.len() == 64)
            .then(|| before.pop())
            .flatten()
            .map(|block| block.order)
            .unwrap_or_default();
        let upper = (after.len() == 64)
            .then(|| after.pop())
            .flatten()
            .map(|block| block.order);
        before.reverse();
        let source_member = BlockOrderMember {
            block: source_block.into(),
            order: source.order.clone(),
            revision: source.revision.clone(),
        };
        let existing = before
            .iter()
            .chain(std::iter::once(&source_member))
            .chain(after.iter())
            .cloned()
            .collect::<Vec<_>>();
        if existing.len() + 1 > crate::rank::MAINTENANCE_WINDOW {
            return Err(Rejection::StateCorrupt);
        }
        let labels = crate::rank::balanced_between(&lower, upper.as_deref(), existing.len() + 1)
            .ok_or(Rejection::StateCorrupt)?;
        let source_at = before.len();
        for (index, block) in existing.iter().enumerate() {
            let label_index = index + usize::from(index > source_at);
            stage_block_order_overlay(
                ctx,
                &mut batch,
                project,
                workflow_state,
                block,
                labels[label_index].clone(),
                &maintenance,
            )?;
        }
        labels[source_at + 1].clone()
    };
    stage_board_split_topology(
        ctx,
        &mut batch,
        project,
        workflow_state,
        source_block,
        &new_block,
        new_order,
    )?;
    let total = members.len() + 1;
    let split = total / 2;
    let left_labels =
        crate::rank::balanced_between("", None, split).ok_or(Rejection::StateCorrupt)?;
    let right_labels =
        crate::rank::balanced_between("", None, total - split).ok_or(Rejection::StateCorrupt)?;
    for (index, member) in members.iter().enumerate() {
        let virtual_index = index + usize::from(index >= insertion);
        let (block, label) = if virtual_index < split {
            (source_block, left_labels[virtual_index].clone())
        } else {
            (
                new_block.as_str(),
                right_labels[virtual_index - split].clone(),
            )
        };
        stage_member_overlay(
            ctx,
            &mut batch,
            member,
            project,
            workflow_state,
            block,
            label,
            &maintenance,
        )?;
    }
    let (block, position) = if insertion < split {
        (source_block.to_string(), left_labels[insertion].clone())
    } else {
        (new_block, right_labels[insertion - split].clone())
    };
    Ok(BoardPlacementPlan {
        placement: Some(v4::BoardPlacement {
            project: project.into(),
            workflow_state: workflow_state.into(),
            block,
            position,
        }),
        maintenance: batch,
    })
}

pub(crate) fn write_issue_coordinate(
    ctx: &Context<'_>,
    batch: &mut Batch,
    doc: &str,
    project: &str,
    workflow_state: &str,
    position: String,
    ordinal: u64,
    tombstone: bool,
) -> Result<(), Rejection> {
    let (_, placement) = issue_coordinate(doc, project, workflow_state, position, ordinal)?;
    write_issue_identity(ctx, batch, doc, ordinal)?;
    let issue = DocId::parse(doc).ok_or(Rejection::StateCorrupt)?;
    let placement_key = v4::issue_placement_key(&issue);
    batch.atomic_value(
        ctx,
        PhysicalSchema::IssuePlacement,
        &placement_key,
        canonical(&placement)?,
    )?;
    let meta_key = v4::issue_meta_key(&issue);
    batch.ensure_body(
        ctx,
        PhysicalSchema::IssueMeta,
        &meta_key,
        doc.as_bytes().to_vec(),
    );
    set_register(
        batch,
        &meta_key,
        v4::roots::TOMBSTONE,
        if tombstone {
            b"1".to_vec()
        } else {
            b"0".to_vec()
        },
    );
    Ok(())
}

pub(crate) fn write_issue_identity(
    ctx: &Context<'_>,
    batch: &mut Batch,
    doc: &str,
    ordinal: u64,
) -> Result<(), Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::StateCorrupt)?;
    let identity = v4::IssueIdentityRecord {
        issue: doc.into(),
        alias: v4::IssueAliasCoordinate::for_issue(ordinal, &issue)
            .map_err(|_| Rejection::StateCorrupt)?,
    };
    identity.validate().map_err(|_| Rejection::StateCorrupt)?;
    let identity_key = v4::issue_identity_key(&issue);
    batch.immutable_record(
        ctx,
        PhysicalSchema::IssueIdentity,
        &identity_key,
        v4::RecordBodyIdentityRecord {
            owner: doc.into(),
            record: "identity".into(),
        },
        canonical(&identity)?,
    )
}

pub(crate) fn write_issue_meta(
    ctx: &Context<'_>,
    doc: &str,
    issue: &IssueState,
    tombstone: bool,
) -> Result<Batch, Rejection> {
    let parsed = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let record = v4::IssueMetaRecord {
        issue: doc.into(),
        title: issue.title.clone(),
        priority: issue.priority.as_str().into(),
        created_by: issue.created_by.as_ref().map(ToString::to_string),
        created_at: issue.created_at,
        due_at: issue.duedate,
        estimate: issue.estimate,
        tombstone,
    };
    record.validate().map_err(|_| Rejection::InvalidRequest)?;
    let key = v4::issue_meta_key(&parsed);
    let mut batch = Batch::default();
    batch.ensure_body(
        ctx,
        PhysicalSchema::IssueMeta,
        &key,
        doc.as_bytes().to_vec(),
    );
    set_register(&mut batch, &key, v4::roots::TITLE, record.title);
    set_register(&mut batch, &key, v4::roots::PRIORITY, record.priority);
    match record.created_by {
        Some(actor) => set_register(&mut batch, &key, v4::roots::CREATED_BY, actor),
        None => clear_register(&mut batch, &key, v4::roots::CREATED_BY),
    }
    set_register(
        &mut batch,
        &key,
        v4::roots::CREATED_AT,
        record.created_at.to_string(),
    );
    match record.due_at {
        Some(due) => set_register(&mut batch, &key, v4::roots::DUE_AT, due.to_string()),
        None => clear_register(&mut batch, &key, v4::roots::DUE_AT),
    }
    match record.estimate {
        Some(estimate) => set_register(&mut batch, &key, v4::roots::ESTIMATE, estimate.to_string()),
        None => clear_register(&mut batch, &key, v4::roots::ESTIMATE),
    }
    set_register(
        &mut batch,
        &key,
        v4::roots::TOMBSTONE,
        if record.tombstone { "1" } else { "0" },
    );
    Ok(batch)
}

pub(crate) fn read_attachment(
    ctx: &Context<'_>,
    doc: &str,
    id: &str,
) -> Result<Option<v4::IssueAttachmentRecord>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let key = v4::issue_attachment_key(&issue, id);
    let Some(raw) = ctx.read_body(&key)? else {
        return Ok(None);
    };
    let record =
        v4::IssueAttachmentRecord::decode_canonical(&raw).map_err(|_| Rejection::StateCorrupt)?;
    if record.issue != doc || record.id != id {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(record))
}

pub(crate) fn write_attachment(
    ctx: &Context<'_>,
    record: &v4::IssueAttachmentRecord,
) -> Result<Batch, Rejection> {
    record.validate().map_err(|_| Rejection::InvalidRequest)?;
    let issue = DocId::parse(&record.issue).ok_or(Rejection::InvalidRequest)?;
    let key = v4::issue_attachment_key(&issue, &record.id);
    let mut batch = Batch::default();
    batch.atomic_value(
        ctx,
        PhysicalSchema::IssueAttachment,
        &key,
        canonical(record)?,
    )?;
    let references = if record.tombstone {
        Vec::new()
    } else {
        let decoded = data_encoding::HEXLOWER
            .decode(record.content.as_bytes())
            .map_err(|_| Rejection::InvalidRequest)?;
        vec![replica::content::ContentRef {
            content_id: <[u8; 32]>::try_from(decoded.as_slice())
                .map_err(|_| Rejection::InvalidRequest)?,
        }]
    };
    batch.content_refs.insert(key, references);
    Ok(batch)
}

pub(crate) fn read_check(
    ctx: &Context<'_>,
    doc: &str,
    run: &str,
) -> Result<Option<v4::IssueCheckRecord>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let key = v4::issue_check_key(&issue, run);
    let Some(raw) = ctx.read_body(&key)? else {
        return Ok(None);
    };
    let record =
        v4::IssueCheckRecord::decode_canonical(&raw).map_err(|_| Rejection::StateCorrupt)?;
    if record.issue != doc || record.run != run {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(record))
}

pub(crate) fn write_check(
    ctx: &Context<'_>,
    record: &v4::IssueCheckRecord,
) -> Result<Batch, Rejection> {
    record.validate().map_err(|_| Rejection::InvalidRequest)?;
    let issue = DocId::parse(&record.issue).ok_or(Rejection::InvalidRequest)?;
    let key = v4::issue_check_key(&issue, &record.run);
    let mut batch = Batch::default();
    batch.atomic_value(ctx, PhysicalSchema::IssueCheck, &key, canonical(record)?)?;
    Ok(batch)
}

pub(crate) fn ordinal(ctx: &Context<'_>, doc: &str) -> Result<u64, Rejection> {
    issue_coordinate_for(ctx, doc)?
        .map(|coordinate| coordinate.identity.alias.ordinal)
        .filter(|ordinal| *ordinal > 0)
        .ok_or(Rejection::StateCorrupt)
}

fn replace_text(
    ctx: &Context<'_>,
    batch: &mut Batch,
    key: &BodyKey,
    path: &str,
    wanted: &str,
) -> Result<(), Rejection> {
    let current = if ctx.body_version(key).is_some() {
        read_view(ctx, key)?
            .texts
            .get(path)
            .cloned()
            .unwrap_or_default()
    } else {
        String::new()
    };
    if current == wanted {
        return Ok(());
    }
    let old: Vec<char> = current.chars().collect();
    let new: Vec<char> = wanted.chars().collect();
    let mut prefix = 0usize;
    while prefix < old.len() && prefix < new.len() && old[prefix] == new[prefix] {
        prefix = prefix.saturating_add(1);
    }
    let mut suffix = 0usize;
    while suffix < old.len().saturating_sub(prefix)
        && suffix < new.len().saturating_sub(prefix)
        && old[old.len().saturating_sub(1).saturating_sub(suffix)]
            == new[new.len().saturating_sub(1).saturating_sub(suffix)]
    {
        suffix = suffix.saturating_add(1);
    }
    let insert: String = new[prefix..new.len().saturating_sub(suffix)]
        .iter()
        .collect();
    batch.operation(
        key,
        Op::TextSplice {
            path: path.into(),
            index: u64::try_from(prefix).map_err(|_| Rejection::LimitExceeded)?,
            delete: u64::try_from(old.len().saturating_sub(prefix).saturating_sub(suffix))
                .map_err(|_| Rejection::LimitExceeded)?,
            insert,
        },
    );
    Ok(())
}

fn ensure_directory(
    ctx: &Context<'_>,
    catalog: &CatalogState,
    batch: &mut Batch,
) -> Result<BodyKey, Rejection> {
    let space = &ctx.principal().space;
    let key = v4::space_directory_key(space);
    let absent = ctx.body_version(&key).is_none()
        && !batch
            .declarations
            .iter()
            .any(|declaration| declaration.key == key);
    batch.ensure_body(
        ctx,
        PhysicalSchema::SpaceDirectory,
        &key,
        space.as_str().as_bytes().to_vec(),
    );
    if absent {
        set_register(
            batch,
            &key,
            v4::roots::NAME,
            catalog.name.as_bytes().to_vec(),
        );
    }
    Ok(key)
}

fn replace_owned_content(
    ctx: &Context<'_>,
    batch: &mut Batch,
    schema: PhysicalSchema,
    key: &BodyKey,
    identity: &str,
    description: &str,
) -> Result<(), Rejection> {
    if ctx.body_version(key).is_none() && description.is_empty() {
        return Ok(());
    }
    batch.ensure_body(ctx, schema, key, identity.as_bytes().to_vec());
    replace_text(ctx, batch, key, v4::roots::DESCRIPTION, description)
}

pub(crate) fn write_space(
    ctx: &Context<'_>,
    catalog: &CatalogState,
    name: &str,
    description: Option<&str>,
) -> Result<Batch, Rejection> {
    let mut batch = Batch::default();
    let key = ensure_directory(ctx, catalog, &mut batch)?;
    set_register(&mut batch, &key, v4::roots::NAME, name.as_bytes().to_vec());
    if let Some(description) = description {
        replace_owned_content(
            ctx,
            &mut batch,
            PhysicalSchema::SpaceContent,
            &v4::space_content_key(&ctx.principal().space),
            ctx.principal().space.as_str(),
            description,
        )?;
    }
    Ok(batch)
}

pub(crate) fn write_label(
    ctx: &Context<'_>,
    _catalog: &CatalogState,
    id: &str,
    meta: &LabelMeta,
    tombstone: bool,
) -> Result<Batch, Rejection> {
    let label = crate::ids::LabelId::parse(id).ok_or(Rejection::InvalidRequest)?;
    let mut batch = Batch::default();
    let record = v4::LabelDirectoryEntry {
        label: id.into(),
        name: meta.name.clone(),
        color: meta.color.clone(),
        tombstone,
    };
    let key = v4::label_key(&label);
    batch.ensure_body(ctx, PhysicalSchema::Label, &key, id.as_bytes().to_vec());
    set_register(&mut batch, &key, v4::roots::RECORD, canonical(&record)?);
    Ok(batch)
}

pub(crate) fn write_project(
    ctx: &Context<'_>,
    _catalog: &CatalogState,
    id: &str,
    meta: &ProjectMeta,
    tombstone: bool,
    description: Option<&str>,
) -> Result<Batch, Rejection> {
    let project = ProjectId::parse(id).ok_or(Rejection::InvalidRequest)?;
    let mut batch = Batch::default();
    let key = v4::project_meta_key(&project);
    batch.ensure_body(
        ctx,
        PhysicalSchema::ProjectMeta,
        &key,
        id.as_bytes().to_vec(),
    );
    for (path, value) in [
        (v4::roots::NAME, meta.name.as_str()),
        (v4::roots::KEY, meta.key.as_str()),
        (v4::roots::COLOR, meta.color.as_str()),
        (v4::roots::LEAD, meta.lead.as_str()),
        (v4::roots::TEAM, meta.team.as_str()),
        (v4::roots::ARCHIVED, if meta.archived { "1" } else { "0" }),
        (v4::roots::TOMBSTONE, if tombstone { "1" } else { "0" }),
    ] {
        set_register(&mut batch, &key, path, value.as_bytes().to_vec());
    }
    for (path, value) in [
        (v4::roots::START_DATE, meta.start_date),
        (v4::roots::TARGET_DATE, meta.target_date),
    ] {
        match value {
            Some(value) => set_register(&mut batch, &key, path, value.to_string().into_bytes()),
            None => clear_register(&mut batch, &key, path),
        }
    }
    if let Some(description) = description {
        replace_owned_content(
            ctx,
            &mut batch,
            PhysicalSchema::ProjectContent,
            &v4::project_content_key(&project),
            id,
            description,
        )?;
    }
    Ok(batch)
}

pub(crate) fn write_governance_revision(
    ctx: &Context<'_>,
    revision: &crate::views::StoredRoleRevision,
) -> Result<Batch, Rejection> {
    let mut batch = write_governance_revision_record(ctx, revision)?;
    let record = v4::GovernanceRevisionRecord {
        role: revision.body.role_id.clone(),
        revision: revision.clone(),
    };
    let heads = v4::governance_heads_key(&record.role);
    batch.ensure_body(
        ctx,
        PhysicalSchema::GovernanceHeads,
        &heads,
        record.role.as_bytes().to_vec(),
    );
    for predecessor in &record.revision.predecessor_ids {
        batch.operation(
            &heads,
            Op::SetRemove {
                path: v4::roots::HEADS.into(),
                value: predecessor.as_bytes().to_vec(),
            },
        );
    }
    batch.operation(
        &heads,
        Op::SetAdd {
            path: v4::roots::HEADS.into(),
            value: record.revision.revision_id.as_bytes().to_vec(),
        },
    );
    Ok(batch)
}

pub(crate) fn write_governance_revision_record(
    ctx: &Context<'_>,
    revision: &crate::views::StoredRoleRevision,
) -> Result<Batch, Rejection> {
    let mut batch = Batch::default();
    let record = v4::GovernanceRevisionRecord {
        role: revision.body.role_id.clone(),
        revision: revision.clone(),
    };
    let key = v4::governance_revision_key(&record.role, &record.revision.revision_id);
    batch.immutable_record(
        ctx,
        PhysicalSchema::GovernanceRevision,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: record.role.clone(),
            record: record.revision.revision_id.clone(),
        },
        canonical(&record)?,
    )?;
    Ok(batch)
}

pub(crate) fn write_workflow_revision(
    ctx: &Context<'_>,
    project: &str,
    revision: &crate::workflow::WorkflowRevision,
) -> Result<Batch, Rejection> {
    let mut batch = write_workflow_revision_record(ctx, project, revision)?;
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let heads = v4::workflow_heads_key(&project_id);
    batch.ensure_body(
        ctx,
        PhysicalSchema::WorkflowHeads,
        &heads,
        project.as_bytes().to_vec(),
    );
    set_register(
        &mut batch,
        &heads,
        v4::roots::PROJECT,
        project.as_bytes().to_vec(),
    );
    set_register(&mut batch, &heads, v4::roots::KIND, b"workflow".to_vec());
    for predecessor in &revision.predecessor_ids {
        batch.operation(
            &heads,
            Op::SetRemove {
                path: v4::roots::HEADS.into(),
                value: predecessor.as_bytes().to_vec(),
            },
        );
    }
    batch.operation(
        &heads,
        Op::SetAdd {
            path: v4::roots::HEADS.into(),
            value: revision.revision_id.as_bytes().to_vec(),
        },
    );
    Ok(batch)
}

pub(crate) fn write_workflow_revision_record(
    ctx: &Context<'_>,
    project: &str,
    revision: &crate::workflow::WorkflowRevision,
) -> Result<Batch, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let mut batch = Batch::default();
    let key = v4::workflow_revision_key(&project_id, &revision.revision_id);
    let record = v4::ProjectWorkflowRevisionRecord {
        project: project.into(),
        revision: revision.clone(),
    };
    batch.immutable_record(
        ctx,
        PhysicalSchema::WorkflowRevision,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: project.into(),
            record: revision.revision_id.clone(),
        },
        canonical(&record)?,
    )?;
    Ok(batch)
}

pub(crate) fn write_spec_revision(
    ctx: &Context<'_>,
    revision: &crate::spec::Revision,
) -> Result<Batch, Rejection> {
    let mut batch = write_spec_revision_record(ctx, revision)?;
    let spec = crate::ids::SpecId::parse(&revision.body.spec).ok_or(Rejection::InvalidRequest)?;
    let heads = v4::spec_heads_key(&spec);
    batch.ensure_body(
        ctx,
        PhysicalSchema::SpecHeads,
        &heads,
        revision.body.spec.as_bytes().to_vec(),
    );
    set_register(
        &mut batch,
        &heads,
        v4::roots::PROJECT,
        revision.body.project.as_bytes().to_vec(),
    );
    set_register(
        &mut batch,
        &heads,
        v4::roots::KIND,
        revision.body.kind.as_str().as_bytes().to_vec(),
    );
    for predecessor in &revision.predecessors {
        batch.operation(
            &heads,
            Op::SetRemove {
                path: v4::roots::HEADS.into(),
                value: predecessor.as_bytes().to_vec(),
            },
        );
    }
    batch.operation(
        &heads,
        Op::SetAdd {
            path: v4::roots::HEADS.into(),
            value: revision.revision.as_bytes().to_vec(),
        },
    );
    Ok(batch)
}

pub(crate) fn write_spec_revision_record(
    ctx: &Context<'_>,
    revision: &crate::spec::Revision,
) -> Result<Batch, Rejection> {
    let spec = crate::ids::SpecId::parse(&revision.body.spec).ok_or(Rejection::InvalidRequest)?;
    let record = v4::SpecRevisionRecord {
        revision: revision.clone(),
    };
    let coordinate = v4::spec_revision_key(&spec, &revision.revision);
    let mut batch = Batch::default();
    batch.immutable_record(
        ctx,
        PhysicalSchema::SpecRevision,
        &coordinate,
        v4::RecordBodyIdentityRecord {
            owner: revision.body.spec.clone(),
            record: revision.revision.clone(),
        },
        canonical(&record)?,
    )?;
    Ok(batch)
}

pub(crate) fn write_issue_transition(
    ctx: &Context<'_>,
    doc: &str,
    predecessors: &[String],
    placement: &v4::BoardPlacement,
    evidence: &str,
    timestamp: u64,
) -> Result<(Batch, String), Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let mut batch = ensure_board_topology(
        ctx,
        &placement.project,
        &placement.workflow_state,
        &placement.block,
    )?;
    let mut predecessors = predecessors.to_vec();
    predecessors.sort();
    predecessors.dedup();
    let record = v4::IssueTransitionRecord {
        issue: doc.into(),
        predecessors,
        placement: placement.clone(),
        actor: ctx.principal().actor.to_string(),
        timestamp,
        evidence: evidence.into(),
    };
    let transition = record
        .transition_id()
        .map_err(|_| Rejection::InvalidRequest)?;
    let coordinate = v4::issue_transition_key(&issue, &transition);
    batch.immutable_record(
        ctx,
        PhysicalSchema::IssueTransition,
        &coordinate,
        v4::RecordBodyIdentityRecord {
            owner: doc.into(),
            record: transition.clone(),
        },
        canonical(&record)?,
    )?;
    let meta = v4::issue_meta_key(&issue);
    batch.ensure_body(
        ctx,
        PhysicalSchema::IssueMeta,
        &meta,
        doc.as_bytes().to_vec(),
    );
    let existing_heads = if ctx.body_version(&meta).is_some() {
        ctx.read_collaborative(&meta)
            .map_err(Rejection::BodyRead)?
            .ok_or(Rejection::StateCorrupt)?
            .sets
            .get(v4::roots::PLACEMENT_HEADS)
            .cloned()
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut surviving = existing_heads
        .iter()
        .map(|value| {
            v4::IssueTransitionHead::decode_canonical(value)
                .map(|head| head.transition)
                .map_err(|_| Rejection::StateCorrupt)
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    for predecessor in &record.predecessors {
        surviving.remove(predecessor);
    }
    surviving.insert(transition.clone());
    if surviving.len() > v4::MAX_CONCURRENT_HEADS {
        return Err(Rejection::LimitExceeded);
    }
    for predecessor in &record.predecessors {
        let encoded = existing_heads
            .iter()
            .find(|value| {
                v4::IssueTransitionHead::decode_canonical(value)
                    .is_ok_and(|head| head.transition == *predecessor)
            })
            .cloned()
            .ok_or(Rejection::StateCorrupt)?;
        batch.operation(
            &meta,
            Op::SetRemove {
                path: v4::roots::PLACEMENT_HEADS.into(),
                value: encoded,
            },
        );
    }
    let head = v4::IssueTransitionHead {
        transition: transition.clone(),
        core: record.core(),
    };
    batch.operation(
        &meta,
        Op::SetAdd {
            path: v4::roots::PLACEMENT_HEADS.into(),
            value: canonical(&head)?,
        },
    );
    Ok((batch, transition))
}

fn board_block_head(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    block: &str,
) -> Result<Option<v4::BoardBlockHead>, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let key = v4::board_block_key(&project_id, workflow_state, block);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let view = ctx
        .read_collaborative(&key)
        .map_err(Rejection::BodyRead)?
        .ok_or(Rejection::StateCorrupt)?;
    let mut heads = view
        .sets
        .get(v4::roots::BLOCK_HEADS)
        .into_iter()
        .flatten()
        .map(|raw| v4::BoardBlockHead::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    heads.sort_by(|left, right| left.revision.cmp(&right.revision));
    heads.dedup_by(|left, right| left.revision == right.revision);
    match heads.as_slice() {
        [head]
            if head.validate().is_ok()
                && head.core.project == project
                && head.core.workflow_state == workflow_state
                && head.core.block == block =>
        {
            Ok(Some(head.clone()))
        }
        [_] => Err(Rejection::StateCorrupt),
        [] => Err(Rejection::StateCorrupt),
        _ => Err(Rejection::Conflict),
    }
}

fn effective_board_block(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    block: &str,
) -> Result<BlockOrderMember, Rejection> {
    let head =
        board_block_head(ctx, project, workflow_state, block)?.ok_or(Rejection::StateCorrupt)?;
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let key = v4::board_block_key(&project_id, workflow_state, block);
    let view = ctx
        .read_collaborative(&key)
        .map_err(Rejection::BodyRead)?
        .ok_or(Rejection::StateCorrupt)?;
    let mut order = head.core.order.clone();
    if let Some(raw) = view
        .registers
        .get(v4::roots::ORDER_OVERLAY)
        .filter(|raw| !raw.is_empty())
    {
        let overlay = v4::BoardBlockOrderOverlay::decode_canonical(raw)
            .map_err(|_| Rejection::StateCorrupt)?;
        overlay.validate().map_err(|_| Rejection::StateCorrupt)?;
        if overlay.block_revision == head.revision {
            order = overlay.order;
        }
    }
    Ok(BlockOrderMember {
        block: block.into(),
        order,
        revision: head.revision,
    })
}

fn board_topology_heads(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
) -> Result<Vec<v4::BoardTopologyHead>, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let key = v4::board_lane_key(&project_id, workflow_state);
    if ctx.body_version(&key).is_none() {
        return Ok(Vec::new());
    }
    let view = ctx
        .read_collaborative(&key)
        .map_err(Rejection::BodyRead)?
        .ok_or(Rejection::StateCorrupt)?;
    let mut heads = view
        .sets
        .get(v4::roots::TOPOLOGY_HEADS)
        .into_iter()
        .flatten()
        .map(|raw| {
            v4::BoardTopologyHead::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    heads.sort_by(|left, right| left.transition.cmp(&right.transition));
    heads.dedup_by(|left, right| left.transition == right.transition);
    if heads.iter().any(|head| {
        head.validate().is_err()
            || head.core.project != project
            || head.core.workflow_state != workflow_state
    }) {
        return Err(Rejection::StateCorrupt);
    }
    Ok(heads)
}

/// Ensure the deterministic first block/lane topology or verify that a split
/// block belongs to the lane. Initial creation converges because its complete
/// topology head is request-independent; subsequent structural changes are
/// predecessor-bound and therefore expose concurrent splits as multiple heads.
fn ensure_board_topology(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    block: &str,
) -> Result<Batch, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let mut batch = Batch::default();
    let topology = board_topology_heads(ctx, project, workflow_state)?;
    if topology.is_empty() {
        let seed = v4::board_seed_block_id(project, workflow_state);
        if block != seed {
            return Err(Rejection::StateCorrupt);
        }
        let block_key = v4::board_block_key(&project_id, workflow_state, &seed);
        batch.ensure_body(
            ctx,
            PhysicalSchema::BoardBlock,
            &block_key,
            composite_identity([project, workflow_state, &seed]),
        );
        let core = v4::BoardBlockCore {
            project: project.into(),
            workflow_state: workflow_state.into(),
            block: seed.clone(),
            order: crate::rank::between("", None),
        };
        let head = v4::BoardBlockHead {
            revision: core
                .revision_id()
                .map_err(|_| Rejection::ContractViolation)?,
            core,
        };
        batch.operation(
            &block_key,
            Op::SetAdd {
                path: v4::roots::BLOCK_HEADS.into(),
                value: canonical(&head)?,
            },
        );
        let lane_key = v4::board_lane_key(&project_id, workflow_state);
        batch.ensure_body(
            ctx,
            PhysicalSchema::BoardLane,
            &lane_key,
            composite_identity([project, workflow_state]),
        );
        let topology_core = v4::BoardTopologyCore {
            project: project.into(),
            workflow_state: workflow_state.into(),
            predecessors: Vec::new(),
            split: v4::BoardTopologySplit {
                source_block: None,
                created_block: seed,
            },
        };
        let topology_head = v4::BoardTopologyHead {
            transition: topology_core
                .transition_id()
                .map_err(|_| Rejection::ContractViolation)?,
            core: topology_core,
        };
        batch.operation(
            &lane_key,
            Op::SetAdd {
                path: v4::roots::TOPOLOGY_HEADS.into(),
                value: canonical(&topology_head)?,
            },
        );
        return Ok(batch);
    }
    if topology.len() != 1 {
        return Err(Rejection::Conflict);
    }
    board_block_head(ctx, project, workflow_state, block)?.ok_or(Rejection::StateCorrupt)?;
    Ok(batch)
}

fn stage_board_split_topology(
    ctx: &Context<'_>,
    batch: &mut Batch,
    project: &str,
    workflow_state: &str,
    source_block: &str,
    new_block: &str,
    new_order: String,
) -> Result<(), Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let topology = board_topology_heads(ctx, project, workflow_state)?;
    let [predecessor] = topology.as_slice() else {
        return Err(if topology.len() > 1 {
            Rejection::Conflict
        } else {
            Rejection::StateCorrupt
        });
    };
    let block_key = v4::board_block_key(&project_id, workflow_state, new_block);
    if ctx.body_version(&block_key).is_some() {
        return Err(Rejection::Conflict);
    }
    batch.ensure_body(
        ctx,
        PhysicalSchema::BoardBlock,
        &block_key,
        composite_identity([project, workflow_state, new_block]),
    );
    let block_core = v4::BoardBlockCore {
        project: project.into(),
        workflow_state: workflow_state.into(),
        block: new_block.into(),
        order: new_order,
    };
    let block_head = v4::BoardBlockHead {
        revision: block_core
            .revision_id()
            .map_err(|_| Rejection::ContractViolation)?,
        core: block_core,
    };
    batch.operation(
        &block_key,
        Op::SetAdd {
            path: v4::roots::BLOCK_HEADS.into(),
            value: canonical(&block_head)?,
        },
    );
    let lane_key = v4::board_lane_key(&project_id, workflow_state);
    let core = v4::BoardTopologyCore {
        project: project.into(),
        workflow_state: workflow_state.into(),
        predecessors: vec![predecessor.transition.clone()],
        split: v4::BoardTopologySplit {
            source_block: Some(source_block.into()),
            created_block: new_block.into(),
        },
    };
    let successor = v4::BoardTopologyHead {
        transition: core
            .transition_id()
            .map_err(|_| Rejection::ContractViolation)?,
        core,
    };
    batch.operation(
        &lane_key,
        Op::SetRemove {
            path: v4::roots::TOPOLOGY_HEADS.into(),
            value: canonical(predecessor)?,
        },
    );
    batch.operation(
        &lane_key,
        Op::SetAdd {
            path: v4::roots::TOPOLOGY_HEADS.into(),
            value: canonical(&successor)?,
        },
    );
    Ok(())
}

fn composite_identity<'a>(parts: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    crate::find::composite_key(parts)
}

/// Resolve the add-wins transition heads for one Issue at this exact
/// publication.  Zero is valid only for a not-yet-migrated historical Issue;
/// more than one is an explicit collaborative conflict and callers must not
/// invent a board winner.
pub(crate) fn issue_transition_heads(
    ctx: &Context<'_>,
    doc: &str,
) -> Result<Vec<(String, v4::IssueTransitionRecord)>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let meta = v4::issue_meta_key(&issue);
    if ctx.body_version(&meta).is_none() {
        return Ok(Vec::new());
    }
    let view = ctx
        .read_collaborative(&meta)
        .map_err(Rejection::BodyRead)?
        .ok_or(Rejection::StateCorrupt)?;
    let mut ids = view
        .sets
        .get(v4::roots::PLACEMENT_HEADS)
        .into_iter()
        .flatten()
        .map(|value| {
            v4::IssueTransitionHead::decode_canonical(value)
                .map(|head| head.transition)
                .map_err(|_| Rejection::StateCorrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    ids.sort();
    ids.dedup();
    if ids.len() > v4::MAX_CONCURRENT_HEADS {
        return Err(Rejection::LimitExceeded);
    }
    let mut heads = Vec::with_capacity(ids.len());
    for id in ids {
        let source = exact_record_source(ctx, crate::find::field::ID, &id, "issue_transition")?
            .ok_or(Rejection::StateCorrupt)?;
        let bytes = ctx.read_body(&source)?.ok_or(Rejection::StateCorrupt)?;
        let envelope = v4::ImmutableRecordEnvelope::decode_canonical(&bytes)
            .map_err(|_| Rejection::StateCorrupt)?;
        let record = v4::IssueTransitionRecord::decode_canonical(&envelope.record)
            .map_err(|_| Rejection::StateCorrupt)?;
        if record.issue != doc
            || record
                .transition_id()
                .map_err(|_| Rejection::StateCorrupt)?
                != id
        {
            return Err(Rejection::StateCorrupt);
        }
        let projected = view
            .sets
            .get(v4::roots::PLACEMENT_HEADS)
            .into_iter()
            .flatten()
            .find_map(|value| {
                v4::IssueTransitionHead::decode_canonical(value)
                    .ok()
                    .filter(|head| head.transition == id)
            })
            .ok_or(Rejection::StateCorrupt)?;
        if projected.validate().is_err() || projected.core != record.core() {
            return Err(Rejection::StateCorrupt);
        }
        heads.push((id, record));
    }
    Ok(heads)
}

fn issue_rank_overlay(
    ctx: &Context<'_>,
    doc: &str,
) -> Result<Option<v4::IssueRankOverlay>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let meta = v4::issue_meta_key(&issue);
    if ctx.body_version(&meta).is_none() {
        return Ok(None);
    }
    let view = read_view(ctx, &meta)?;
    let Some(bytes) = view.registers.get(v4::roots::RANK_OVERLAY) else {
        return Ok(None);
    };
    if bytes.is_empty() {
        return Ok(None);
    }
    let overlay =
        v4::IssueRankOverlay::decode_canonical(bytes).map_err(|_| Rejection::StateCorrupt)?;
    overlay.validate().map_err(|_| Rejection::StateCorrupt)?;
    if overlay.issue != doc {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(overlay))
}

fn effective_issue_placement(
    ctx: &Context<'_>,
    doc: &str,
    transition: &str,
    placement: &v4::BoardPlacement,
) -> Result<v4::BoardPlacement, Rejection> {
    let mut effective = placement.clone();
    if let Some(overlay) = issue_rank_overlay(ctx, doc)? {
        if overlay.transition == transition
            && overlay.project == placement.project
            && overlay.workflow_state == placement.workflow_state
        {
            effective.block = overlay.block;
            effective.position = overlay.position;
        }
    }
    Ok(effective)
}

pub(crate) fn write_issue_rank_overlay(
    ctx: &Context<'_>,
    overlay: &v4::IssueRankOverlay,
) -> Result<Batch, Rejection> {
    overlay.validate().map_err(|_| Rejection::InvalidRequest)?;
    let issue = DocId::parse(&overlay.issue).ok_or(Rejection::InvalidRequest)?;
    let key = v4::issue_meta_key(&issue);
    if ctx.body_version(&key).is_none() {
        return Err(Rejection::StateCorrupt);
    }
    let mut batch = Batch::default();
    set_register(
        &mut batch,
        &key,
        v4::roots::RANK_OVERLAY,
        canonical(overlay)?,
    );
    Ok(batch)
}

pub(crate) fn write_spec_issued_heads(
    ctx: &Context<'_>,
    spec: &str,
    remove: &[String],
    add: Option<&str>,
) -> Result<Batch, Rejection> {
    let spec = crate::ids::SpecId::parse(spec).ok_or(Rejection::InvalidRequest)?;
    let key = v4::spec_heads_key(&spec);
    // A migration may stage this projection in the same transaction that
    // creates the heads Body. Ordinary callers have already resolved the
    // exact heads source; the substrate still rejects an operation with no
    // matching declaration/body.
    let _ = ctx;
    let mut batch = Batch::default();
    for revision in remove {
        batch.operation(
            &key,
            Op::SetRemove {
                path: v4::roots::ISSUED_HEADS.into(),
                value: revision.as_bytes().to_vec(),
            },
        );
    }
    if let Some(revision) = add {
        batch.operation(
            &key,
            Op::SetAdd {
                path: v4::roots::ISSUED_HEADS.into(),
                value: revision.as_bytes().to_vec(),
            },
        );
    }
    Ok(batch)
}

pub(crate) fn write_baseline_revision(
    ctx: &Context<'_>,
    revision: &crate::spec::BaselineRevision,
) -> Result<Batch, Rejection> {
    let mut batch = write_baseline_revision_record(ctx, revision)?;
    let baseline =
        crate::ids::BaselineId::parse(&revision.body.baseline).ok_or(Rejection::InvalidRequest)?;
    let heads = v4::baseline_heads_key(&baseline);
    batch.ensure_body(
        ctx,
        PhysicalSchema::BaselineHeads,
        &heads,
        revision.body.baseline.as_bytes().to_vec(),
    );
    set_register(
        &mut batch,
        &heads,
        v4::roots::PROJECT,
        revision.body.project.as_bytes().to_vec(),
    );
    set_register(&mut batch, &heads, v4::roots::KIND, b"baseline".to_vec());
    for predecessor in &revision.predecessors {
        batch.operation(
            &heads,
            Op::SetRemove {
                path: v4::roots::HEADS.into(),
                value: predecessor.as_bytes().to_vec(),
            },
        );
    }
    batch.operation(
        &heads,
        Op::SetAdd {
            path: v4::roots::HEADS.into(),
            value: revision.revision.as_bytes().to_vec(),
        },
    );
    Ok(batch)
}

pub(crate) fn write_baseline_revision_record(
    ctx: &Context<'_>,
    revision: &crate::spec::BaselineRevision,
) -> Result<Batch, Rejection> {
    let baseline =
        crate::ids::BaselineId::parse(&revision.body.baseline).ok_or(Rejection::InvalidRequest)?;
    let record = v4::BaselineRevisionRecord {
        revision: revision.clone(),
    };
    let coordinate = v4::baseline_revision_key(&baseline, &revision.revision);
    let mut batch = Batch::default();
    batch.immutable_record(
        ctx,
        PhysicalSchema::BaselineRevision,
        &coordinate,
        v4::RecordBodyIdentityRecord {
            owner: revision.body.baseline.clone(),
            record: revision.revision.clone(),
        },
        canonical(&record)?,
    )?;
    Ok(batch)
}

pub(crate) fn write_baseline_issued_heads(
    ctx: &Context<'_>,
    baseline: &str,
    remove: &[String],
    add: Option<&str>,
) -> Result<Batch, Rejection> {
    let baseline = crate::ids::BaselineId::parse(baseline).ok_or(Rejection::InvalidRequest)?;
    let key = v4::baseline_heads_key(&baseline);
    let _ = ctx;
    let mut batch = Batch::default();
    for revision in remove {
        batch.operation(
            &key,
            Op::SetRemove {
                path: v4::roots::ISSUED_HEADS.into(),
                value: revision.as_bytes().to_vec(),
            },
        );
    }
    if let Some(revision) = add {
        batch.operation(
            &key,
            Op::SetAdd {
                path: v4::roots::ISSUED_HEADS.into(),
                value: revision.as_bytes().to_vec(),
            },
        );
    }
    Ok(batch)
}

pub(crate) fn write_spec_observation(
    ctx: &Context<'_>,
    record: &v4::SpecObservationRecord,
) -> Result<Batch, Rejection> {
    record.validate().map_err(|_| Rejection::InvalidRequest)?;
    let spec = crate::ids::SpecId::parse(record.spec()).ok_or(Rejection::InvalidRequest)?;
    let identity = record.identity();
    let coordinate = v4::spec_observation_key(&spec, &identity);
    let mut batch = Batch::default();
    batch.immutable_record(
        ctx,
        PhysicalSchema::SpecObservation,
        &coordinate,
        v4::RecordBodyIdentityRecord {
            owner: record.spec().into(),
            record: identity,
        },
        canonical(record)?,
    )?;
    Ok(batch)
}

pub(crate) fn write_revision_alias(
    ctx: &Context<'_>,
    alias: &v4::RevisionAliasRecord,
) -> Result<Batch, Rejection> {
    let mut batch = Batch::default();
    let key = v4::revision_alias_key(&alias.spec, &alias.legacy_revision);
    batch.immutable_record(
        ctx,
        PhysicalSchema::RevisionAlias,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: alias.spec.clone(),
            record: alias.legacy_revision.clone(),
        },
        canonical(alias)?,
    )?;
    Ok(batch)
}

pub(crate) fn write_project_update(
    ctx: &Context<'_>,
    update: &ProjectUpdate,
) -> Result<Batch, Rejection> {
    let record = v4::ProjectUpdateRecord {
        update: update.id.clone(),
        project: update.project_id.clone(),
        author: update.author.clone(),
        timestamp: update.ts,
        body: update.body.clone(),
        health: update.health.clone(),
    };
    let bytes = canonical(&record)?;
    let project = ProjectId::parse(&update.project_id).ok_or(Rejection::InvalidRequest)?;
    let key = v4::project_updates_key(&project, &record.update);
    let mut batch = Batch::default();
    batch.immutable_record(
        ctx,
        PhysicalSchema::ProjectUpdates,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: update.project_id.clone(),
            record: record.update.clone(),
        },
        bytes,
    )?;
    Ok(batch)
}

pub(crate) fn write_milestone(
    ctx: &Context<'_>,
    milestone: &Milestone,
) -> Result<Batch, Rejection> {
    let project = ProjectId::parse(&milestone.project_id).ok_or(Rejection::InvalidRequest)?;
    let key = v4::project_schedule_key(&project, &milestone.id);
    let mut batch = Batch::default();
    batch.ensure_body(
        ctx,
        PhysicalSchema::ProjectSchedule,
        &key,
        canonical(&v4::RecordBodyIdentityRecord {
            owner: milestone.project_id.clone(),
            record: milestone.id.clone(),
        })?,
    );
    let record = v4::ScheduleRecord::Milestone {
        milestone: milestone.id.clone(),
        project: milestone.project_id.clone(),
        name: milestone.name.clone(),
        description: milestone.description.clone(),
        target_date: milestone.target_date,
        position: milestone.rank.clone(),
        tombstone: milestone.tombstone,
    };
    set_register(&mut batch, &key, v4::roots::RECORD, canonical(&record)?);
    Ok(batch)
}

pub(crate) fn write_cycle(ctx: &Context<'_>, cycle: &Cycle) -> Result<Batch, Rejection> {
    let project = ProjectId::parse(&cycle.project_id).ok_or(Rejection::InvalidRequest)?;
    let key = v4::project_schedule_key(&project, &cycle.id);
    let mut batch = Batch::default();
    batch.ensure_body(
        ctx,
        PhysicalSchema::ProjectSchedule,
        &key,
        canonical(&v4::RecordBodyIdentityRecord {
            owner: cycle.project_id.clone(),
            record: cycle.id.clone(),
        })?,
    );
    let record = v4::ScheduleRecord::Cycle {
        cycle: cycle.id.clone(),
        project: cycle.project_id.clone(),
        name: cycle.name.clone(),
        start: cycle.start,
        end: cycle.end,
        tombstone: cycle.tombstone,
    };
    set_register(&mut batch, &key, v4::roots::RECORD, canonical(&record)?);
    Ok(batch)
}

pub(crate) fn write_parent(
    ctx: &Context<'_>,
    project: &str,
    child: &str,
    parent: Option<String>,
) -> Result<Batch, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let record = v4::HierarchyRecord {
        project: project.into(),
        child: child.into(),
        parent,
    };
    let identity = child.to_string();
    let key = v4::project_hierarchy_key(&project_id, &identity);
    let mut batch = Batch::default();
    batch.atomic_value(
        ctx,
        PhysicalSchema::ProjectHierarchy,
        &key,
        canonical(&v4::TopologyRecord::Parent(record))?,
    )?;
    Ok(batch)
}

pub(crate) fn write_link(
    ctx: &Context<'_>,
    project: &str,
    from: &str,
    kind: &str,
    to: &str,
    present: bool,
) -> Result<Batch, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let record = v4::ProjectLinkRecord {
        project: project.into(),
        from: from.into(),
        kind: kind.into(),
        to: to.into(),
        present,
    };
    let identity = data_encoding::HEXLOWER.encode(&record.relation_identity());
    let key = v4::project_hierarchy_key(&project_id, &identity);
    let mut batch = Batch::default();
    batch.atomic_value(
        ctx,
        PhysicalSchema::ProjectHierarchy,
        &key,
        canonical(&v4::TopologyRecord::Link(record))?,
    )?;
    Ok(batch)
}

pub(crate) fn read_link(
    ctx: &Context<'_>,
    project: &str,
    from: &str,
    kind: &str,
    to: &str,
) -> Result<Option<v4::ProjectLinkRecord>, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    let probe = v4::ProjectLinkRecord {
        project: project.into(),
        from: from.into(),
        kind: kind.into(),
        to: to.into(),
        present: false,
    };
    probe.validate().map_err(|_| Rejection::InvalidRequest)?;
    let identity = data_encoding::HEXLOWER.encode(&probe.relation_identity());
    let key = v4::project_hierarchy_key(&project_id, &identity);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let raw = ctx.read_body(&key)?.ok_or(Rejection::StateCorrupt)?;
    let v4::TopologyRecord::Link(record) =
        v4::TopologyRecord::decode_canonical(&raw).map_err(|_| Rejection::StateCorrupt)?
    else {
        return Err(Rejection::StateCorrupt);
    };
    if record.relation_identity() != probe.relation_identity() || record.project != project {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(record))
}

pub(crate) fn read_parent(
    ctx: &Context<'_>,
    project: &str,
    child: &str,
) -> Result<Option<v4::HierarchyRecord>, Rejection> {
    let project_id = ProjectId::parse(project).ok_or(Rejection::InvalidRequest)?;
    DocId::parse(child).ok_or(Rejection::InvalidRequest)?;
    let key = v4::project_hierarchy_key(&project_id, child);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let raw = ctx.read_body(&key)?.ok_or(Rejection::StateCorrupt)?;
    let v4::TopologyRecord::Parent(record) =
        v4::TopologyRecord::decode_canonical(&raw).map_err(|_| Rejection::StateCorrupt)?
    else {
        return Err(Rejection::StateCorrupt);
    };
    if record.project != project || record.child != child {
        return Err(Rejection::StateCorrupt);
    }
    Ok(Some(record))
}

pub(crate) fn write_triage_submission(
    ctx: &Context<'_>,
    item: &TriageItem,
) -> Result<Batch, Rejection> {
    let record = v4::TriageSubmissionRecord {
        triage: item.id.clone(),
        title: item.title.clone(),
        body: item.body.clone(),
        source: item.source.clone(),
        submitted_by: item.submitted_by.clone(),
        timestamp: item.ts,
    };
    let key = v4::space_triage_key(&ctx.principal().space, &record.triage);
    let mut batch = Batch::default();
    batch.immutable_record(
        ctx,
        PhysicalSchema::SpaceTriage,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: ctx.principal().space.as_str().into(),
            record: record.triage.clone(),
        },
        canonical(&v4::TriageRecord::Submission(record))?,
    )?;
    Ok(batch)
}

pub(crate) fn write_triage_decision(
    ctx: &Context<'_>,
    item: &TriageItem,
    accepted_project: Option<&str>,
) -> Result<Batch, Rejection> {
    let mut batch = Batch::default();
    let mut material = Vec::new();
    for part in [
        item.id.as_str(),
        item.outcome.as_str(),
        item.decided_by.as_str(),
        &item.decided_ts.to_string(),
    ] {
        material.extend_from_slice(part.as_bytes());
        material.push(0);
    }
    let decision = data_encoding::HEXLOWER.encode(&blake3::derive_key(
        "lait.issues.triage-decision.v1",
        &material,
    ));
    let outcome = match item.outcome.as_str() {
        "accepted" => v4::TriageOutcome::Accepted,
        "declined" => v4::TriageOutcome::Declined,
        "duplicate" => v4::TriageOutcome::Duplicate,
        _ => return Err(Rejection::InvalidRequest),
    };
    let record = v4::TriageDecisionRecord {
        decision: decision.clone(),
        triage: item.id.clone(),
        outcome,
        decided_by: item.decided_by.clone(),
        timestamp: item.decided_ts,
        project: accepted_project.map(str::to_owned),
        issue: (!item.doc.is_empty()).then(|| item.doc.clone()),
        note: item.note.clone(),
    };
    let key = v4::space_triage_key(&ctx.principal().space, &decision);
    batch.immutable_record(
        ctx,
        PhysicalSchema::SpaceTriage,
        &key,
        v4::RecordBodyIdentityRecord {
            owner: ctx.principal().space.as_str().into(),
            record: decision.clone(),
        },
        canonical(&v4::TriageRecord::Decision(record))?,
    )?;
    let resolution = v4::TriageResolutionRecord {
        triage: item.id.clone(),
        decision,
        resolved_by: item.decided_by.clone(),
        timestamp: item.decided_ts,
    };
    let resolution_identity = resolution.identity();
    let resolution_key = v4::space_triage_key(&ctx.principal().space, &resolution_identity);
    batch.immutable_record(
        ctx,
        PhysicalSchema::SpaceTriage,
        &resolution_key,
        v4::RecordBodyIdentityRecord {
            owner: ctx.principal().space.as_str().into(),
            record: resolution_identity,
        },
        canonical(&v4::TriageRecord::Resolution(resolution))?,
    )?;
    Ok(batch)
}

pub(crate) fn write_initiative(
    ctx: &Context<'_>,
    initiative: &Initiative,
    description: Option<&str>,
) -> Result<Batch, Rejection> {
    let id = crate::ids::InitiativeId::parse(&initiative.id).ok_or(Rejection::InvalidRequest)?;
    let key = v4::initiative_key(&id);
    let mut batch = Batch::default();
    batch.ensure_body(
        ctx,
        PhysicalSchema::Initiative,
        &key,
        initiative.id.as_bytes().to_vec(),
    );
    for (path, value) in [
        (v4::roots::NAME, initiative.name.as_str()),
        (v4::roots::OWNER, initiative.owner.as_str()),
        (v4::roots::HEALTH, initiative.health.as_str()),
        (
            v4::roots::TOMBSTONE,
            if initiative.tombstone { "1" } else { "0" },
        ),
    ] {
        set_register(&mut batch, &key, path, value.as_bytes().to_vec());
    }
    match initiative.target_date {
        Some(target) => set_register(
            &mut batch,
            &key,
            v4::roots::TARGET_DATE,
            target.to_string().into_bytes(),
        ),
        None => clear_register(&mut batch, &key, v4::roots::TARGET_DATE),
    }
    if let Some(description) = description {
        replace_owned_content(
            ctx,
            &mut batch,
            PhysicalSchema::InitiativeContent,
            &v4::initiative_content_key(&id),
            &initiative.id,
            description,
        )?;
    }
    Ok(batch)
}

pub(crate) fn write_team(ctx: &Context<'_>, team: &Team) -> Result<Batch, Rejection> {
    let id = crate::ids::TeamId::parse(&team.id).ok_or(Rejection::InvalidRequest)?;
    let key = v4::team_key(&id);
    let mut batch = Batch::default();
    batch.ensure_body(ctx, PhysicalSchema::Team, &key, team.id.as_bytes().to_vec());
    for (path, value) in [
        (v4::roots::NAME, team.name.as_str()),
        (v4::roots::KEY, team.key.as_str()),
        (v4::roots::ICON, team.icon.as_str()),
        (v4::roots::LEAD, team.lead.as_str()),
        (v4::roots::TOMBSTONE, if team.tombstone { "1" } else { "0" }),
    ] {
        set_register(&mut batch, &key, path, value.as_bytes().to_vec());
    }
    Ok(batch)
}

const MIGRATION_MAX_OPERATIONS: usize = 3_500;
const MIGRATION_MAX_ESTIMATED_BYTES: usize = 700 * 1_024;

/// Preferred-v4 physical sources whose historical representation is copied
/// by a migration phase or constructed as a required projection of a copied
/// record. A newly added preferred source fails this exact comparison until
/// its migrator phase is deliberately added here as well.
const MIGRATION_BACKFILLED_SCHEMAS: &[PhysicalSchema] = &[
    PhysicalSchema::SpaceDirectory,
    PhysicalSchema::SpaceContent,
    PhysicalSchema::ProjectMeta,
    PhysicalSchema::ProjectContent,
    PhysicalSchema::ProjectSchedule,
    PhysicalSchema::ProjectHierarchy,
    PhysicalSchema::ProjectUpdates,
    PhysicalSchema::SpaceTriage,
    PhysicalSchema::IssueComment,
    PhysicalSchema::IssueReaction,
    PhysicalSchema::IssueActivity,
    PhysicalSchema::IssueRelation,
    PhysicalSchema::IssueIdentity,
    PhysicalSchema::IssueMeta,
    PhysicalSchema::IssueTransition,
    PhysicalSchema::BoardBlock,
    PhysicalSchema::BoardLane,
    PhysicalSchema::IssueAttachment,
    PhysicalSchema::IssueCheck,
    PhysicalSchema::Initiative,
    PhysicalSchema::InitiativeContent,
    PhysicalSchema::Team,
    PhysicalSchema::Label,
    PhysicalSchema::EntityRelation,
    PhysicalSchema::RevisionAlias,
    PhysicalSchema::GovernanceRevision,
    PhysicalSchema::GovernanceHeads,
    PhysicalSchema::WorkflowRevision,
    PhysicalSchema::WorkflowHeads,
    PhysicalSchema::SpecRevision,
    PhysicalSchema::SpecHeads,
    PhysicalSchema::SpecObservation,
    PhysicalSchema::BaselineRevision,
    PhysicalSchema::BaselineHeads,
];

pub(crate) fn migration_source_coverage_complete() -> bool {
    let preferred = crate::find::preferred_source_coordinates();
    let mut covered = MIGRATION_BACKFILLED_SCHEMAS
        .iter()
        .copied()
        .map(|schema| (schema.name().to_string(), v4::SCHEMA_VERSION))
        .collect::<BTreeSet<_>>();
    // The anchored description stays in the current Issue Body. Its migrator
    // phase advances the document schema marker and clears scalar title truth
    // after copying that title to IssueMeta.
    covered.insert((
        contract::ISSUE_SCHEMA.to_string(),
        contract::ISSUE_SCHEMA_VERSION,
    ));
    preferred == covered
}

fn migration_marker(ctx: &Context<'_>) -> Result<Option<v4::MigrationMarkerRecord>, Rejection> {
    let key = v4::space_directory_key(&ctx.principal().space);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let view = read_view(ctx, &key)?;
    migration_marker_from_view(&view)
}

fn migration_marker_from_view(
    view: &fabric::CollaborativeView,
) -> Result<Option<v4::MigrationMarkerRecord>, Rejection> {
    view.registers
        .get(v4::roots::MIGRATION)
        .map(|raw| {
            v4::MigrationMarkerRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)
        })
        .transpose()
}

/// Bind a compact signed lifecycle plan to the exact durable checkpoint it
/// was prepared after. A new frozen source resets only its source cursor; the
/// global audit batch remains monotonic across source epochs.
pub(crate) fn validate_migration_plan(
    ctx: &Context<'_>,
    plan: &contract::V4MigrationPlan,
) -> Result<(), Rejection> {
    let previous = migration_marker(ctx)?;
    let durable_batch = previous.as_ref().map_or(0, |marker| marker.batch);
    if plan.previous_batch != durable_batch {
        return Err(Rejection::Conflict);
    }
    if let Some(marker) = previous {
        let same_source =
            marker.publication == plan.source && marker.source_frontier == plan.source_frontier;
        if (same_source && marker.cursor != plan.previous_cursor)
            || (!same_source && !plan.previous_cursor.is_empty())
        {
            return Err(Rejection::Conflict);
        }
    } else if !plan.previous_cursor.is_empty() {
        return Err(Rejection::Conflict);
    }
    Ok(())
}

pub(crate) fn migration_verification(
    ctx: &Context<'_>,
) -> Result<Option<contract::MigrationVerification>, Rejection> {
    let key = v4::space_directory_key(&ctx.principal().space);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let view = read_view(ctx, &key)?;
    let Some(marker) = migration_marker_from_view(&view)? else {
        return Ok(None);
    };
    let (audit_records, entries) = view
        .logs
        .get(v4::roots::MIGRATION_AUDIT)
        .map_or((0, &[][..]), |log| (log.appended, log.entries.as_slice()));
    if entries.len() > usize::try_from(v4::MIGRATION_AUDIT_RECORDS).unwrap_or(usize::MAX) {
        return Err(Rejection::StateCorrupt);
    }
    let mut tail = None;
    for entry in entries {
        let audit = v4::MigrationAuditRecord::decode_canonical(&entry.value)
            .map_err(|_| Rejection::StateCorrupt)?;
        if audit.batch == marker.batch {
            if tail.replace(audit).is_some() {
                return Err(Rejection::StateCorrupt);
            }
        }
    }
    Ok(Some(contract::MigrationVerification {
        batch: marker.batch,
        cursor: marker.cursor.clone(),
        marker_complete: marker.complete,
        audit_records,
        audit_tail_complete: tail.as_ref().is_some_and(|audit| audit.complete),
        audit_tail_matches: tail.as_ref().is_some_and(|audit| {
            audit.migration == marker.migration
                && audit.batch == marker.batch
                && audit.actor == marker.actor
                && audit.last == marker.cursor
                && audit.complete == marker.complete
        }),
        source_coverage_complete: migration_source_coverage_complete(),
        source_snapshot_pinned: marker.source_snapshot_pinned,
        source_publication: marker.publication,
        source_frontier: marker.source_frontier,
    }))
}

#[derive(serde::Deserialize)]
struct LegacyAttachmentRecord {
    id: String,
    name: String,
    #[serde(default)]
    mime: String,
    size: u64,
    by: String,
    #[serde(rename = "ts")]
    timestamp: u64,
    #[serde(default)]
    comment: String,
    /// V3 content-addressed attachments already name a protected descriptor.
    /// Older inline `data_b64` records deliberately fail this decode: a World
    /// migration cannot invent a content-plane descriptor for plaintext bytes.
    content: String,
}

fn migration_digest(domain: &str, parts: &[&[u8]]) -> [u8; 32] {
    let mut material = Vec::new();
    for part in parts {
        material.extend_from_slice(&u64::try_from(part.len()).unwrap_or(u64::MAX).to_be_bytes());
        material.extend_from_slice(part);
    }
    blake3::derive_key(domain, &material)
}

fn migration_comment_at(
    doc: &str,
    coordinate: &str,
    raw: &[u8],
) -> Result<contract::StoredComment, Rejection> {
    let mut comment: contract::StoredComment =
        serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
    if comment.id.is_none() {
        let encoded = serde_json::to_vec(&comment).map_err(|_| Rejection::StateCorrupt)?;
        let digest = migration_digest(
            "lait.issues.migration-comment.v2",
            &[doc.as_bytes(), coordinate.as_bytes(), &encoded],
        );
        let mut id = [0u8; 16];
        id.copy_from_slice(&digest[..16]);
        comment.id = Some(format!("cmt_{}", crockford_128(u128::from_be_bytes(id))));
    }
    comment.node = None;
    comment.parent_node = None;
    Ok(comment)
}

fn migration_issue_id(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<String, Rejection> {
    let doc = view
        .registers
        .get(v4::roots::ISSUE_ID)
        .map(|raw| String::from_utf8_lossy(raw).into_owned())
        .ok_or(Rejection::StateCorrupt)?;
    if contract::issue_key(&doc) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(doc)
}

fn migration_index(raw: &str) -> Result<usize, Rejection> {
    raw.parse::<usize>().map_err(|_| Rejection::StateCorrupt)
}

fn migration_maximal_heads<'a>(
    revisions: impl IntoIterator<Item = (&'a str, &'a [String])>,
) -> Result<BTreeSet<String>, Rejection> {
    let mut ids = BTreeSet::new();
    let mut predecessors = BTreeSet::new();
    for (revision, parents) in revisions {
        if !ids.insert(revision.to_string()) {
            return Err(Rejection::StateCorrupt);
        }
        predecessors.extend(parents.iter().cloned());
    }
    if !predecessors.is_subset(&ids) {
        return Err(Rejection::StateCorrupt);
    }
    Ok(ids.difference(&predecessors).cloned().collect())
}

fn migration_governance_heads(
    view: &fabric::CollaborativeView,
    role: &str,
) -> Result<BTreeSet<String>, Rejection> {
    let mut revisions = BTreeMap::<String, crate::views::StoredRoleRevision>::new();
    for (path, direct) in [("roles", true), ("role_revisions", false)] {
        for (key, raw) in view.maps.get(path).into_iter().flatten() {
            let revision: crate::views::StoredRoleRevision =
                serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
            let selected = if direct {
                key == role
            } else {
                key.rsplit_once('/').is_some_and(|(owner, revision_id)| {
                    owner == role && revision_id == revision.revision_id
                })
            };
            if selected {
                if revision.body.role_id != role {
                    return Err(Rejection::StateCorrupt);
                }
                match revisions.get(&revision.revision_id) {
                    Some(existing) if existing == &revision => {}
                    Some(_) => return Err(Rejection::Conflict),
                    None => {
                        revisions.insert(revision.revision_id.clone(), revision);
                    }
                }
            }
        }
    }
    migration_maximal_heads(
        revisions
            .iter()
            .map(|(id, revision)| (id.as_str(), revision.predecessor_ids.as_slice())),
    )
}

fn migration_workflow_heads(
    view: &fabric::CollaborativeView,
    project: &str,
) -> Result<BTreeSet<String>, Rejection> {
    let mut revisions = BTreeMap::<String, crate::workflow::WorkflowRevision>::new();
    for (key, raw) in view.maps.get("workflow_revisions").into_iter().flatten() {
        let (owner, revision_id) = key.rsplit_once('/').ok_or(Rejection::StateCorrupt)?;
        if owner != project {
            continue;
        }
        let revision: crate::workflow::WorkflowRevision =
            serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
        if revision.revision_id != revision_id
            || revisions
                .insert(revision.revision_id.clone(), revision)
                .is_some()
        {
            return Err(Rejection::StateCorrupt);
        }
    }
    migration_maximal_heads(
        revisions
            .iter()
            .map(|(id, revision)| (id.as_str(), revision.predecessor_ids.as_slice())),
    )
}

/// Migration never participates in ordinary LWW replacement. A destination
/// tuple is created when absent, skipped when byte-identical, and otherwise
/// left untouched behind an explicit conflict.
fn migration_atomic_absent(
    ctx: &Context<'_>,
    key: &BodyKey,
    expected: &[u8],
) -> Result<bool, Rejection> {
    let Some(current) = ctx.read_body(key)? else {
        return if ctx.body_version(key).is_none() {
            Ok(true)
        } else {
            Err(Rejection::StateCorrupt)
        };
    };
    if current.as_ref() == expected {
        Ok(false)
    } else {
        Err(Rejection::Conflict)
    }
}

pub(crate) fn migration_immutable_present(
    ctx: &Context<'_>,
    schema: PhysicalSchema,
    identity: v4::RecordBodyIdentityRecord,
    record: Vec<u8>,
    semantic_field: &str,
    semantic_value: &str,
    semantic_predicates: &[(&str, &str)],
) -> Result<bool, Rejection> {
    let bytes = canonical(&v4::ImmutableRecordEnvelope { identity, record })?;
    let expected_key = v4::immutable_record_key(schema, &bytes);
    let Some(source) =
        exact_record_source_matching(ctx, semantic_field, semantic_value, semantic_predicates)?
    else {
        // A physically present immutable record without its required Corpus
        // fact means the pinned publication is internally inconsistent. It
        // must not be treated as an absent semantic coordinate.
        return if ctx.body_version(&expected_key).is_none()
            && ctx.read_body(&expected_key)?.is_none()
        {
            Ok(false)
        } else {
            Err(Rejection::StateCorrupt)
        };
    };
    let current = ctx.read_body(&source)?.ok_or(Rejection::StateCorrupt)?;
    classify_migration_immutable(
        &expected_key,
        &bytes,
        Some((&source, current.as_ref())),
        false,
    )
}

fn classify_migration_immutable(
    expected_key: &BodyKey,
    expected_bytes: &[u8],
    semantic_source: Option<(&BodyKey, &[u8])>,
    expected_physical_present_without_fact: bool,
) -> Result<bool, Rejection> {
    match semantic_source {
        Some((source, current)) if source == expected_key && current == expected_bytes => Ok(true),
        Some(_) => {
            // The semantic coordinate already exists, but its immutable
            // payload differs. Content addressing gives it another BodyKey;
            // silently writing our expected key would create two truths for
            // one id.
            Err(Rejection::Conflict)
        }
        None if expected_physical_present_without_fact => Err(Rejection::StateCorrupt),
        None => Ok(false),
    }
}

/// Migrator Find intentionally excludes unbounded legacy aggregate Bodies.
/// Hosting currently activates that package for the duration of migration,
/// so ambient reads could otherwise swap from the complete legacy view to a
/// partial v4 projection. Completion stays closed until the host retains the
/// prior view as ambient throughout the bounded backfill.
pub(crate) const fn migration_ambient_view_safe() -> bool {
    false
}

fn migration_window_within_bounds(batch: &Batch) -> bool {
    batch.operations.len() <= MIGRATION_MAX_OPERATIONS
        && batch.estimated_bytes() <= MIGRATION_MAX_ESTIMATED_BYTES
}

/// Install one causally-derived head/issued set only when that set has no
/// destination truth yet. An exact replay is empty; any other existing set is
/// a semantic conflict and is never reconciled by migration-side LWW.
pub(crate) fn migration_exact_set(
    ctx: &Context<'_>,
    schema: PhysicalSchema,
    key: &BodyKey,
    identity: &str,
    registers: &[(&str, &str)],
    root: &str,
    expected: &BTreeSet<String>,
    allow_create: bool,
) -> Result<Batch, Rejection> {
    if expected.len() > v4::MAX_CONCURRENT_HEADS {
        return Err(Rejection::Conflict);
    }
    let exists = ctx.body_version(key).is_some();
    if exists {
        let view = read_view(ctx, key)?;
        if view.registers.get(v4::roots::IDENTITY).map(Vec::as_slice) != Some(identity.as_bytes())
            || registers.iter().any(|(path, value)| {
                view.registers.get(*path).map(Vec::as_slice) != Some(value.as_bytes())
            })
        {
            return Err(Rejection::StateCorrupt);
        }
        if let Some(values) = view.sets.get(root) {
            let current = values
                .iter()
                .map(|value| String::from_utf8(value.clone()).map_err(|_| Rejection::StateCorrupt))
                .collect::<Result<BTreeSet<_>, _>>()?;
            return if &current == expected {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
    } else if !allow_create {
        return Err(Rejection::StateCorrupt);
    }

    let mut batch = Batch::default();
    if !exists {
        batch.ensure_body(ctx, schema, key, identity.as_bytes().to_vec());
        for (path, value) in registers {
            set_register(&mut batch, key, path, value.as_bytes().to_vec());
        }
    }
    for value in expected {
        batch.operation(
            key,
            Op::SetAdd {
                path: root.into(),
                value: value.as_bytes().to_vec(),
            },
        );
    }
    Ok(batch)
}

fn equivalent_migration_base_transition(
    body: &BodyKey,
    issue: &IssueState,
    head: &v4::IssueTransitionRecord,
) -> bool {
    head.predecessors.is_empty()
        && head.placement
            == (v4::BoardPlacement {
                project: issue.project.clone(),
                workflow_state: issue.status.clone(),
                block: v4::board_seed_block_id(&issue.project, &issue.status),
                position: format!("{}V", body.body.render()),
            })
        && head.evidence == "migration"
        && head.timestamp == issue.created_at.max(1)
}

/// Stage exactly one logical fact from one frozen legacy Issue Body.
pub(crate) fn migration_issue_window(
    ctx: &Context<'_>,
    body: &BodyKey,
    subitem: &str,
    view: &fabric::CollaborativeView,
) -> Result<Batch, Rejection> {
    let doc = migration_issue_id(body, view)?;
    let issue = IssueState::from_view(view);
    if issue.project.is_empty() || !contract::valid_text(&issue.description) {
        return Err(Rejection::StateCorrupt);
    }
    if subitem == "$empty" {
        return Ok(Batch::default());
    }
    if subitem == "20:base" {
        let document_current = register(view, "document_schema")
            .parse::<u32>()
            .ok()
            .is_some_and(|version| version >= contract::DOCUMENT_SCHEMA_VERSION);
        let existing_meta = issue_meta_for(ctx, &doc)?;
        if !document_current && !contract::valid_title(&issue.title) {
            return Err(Rejection::StateCorrupt);
        }
        if let Some(meta) = &existing_meta {
            if !document_current
                && (meta.title != issue.title
                    || meta.priority != issue.priority.as_str()
                    || meta.created_by.as_deref()
                        != issue
                            .created_by
                            .as_ref()
                            .map(ToString::to_string)
                            .as_deref()
                    || meta.created_at != issue.created_at
                    || meta.due_at != issue.duedate
                    || meta.estimate != issue.estimate)
            {
                return Err(Rejection::Conflict);
            }
        }
        let mut batch = Batch::default();
        if !document_current {
            batch.operation(
                body,
                Op::RegisterSet {
                    path: v4::roots::ISSUE_ID.into(),
                    value: doc.as_bytes().to_vec(),
                },
            );
            batch.operation(
                body,
                Op::RegisterClear {
                    path: "title".into(),
                },
            );
            batch.operation(
                body,
                Op::RegisterSet {
                    path: "document_schema".into(),
                    value: contract::DOCUMENT_SCHEMA_VERSION.to_string().into_bytes(),
                },
            );
        }
        if existing_meta.is_none() {
            let mut meta = write_issue_meta(ctx, &doc, &issue, false)?;
            // Tombstone truth belongs to the later Catalog coordinate. An
            // absent root projects false and lets that first coordinate create
            // true; an explicit false written by a later preferred restore is
            // therefore distinguishable and conflicts on re-consent.
            meta.operations.retain(|(_, operation)| {
                !matches!(operation, Op::RegisterSet { path, .. } if path == v4::roots::TOMBSTONE)
            });
            batch.absorb(meta);
        }
        // The Issue Body is the sole frozen owner of project/workflow state.
        // Create one provisional authenticated head here; the later Catalog
        // coordinate writes only an exact-head-fenced rank overlay, never a
        // second transition.
        let heads = issue_transition_heads(ctx, &doc)?;
        if heads.is_empty() {
            let mut position = body.body.render();
            position.push('V');
            batch.absorb(
                write_issue_transition(
                    ctx,
                    &doc,
                    &[],
                    &v4::BoardPlacement {
                        project: issue.project.clone(),
                        workflow_state: issue.status.clone(),
                        block: v4::board_seed_block_id(&issue.project, &issue.status),
                        position,
                    },
                    "migration",
                    issue.created_at.max(1),
                )?
                .0,
            );
        } else if let [(.., head)] = heads.as_slice() {
            if equivalent_migration_base_transition(body, &issue, head) {
                // Equivalent replay from a fresh frozen source epoch.
            } else {
                return Err(Rejection::Conflict);
            }
        } else {
            return Err(Rejection::Conflict);
        }
        return Ok(batch);
    }
    if let Some(rest) = subitem.strip_prefix("21:comment:") {
        let mut parts = rest.split(':');
        let kind = parts.next().ok_or(Rejection::StateCorrupt)?;
        let ordinal = migration_index(parts.next().ok_or(Rejection::StateCorrupt)?)?;
        let raw = match kind {
            "list" => {
                &view
                    .lists
                    .get("comments")
                    .and_then(|rows| rows.get(ordinal))
                    .ok_or(Rejection::StateCorrupt)?
                    .value
            }
            "tree" => {
                &view
                    .trees
                    .get("comments")
                    .and_then(|rows| rows.get(ordinal))
                    .ok_or(Rejection::StateCorrupt)?
                    .value
            }
            _ => return Err(Rejection::StateCorrupt),
        };
        let mut comment = migration_comment_at(&doc, subitem, raw)?;
        let id = comment.id.clone().ok_or(Rejection::StateCorrupt)?;
        // Match write_comment's canonical record projection before probing the
        // semantic id. Legacy engine-local node handles never enter v4.
        comment.node = None;
        comment.parent_node = None;
        if migration_immutable_present(
            ctx,
            PhysicalSchema::IssueComment,
            v4::RecordBodyIdentityRecord {
                owner: doc.clone(),
                record: id.clone(),
            },
            canonical(&v4::DiscussionRecord::Comment(comment.clone()))?,
            crate::find::field::ID,
            &id,
            &[
                (crate::find::field::KIND, "comment"),
                (crate::find::field::SOURCE_ID, &doc),
            ],
        )? {
            return Ok(Batch::default());
        }
        return write_comment(ctx, &doc, comment).map(|(batch, _)| batch);
    }
    if let Some(rest) = subitem.strip_prefix("22:reaction:") {
        let (path, ordinal) = rest.rsplit_once(':').ok_or(Rejection::StateCorrupt)?;
        let ordinal = migration_index(ordinal)?;
        let (comment, emoji, actor) = if path == "current" {
            contract::parse_reaction_value(
                view.sets
                    .get(contract::REACTIONS_PATH)
                    .and_then(|values| values.get(ordinal))
                    .ok_or(Rejection::StateCorrupt)?,
            )
            .ok_or(Rejection::StateCorrupt)?
        } else {
            let (emoji, actor) = contract::parse_legacy_reaction_value(
                view.sets
                    .get(&format!("reactions/{path}"))
                    .and_then(|values| values.get(ordinal))
                    .ok_or(Rejection::StateCorrupt)?,
            )
            .ok_or(Rejection::StateCorrupt)?;
            (path.to_string(), emoji, actor)
        };
        let issue_id = DocId::parse(&doc).ok_or(Rejection::StateCorrupt)?;
        let reaction = v4::ReactionRecord {
            issue: doc.clone(),
            comment: comment.clone(),
            emoji: emoji.clone(),
            actor: actor.clone(),
            on: true,
        };
        let key = v4::issue_reaction_key(&issue_id, &reaction.identity());
        let expected = canonical(&v4::DiscussionRecord::Reaction(reaction))?;
        return if migration_atomic_absent(ctx, &key, &expected)? {
            write_reaction(ctx, &doc, &comment, &emoji, &actor, true)
        } else {
            Ok(Batch::default())
        };
    }
    if let Some(rest) = subitem.strip_prefix("23:relation:") {
        let (kind, ordinal) = rest.rsplit_once(':').ok_or(Rejection::StateCorrupt)?;
        let target = if ordinal == "single" {
            let raw = view.registers.get(kind).ok_or(Rejection::StateCorrupt)?;
            String::from_utf8(raw.clone()).map_err(|_| Rejection::StateCorrupt)?
        } else {
            let ordinal = migration_index(ordinal)?;
            let path = match kind {
                "assignee" => "assignees",
                "follower" => "followers",
                "label" => "labels",
                _ => return Err(Rejection::StateCorrupt),
            };
            String::from_utf8(
                view.sets
                    .get(path)
                    .and_then(|values| values.get(ordinal))
                    .ok_or(Rejection::StateCorrupt)?
                    .clone(),
            )
            .map_err(|_| Rejection::StateCorrupt)?
        };
        let issue_id = DocId::parse(&doc).ok_or(Rejection::StateCorrupt)?;
        let record = v4::IssueRelationRecord {
            issue: doc.clone(),
            project: issue.project.clone(),
            kind: kind.into(),
            target: target.clone(),
            present: true,
        };
        let key = v4::issue_relation_key(&issue_id, &record.identity());
        let expected = canonical(&record)?;
        return if migration_atomic_absent(ctx, &key, &expected)? {
            write_issue_relation(ctx, &doc, &issue.project, kind, &target, true)
        } else {
            Ok(Batch::default())
        };
    }
    if let Some(id) = subitem.strip_prefix("24:attachment:") {
        let raw = view
            .maps
            .get("attachments")
            .and_then(|records| records.get(id))
            .ok_or(Rejection::StateCorrupt)?;
        let legacy: LegacyAttachmentRecord =
            serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
        if legacy.id != id {
            return Err(Rejection::StateCorrupt);
        }
        let record = v4::IssueAttachmentRecord {
            issue: doc,
            id: legacy.id,
            name: legacy.name,
            mime: legacy.mime,
            size: legacy.size,
            by: legacy.by,
            timestamp: legacy.timestamp,
            comment: (!legacy.comment.is_empty()).then_some(legacy.comment),
            content: legacy.content,
            tombstone: false,
        };
        let issue = DocId::parse(&record.issue).ok_or(Rejection::StateCorrupt)?;
        let key = v4::issue_attachment_key(&issue, &record.id);
        let expected = canonical(&record)?;
        return if migration_atomic_absent(ctx, &key, &expected)? {
            write_attachment(ctx, &record)
        } else {
            Ok(Batch::default())
        };
    }
    if let Some(run) = subitem.strip_prefix("25:check:") {
        let raw = view
            .maps
            .get("checks")
            .and_then(|records| records.get(run))
            .ok_or(Rejection::StateCorrupt)?;
        let check = serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
        let record = v4::IssueCheckRecord {
            issue: doc,
            run: run.into(),
            check,
        };
        let issue = DocId::parse(&record.issue).ok_or(Rejection::StateCorrupt)?;
        let key = v4::issue_check_key(&issue, &record.run);
        let expected = canonical(&record)?;
        return if migration_atomic_absent(ctx, &key, &expected)? {
            write_check(ctx, &record)
        } else {
            Ok(Batch::default())
        };
    }
    if let Some(rest) = subitem.strip_prefix("26:activity:") {
        let mut parts = rest.split(':');
        let kind = parts.next().ok_or(Rejection::StateCorrupt)?;
        let ordinal = migration_index(parts.next().ok_or(Rejection::StateCorrupt)?)?;
        let raw = match kind {
            "list" => {
                &view
                    .lists
                    .get("events")
                    .and_then(|rows| rows.get(ordinal))
                    .ok_or(Rejection::StateCorrupt)?
                    .value
            }
            "log" => {
                &view
                    .logs
                    .get(contract::EVENTS_PATH)
                    .and_then(|log| log.entries.get(ordinal))
                    .ok_or(Rejection::StateCorrupt)?
                    .value
            }
            _ => return Err(Rejection::StateCorrupt),
        };
        let event: contract::IssueEvent =
            serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
        let record = data_encoding::HEXLOWER.encode(&migration_digest(
            "lait.issues.migration-activity.v2",
            &[doc.as_bytes(), subitem.as_bytes(), raw],
        ));
        return write_activity_record(
            ctx,
            &doc,
            &record,
            &event,
            &migration_activity_recipients(&issue, &event)?,
        );
    }
    Err(Rejection::StateCorrupt)
}

/// Stage exactly one logical fact from the frozen legacy Catalog Body.
///
/// This first slice deliberately contains only the Space record. Additional
/// Catalog coordinates are added as independent subitems; this function must
/// never decode the whole Catalog into `CatalogState` merely to migrate one
/// fact.
pub(crate) fn migration_catalog_window(
    ctx: &Context<'_>,
    subitem: &str,
    view: &fabric::CollaborativeView,
) -> Result<Batch, Rejection> {
    if subitem == "$empty" {
        return Ok(Batch::default());
    }
    if subitem == "00:space" {
        let name = register(view, "name");
        let description = register(view, "description");
        if !contract::valid_name(&name) || !contract::valid_text(&description) {
            return Err(Rejection::StateCorrupt);
        }
        let catalog = CatalogState {
            name: name.clone(),
            description: description.clone(),
            ..CatalogState::default()
        };
        let key = v4::space_directory_key(&ctx.principal().space);
        if ctx.body_version(&key).is_some() {
            let mut current = CatalogState::default();
            apply_space(ctx, &mut current)?;
            return if current.name == name && current.description == description {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
        return write_space(ctx, &catalog, &name, Some(&description));
    }
    fn record<T: serde::de::DeserializeOwned>(
        view: &fabric::CollaborativeView,
        path: &str,
        key: &str,
    ) -> Result<T, Rejection> {
        serde_json::from_slice(
            view.maps
                .get(path)
                .and_then(|entries| entries.get(key))
                .ok_or(Rejection::StateCorrupt)?,
        )
        .map_err(|_| Rejection::StateCorrupt)
    }
    if let Some(id) = subitem.strip_prefix("02:label:") {
        let label: LabelMeta = record(view, "labels", id)?;
        let key = v4::label_key(&crate::ids::LabelId::parse(id).ok_or(Rejection::StateCorrupt)?);
        if ctx.body_version(&key).is_some() {
            let mut current = CatalogState::default();
            apply_label(ctx, &mut current, id)?;
            return if current.labels.get(id) == Some(&label) {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
        return write_label(ctx, &CatalogState::default(), id, &label, false);
    }
    if let Some(key) = subitem.strip_prefix("05:governance:") {
        let revision: crate::views::StoredRoleRevision = record(view, "roles", key)?;
        if revision.body.role_id != key {
            return Err(Rejection::StateCorrupt);
        }
        let stored = v4::GovernanceRevisionRecord {
            role: revision.body.role_id.clone(),
            revision: revision.clone(),
        };
        if migration_immutable_present(
            ctx,
            PhysicalSchema::GovernanceRevision,
            v4::RecordBodyIdentityRecord {
                owner: stored.role.clone(),
                record: stored.revision.revision_id.clone(),
            },
            canonical(&stored)?,
            crate::find::field::REVISION,
            &stored.revision.revision_id,
            &[
                (crate::find::field::KIND, "governance_revision"),
                (crate::find::field::SOURCE_ID, &stored.role),
            ],
        )? {
            return Ok(Batch::default());
        }
        return write_governance_revision_record(ctx, &revision);
    }
    if let Some(key) = subitem.strip_prefix("06:governance:") {
        let revision: crate::views::StoredRoleRevision = record(view, "role_revisions", key)?;
        let (role, revision_id) = key.rsplit_once('/').ok_or(Rejection::StateCorrupt)?;
        if revision.body.role_id != role || revision.revision_id != revision_id {
            return Err(Rejection::StateCorrupt);
        }
        let stored = v4::GovernanceRevisionRecord {
            role: role.into(),
            revision: revision.clone(),
        };
        if migration_immutable_present(
            ctx,
            PhysicalSchema::GovernanceRevision,
            v4::RecordBodyIdentityRecord {
                owner: role.into(),
                record: revision_id.into(),
            },
            canonical(&stored)?,
            crate::find::field::REVISION,
            revision_id,
            &[
                (crate::find::field::KIND, "governance_revision"),
                (crate::find::field::SOURCE_ID, role),
            ],
        )? {
            return Ok(Batch::default());
        }
        return write_governance_revision_record(ctx, &revision);
    }
    if let Some(role) = subitem.strip_prefix("07:governance-heads:") {
        let expected = migration_governance_heads(view, role)?;
        return migration_exact_set(
            ctx,
            PhysicalSchema::GovernanceHeads,
            &v4::governance_heads_key(role),
            role,
            &[],
            v4::roots::HEADS,
            &expected,
            true,
        );
    }
    if let Some(id) = subitem.strip_prefix("10:project:") {
        let meta: ProjectMeta = record(view, "projects", id)?;
        let key = v4::project_meta_key(&ProjectId::parse(id).ok_or(Rejection::StateCorrupt)?);
        if ctx.body_version(&key).is_some() {
            let mut current = CatalogState::default();
            apply_project(ctx, &mut current, id)?;
            return if current.projects.get(id) == Some(&meta) {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
        let description = meta.description.clone();
        return write_project(
            ctx,
            &CatalogState::default(),
            id,
            &meta,
            false,
            Some(&description),
        );
    }
    if let Some(key) = subitem.strip_prefix("11:workflow:") {
        let revision: crate::workflow::WorkflowRevision = record(view, "workflow_revisions", key)?;
        let (project, revision_id) = key.rsplit_once('/').ok_or(Rejection::StateCorrupt)?;
        if revision.revision_id != revision_id {
            return Err(Rejection::StateCorrupt);
        }
        let stored = v4::ProjectWorkflowRevisionRecord {
            project: project.into(),
            revision: revision.clone(),
        };
        if migration_immutable_present(
            ctx,
            PhysicalSchema::WorkflowRevision,
            v4::RecordBodyIdentityRecord {
                owner: project.into(),
                record: revision_id.into(),
            },
            canonical(&stored)?,
            crate::find::field::REVISION,
            revision_id,
            &[
                (crate::find::field::KIND, "workflow_revision"),
                (crate::find::field::SOURCE_ID, project),
            ],
        )? {
            return Ok(Batch::default());
        }
        return write_workflow_revision_record(ctx, project, &revision);
    }
    if let Some(project) = subitem.strip_prefix("11z:workflow-heads:") {
        ProjectId::parse(project).ok_or(Rejection::StateCorrupt)?;
        let expected = migration_workflow_heads(view, project)?;
        return migration_exact_set(
            ctx,
            PhysicalSchema::WorkflowHeads,
            &v4::workflow_heads_key(&ProjectId::parse(project).ok_or(Rejection::StateCorrupt)?),
            project,
            &[(v4::roots::PROJECT, project), (v4::roots::KIND, "workflow")],
            v4::roots::HEADS,
            &expected,
            true,
        );
    }
    if let Some(key) = subitem.strip_prefix("12:milestone:") {
        let milestone: Milestone = record(view, "project_milestones", key)?;
        let expected = format!("{}/{}", milestone.project_id, milestone.id);
        if expected != key {
            return Err(Rejection::StateCorrupt);
        }
        let body = v4::project_schedule_key(
            &ProjectId::parse(&milestone.project_id).ok_or(Rejection::StateCorrupt)?,
            &milestone.id,
        );
        if ctx.body_version(&body).is_some() {
            let mut current = CatalogState::default();
            apply_schedule_record(ctx, &mut current, &milestone.project_id, &milestone.id)?;
            return if current
                .milestones
                .get(&milestone.project_id)
                .and_then(|records| records.get(&milestone.id))
                == Some(&milestone)
            {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
        return write_milestone(ctx, &milestone);
    }
    if let Some(key) = subitem.strip_prefix("13:cycle:") {
        let cycle: Cycle = record(view, "cycles", key)?;
        let expected = format!("{}/{}", cycle.project_id, cycle.id);
        if expected != key {
            return Err(Rejection::StateCorrupt);
        }
        let body = v4::project_schedule_key(
            &ProjectId::parse(&cycle.project_id).ok_or(Rejection::StateCorrupt)?,
            &cycle.id,
        );
        if ctx.body_version(&body).is_some() {
            let mut current = CatalogState::default();
            apply_schedule_record(ctx, &mut current, &cycle.project_id, &cycle.id)?;
            return if current
                .cycles
                .get(&cycle.project_id)
                .and_then(|records| records.get(&cycle.id))
                == Some(&cycle)
            {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
        return write_cycle(ctx, &cycle);
    }
    if let Some(key) = subitem.strip_prefix("40:update:") {
        let update: ProjectUpdate = record(view, "project_updates", key)?;
        let expected = format!("{}/{}", update.project_id, update.id);
        if expected != key {
            return Err(Rejection::StateCorrupt);
        }
        let stored = v4::ProjectUpdateRecord {
            update: update.id.clone(),
            project: update.project_id.clone(),
            author: update.author.clone(),
            timestamp: update.ts,
            body: update.body.clone(),
            health: update.health.clone(),
        };
        if migration_immutable_present(
            ctx,
            PhysicalSchema::ProjectUpdates,
            v4::RecordBodyIdentityRecord {
                owner: stored.project.clone(),
                record: stored.update.clone(),
            },
            canonical(&stored)?,
            crate::find::field::ID,
            &stored.update,
            &[
                (crate::find::field::KIND, "project_update"),
                (crate::find::field::PROJECT, &stored.project),
            ],
        )? {
            return Ok(Batch::default());
        }
        return write_project_update(ctx, &update);
    }
    if let Some(id) = subitem.strip_prefix("50:initiative:") {
        let initiative: Initiative = record(view, "initiatives", id)?;
        if initiative.id != id {
            return Err(Rejection::StateCorrupt);
        }
        let key = v4::initiative_key(
            &crate::ids::InitiativeId::parse(id).ok_or(Rejection::StateCorrupt)?,
        );
        if ctx.body_version(&key).is_some() {
            let mut current = CatalogState::default();
            apply_initiative(ctx, &mut current, id)?;
            let mut expected = initiative.clone();
            expected.projects.clear();
            return if current.initiatives.get(id) == Some(&expected) {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
        let description = initiative.description.clone();
        return write_initiative(ctx, &initiative, Some(&description));
    }
    if let Some(pair) = subitem.strip_prefix("51:initiative-project:") {
        let (id, project) = pair.split_once(':').ok_or(Rejection::StateCorrupt)?;
        let initiative: Initiative = record(view, "initiatives", id)?;
        if initiative.id != id || !initiative.projects.iter().any(|value| value == project) {
            return Err(Rejection::StateCorrupt);
        }
        let record = v4::EntityRelationRecord {
            owner: id.into(),
            kind: "initiative_project".into(),
            target: project.into(),
            present: true,
        };
        let key = v4::entity_relation_key(id, &record.identity());
        let expected = canonical(&record)?;
        return if migration_atomic_absent(ctx, &key, &expected)? {
            write_entity_relation(ctx, id, "initiative_project", project, true)
        } else {
            Ok(Batch::default())
        };
    }
    if let Some(id) = subitem.strip_prefix("60:team:") {
        let team: Team = record(view, "teams", id)?;
        if team.id != id {
            return Err(Rejection::StateCorrupt);
        }
        let key = v4::team_key(&crate::ids::TeamId::parse(id).ok_or(Rejection::StateCorrupt)?);
        if ctx.body_version(&key).is_some() {
            let mut current = CatalogState::default();
            apply_team(ctx, &mut current, id)?;
            let mut expected = team.clone();
            expected.members.clear();
            return if current.teams.get(id) == Some(&expected) {
                Ok(Batch::default())
            } else {
                Err(Rejection::Conflict)
            };
        }
        return write_team(ctx, &team);
    }
    if let Some(pair) = subitem.strip_prefix("61:team-member:") {
        let (id, member) = pair.split_once(':').ok_or(Rejection::StateCorrupt)?;
        let team: Team = record(view, "teams", id)?;
        if team.id != id || !team.members.iter().any(|value| value == member) {
            return Err(Rejection::StateCorrupt);
        }
        let record = v4::EntityRelationRecord {
            owner: id.into(),
            kind: "team_member".into(),
            target: member.into(),
            present: true,
        };
        let key = v4::entity_relation_key(id, &record.identity());
        let expected = canonical(&record)?;
        return if migration_atomic_absent(ctx, &key, &expected)? {
            write_entity_relation(ctx, id, "team_member", member, true)
        } else {
            Ok(Batch::default())
        };
    }
    if let Some(id) = subitem.strip_prefix("70:triage:") {
        let triage: TriageItem = record(view, "triage", id)?;
        if triage.id != id {
            return Err(Rejection::StateCorrupt);
        }
        let stored = v4::TriageSubmissionRecord {
            triage: triage.id.clone(),
            title: triage.title.clone(),
            body: triage.body.clone(),
            source: triage.source.clone(),
            submitted_by: triage.submitted_by.clone(),
            timestamp: triage.ts,
        };
        if migration_immutable_present(
            ctx,
            PhysicalSchema::SpaceTriage,
            v4::RecordBodyIdentityRecord {
                owner: ctx.principal().space.as_str().into(),
                record: stored.triage.clone(),
            },
            canonical(&v4::TriageRecord::Submission(stored))?,
            crate::find::field::ID,
            &triage.id,
            &[
                (crate::find::field::KIND, "triage_fact"),
                (crate::find::field::STATE, "submission"),
            ],
        )? {
            return Ok(Batch::default());
        }
        return write_triage_submission(ctx, &triage);
    }
    Err(Rejection::ContractViolation)
}

fn migration_identity(
    ctx: &Context<'_>,
    doc: &str,
) -> Result<Option<v4::IssueIdentityRecord>, Rejection> {
    let Some(key) = exact_record_source(ctx, crate::find::field::SOURCE_ID, doc, "issue_identity")?
    else {
        return Ok(None);
    };
    let envelope = read_immutable(ctx, &key)?;
    if envelope.identity.owner != doc || envelope.identity.record != "identity" {
        return Err(Rejection::StateCorrupt);
    }
    let identity = v4::IssueIdentityRecord::decode_canonical(&envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    (identity.issue == doc)
        .then_some(identity)
        .ok_or(Rejection::StateCorrupt)
        .map(Some)
}

fn migration_heads(
    ctx: &Context<'_>,
    doc: &str,
) -> Result<(String, v4::IssueTransitionRecord), Rejection> {
    let heads = issue_transition_heads(ctx, doc)?;
    match heads.as_slice() {
        [(transition, record)] => Ok((transition.clone(), record.clone())),
        [] => Err(Rejection::StateCorrupt),
        _ => Err(Rejection::Conflict),
    }
}

fn migration_rank(ordinal: usize) -> String {
    // Fixed-width decimal preserves the legacy list order bytewise; the final
    // non-zero base-62 digit keeps the rank canonical.
    format!("{ordinal:020}V")
}

fn migration_project_from_path(
    view: &fabric::CollaborativeView,
    path: &str,
) -> Result<String, Rejection> {
    let folded = path.strip_prefix("board/").ok_or(Rejection::StateCorrupt)?;
    let projects = view.maps.get("projects").ok_or(Rejection::StateCorrupt)?;
    projects
        .keys()
        .find(|project| project.to_ascii_lowercase() == folded)
        .cloned()
        .ok_or(Rejection::StateCorrupt)
}

/// Stage one Catalog-owned coordinate after every frozen Issue base has been
/// copied. The only placement mutation is an exact-transition-fenced rank
/// overlay; workflow intent remains the authenticated Issue transition.
pub(crate) fn migration_coordinate_window(
    ctx: &Context<'_>,
    subitem: &str,
    view: &fabric::CollaborativeView,
) -> Result<Batch, Rejection> {
    if subitem == "$empty" {
        return Ok(Batch::default());
    }
    if let Some(doc) = subitem.strip_prefix("14:identity:") {
        let issue = DocId::parse(doc).ok_or(Rejection::StateCorrupt)?;
        let ordinal = view
            .maps
            .get("seqs")
            .and_then(|entries| entries.get(doc))
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .and_then(|raw| raw.parse::<u64>().ok())
            .ok_or(Rejection::StateCorrupt)?;
        let expected = v4::IssueIdentityRecord {
            issue: doc.into(),
            alias: v4::IssueAliasCoordinate::for_issue(ordinal, &issue)
                .map_err(|_| Rejection::StateCorrupt)?,
        };
        match migration_identity(ctx, doc)? {
            Some(existing) if existing == expected => return Ok(Batch::default()),
            Some(_) => return Err(Rejection::Conflict),
            None => {}
        }
        let mut batch = Batch::default();
        write_issue_identity(ctx, &mut batch, doc, ordinal)?;
        return Ok(batch);
    }
    if let Some(doc) = subitem.strip_prefix("15:tombstone:") {
        let raw = view
            .maps
            .get("tombstones")
            .and_then(|entries| entries.get(doc))
            .ok_or(Rejection::StateCorrupt)?;
        if raw.as_slice() != b"1" {
            return Err(Rejection::StateCorrupt);
        }
        let issue = DocId::parse(doc).ok_or(Rejection::StateCorrupt)?;
        let key = v4::issue_meta_key(&issue);
        if ctx.body_version(&key).is_none() {
            return Err(Rejection::StateCorrupt);
        }
        let meta = read_view(ctx, &key)?;
        match meta.registers.get(v4::roots::TOMBSTONE) {
            Some(raw) if raw.as_slice() == b"1" => return Ok(Batch::default()),
            Some(_) => return Err(Rejection::Conflict),
            None => {}
        }
        let mut batch = Batch::default();
        set_register(&mut batch, &key, v4::roots::TOMBSTONE, b"1".to_vec());
        return Ok(batch);
    }
    if let Some(rest) = subitem.strip_prefix("16:board:") {
        let (path, ordinal) = rest.rsplit_once(':').ok_or(Rejection::StateCorrupt)?;
        let ordinal = migration_index(ordinal)?;
        let entry = view
            .lists
            .get(path)
            .and_then(|entries| entries.get(ordinal))
            .ok_or(Rejection::StateCorrupt)?;
        let doc = std::str::from_utf8(&entry.value).map_err(|_| Rejection::StateCorrupt)?;
        DocId::parse(doc).ok_or(Rejection::StateCorrupt)?;
        let project = migration_project_from_path(view, path)?;
        let (transition, head) = migration_heads(ctx, doc)?;
        if head.placement.project != project {
            return Err(Rejection::Conflict);
        }
        let overlay = v4::IssueRankOverlay {
            issue: doc.into(),
            transition,
            project: head.placement.project.clone(),
            workflow_state: head.placement.workflow_state.clone(),
            block: head.placement.block.clone(),
            position: migration_rank(ordinal),
            maintenance: data_encoding::HEXLOWER.encode(&migration_digest(
                "lait.issues.migration-rank-overlay.v1",
                &[subitem.as_bytes()],
            )),
        };
        match issue_rank_overlay(ctx, doc)? {
            Some(existing) if existing == overlay => return Ok(Batch::default()),
            Some(_) => return Err(Rejection::Conflict),
            None => {}
        }
        return write_issue_rank_overlay(ctx, &overlay);
    }
    let (child, parent) = if let Some(child) = subitem.strip_prefix("30:map:") {
        let parent = view
            .maps
            .get("parents")
            .and_then(|entries| entries.get(child))
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .filter(|parent| !parent.is_empty())
            .ok_or(Rejection::StateCorrupt)?;
        (child.to_string(), Some(parent.to_string()))
    } else if let Some(raw) = subitem.strip_prefix("30:tree:") {
        let ordinal = migration_index(raw)?;
        let nodes = view
            .trees
            .get(contract::HIERARCHY_PATH)
            .ok_or(Rejection::StateCorrupt)?;
        let node = nodes.get(ordinal).ok_or(Rejection::StateCorrupt)?;
        let child = node.anchor.clone().ok_or(Rejection::StateCorrupt)?;
        let parent = node.parent.as_deref().and_then(|parent_node| {
            nodes
                .iter()
                .find(|candidate| candidate.node == parent_node)
                .and_then(|candidate| candidate.anchor.clone())
        });
        (child, parent)
    } else {
        (String::new(), None)
    };
    if !child.is_empty() {
        let (_, head) = migration_heads(ctx, &child)?;
        return match read_parent(ctx, &head.placement.project, &child)? {
            Some(existing) if existing.parent == parent => Ok(Batch::default()),
            Some(_) => Err(Rejection::Conflict),
            None => write_parent(ctx, &head.placement.project, &child, parent),
        };
    }
    if let Some(edge) = subitem.strip_prefix("31:link:") {
        if !view
            .maps
            .get("edges")
            .is_some_and(|entries| entries.contains_key(edge))
        {
            return Err(Rejection::StateCorrupt);
        }
        let mut parts = edge.splitn(3, '|');
        let (Some(from), Some(kind), Some(to)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(Rejection::StateCorrupt);
        };
        let (_, head) = migration_heads(ctx, from)?;
        return match read_link(ctx, &head.placement.project, from, kind, to)? {
            Some(existing) if existing.present => Ok(Batch::default()),
            Some(_) => Err(Rejection::Conflict),
            None => write_link(ctx, &head.placement.project, from, kind, to, true),
        };
    }
    if let Some(id) = subitem.strip_prefix("71:triage-decision:") {
        let raw = view
            .maps
            .get("triage")
            .and_then(|entries| entries.get(id))
            .ok_or(Rejection::StateCorrupt)?;
        let triage: TriageItem =
            serde_json::from_slice(raw).map_err(|_| Rejection::StateCorrupt)?;
        if triage.id != id || triage.outcome.is_empty() {
            return Err(Rejection::StateCorrupt);
        }
        let accepted_project = if triage.outcome == "accepted" {
            let (_, head) = migration_heads(ctx, &triage.doc)?;
            Some(head.placement.project)
        } else {
            None
        };
        return write_triage_decision(ctx, &triage, accepted_project.as_deref());
    }
    Err(Rejection::ContractViolation)
}

/// Attach one migrated fact to its monotone cursor and audit coordinate.
/// Output, cursor, and audit are committed atomically; terminal completion is
/// intentionally impossible until every source family has a bounded handler
/// and terminal lookahead tests prove exhaustion of the frozen source.
pub(crate) fn finalize_migration_window(
    ctx: &Context<'_>,
    plan: &contract::V4MigrationPlan,
    mut out: Batch,
) -> Result<Batch, Rejection> {
    let actor = ctx.principal().actor.as_str();
    if crate::ids::ActorId::parse(actor).is_none() || plan.timestamp == 0 {
        return Err(Rejection::InvalidRequest);
    }
    let complete = plan.window.terminal()
        && migration_source_coverage_complete()
        && migration_ambient_view_safe();
    if plan.window.terminal() && !complete {
        return Err(Rejection::Conflict);
    }
    if !migration_window_within_bounds(&out) {
        return Err(Rejection::LimitExceeded);
    }
    let previous = migration_marker(ctx)?;
    let batch_number = previous
        .as_ref()
        .map_or(1, |marker| marker.batch.saturating_add(1));
    if batch_number != plan.previous_batch.saturating_add(1) {
        return Err(Rejection::Conflict);
    }
    let started_at = previous
        .as_ref()
        .map_or(plan.timestamp, |marker| marker.started_at);
    let directory = ensure_directory(ctx, &CatalogState::default(), &mut out)?;
    let marker = v4::MigrationMarkerRecord {
        migration: v4::MIGRATION_V3_TO_V4.into(),
        source_version: 3,
        target_version: 4,
        publication: plan.source,
        source_frontier: plan.source_frontier,
        source_snapshot_pinned: complete,
        batch: batch_number,
        cursor: plan.window.cursor.clone(),
        complete,
        actor: actor.into(),
        started_at,
        updated_at: plan.timestamp,
    };
    let operations = u32::try_from(out.operations.len().saturating_add(2))
        .map_err(|_| Rejection::LimitExceeded)?;
    let audit = v4::MigrationAuditRecord {
        migration: v4::MIGRATION_V3_TO_V4.into(),
        batch: batch_number,
        actor: actor.into(),
        timestamp: plan.timestamp,
        first: plan.window.cursor.clone(),
        last: plan.window.cursor.clone(),
        items: 1,
        operations,
        complete,
    };
    set_register(
        &mut out,
        &directory,
        v4::roots::MIGRATION,
        canonical(&marker)?,
    );
    out.operation(
        &directory,
        Op::LogAppend {
            path: v4::roots::MIGRATION_AUDIT.into(),
            value: canonical(&audit)?,
            retain: v4::MIGRATION_AUDIT_RECORDS,
        },
    );
    if !migration_window_within_bounds(&out) {
        return Err(Rejection::LimitExceeded);
    }
    Ok(out)
}

fn migration_activity_recipients(
    issue: &IssueState,
    event: &contract::IssueEvent,
) -> Result<Vec<String>, Rejection> {
    if event.inbox_kind().is_none() {
        return Ok(Vec::new());
    }
    let mut recipients = issue
        .assignees
        .iter()
        .chain(&issue.followers)
        .map(|actor| actor.as_str().to_string())
        .collect::<BTreeSet<_>>();
    if event.k == "assigned" {
        recipients.extend(
            event
                .c
                .iter()
                .filter(|change| change.f == "assignees")
                .filter_map(|change| change.to.clone()),
        );
    }
    if recipients.len() > contract::MAX_ISSUE_AUDIENCE {
        return Err(Rejection::StateCorrupt);
    }
    Ok(recipients.into_iter().collect())
}

/// Independently addressable facts formerly co-located with one Issue Body.
/// Each returned item has its own stable cursor coordinate, so a large thread
/// cannot force one migration transaction over the product byte/operation
/// ceilings.
#[cfg(test)]
mod migration_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    struct MigrationStatusReader {
        directory: BodyKey,
        view: fabric::CollaborativeView,
        reads: AtomicUsize,
    }

    impl runtime::world::BodyReader for MigrationStatusReader {
        fn read_body(
            &self,
            _key: &BodyKey,
        ) -> Result<Option<runtime::world::BodyBytes>, runtime::world::BodyReadFailure> {
            panic!("migration status must not open atomic product state")
        }

        fn read_collaborative_body(
            &self,
            key: &BodyKey,
        ) -> Result<Option<runtime::world::CollaborativeBody>, runtime::world::BodyReadFailure>
        {
            assert_eq!(key, &self.directory);
            self.reads.fetch_add(1, Ordering::SeqCst);
            Ok(Some(runtime::world::CollaborativeBody::owned(
                self.view.clone(),
            )))
        }

        fn bodies_with_schema(
            &self,
            _world: &replica::body::WorldId,
            _schema: &replica::body::SchemaId,
        ) -> Vec<BodyKey> {
            panic!("migration status must not enumerate tracker Bodies")
        }

        fn body_version(&self, key: &BodyKey) -> Option<fabric::Version> {
            (key == &self.directory).then(fabric::Version::empty)
        }

        fn anchor_in_body(
            &self,
            _key: &BodyKey,
            _path: &str,
            _position: u64,
        ) -> Result<Option<fabric::Anchor>, runtime::world::BodyReadFailure> {
            panic!("migration status must not resolve anchors")
        }

        fn resolve_anchor(
            &self,
            _key: &BodyKey,
            _anchor: &fabric::Anchor,
        ) -> Result<fabric::AnchorResolution, runtime::world::BodyReadFailure> {
            panic!("migration status must not resolve anchors")
        }

        fn content_status(
            &self,
            _content: &replica::content::ContentRef,
        ) -> Option<runtime::world::ContentStatus> {
            panic!("migration status must not inspect content")
        }
    }

    fn principal(actor: &crate::ids::ActorId) -> runtime::world::PrincipalFacts {
        let device = mechanics::actor::device_from_seed(&[7u8; 32]);
        runtime::world::PrincipalFacts {
            actor: actor.clone(),
            station: mechanics::station::Key::from_device(&device).expect("test station"),
            device,
            space: mechanics::ids::SpaceId::from_digest([8u8; 16]),
            authority_frontier: replica::frontier::AuthorityFrontier::from_canonical_bytes(vec![]),
        }
    }

    #[test]
    fn migration_status_reads_only_the_bounded_marker_and_audit_body() {
        let actor = crate::ids::ActorId::from_incept_hash(&"cd".repeat(32));
        let facts = principal(&actor);
        let cursor = contract::V4MigrationWindow::render_cursor(
            contract::V4MigrationWindow::TERMINAL_PHASE,
            None,
            "",
        )
        .expect("terminal cursor");
        let publication = runtime::publication::PublicationId::new(
            [1; 32],
            [2; 32],
            runtime::publication::ExtractorSchemaDigest::from_digest([3; 32]),
        );
        let frontier = replica::frontier::ReplicaFrontier::new([4; 32], 7);
        let marker = v4::MigrationMarkerRecord {
            migration: v4::MIGRATION_V3_TO_V4.into(),
            source_version: 3,
            target_version: 4,
            publication,
            source_frontier: frontier,
            source_snapshot_pinned: true,
            batch: 9,
            cursor: cursor.clone(),
            complete: true,
            actor: actor.as_str().into(),
            started_at: 10,
            updated_at: 11,
        };
        let audit = v4::MigrationAuditRecord {
            migration: v4::MIGRATION_V3_TO_V4.into(),
            batch: 9,
            actor: actor.as_str().into(),
            timestamp: 11,
            first: cursor.clone(),
            last: cursor.clone(),
            items: 1,
            operations: 2,
            complete: true,
        };
        let mut view = fabric::CollaborativeView::default();
        view.registers.insert(
            v4::roots::MIGRATION.into(),
            canonical(&marker).expect("marker"),
        );
        view.logs.insert(
            v4::roots::MIGRATION_AUDIT.into(),
            fabric::LogView {
                entries: vec![fabric::ListElement {
                    element: "tail".into(),
                    value: canonical(&audit).expect("audit"),
                }],
                // This is the exact historical count, not work the status
                // query visits. Only the retained tail above is decoded.
                appended: 1_000_000,
            },
        );
        let reader = MigrationStatusReader {
            directory: v4::space_directory_key(&facts.space),
            view,
            reads: AtomicUsize::new(0),
        };
        let ctx = Context::with_reads(&facts, &reader, [0; 32]);
        let verification = migration_verification(&ctx)
            .expect("bounded status")
            .expect("marker exists");
        assert!(verification.verified());
        assert_eq!(verification.audit_records, 1_000_000);
        assert_eq!(reader.reads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn same_semantic_immutable_id_with_different_bytes_is_a_conflict() {
        let doc = DocId::from_digest([0x31; 16]).as_str().to_string();
        let id = "cmt_00000000000000000000000000".to_string();
        let identity = v4::RecordBodyIdentityRecord {
            owner: doc,
            record: id.clone(),
        };
        let comment = |body: &str| contract::StoredComment {
            a: crate::ids::ActorId::from_incept_hash(&"32".repeat(32))
                .as_str()
                .into(),
            t: 7,
            b: body.into(),
            id: Some(id.clone()),
            parent: None,
            at: None,
            node: None,
            parent_node: None,
        };
        let expected = canonical(&v4::ImmutableRecordEnvelope {
            identity: identity.clone(),
            record: canonical(&v4::DiscussionRecord::Comment(comment("expected")))
                .expect("expected comment"),
        })
        .expect("expected envelope");
        let conflicting = canonical(&v4::ImmutableRecordEnvelope {
            identity,
            record: canonical(&v4::DiscussionRecord::Comment(comment("different")))
                .expect("conflicting comment"),
        })
        .expect("conflicting envelope");
        let expected_key = v4::immutable_record_key(PhysicalSchema::IssueComment, &expected);
        let conflicting_key = v4::immutable_record_key(PhysicalSchema::IssueComment, &conflicting);

        assert_ne!(
            expected_key, conflicting_key,
            "content addressing must differ"
        );
        assert_eq!(
            classify_migration_immutable(
                &expected_key,
                &expected,
                Some((&expected_key, &expected)),
                false,
            ),
            Ok(true),
            "an exact semantic replay is quiet"
        );
        assert!(matches!(
            classify_migration_immutable(
                &expected_key,
                &expected,
                Some((&conflicting_key, &conflicting)),
                false,
            ),
            Err(Rejection::Conflict)
        ));
        assert_eq!(
            classify_migration_immutable(&expected_key, &expected, None, false),
            Ok(false),
            "a genuinely absent semantic coordinate may be created"
        );
        assert!(matches!(
            classify_migration_immutable(&expected_key, &expected, None, true),
            Err(Rejection::StateCorrupt)
        ));
    }

    #[test]
    fn delayed_source_epoch_keeps_global_batch_and_resets_only_its_cursor() {
        let actor = crate::ids::ActorId::from_incept_hash(&"61".repeat(32));
        let facts = principal(&actor);
        let old_source = runtime::publication::PublicationId::new(
            [0x62; 32],
            [0x63; 32],
            runtime::publication::ExtractorSchemaDigest::from_digest([0x64; 32]),
        );
        let new_source = runtime::publication::PublicationId::new(
            [0x65; 32],
            [0x66; 32],
            runtime::publication::ExtractorSchemaDigest::from_digest([0x67; 32]),
        );
        let frontier = replica::frontier::ReplicaFrontier::new([0x68; 32], 8);
        let marker = v4::MigrationMarkerRecord {
            migration: v4::MIGRATION_V3_TO_V4.into(),
            source_version: 3,
            target_version: 4,
            publication: old_source,
            source_frontier: frontier,
            source_snapshot_pinned: false,
            batch: 41,
            cursor: "m1:issue:aaaaaaaaaaaaaaaaaaaaaaaaaa:MjA6YmFzZQ".into(),
            complete: false,
            actor: actor.as_str().into(),
            started_at: 1,
            updated_at: 2,
        };
        let mut view = fabric::CollaborativeView::default();
        view.registers.insert(
            v4::roots::MIGRATION.into(),
            canonical(&marker).expect("marker"),
        );
        let reader = MigrationStatusReader {
            directory: v4::space_directory_key(&facts.space),
            view,
            reads: AtomicUsize::new(0),
        };
        let ctx = Context::with_reads(&facts, &reader, [0x69; 32]);
        let body = contract::issue_key(&DocId::from_digest([0x6a; 16]).as_str());
        let cursor = contract::V4MigrationWindow::render_cursor("issue", Some(&body), "20:base")
            .expect("cursor");
        let plan = contract::V4MigrationPlan {
            version: contract::V4MigrationPlan::VERSION,
            source: new_source,
            source_frontier: frontier,
            previous_batch: 41,
            previous_cursor: String::new(),
            window: contract::V4MigrationWindow {
                phase: "issue".into(),
                body: Some(body),
                subitem: "20:base".into(),
                digest: [0x6b; 32],
                cursor,
            },
            timestamp: 3,
        };
        assert!(validate_migration_plan(&ctx, &plan).is_ok());

        let mut reset_batch = plan.clone();
        reset_batch.previous_batch = 0;
        assert!(matches!(
            validate_migration_plan(&ctx, &reset_batch),
            Err(Rejection::Conflict)
        ));
        let mut stale_cursor = plan;
        stale_cursor.previous_cursor = marker.cursor;
        assert!(matches!(
            validate_migration_plan(&ctx, &stale_cursor),
            Err(Rejection::Conflict)
        ));
    }

    #[test]
    fn delayed_equivalent_transition_is_quiet_but_a_preferred_move_conflicts() {
        let doc = DocId::from_digest([0x71; 16]).as_str().to_string();
        let project = ProjectId::from_digest([0x72; 16]).as_str().to_string();
        let body = contract::issue_key(&doc);
        let issue = IssueState {
            project: project.clone(),
            status: "backlog".into(),
            created_at: 9,
            ..IssueState::default()
        };
        let expected_placement = v4::BoardPlacement {
            project: project.clone(),
            workflow_state: "backlog".into(),
            block: v4::board_seed_block_id(&project, "backlog"),
            position: format!("{}V", body.body.render()),
        };
        let replay = v4::IssueTransitionRecord {
            issue: doc.clone(),
            predecessors: Vec::new(),
            placement: expected_placement.clone(),
            actor: crate::ids::ActorId::from_incept_hash(&"73".repeat(32))
                .as_str()
                .into(),
            timestamp: 9,
            evidence: "migration".into(),
        };
        assert!(equivalent_migration_base_transition(&body, &issue, &replay));

        let mut preferred_move = replay.clone();
        preferred_move.predecessors = vec!["74".repeat(32)];
        preferred_move.placement.workflow_state = "done".into();
        preferred_move.evidence = "user".into();
        assert!(!equivalent_migration_base_transition(
            &body,
            &issue,
            &preferred_move
        ));

        let mut differing_scalar = replay;
        differing_scalar.placement = v4::BoardPlacement {
            project,
            workflow_state: "active".into(),
            block: "75".repeat(32),
            position: "U".into(),
        };
        assert!(!equivalent_migration_base_transition(
            &body,
            &issue,
            &differing_scalar
        ));
    }

    #[test]
    fn terminal_activation_remains_closed_while_migrator_reads_are_partial() {
        assert!(migration_source_coverage_complete());
        assert!(!migration_ambient_view_safe());
    }

    #[test]
    fn one_window_cannot_exceed_the_operation_or_byte_envelope() {
        let key = BodyKey::new(
            contract::world_id(),
            replica::body::BodyId::from_bytes([0x52; 16]),
        );
        let mut operations = Batch::default();
        for ordinal in 0..=MIGRATION_MAX_OPERATIONS {
            operations.operation(
                &key,
                Op::RegisterSet {
                    path: format!("bounded/{ordinal}"),
                    value: vec![0],
                },
            );
        }
        assert!(!migration_window_within_bounds(&operations));

        let mut bytes = Batch::default();
        bytes.operation(
            &key,
            Op::ReplaceAtomic {
                value: vec![0; MIGRATION_MAX_ESTIMATED_BYTES.saturating_add(1)],
            },
        );
        assert!(!migration_window_within_bounds(&bytes));
        assert!(migration_window_within_bounds(&Batch::default()));
    }

    #[test]
    fn final_head_projection_is_causal_when_child_id_sorts_before_parent() {
        let role = "role.example";
        let parent = "ff";
        let child = "00";
        let role_body = crate::roles::RoleBody {
            role_id: role.into(),
            scope_kind: crate::roles::ScopeKind::Space,
            name: "Example".into(),
            description: String::new(),
            capabilities: Vec::new(),
            tombstone: false,
        };
        let mut catalog = fabric::CollaborativeView::default();
        for revision in [
            crate::views::StoredRoleRevision {
                revision_id: parent.into(),
                predecessor_ids: Vec::new(),
                body: role_body.clone(),
            },
            crate::views::StoredRoleRevision {
                revision_id: child.into(),
                predecessor_ids: vec![parent.into()],
                body: role_body,
            },
        ] {
            catalog
                .maps
                .entry("role_revisions".into())
                .or_default()
                .insert(
                    format!("{role}/{}", revision.revision_id),
                    serde_json::to_vec(&revision).expect("role revision"),
                );
        }
        assert_eq!(
            migration_governance_heads(&catalog, role).expect("causal heads"),
            BTreeSet::from([child.to_string()])
        );

        let project = ProjectId::from_digest([0x53; 16]).as_str().to_string();
        let workflow_body = crate::workflow::WorkflowBody {
            project_id: project.clone(),
            name: "Workflow".into(),
            states: Vec::new(),
            transitions: Vec::new(),
            tombstone: false,
        };
        for revision in [
            crate::workflow::WorkflowRevision {
                revision_id: parent.into(),
                predecessor_ids: Vec::new(),
                body: workflow_body.clone(),
            },
            crate::workflow::WorkflowRevision {
                revision_id: child.into(),
                predecessor_ids: vec![parent.into()],
                body: workflow_body,
            },
        ] {
            catalog
                .maps
                .entry("workflow_revisions".into())
                .or_default()
                .insert(
                    format!("{project}/{}", revision.revision_id),
                    serde_json::to_vec(&revision).expect("workflow revision"),
                );
        }
        assert_eq!(
            migration_workflow_heads(&catalog, &project).expect("causal workflow heads"),
            BTreeSet::from([child.to_string()])
        );
    }
}
