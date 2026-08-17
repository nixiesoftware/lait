//! Transitional v3/v4 projection and v4 write helpers.
//!
//! The durable rule is simple: a v4 record, when present, owns the fact it
//! names. Legacy Catalog state fills only facts which have not been
//! materialized yet. This lets migration advance in bounded transactions while
//! every publication remains readable. New writes use the same helpers as the
//! migrator, so human and agent access paths cannot create different shapes.

use std::collections::{BTreeMap, BTreeSet};

use replica::body::{BodyKey, Op};
use runtime::world::{BodyDeclaration, Context, Rejection};

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
#[derive(Debug, Clone, Default)]
pub(crate) struct Batch {
    pub operations: Vec<(BodyKey, Op)>,
    pub bodies: Vec<BodyKey>,
    pub declarations: Vec<BodyDeclaration>,
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
            // Atomic Bodies are born by ReplaceAtomic. Op::Create is a
            // collaborative-list birth and Runtime refuses it on an atomic
            // binding — which is how initialize_tracker was dying.
            if schema.atomic() {
                if !self.bodies.contains(key) {
                    self.bodies.push(key.clone());
                }
            } else {
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
        key: &BodyKey,
        identity: v4::RecordBodyIdentityRecord,
        record: Vec<u8>,
    ) -> Result<(), Rejection> {
        if !schema.immutable() {
            return Err(Rejection::ContractViolation);
        }
        let bytes = canonical(&v4::ImmutableRecordEnvelope { identity, record })?;
        if let Some(existing) = ctx.read_body(key) {
            return if existing == bytes {
                Ok(())
            } else {
                Err(Rejection::Conflict)
            };
        }
        if ctx.body_version(key).is_some() {
            return Err(Rejection::StateCorrupt);
        }
        self.create(schema, key);
        self.operation(key, Op::ReplaceAtomic { value: bytes });
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

fn read_view(ctx: &Context<'_>, key: &BodyKey) -> Result<fabric::CollaborativeView, Rejection> {
    ctx.read_collaborative(key)
        .map_err(|_| Rejection::StateCorrupt)
}

fn read_immutable(
    ctx: &Context<'_>,
    key: &BodyKey,
) -> Result<v4::ImmutableRecordEnvelope, Rejection> {
    let bytes = ctx.read_body(key).ok_or(Rejection::StateCorrupt)?;
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
        let identity = v4::IssueIdentityRecord::decode_canonical(&envelope.record)
            .map_err(|_| Rejection::StateCorrupt)?;
        let issue = DocId::parse(&identity.issue).ok_or(Rejection::StateCorrupt)?;
        if envelope.identity.owner != identity.issue
            || envelope.identity.record != "identity"
            || v4::issue_identity_key(&issue) != key
        {
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
    let identity_key = v4::issue_identity_key(&issue);
    let placement_key = v4::issue_placement_key(&issue);
    let identity_present = ctx.body_version(&identity_key).is_some();
    let placement_present = ctx.body_version(&placement_key).is_some();
    if !identity_present && !placement_present {
        return Ok(None);
    }
    if !identity_present || !placement_present {
        return Err(Rejection::StateCorrupt);
    }
    let identity_envelope = read_immutable(ctx, &identity_key)?;
    let identity = v4::IssueIdentityRecord::decode_canonical(&identity_envelope.record)
        .map_err(|_| Rejection::StateCorrupt)?;
    let placement_bytes = ctx
        .read_body(&placement_key)
        .ok_or(Rejection::StateCorrupt)?;
    let placement_record = v4::IssuePlacementRecord::decode_canonical(&placement_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    if identity.issue != doc
        || identity_envelope.identity.owner != doc
        || identity_envelope.identity.record != "identity"
        || placement_record.issue != doc
    {
        return Err(Rejection::StateCorrupt);
    }
    let meta = issue_meta_for(ctx, doc)?;
    Ok(Some(IssueCoordinate {
        identity,
        placement: placement_record.placement,
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
        let envelope = read_immutable(ctx, &key)?;
        let identity = envelope.identity;
        if v4::governance_revision_key(&identity.owner, &identity.record) != key {
            return Err(Rejection::StateCorrupt);
        }
        let record = v4::GovernanceRevisionRecord::decode_canonical(&envelope.record)
            .map_err(|_| Rejection::StateCorrupt)?;
        if record.role != identity.owner || record.revision.revision_id != identity.record {
            return Err(Rejection::StateCorrupt);
        }
        if crate::roles::BUILT_IN_ROLE_IDS.contains(&record.role.as_str()) {
            catalog.roles.insert(record.role, record.revision);
        } else {
            custom.entry(record.role).or_default().push(record.revision);
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
        description: view
            .texts
            .get(v4::roots::DESCRIPTION)
            .cloned()
            .unwrap_or_default(),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    catalog.name = record.name;
    catalog.description = record.description;
    Ok(())
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
        description: view
            .texts
            .get(v4::roots::DESCRIPTION)
            .cloned()
            .unwrap_or_default(),
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
    let project_id = ProjectId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
    if v4::workflow_revision_key(&project_id, &identity.record) != *key {
        return Err(Rejection::StateCorrupt);
    }
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
        let view = read_view(ctx, &key)?;
        let identity = view
            .registers
            .get(v4::roots::IDENTITY)
            .ok_or(Rejection::StateCorrupt)?;
        let identity = v4::RecordBodyIdentityRecord::decode_canonical(identity)
            .map_err(|_| Rejection::StateCorrupt)?;
        let project = identity.owner;
        let project_id = ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
        if v4::project_schedule_key(&project_id, &identity.record) != key {
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
                            project_id: project.clone(),
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
                        project_id: project.clone(),
                        name,
                        start,
                        end,
                        tombstone,
                    },
                );
            }
        }
    }
    Ok(())
}

fn apply_hierarchy(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::ProjectHierarchy) {
        let raw = ctx.read_body(&key).ok_or(Rejection::StateCorrupt)?;
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
        let project_id = ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
        if v4::project_updates_key(&project_id, &identity.record) != key {
            return Err(Rejection::StateCorrupt);
        }
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
        let view = read_view(ctx, &key)?;
        let id = register(&view, v4::roots::IDENTITY);
        let parsed = crate::ids::InitiativeId::parse(&id).ok_or(Rejection::StateCorrupt)?;
        if v4::initiative_key(&parsed) != key {
            return Err(Rejection::StateCorrupt);
        }
        let record = v4::InitiativeRecord {
            initiative: id.clone(),
            name: register(&view, v4::roots::NAME),
            description: view
                .texts
                .get(v4::roots::DESCRIPTION)
                .cloned()
                .unwrap_or_default(),
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
    }
    Ok(())
}

fn apply_teams(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::Team) {
        let view = read_view(ctx, &key)?;
        let id = register(&view, v4::roots::IDENTITY);
        let parsed = crate::ids::TeamId::parse(&id).ok_or(Rejection::StateCorrupt)?;
        if v4::team_key(&parsed) != key {
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
    }
    Ok(())
}

fn apply_entity_relations(ctx: &Context<'_>, catalog: &mut CatalogState) -> Result<(), Rejection> {
    for key in schema_bodies(ctx, PhysicalSchema::EntityRelation) {
        let raw = ctx.read_body(&key).ok_or(Rejection::StateCorrupt)?;
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
        if space_id != ctx.principal().space
            || v4::space_triage_key(&space_id, &identity.record) != key
        {
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
            .or_else(|| choices.first().filter(|_| choices.len() == 1));
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
        if let (Some(slot), Some(glyph)) = (out.get_mut(index), ALPHABET.get(digit)) {
            *slot = *glyph;
        }
        value >>= 5;
    }
    match String::from_utf8(out.to_vec()) {
        Ok(encoded) => encoded,
        Err(_) => "0".repeat(26),
    }
}

fn request_record_id(
    ctx: &Context<'_>,
    domain: &str,
    issue: &DocId,
    extra: &[u8],
) -> Result<String, Rejection> {
    let request = ctx.request_id().ok_or(Rejection::InvalidRequest)?;
    let mut material = Vec::with_capacity(
        16usize
            .saturating_add(issue.as_str().len())
            .saturating_add(extra.len()),
    );
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
        let raw = bytes
            .get(..16)
            .and_then(|head| <[u8; 16]>::try_from(head).ok())
            .ok_or(Rejection::StateCorrupt)?;
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
    let raw = ctx.read_body(&key).ok_or(Rejection::StateCorrupt)?;
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
) -> Result<Batch, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let payload = serde_json::to_vec(event).map_err(|_| Rejection::StateCorrupt)?;
    let record = request_record_id(ctx, "lait.issues.activity-request.v1", &issue, &payload)?;
    let descriptor = v4::SegmentDescriptor {
        issue: doc.into(),
        kind: v4::SegmentKind::Activity,
        record: record.clone(),
    };
    let key = v4::issue_activity_key(&issue, &record);
    let mut batch = Batch::default();
    let activity = v4::ActivityRecord {
        issue: doc.into(),
        event: event.clone(),
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
            position,
        },
    };
    identity.validate().map_err(|_| Rejection::StateCorrupt)?;
    placement.validate().map_err(|_| Rejection::StateCorrupt)?;
    Ok((identity, placement))
}

fn board_neighbor(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    moving: &str,
    descending: bool,
    pivot: Vec<u8>,
) -> Result<Option<(String, String)>, Rejection> {
    use runtime::find as find_api;
    let bound = find_api::Bound {
        decoded_bodies: 4,
        postings_read: 8,
        edges_visited: 1,
        nodes_visited: 8,
        paths_retained: 1,
        candidates_per_branch: 4,
        score_evaluations: 1,
        projected_bytes: 16 * 1024,
        packed_tokens: 128,
        wall_millis: 250,
    };
    let seek = find_api::StepId::new(1).ok_or(Rejection::StateCorrupt)?;
    let pack = find_api::StepId::new(2).ok_or(Rejection::StateCorrupt)?;
    let index = if descending {
        crate::find::field::PROJECT_STATE_POSITION_DESC
    } else {
        crate::find::field::PROJECT_STATE_POSITION
    };
    let mut fields = [
        crate::find::field::ID,
        crate::find::field::KIND,
        crate::find::field::PROJECT,
        crate::find::field::STATE,
        crate::find::field::POSITION,
        crate::find::field::SOURCE_ID,
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
            page_size: 2,
            cursor: None,
        })
        .map_err(|_| Rejection::StateCorrupt)?;
    for row in answer.rows() {
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
        if text(crate::find::field::KIND).as_deref() != Some("issue_placement")
            || text(crate::find::field::PROJECT).as_deref() != Some(project)
            || text(crate::find::field::STATE).as_deref() != Some(workflow_state)
        {
            // Ordered composite postings have left this lane. There can be no
            // later in-lane candidate.
            return Ok(None);
        }
        let id = text(crate::find::field::SOURCE_ID).ok_or(Rejection::StateCorrupt)?;
        if id == moving {
            continue;
        }
        let position = text(crate::find::field::POSITION).ok_or(Rejection::StateCorrupt)?;
        return Ok(Some((id, position)));
    }
    Ok(None)
}

/// Resolve a board placement from the publication's ordered composite
/// postings. One card move reads at most the target Body plus two posting rows;
/// it never decodes or sorts the project lane.
pub(crate) fn board_position(
    ctx: &Context<'_>,
    project: &str,
    workflow_state: &str,
    moving: &str,
    position: Option<&contract::Pos>,
) -> Result<String, Rejection> {
    // Status-only changes preserve the atomic placement and must not inspect a
    // project lane. This is the common action path.
    if position.is_none() {
        if let Some(current) = issue_coordinate_for(ctx, moving)? {
            return Ok(current.placement.position);
        }
    }
    let (lower, upper) = match position {
        None => return Err(Rejection::StateCorrupt),
        Some(contract::Pos::Top) => {
            let upper = board_neighbor(
                ctx,
                project,
                workflow_state,
                moving,
                false,
                crate::find::board_lane_prefix(project, workflow_state),
            )?;
            (String::new(), upper.map(|(_, rank)| rank))
        }
        Some(contract::Pos::Bottom) => {
            let lower = board_neighbor(
                ctx,
                project,
                workflow_state,
                moving,
                true,
                crate::find::board_lane_prefix(project, workflow_state),
            )?;
            (lower.map_or_else(String::new, |(_, rank)| rank), None)
        }
        Some(contract::Pos::Before { doc }) | Some(contract::Pos::After { doc }) => {
            if doc == moving {
                return Err(Rejection::InvalidRequest);
            }
            let target = issue_coordinate_for(ctx, doc)?
                .filter(|coordinate| {
                    coordinate.placement.project == project
                        && coordinate.placement.workflow_state == workflow_state
                })
                .ok_or(Rejection::InvalidRequest)?;
            let rank = target.placement.position;
            if matches!(position, Some(contract::Pos::After { .. })) {
                let upper = board_neighbor(
                    ctx,
                    project,
                    workflow_state,
                    moving,
                    false,
                    crate::find::board_position_key(project, workflow_state, &rank, doc),
                )?;
                (rank, upper.map(|(_, rank)| rank))
            } else {
                let lower = board_neighbor(
                    ctx,
                    project,
                    workflow_state,
                    moving,
                    true,
                    crate::find::board_position_desc_key(project, workflow_state, &rank, doc),
                )?;
                (lower.map_or_else(String::new, |(_, rank)| rank), Some(rank))
            }
        }
    };
    Ok(crate::rank::between(&lower, upper.as_deref()))
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
    let (identity, placement) = issue_coordinate(doc, project, workflow_state, position, ordinal)?;
    let issue = DocId::parse(doc).ok_or(Rejection::StateCorrupt)?;
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
    )?;
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
    let Some(raw) = ctx.read_body(&key) else {
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
    Ok(batch)
}

pub(crate) fn read_check(
    ctx: &Context<'_>,
    doc: &str,
    run: &str,
) -> Result<Option<v4::IssueCheckRecord>, Rejection> {
    let issue = DocId::parse(doc).ok_or(Rejection::InvalidRequest)?;
    let key = v4::issue_check_key(&issue, run);
    let Some(raw) = ctx.read_body(&key) else {
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
    while let (Some(left), Some(right)) = (old.get(prefix), new.get(prefix)) {
        if left != right {
            break;
        }
        prefix = prefix.saturating_add(1);
    }
    let mut suffix = 0usize;
    loop {
        if suffix >= old.len().saturating_sub(prefix) || suffix >= new.len().saturating_sub(prefix)
        {
            break;
        }
        let left = old.get(old.len().saturating_sub(1).saturating_sub(suffix));
        let right = new.get(new.len().saturating_sub(1).saturating_sub(suffix));
        match (left, right) {
            (Some(left), Some(right)) if left == right => {
                suffix = suffix.saturating_add(1);
            }
            _ => break,
        }
    }
    let insert: String = new
        .get(prefix..new.len().saturating_sub(suffix))
        .unwrap_or(&[])
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
        replace_text(
            ctx,
            batch,
            &key,
            v4::roots::DESCRIPTION,
            &catalog.description,
        )?;
    }
    Ok(key)
}

pub(crate) fn write_space(
    ctx: &Context<'_>,
    catalog: &CatalogState,
    name: &str,
    description: &str,
) -> Result<Batch, Rejection> {
    let mut batch = Batch::default();
    let key = ensure_directory(ctx, catalog, &mut batch)?;
    set_register(&mut batch, &key, v4::roots::NAME, name.as_bytes().to_vec());
    replace_text(ctx, &mut batch, &key, v4::roots::DESCRIPTION, description)?;
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
    replace_text(
        ctx,
        &mut batch,
        &key,
        v4::roots::DESCRIPTION,
        &meta.description,
    )?;
    Ok(batch)
}

pub(crate) fn write_governance_revision(
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
    let raw = ctx.read_body(&key).ok_or(Rejection::StateCorrupt)?;
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
    let raw = ctx.read_body(&key).ok_or(Rejection::StateCorrupt)?;
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
    replace_text(
        ctx,
        &mut batch,
        &key,
        v4::roots::DESCRIPTION,
        &initiative.description,
    )?;
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

const MIGRATION_MAX_ITEMS: u32 = 256;
const MIGRATION_MAX_OPERATIONS: usize = 3_500;
const MIGRATION_MAX_ESTIMATED_BYTES: usize = 700 * 1_024;

fn migration_marker(ctx: &Context<'_>) -> Result<Option<v4::MigrationMarkerRecord>, Rejection> {
    let key = v4::space_directory_key(&ctx.principal().space);
    if ctx.body_version(&key).is_none() {
        return Ok(None);
    }
    let view = read_view(ctx, &key)?;
    view.registers
        .get(v4::roots::MIGRATION)
        .map(|raw| {
            v4::MigrationMarkerRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)
        })
        .transpose()
}

pub(crate) fn migration_publication(
    ctx: &Context<'_>,
    current: runtime::publication::PublicationId,
) -> Result<runtime::publication::PublicationId, Rejection> {
    Ok(migration_marker(ctx)?
        .map(|marker| marker.publication)
        .unwrap_or(current))
}

fn migration_positions(
    catalog: &CatalogState,
    issues: &BTreeMap<String, IssueState>,
) -> BTreeMap<String, String> {
    let mut positions = BTreeMap::new();
    let mut last_by_project = BTreeMap::<String, String>::new();
    for (project, entries) in &catalog.boards {
        let mut last = String::new();
        for (_, doc) in entries {
            last = crate::rank::between(&last, None);
            positions.insert(doc.clone(), last.clone());
        }
        last_by_project.insert(project.clone(), last);
    }
    for (doc, issue) in issues {
        if positions.contains_key(doc) {
            continue;
        }
        let last = last_by_project.entry(issue.project.clone()).or_default();
        *last = crate::rank::between(last, None);
        positions.insert(doc.clone(), last.clone());
    }
    positions
}

fn offer_migration_item(
    out: &mut Batch,
    cursor: &str,
    key: String,
    item: Batch,
    first: &mut Option<String>,
    last: &mut String,
    items: &mut u32,
) -> bool {
    if key.as_str() <= cursor {
        return true;
    }
    let mut candidate = out.clone();
    candidate.absorb(item);
    if *items == MIGRATION_MAX_ITEMS
        || candidate.operations.len() > MIGRATION_MAX_OPERATIONS
        || candidate.estimated_bytes() > MIGRATION_MAX_ESTIMATED_BYTES
    {
        return false;
    }
    out.absorb(candidate_delta(out, candidate));
    first.get_or_insert_with(|| key.clone());
    *last = key;
    *items = items.saturating_add(1);
    true
}

/// Return only the additions in `candidate` relative to `base`. This keeps the
/// offer path simple without making Batch's fields a second transaction API.
fn candidate_delta(base: &Batch, candidate: Batch) -> Batch {
    Batch {
        operations: candidate
            .operations
            .get(base.operations.len()..)
            .unwrap_or(&[])
            .to_vec(),
        bodies: candidate
            .bodies
            .into_iter()
            .filter(|body| !base.bodies.contains(body))
            .collect(),
        declarations: candidate
            .declarations
            .into_iter()
            .filter(|declaration| {
                !base
                    .declarations
                    .iter()
                    .any(|existing| existing.key == declaration.key)
            })
            .collect(),
    }
}

/// Stage one bounded, crash-resumable v3 -> v4 migration transaction. The
/// caller repeats the same administrative intent until this returns `None`.
/// Cursor and audit advance in the transaction containing the copied facts.
pub(crate) fn migration_batch(
    ctx: &Context<'_>,
    catalog: &CatalogState,
    issues: &BTreeMap<String, IssueState>,
    spec_successors: &[(String, Batch)],
    publication: runtime::publication::PublicationId,
    actor: &str,
    timestamp: u64,
) -> Result<Option<Batch>, Rejection> {
    let previous = migration_marker(ctx)?;
    if previous.as_ref().is_some_and(|marker| marker.complete) {
        return Ok(None);
    }
    if previous
        .as_ref()
        .is_some_and(|marker| marker.publication != publication)
    {
        return Err(Rejection::StateCorrupt);
    }
    if crate::ids::ActorId::parse(actor).is_none() || timestamp == 0 {
        return Err(Rejection::InvalidRequest);
    }
    let cursor = previous
        .as_ref()
        .map(|marker| marker.cursor.as_str())
        .unwrap_or("");
    let mut out = Batch::default();
    let mut first = None;
    let mut last = String::new();
    let mut items = 0u32;
    let space_item = write_space(ctx, catalog, &catalog.name, &catalog.description)?;
    let mut stopped = !offer_migration_item(
        &mut out,
        cursor,
        "00:space".into(),
        space_item,
        &mut first,
        &mut last,
        &mut items,
    );

    if !stopped {
        for (id, label) in &catalog.labels {
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("02:label:{id}"),
                write_label(ctx, catalog, id, label, false)?,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (id, revision) in &catalog.roles {
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("05:governance:{id}:{}", revision.revision_id),
                write_governance_revision(ctx, revision)?,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (id, revisions) in &catalog.role_revisions {
            for revision in revisions {
                if !offer_migration_item(
                    &mut out,
                    cursor,
                    format!("06:governance:{id}:{}", revision.revision_id),
                    write_governance_revision(ctx, revision)?,
                    &mut first,
                    &mut last,
                    &mut items,
                ) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
    }
    if !stopped {
        for (id, meta) in &catalog.projects {
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("10:project:{id}"),
                write_project(ctx, catalog, id, meta, false)?,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (project, revisions) in &catalog.workflow_revisions {
            for revision in revisions {
                if !offer_migration_item(
                    &mut out,
                    cursor,
                    format!("11:workflow:{project}:{}", revision.revision_id),
                    write_workflow_revision(ctx, project, revision)?,
                    &mut first,
                    &mut last,
                    &mut items,
                ) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
    }
    if !stopped {
        for (migration_key, successor) in spec_successors {
            if !offer_migration_item(
                &mut out,
                cursor,
                migration_key.clone(),
                successor.clone(),
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (project, records) in &catalog.milestones {
            for record in records.values() {
                if !offer_migration_item(
                    &mut out,
                    cursor,
                    format!("12:milestone:{project}:{}", record.id),
                    write_milestone(ctx, record)?,
                    &mut first,
                    &mut last,
                    &mut items,
                ) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
    }
    if !stopped {
        for (project, records) in &catalog.cycles {
            for record in records.values() {
                if !offer_migration_item(
                    &mut out,
                    cursor,
                    format!("13:cycle:{project}:{}", record.id),
                    write_cycle(ctx, record)?,
                    &mut first,
                    &mut last,
                    &mut items,
                ) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
    }
    let positions = migration_positions(catalog, issues);
    let existing_coordinates = issue_coordinates(ctx)?;
    if !stopped {
        for (doc, issue) in issues {
            if !contract::valid_title(&issue.title) || !contract::valid_text(&issue.description) {
                return Err(Rejection::StateCorrupt);
            }
            let ordinal = catalog
                .seqs
                .get(doc)
                .copied()
                .map(u64::from)
                .or_else(|| {
                    existing_coordinates
                        .get(doc)
                        .map(|coordinate| coordinate.identity.alias.ordinal)
                })
                .ok_or(Rejection::StateCorrupt)?;
            let mut item = Batch::default();
            item.operation(
                &contract::issue_key(doc),
                Op::RegisterSet {
                    path: v4::roots::ISSUE_ID.into(),
                    value: doc.as_bytes().to_vec(),
                },
            );
            item.operation(
                &contract::issue_key(doc),
                Op::RegisterClear {
                    path: "title".into(),
                },
            );
            write_issue_coordinate(
                ctx,
                &mut item,
                doc,
                &issue.project,
                &issue.status,
                positions.get(doc).cloned().ok_or(Rejection::StateCorrupt)?,
                ordinal,
                catalog.tombstones.contains(doc),
            )?;
            item.absorb(write_issue_meta(
                ctx,
                doc,
                issue,
                catalog.tombstones.contains(doc),
            )?);
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("20:issue:{doc}"),
                item,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (child, parent) in &catalog.parents {
            let project = issues
                .get(child)
                .ok_or(Rejection::StateCorrupt)?
                .project
                .as_str();
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("30:parent:{child}"),
                write_parent(ctx, project, child, Some(parent.clone()))?,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (from, kind, to) in &catalog.edges {
            let project = issues
                .get(from)
                .ok_or(Rejection::StateCorrupt)?
                .project
                .as_str();
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("31:link:{from}:{kind}:{to}"),
                write_link(ctx, project, from, kind, to, true)?,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (project, updates) in &catalog.project_updates {
            for update in updates {
                if !offer_migration_item(
                    &mut out,
                    cursor,
                    format!("40:update:{project}:{}", update.id),
                    write_project_update(ctx, update)?,
                    &mut first,
                    &mut last,
                    &mut items,
                ) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
    }
    if !stopped {
        for (id, initiative) in &catalog.initiatives {
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("50:initiative:{id}"),
                write_initiative(ctx, initiative)?,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (id, initiative) in &catalog.initiatives {
            for project in &initiative.projects {
                if !offer_migration_item(
                    &mut out,
                    cursor,
                    format!("51:initiative-project:{id}:{project}"),
                    write_entity_relation(ctx, id, "initiative_project", project, true)?,
                    &mut first,
                    &mut last,
                    &mut items,
                ) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
    }
    if !stopped {
        for (id, team) in &catalog.teams {
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("60:team:{id}"),
                write_team(ctx, team)?,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }
    if !stopped {
        for (id, team) in &catalog.teams {
            for member in &team.members {
                if !offer_migration_item(
                    &mut out,
                    cursor,
                    format!("61:team-member:{id}:{member}"),
                    write_entity_relation(ctx, id, "team_member", member, true)?,
                    &mut first,
                    &mut last,
                    &mut items,
                ) {
                    stopped = true;
                    break;
                }
            }
            if stopped {
                break;
            }
        }
    }
    if !stopped {
        for (id, triage) in &catalog.triage {
            let mut item = write_triage_submission(ctx, triage)?;
            if !triage.outcome.is_empty() {
                let accepted_project = (triage.outcome == "accepted")
                    .then(|| issues.get(&triage.doc).map(|issue| issue.project.as_str()))
                    .flatten();
                item.absorb(write_triage_decision(ctx, triage, accepted_project)?);
            }
            if !offer_migration_item(
                &mut out,
                cursor,
                format!("70:triage:{id}"),
                item,
                &mut first,
                &mut last,
                &mut items,
            ) {
                stopped = true;
                break;
            }
        }
    }

    let Some(first) = first else {
        return if stopped {
            Err(Rejection::LimitExceeded)
        } else if previous.is_some() {
            Ok(None)
        } else {
            Err(Rejection::StateCorrupt)
        };
    };
    let directory = ensure_directory(ctx, catalog, &mut out)?;
    let batch_number = previous
        .as_ref()
        .map_or(1, |marker| marker.batch.saturating_add(1));
    let started_at = previous
        .as_ref()
        .map_or(timestamp, |marker| marker.started_at);
    let marker_actor = previous
        .as_ref()
        .map_or_else(|| actor.to_string(), |marker| marker.actor.clone());
    // Completion is intentionally withheld until the remaining physical DAG
    // cutover (legacy comment/event records and Spec/Baseline revision Bodies)
    // has run. Publishing `complete=true` here would activate an extractor
    // that correctly ignores those coarse legacy roots and therefore lose
    // visible facts. A later migration phase advances this same marker.
    let complete = false;
    let marker = v4::MigrationMarkerRecord {
        migration: v4::MIGRATION_V3_TO_V4.into(),
        source_version: 3,
        target_version: 4,
        publication,
        batch: batch_number,
        cursor: last.clone(),
        complete,
        actor: marker_actor,
        started_at,
        updated_at: timestamp,
    };
    let operations = u32::try_from(out.operations.len().saturating_add(2))
        .map_err(|_| Rejection::LimitExceeded)?;
    let audit = v4::MigrationAuditRecord {
        migration: v4::MIGRATION_V3_TO_V4.into(),
        batch: batch_number,
        actor: actor.into(),
        timestamp,
        first,
        last,
        items,
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
    Ok(Some(out))
}
