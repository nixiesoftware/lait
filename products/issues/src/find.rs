#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "Find declaration constants are compile-time validated and tests unwrap fixed canonical fixtures"
)]
//! Issues-owned Find vocabulary and principal-neutral extractors.
//!
//! Entity nodes are owned by exactly one Body. Facts stored in another Body
//! (hierarchy, labels, assignment, containment) are relation nodes with stable
//! source/target edges; no extractor overlays fields onto an Issue row. This
//! is what lets Runtime replace one Body extraction and structurally share the
//! rest of a published corpus without leaving stale cross-Body fields behind.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use replica::body::{BodyKey, SchemaId};
use runtime::{
    find::{
        Analyzer, AnalyzerRef, BodyExtraction, Bound, Edge, EdgeRef, ExtractedEdge, ExtractedField,
        ExtractedNode, Extractor, Field, FieldKind, FieldRef, Gate, GateRef, ModeSet, NodeId,
        NodeKey, OpSet, Schema, SchemaRef, SourceRef, Value,
    },
    world::{ExtractionContext, Rejection},
};
use unicode_normalization::UnicodeNormalization as _;
use unicode_segmentation::UnicodeSegmentation as _;

use crate::{contract, ids::ProjectId, v4::CanonicalRecord as _};

pub const ENTITY_SCHEMA: &str = "issues_entity";
pub const ENTITY_SCHEMA_VERSION: u32 = 1;
pub const READ_GATE: &str = "read";
pub const WORD_ANALYZER: &str = "word";
pub const WORD_ANALYZER_CONFIGURATION: &[u8] =
    b"unicode-segmentation-1.13.3+nfkc-0.1.25+lowercase.v1";
const MAX_TERMS_PER_VALUE: usize = 4_096;
const MAX_TERM_BYTES: usize = 256;

pub mod field {
    pub const ID: &str = "id";
    pub const KIND: &str = "kind";
    pub const TITLE: &str = "title";
    pub const EXACT_NAME: &str = "exact_name";
    pub const ENTITY_KEY: &str = "entity_key";
    pub const TEXT: &str = "text";
    pub const SEARCH: &str = "search";
    pub const PROJECT: &str = "project";
    pub const STATE: &str = "state";
    pub const STATE_CATEGORY: &str = "state_category";
    pub const KIND_PROJECT: &str = "kind_project";
    pub const KIND_PROJECT_STATE: &str = "kind_project_state";
    pub const PROJECT_STATE_POSITION: &str = "project_state_position";
    pub const PROJECT_STATE_POSITION_DESC: &str = "project_state_position_desc";
    pub const PRIORITY: &str = "priority";
    pub const AUTHOR: &str = "author";
    pub const CREATED_AT: &str = "created_at";
    pub const DUE_AT: &str = "due_at";
    pub const ESTIMATE: &str = "estimate";
    pub const HEALTH: &str = "health";
    pub const TARGET_DATE: &str = "target_date";
    pub const ARCHIVED: &str = "archived";
    pub const TOMBSTONE: &str = "tombstone";
    pub const POSITION: &str = "position";
    pub const ALIAS_ORDINAL: &str = "alias_ordinal";
    pub const ALIAS_DISAMBIGUATOR: &str = "alias_disambiguator";
    pub const RELATION_KIND: &str = "relation_kind";
    pub const SOURCE_ID: &str = "source_id";
    pub const TARGET_ID: &str = "target_id";
    pub const REVISION: &str = "revision";
    pub const HEAD: &str = "head";
    pub const ISSUED: &str = "issued";
    pub const CONFLICTED: &str = "conflicted";
}

pub mod edge {
    pub const SOURCE: &str = "source";
    pub const TARGET: &str = "target";
}

fn schema_id(value: &str) -> SchemaId {
    SchemaId::parse(value).expect("Issues Find identifiers are compile-time constants")
}

pub fn entity_schema_ref() -> SchemaRef {
    SchemaRef {
        name: schema_id(ENTITY_SCHEMA),
        version: ENTITY_SCHEMA_VERSION,
    }
}

pub fn field_ref(name: &str) -> FieldRef {
    FieldRef {
        schema: entity_schema_ref(),
        name: schema_id(name),
    }
}

fn edge_ref(name: &str) -> EdgeRef {
    EdgeRef {
        schema: entity_schema_ref(),
        name: schema_id(name),
    }
}

fn gate_ref() -> GateRef {
    GateRef {
        schema: entity_schema_ref(),
        name: schema_id(READ_GATE),
    }
}

fn analyzer_ref() -> AnalyzerRef {
    AnalyzerRef {
        schema: entity_schema_ref(),
        name: schema_id(WORD_ANALYZER),
    }
}

fn source(name: &str, version: u32) -> SourceRef {
    SourceRef {
        name: schema_id(name),
        version,
    }
}

fn sources() -> Vec<SourceRef> {
    let mut sources = vec![
        source(contract::ISSUE_SCHEMA, 1),
        source(contract::ISSUE_SCHEMA, 2),
        source(contract::ISSUE_SCHEMA, contract::ISSUE_SCHEMA_VERSION),
        source(contract::SPEC_SCHEMA, contract::SPEC_SCHEMA_VERSION),
        source(contract::BASELINE_SCHEMA, contract::BASELINE_SCHEMA_VERSION),
    ];
    sources.extend(
        crate::v4::PHYSICAL_SCHEMAS
            .iter()
            .map(|schema| source(schema.name(), crate::v4::SCHEMA_VERSION)),
    );
    sources.sort();
    sources.dedup();
    sources
}

pub fn schemas() -> Vec<Schema> {
    let schema = entity_schema_ref();
    let analyzer = analyzer_ref();
    let mut declaration = Schema {
        reference: schema.clone(),
        sources: sources(),
        fields: [
            (field::ID, FieldKind::Text, false),
            (field::KIND, FieldKind::Text, false),
            (field::TITLE, FieldKind::Text, true),
            (field::EXACT_NAME, FieldKind::Text, false),
            (field::ENTITY_KEY, FieldKind::Text, false),
            (field::TEXT, FieldKind::Text, true),
            (field::SEARCH, FieldKind::Text, true),
            (field::PROJECT, FieldKind::Text, false),
            (field::STATE, FieldKind::Text, false),
            (field::STATE_CATEGORY, FieldKind::Text, false),
            (field::KIND_PROJECT, FieldKind::Bytes, false),
            (field::KIND_PROJECT_STATE, FieldKind::Bytes, false),
            (field::PROJECT_STATE_POSITION, FieldKind::Bytes, false),
            (field::PROJECT_STATE_POSITION_DESC, FieldKind::Bytes, false),
            (field::PRIORITY, FieldKind::Text, false),
            (field::AUTHOR, FieldKind::Text, false),
            (field::CREATED_AT, FieldKind::Unsigned, false),
            (field::DUE_AT, FieldKind::Unsigned, false),
            (field::ESTIMATE, FieldKind::Unsigned, false),
            (field::HEALTH, FieldKind::Text, false),
            (field::TARGET_DATE, FieldKind::Unsigned, false),
            (field::ARCHIVED, FieldKind::Bool, false),
            (field::TOMBSTONE, FieldKind::Bool, false),
            (field::POSITION, FieldKind::Text, false),
            (field::ALIAS_ORDINAL, FieldKind::Unsigned, false),
            (field::ALIAS_DISAMBIGUATOR, FieldKind::Bytes, false),
            (field::RELATION_KIND, FieldKind::Text, false),
            (field::SOURCE_ID, FieldKind::Text, false),
            (field::TARGET_ID, FieldKind::Text, false),
            (field::REVISION, FieldKind::Text, false),
            (field::HEAD, FieldKind::Bool, false),
            (field::ISSUED, FieldKind::Bool, false),
            (field::CONFLICTED, FieldKind::Bool, false),
        ]
        .into_iter()
        .map(|(name, kind, analyzed)| Field {
            reference: field_ref(name),
            kind,
            analyzer: analyzed.then(|| analyzer.clone()),
        })
        .collect(),
        edges: [edge::SOURCE, edge::TARGET]
            .into_iter()
            .map(|name| Edge {
                reference: edge_ref(name),
                target: schema.clone(),
                gate: gate_ref(),
            })
            .collect(),
        gates: vec![Gate {
            reference: gate_ref(),
            demand: contract::demand_read(),
        }],
        analyzers: vec![Analyzer {
            reference: analyzer,
            configuration: WORD_ANALYZER_CONFIGURATION.to_vec(),
        }],
        features: Vec::new(),
        ops: OpSet::ALL,
        modes: ModeSet::EXACT,
        bound: Bound {
            decoded_bodies: 10_000,
            postings_read: 100_000,
            edges_visited: 100_000,
            nodes_visited: 100_000,
            paths_retained: 10_000,
            candidates_per_branch: 10_000,
            score_evaluations: 100_000,
            projected_bytes: 8 * 1_024 * 1_024,
            packed_tokens: 32_768,
            wall_millis: 10_000,
        },
    };
    declaration = declaration.canonicalized();
    vec![declaration]
}

pub fn extractors() -> Vec<Extractor> {
    sources()
        .into_iter()
        .map(|source| Extractor {
            schema: entity_schema_ref(),
            semantic_digest: blake3::derive_key(
                "lait.issues.find.extractor.v1",
                &postcard::to_stdvec(&source).expect("canonical extractor source"),
            ),
            abi_version: runtime::find::EXTRACTOR_ABI_VERSION,
            source,
        })
        .collect()
}

fn terms(value: &str) -> Vec<Arc<[u8]>> {
    let normalized: String = value.nfkc().collect();
    let mut terms = BTreeSet::<Vec<u8>>::new();
    for word in normalized.unicode_words() {
        if terms.len() >= MAX_TERMS_PER_VALUE {
            break;
        }
        let lowered: String = word.chars().flat_map(char::to_lowercase).collect();
        let bytes = lowered.into_bytes();
        if !bytes.is_empty() && bytes.len() <= MAX_TERM_BYTES {
            terms.insert(bytes);
        }
    }
    terms.into_iter().map(Arc::from).collect()
}

fn exact_text(name: &str, value: impl Into<String>) -> ExtractedField {
    ExtractedField {
        reference: field_ref(name),
        value: Value::text(value),
        gate: None,
        terms: Vec::new(),
    }
}

fn analyzed_text(name: &str, value: impl Into<String>) -> ExtractedField {
    let value = value.into();
    ExtractedField {
        reference: field_ref(name),
        terms: terms(&value),
        value: Value::text(value),
        gate: None,
    }
}

fn unsigned(name: &str, value: u64) -> ExtractedField {
    ExtractedField {
        reference: field_ref(name),
        value: Value::Unsigned(value),
        gate: None,
        terms: Vec::new(),
    }
}

fn boolean(name: &str, value: bool) -> ExtractedField {
    ExtractedField {
        reference: field_ref(name),
        value: Value::Bool(value),
        gate: None,
        terms: Vec::new(),
    }
}

fn bytes(name: &str, value: impl Into<Vec<u8>>) -> ExtractedField {
    ExtractedField {
        reference: field_ref(name),
        value: Value::bytes(value),
        gate: None,
        terms: Vec::new(),
    }
}

fn node_key(id: &[u8]) -> Result<NodeKey, Rejection> {
    Ok(NodeKey {
        schema: entity_schema_ref(),
        node: NodeId::new(id.to_vec()).map_err(|_| Rejection::StateCorrupt)?,
    })
}

/// Canonical composite encoding used by exact postings. Each component is
/// zero-escaped and terminated, so neither prefix ambiguity nor delimiter
/// injection can make two coordinate tuples share bytes.
pub(crate) fn composite_key<'a>(components: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    let mut encoded = Vec::new();
    for component in components {
        for byte in component.as_bytes() {
            if *byte == 0 {
                encoded.extend_from_slice(&[0, 0xff]);
            } else {
                encoded.push(*byte);
            }
        }
        encoded.extend_from_slice(&[0, 0]);
    }
    encoded
}

/// Board coordinate ordered by rank inside an exact project/state prefix.
pub(crate) fn board_lane_prefix(project: &str, state: &str) -> Vec<u8> {
    composite_key([project, state])
}

pub(crate) fn board_position_key(
    project: &str,
    state: &str,
    position: &str,
    issue: &str,
) -> Vec<u8> {
    composite_key([project, state, position, issue])
}

/// Same lane with the terminal rank byte order inverted. This lets a bounded
/// ascending posting seek return the immediate predecessor without scanning
/// every earlier card.
pub(crate) fn board_position_desc_key(
    project: &str,
    state: &str,
    position: &str,
    issue: &str,
) -> Vec<u8> {
    let mut encoded = board_lane_prefix(project, state);
    encoded.extend(
        composite_key([position, issue])
            .into_iter()
            .map(|byte| 0xff_u8 - byte),
    );
    encoded
}

fn text_field<'a>(fields: &'a [ExtractedField], name: &str) -> Option<&'a str> {
    fields.iter().find_map(|item| {
        (item.reference == field_ref(name))
            .then_some(&item.value)
            .and_then(|value| match value {
                Value::Text(value) => Some(value.as_ref()),
                _ => None,
            })
    })
}

fn entity(
    id: &str,
    kind: &str,
    mut fields: Vec<ExtractedField>,
) -> Result<ExtractedNode, Rejection> {
    let searchable = fields
        .iter()
        .filter(|item| {
            item.reference == field_ref(field::TITLE) || item.reference == field_ref(field::TEXT)
        })
        .filter_map(|item| match &item.value {
            Value::Text(value) => Some(value.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if !searchable.is_empty() {
        fields.push(analyzed_text(field::SEARCH, searchable));
    }
    if let Some(project) = text_field(&fields, field::PROJECT).map(str::to_owned) {
        fields.push(bytes(
            field::KIND_PROJECT,
            composite_key([kind, project.as_str()]),
        ));
        if let Some(state) = text_field(&fields, field::STATE).map(str::to_owned) {
            fields.push(bytes(
                field::KIND_PROJECT_STATE,
                composite_key([kind, project.as_str(), state.as_str()]),
            ));
            if let Some(position) = text_field(&fields, field::POSITION).map(str::to_owned) {
                fields.push(bytes(
                    field::PROJECT_STATE_POSITION,
                    board_position_key(&project, &state, &position, id),
                ));
                fields.push(bytes(
                    field::PROJECT_STATE_POSITION_DESC,
                    board_position_desc_key(&project, &state, &position, id),
                ));
            }
        }
    }
    fields.push(exact_text(field::ID, id));
    fields.push(exact_text(field::KIND, kind));
    Ok(ExtractedNode {
        key: node_key(id.as_bytes())?,
        gate: Some(gate_ref()),
        fields,
        edges: Vec::new(),
        features: Vec::new(),
    })
}

fn relation_identity(kind: &str, source: &str, target: &str) -> [u8; 32] {
    let mut material = Vec::new();
    for value in [kind, source, target] {
        material.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        material.extend_from_slice(value.as_bytes());
    }
    blake3::derive_key("lait.issues.find-relation.v1", &material)
}

fn relation_with_identity(
    identity: [u8; 32],
    relation_kind: &str,
    source: &str,
    target: &str,
    project: Option<&str>,
) -> Result<ExtractedNode, Rejection> {
    let mut stable = b"relation\0".to_vec();
    stable.extend_from_slice(&identity);
    let rendered = data_encoding::HEXLOWER.encode(&identity);
    let mut fields = vec![
        exact_text(field::ID, rendered),
        exact_text(field::KIND, "relation"),
        exact_text(field::RELATION_KIND, relation_kind),
        exact_text(field::SOURCE_ID, source),
        exact_text(field::TARGET_ID, target),
    ];
    if let Some(project) = project {
        fields.push(exact_text(field::PROJECT, project));
    }
    Ok(ExtractedNode {
        key: node_key(&stable)?,
        gate: Some(gate_ref()),
        fields,
        edges: vec![
            ExtractedEdge {
                reference: edge_ref(edge::SOURCE),
                gate: gate_ref(),
                targets: vec![node_key(source.as_bytes())?],
            },
            ExtractedEdge {
                reference: edge_ref(edge::TARGET),
                gate: gate_ref(),
                targets: vec![node_key(target.as_bytes())?],
            },
        ],
        features: Vec::new(),
    })
}

fn relation(
    kind: &str,
    source: &str,
    target: &str,
    project: Option<&str>,
) -> Result<ExtractedNode, Rejection> {
    relation_with_identity(
        relation_identity(kind, source, target),
        kind,
        source,
        target,
        project,
    )
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

fn text(view: &fabric::CollaborativeView, path: &str) -> String {
    view.texts.get(path).cloned().unwrap_or_default()
}

fn finish(
    ctx: &ExtractionContext<'_>,
    body: &BodyKey,
    nodes: Vec<ExtractedNode>,
) -> BodyExtraction {
    BodyExtraction {
        body: body.clone(),
        stamp: ctx.body_stamp(body).unwrap_or_default(),
        nodes,
    }
}

pub fn extract(
    ctx: &ExtractionContext<'_>,
    extractor: &Extractor,
    body: &BodyKey,
) -> Result<BodyExtraction, Rejection> {
    if extractor.schema != entity_schema_ref() || !sources().contains(&extractor.source) {
        return Err(Rejection::ContractViolation);
    }
    let name = extractor.source.name.as_str();
    if name == crate::v4::ISSUE_PLACEMENT_SCHEMA {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_placement(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_ATTACHMENT_SCHEMA {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_attachment(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_CHECK_SCHEMA {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_check(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_REACTION_SCHEMA {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_reaction(body, &bytes)?));
    }
    if name == crate::v4::PROJECT_HIERARCHY_SCHEMA {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_hierarchy(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_RELATION_SCHEMA {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_relation(body, &bytes)?));
    }
    if name == crate::v4::ENTITY_RELATION_SCHEMA {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_entity_relation(body, &bytes)?));
    }
    let view = if crate::v4::PHYSICAL_SCHEMAS
        .iter()
        .copied()
        .find(|schema| schema.name() == name)
        .is_some_and(crate::v4::PhysicalSchema::immutable)
    {
        let bytes = ctx.read_body(body).ok_or(Rejection::StateCorrupt)?;
        let envelope = crate::v4::ImmutableRecordEnvelope::decode_canonical(&bytes)
            .map_err(|_| Rejection::StateCorrupt)?;
        let mut view = fabric::CollaborativeView::default();
        view.registers.insert(
            crate::v4::roots::IDENTITY.into(),
            envelope
                .identity
                .encode_canonical()
                .map_err(|_| Rejection::StateCorrupt)?,
        );
        view.registers
            .insert(crate::v4::roots::RECORD.into(), envelope.record);
        view
    } else {
        ctx.read_collaborative(body)
            .map_err(|_| Rejection::StateCorrupt)?
    };
    let nodes = match name {
        contract::ISSUE_SCHEMA => extract_issue(body, &view)?,
        contract::SPEC_SCHEMA => extract_spec_marker(body, &view)?,
        contract::BASELINE_SCHEMA => extract_baseline_marker(body, &view)?,
        crate::v4::SPACE_DIRECTORY_SCHEMA => extract_space(body, &view)?,
        crate::v4::GOVERNANCE_REVISION_SCHEMA => extract_governance(body, &view)?,
        crate::v4::PROJECT_META_SCHEMA => extract_project(body, &view)?,
        crate::v4::WORKFLOW_REVISION_SCHEMA => extract_workflow(body, &view)?,
        crate::v4::PROJECT_SCHEDULE_SCHEMA => extract_schedule(body, &view)?,
        crate::v4::PROJECT_UPDATES_SCHEMA => extract_updates(body, &view)?,
        crate::v4::SPACE_TRIAGE_SCHEMA => extract_triage(body, &view)?,
        crate::v4::ISSUE_COMMENT_SCHEMA => extract_comment(body, &view)?,
        crate::v4::ISSUE_ACTIVITY_SCHEMA => extract_activity(body, &view)?,
        crate::v4::ISSUE_IDENTITY_SCHEMA => extract_issue_identity(body, &view)?,
        crate::v4::ISSUE_META_SCHEMA => extract_issue_meta(body, &view)?,
        crate::v4::INITIATIVE_SCHEMA => extract_initiative(body, &view)?,
        crate::v4::TEAM_SCHEMA => extract_team(body, &view)?,
        crate::v4::LABEL_SCHEMA => extract_label(body, &view)?,
        crate::v4::REVISION_ALIAS_SCHEMA => extract_revision_alias(body, &view)?,
        _ => return Err(Rejection::ContractViolation),
    };
    Ok(finish(ctx, body, nodes))
}

fn extract_spec_marker(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let spec = crate::spec::Spec::from_view(view);
    let raw_revisions = view.maps.get("revisions").map_or(0, BTreeMap::len);
    let first = spec.revisions.first().ok_or(Rejection::StateCorrupt)?;
    if contract::spec_key(&first.body.spec) != *body
        || raw_revisions != spec.revisions.len()
        || spec.revisions.iter().any(|revision| {
            revision.body.spec != first.body.spec || revision.body.validate().is_err()
        })
    {
        return Err(Rejection::StateCorrupt);
    }
    let heads: BTreeSet<&str> = spec
        .heads()
        .into_iter()
        .map(|revision| revision.revision.as_str())
        .collect();
    let conflicted = heads.len() > 1;
    let issued: BTreeSet<&str> = match spec.issued() {
        crate::spec::Issued::None => BTreeSet::new(),
        crate::spec::Issued::One(revision) => [revision.revision.as_str()].into_iter().collect(),
        crate::spec::Issued::Conflict(revisions) => revisions
            .into_iter()
            .map(|revision| revision.revision.as_str())
            .collect(),
    };
    let mut nodes = vec![entity(
        &first.body.spec,
        "spec",
        vec![exact_text(field::PROJECT, &first.body.project)],
    )?];
    for revision in &spec.revisions {
        let revision_id = revision.revision.as_str();
        nodes.push(entity(
            revision_id,
            revision.body.kind.as_str(),
            vec![
                analyzed_text(field::TITLE, &revision.body.title),
                analyzed_text(field::TEXT, &revision.body.text),
                exact_text(field::PROJECT, &revision.body.project),
                exact_text(field::STATE, revision.body.state.as_str()),
                exact_text(field::AUTHOR, &revision.body.author),
                unsigned(field::CREATED_AT, revision.body.ts),
                exact_text(field::REVISION, revision_id),
                exact_text(field::SOURCE_ID, &revision.body.spec),
                boolean(field::HEAD, heads.contains(revision_id)),
                boolean(field::ISSUED, issued.contains(revision_id)),
                boolean(field::CONFLICTED, conflicted && heads.contains(revision_id)),
            ],
        )?);
        nodes.push(relation(
            "spec_revision",
            &revision.body.spec,
            revision_id,
            Some(&revision.body.project),
        )?);
        for predecessor in &revision.predecessors {
            nodes.push(relation(
                "predecessor",
                revision_id,
                predecessor,
                Some(&revision.body.project),
            )?);
        }
        for link in &revision.body.links {
            let target = match &link.target {
                crate::spec::Target::Spec { revision, .. }
                | crate::spec::Target::Baseline { revision, .. } => revision.as_str(),
                crate::spec::Target::Issue { issue } => issue.as_str(),
            };
            nodes.push(relation(
                link.rel.as_str(),
                revision_id,
                target,
                Some(&revision.body.project),
            )?);
        }
        if let Some(plan) = &revision.body.plan {
            for root in &plan.roots {
                nodes.push(relation(
                    "plan_root",
                    revision_id,
                    root,
                    Some(&revision.body.project),
                )?);
            }
        }
    }
    Ok(nodes)
}

fn extract_baseline_marker(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let baseline = crate::spec::Baseline::from_view(view);
    let raw_revisions = view.maps.get("revisions").map_or(0, BTreeMap::len);
    let first = baseline.revisions.first().ok_or(Rejection::StateCorrupt)?;
    if contract::baseline_key(&first.body.baseline) != *body
        || raw_revisions != baseline.revisions.len()
        || baseline.revisions.iter().any(|revision| {
            revision.body.baseline != first.body.baseline || revision.body.validate().is_err()
        })
    {
        return Err(Rejection::StateCorrupt);
    }
    let heads: BTreeSet<&str> = baseline
        .heads()
        .into_iter()
        .map(|revision| revision.revision.as_str())
        .collect();
    let conflicted = heads.len() > 1;
    let issued: BTreeSet<&str> = match baseline.issued() {
        crate::spec::BaselineIssued::None => BTreeSet::new(),
        crate::spec::BaselineIssued::One(revision) => {
            [revision.revision.as_str()].into_iter().collect()
        }
        crate::spec::BaselineIssued::Conflict(revisions) => revisions
            .into_iter()
            .map(|revision| revision.revision.as_str())
            .collect(),
    };
    let mut nodes = vec![entity(
        &first.body.baseline,
        "baseline",
        vec![exact_text(field::PROJECT, &first.body.project)],
    )?];
    for revision in &baseline.revisions {
        let revision_id = revision.revision.as_str();
        nodes.push(entity(
            revision_id,
            "baseline_revision",
            vec![
                analyzed_text(field::TITLE, &revision.body.name),
                exact_text(field::PROJECT, &revision.body.project),
                exact_text(field::STATE, revision.body.state.as_str()),
                exact_text(field::AUTHOR, &revision.body.author),
                unsigned(field::CREATED_AT, revision.body.ts),
                exact_text(field::REVISION, revision_id),
                exact_text(field::SOURCE_ID, &revision.body.baseline),
                boolean(field::HEAD, heads.contains(revision_id)),
                boolean(field::ISSUED, issued.contains(revision_id)),
                boolean(field::CONFLICTED, conflicted && heads.contains(revision_id)),
            ],
        )?);
        nodes.push(relation(
            "baseline_revision",
            &revision.body.baseline,
            revision_id,
            Some(&revision.body.project),
        )?);
        for predecessor in &revision.predecessors {
            nodes.push(relation(
                "predecessor",
                revision_id,
                predecessor,
                Some(&revision.body.project),
            )?);
        }
        for member in &revision.body.members {
            nodes.push(relation(
                "baseline_member",
                revision_id,
                &member.revision,
                Some(&revision.body.project),
            )?);
        }
    }
    Ok(nodes)
}

fn extract_governance(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
    if crate::v4::governance_revision_key(&identity.owner, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::GovernanceRevisionRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    if record.role != identity.owner || record.revision.revision_id != identity.record {
        return Err(Rejection::StateCorrupt);
    }
    let id = format!(
        "role-revision:{}:{}",
        record.role, record.revision.revision_id
    );
    let mut nodes = vec![entity(
        &id,
        "governance_revision",
        vec![
            analyzed_text(field::TITLE, record.revision.body.name),
            analyzed_text(field::TEXT, record.revision.body.description),
            exact_text(field::SOURCE_ID, &record.role),
            exact_text(field::REVISION, &record.revision.revision_id),
            boolean(field::TOMBSTONE, record.revision.body.tombstone),
        ],
    )?];
    for predecessor in record.revision.predecessor_ids {
        let target = format!("role-revision:{}:{predecessor}", record.role);
        nodes.push(relation("predecessor", &id, &target, None)?);
    }
    Ok(nodes)
}

fn extract_workflow(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
    let project_id = ProjectId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::workflow_revision_key(&project_id, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::ProjectWorkflowRevisionRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    if record.project != identity.owner || record.revision.revision_id != identity.record {
        return Err(Rejection::StateCorrupt);
    }
    let id = format!(
        "workflow-revision:{}:{}",
        record.project, record.revision.revision_id
    );
    let revision_id = record.revision.revision_id.clone();
    let revision_node_id = id.clone();
    let mut nodes = vec![entity(
        &id,
        "workflow_revision",
        vec![
            analyzed_text(field::TITLE, record.revision.body.name),
            exact_text(field::PROJECT, &record.project),
            exact_text(field::SOURCE_ID, &record.project),
            exact_text(field::REVISION, &record.revision.revision_id),
            boolean(field::TOMBSTONE, record.revision.body.tombstone),
        ],
    )?];
    for state in &record.revision.body.states {
        let state_id = format!(
            "workflow-state:{}:{}:{}",
            record.project, revision_id, state.state_id
        );
        nodes.push(entity(
            &state_id,
            "workflow_state",
            vec![
                exact_text(field::PROJECT, &record.project),
                exact_text(field::REVISION, &revision_id),
                exact_text(field::STATE, &state.state_id),
                exact_text(field::STATE_CATEGORY, &state.category),
                analyzed_text(field::TITLE, state.name.clone()),
            ],
        )?);
        nodes.push(relation(
            "workflow_state",
            &state_id,
            &revision_node_id,
            Some(&record.project),
        )?);
    }
    for predecessor in record.revision.predecessor_ids {
        let target = format!("workflow-revision:{}:{predecessor}", record.project);
        nodes.push(relation(
            "predecessor",
            &id,
            &target,
            Some(&record.project),
        )?);
    }
    Ok(nodes)
}

fn extract_issue(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let issue = register(view, crate::v4::roots::ISSUE_ID);
    if issue.is_empty() {
        // The offline migration may publish its bounded batches before the
        // final v4-only activation. Unmigrated content is deliberately absent
        // from that in-progress corpus rather than guessed from a BodyId.
        return Ok(Vec::new());
    }
    if crate::ids::DocId::parse(&issue).is_none() || contract::issue_key(&issue) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let description = text(view, "description");
    if !crate::contract::valid_text(&description) {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("issue-content:{issue}"),
        "issue_content",
        vec![
            exact_text(field::SOURCE_ID, issue),
            analyzed_text(field::TEXT, description),
        ],
    )?])
}

fn extract_issue_identity(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let envelope_identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let envelope_identity =
        crate::v4::RecordBodyIdentityRecord::decode_canonical(envelope_identity)
            .map_err(|_| Rejection::StateCorrupt)?;
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::IssueIdentityRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let issue = crate::ids::DocId::parse(&record.issue).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_identity_key(&issue) != *body
        || envelope_identity.owner != record.issue
        || envelope_identity.record != "identity"
    {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("issue-identity:{}", record.issue),
        "issue_identity",
        vec![
            exact_text(field::SOURCE_ID, record.issue),
            unsigned(field::ALIAS_ORDINAL, record.alias.ordinal),
            bytes(
                field::ALIAS_DISAMBIGUATOR,
                record.alias.disambiguator.to_vec(),
            ),
        ],
    )?])
}

fn extract_issue_placement(body: &BodyKey, raw: &[u8]) -> Result<Vec<ExtractedNode>, Rejection> {
    let record = crate::v4::IssuePlacementRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let issue = crate::ids::DocId::parse(&record.issue).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_placement_key(&issue) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("issue-placement:{}", record.issue),
        "issue_placement",
        vec![
            exact_text(field::SOURCE_ID, &record.issue),
            exact_text(field::PROJECT, &record.placement.project),
            exact_text(field::STATE, &record.placement.workflow_state),
            exact_text(field::POSITION, &record.placement.position),
        ],
    )?])
}

fn extract_issue_meta(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let issue = register(view, crate::v4::roots::IDENTITY);
    let parsed = crate::ids::DocId::parse(&issue).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_meta_key(&parsed) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let priority = register(view, crate::v4::roots::PRIORITY);
    let title = register(view, crate::v4::roots::TITLE);
    let created_by = register(view, crate::v4::roots::CREATED_BY);
    let created_at =
        optional_u64(view, crate::v4::roots::CREATED_AT).ok_or(Rejection::StateCorrupt)?;
    let due_at = optional_u64(view, crate::v4::roots::DUE_AT);
    let estimate = optional_u64(view, crate::v4::roots::ESTIMATE);
    let record = crate::v4::IssueMetaRecord {
        issue: issue.clone(),
        title: title.clone(),
        priority: priority.clone(),
        created_by: (!created_by.is_empty()).then_some(created_by.clone()),
        created_at,
        due_at,
        estimate: estimate.and_then(|value| value.try_into().ok()),
        tombstone: matches!(
            register(view, crate::v4::roots::TOMBSTONE).as_str(),
            "1" | "true"
        ),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    let mut fields = vec![
        exact_text(field::SOURCE_ID, &issue),
        analyzed_text(field::TITLE, title),
        exact_text(field::PRIORITY, priority),
        unsigned(field::CREATED_AT, created_at),
        boolean(field::TOMBSTONE, record.tombstone),
    ];
    if !created_by.is_empty() {
        fields.push(exact_text(field::AUTHOR, created_by));
    }
    if let Some(due_at) = due_at {
        fields.push(unsigned(field::DUE_AT, due_at));
    }
    if let Some(estimate) = estimate {
        fields.push(unsigned(field::ESTIMATE, estimate));
    }
    Ok(vec![entity(&issue, "issue", fields)?])
}

fn extract_issue_attachment(body: &BodyKey, raw: &[u8]) -> Result<Vec<ExtractedNode>, Rejection> {
    let record = crate::v4::IssueAttachmentRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let issue = crate::ids::DocId::parse(&record.issue).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_attachment_key(&issue, &record.id) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("attachment:{}:{}", record.issue, record.id),
        "issue_attachment",
        vec![
            exact_text(field::SOURCE_ID, record.issue),
            exact_text(field::TARGET_ID, record.id),
            analyzed_text(field::TITLE, record.name),
            exact_text(field::AUTHOR, record.by),
            unsigned(field::CREATED_AT, record.timestamp),
            boolean(field::TOMBSTONE, record.tombstone),
        ],
    )?])
}

fn extract_issue_check(body: &BodyKey, raw: &[u8]) -> Result<Vec<ExtractedNode>, Rejection> {
    let record =
        crate::v4::IssueCheckRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?;
    let issue = crate::ids::DocId::parse(&record.issue).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_check_key(&issue, &record.run) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("check:{}:{}", record.issue, record.run),
        "issue_check",
        vec![
            exact_text(field::SOURCE_ID, record.issue),
            exact_text(field::TARGET_ID, record.run),
            exact_text(field::STATE, record.check.state),
            exact_text(field::AUTHOR, record.check.by),
            unsigned(field::CREATED_AT, record.check.ts),
        ],
    )?])
}

fn extract_space(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let id = register(view, crate::v4::roots::IDENTITY);
    let Some(space) = mechanics::ids::SpaceId::parse(&id) else {
        return Err(Rejection::StateCorrupt);
    };
    if crate::v4::space_directory_key(&space) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let record = crate::v4::SpaceDirectoryRecord {
        name: register(view, crate::v4::roots::NAME),
        description: text(view, crate::v4::roots::DESCRIPTION),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    Ok(vec![entity(
        &id,
        "space",
        vec![
            analyzed_text(field::TITLE, record.name),
            analyzed_text(field::TEXT, record.description),
        ],
    )?])
}

fn extract_revision_alias(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
    if crate::v4::revision_alias_key(&identity.owner, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let alias = crate::v4::RevisionAliasRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    if alias.spec != identity.owner || alias.legacy_revision != identity.record {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![relation(
        "revision_alias",
        &alias.legacy_revision,
        &alias.canonical_revision,
        None,
    )?])
}

fn extract_project(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let project = register(view, crate::v4::roots::IDENTITY);
    let Some(project_id) = ProjectId::parse(&project) else {
        return Err(Rejection::StateCorrupt);
    };
    if crate::v4::project_meta_key(&project_id) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let archived = matches!(
        register(view, crate::v4::roots::ARCHIVED).as_str(),
        "1" | "true"
    );
    let meta = crate::v4::ProjectMetaRecord {
        project: project.clone(),
        name: register(view, crate::v4::roots::NAME),
        key: register(view, crate::v4::roots::KEY),
        color: register(view, crate::v4::roots::COLOR),
        description: text(view, crate::v4::roots::DESCRIPTION),
        lead: register(view, crate::v4::roots::LEAD),
        start_date: optional_u64(view, crate::v4::roots::START_DATE),
        target_date: optional_u64(view, crate::v4::roots::TARGET_DATE),
        archived,
        team: register(view, crate::v4::roots::TEAM),
        tombstone: matches!(
            register(view, crate::v4::roots::TOMBSTONE).as_str(),
            "1" | "true"
        ),
    };
    meta.validate().map_err(|_| Rejection::StateCorrupt)?;
    let mut fields = vec![
        analyzed_text(field::TITLE, meta.name.clone()),
        exact_text(field::EXACT_NAME, meta.name.trim().to_ascii_lowercase()),
        exact_text(field::ENTITY_KEY, meta.key),
        analyzed_text(field::TEXT, meta.description),
        exact_text(field::PROJECT, &project),
        boolean(field::ARCHIVED, meta.archived),
        boolean(field::TOMBSTONE, meta.tombstone),
    ];
    if let Some(target) = meta.target_date {
        fields.push(unsigned(field::TARGET_DATE, target));
    }
    let mut nodes = vec![entity(&project, "project", fields)?];
    if !meta.team.is_empty() {
        nodes.push(relation("team", &project, &meta.team, Some(&project))?);
    }
    Ok(nodes)
}

fn extract_label(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let id = register(view, crate::v4::roots::IDENTITY);
    let label = crate::ids::LabelId::parse(&id).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::label_key(&label) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::LabelDirectoryEntry::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    if record.label != id {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &id,
        "label",
        vec![
            analyzed_text(field::TITLE, record.name.clone()),
            exact_text(field::EXACT_NAME, record.name.trim().to_ascii_lowercase()),
            exact_text(field::HEALTH, record.color),
            boolean(field::TOMBSTONE, record.tombstone),
        ],
    )?])
}

fn extract_schedule(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
    let project_id = ProjectId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::project_schedule_key(&project_id, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record =
        crate::v4::ScheduleRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?;
    let project = identity.owner;
    let mut nodes = Vec::with_capacity(2);
    match record {
        crate::v4::ScheduleRecord::Milestone {
            milestone,
            project: owner,
            name,
            description,
            target_date,
            position,
            tombstone,
        } => {
            if owner != project || milestone != identity.record {
                return Err(Rejection::StateCorrupt);
            }
            let mut fields = vec![
                analyzed_text(field::TITLE, name),
                analyzed_text(field::TEXT, description),
                exact_text(field::PROJECT, &project),
                exact_text(field::POSITION, position),
                boolean(field::TOMBSTONE, tombstone),
            ];
            if let Some(target) = target_date {
                fields.push(unsigned(field::TARGET_DATE, target));
            }
            nodes.push(entity(&milestone, "milestone", fields)?);
            nodes.push(relation("project", &milestone, &project, Some(&project))?);
        }
        crate::v4::ScheduleRecord::Cycle {
            cycle,
            project: owner,
            name,
            start,
            end,
            tombstone,
        } => {
            if owner != project || cycle != identity.record {
                return Err(Rejection::StateCorrupt);
            }
            nodes.push(entity(
                &cycle,
                "cycle",
                vec![
                    analyzed_text(field::TITLE, name),
                    exact_text(field::PROJECT, &project),
                    unsigned(field::CREATED_AT, start),
                    unsigned(field::DUE_AT, end),
                    boolean(field::TOMBSTONE, tombstone),
                ],
            )?);
            nodes.push(relation("project", &cycle, &project, Some(&project))?);
        }
    }
    Ok(nodes)
}

fn extract_hierarchy(body: &BodyKey, raw: &[u8]) -> Result<Vec<ExtractedNode>, Rejection> {
    let record =
        crate::v4::TopologyRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?;
    let mut nodes = Vec::new();
    match record {
        crate::v4::TopologyRecord::Parent(record) => {
            let project_id = ProjectId::parse(&record.project).ok_or(Rejection::StateCorrupt)?;
            if crate::v4::project_hierarchy_key(&project_id, &record.child) != *body {
                return Err(Rejection::StateCorrupt);
            }
            if let Some(parent) = &record.parent {
                nodes.push(relation_with_identity(
                    record.relation_identity(),
                    "parent",
                    &record.child,
                    parent,
                    Some(&record.project),
                )?);
            }
        }
        crate::v4::TopologyRecord::Link(record) => {
            let project_id = ProjectId::parse(&record.project).ok_or(Rejection::StateCorrupt)?;
            let identity = data_encoding::HEXLOWER.encode(&record.relation_identity());
            if crate::v4::project_hierarchy_key(&project_id, &identity) != *body {
                return Err(Rejection::StateCorrupt);
            }
            if record.present {
                nodes.push(relation_with_identity(
                    record.relation_identity(),
                    &record.kind,
                    &record.from,
                    &record.to,
                    Some(&record.project),
                )?);
            }
        }
    }
    Ok(nodes)
}

fn extract_updates(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity_bytes = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let project = identity.owner;
    let project_id = ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::project_updates_key(&project_id, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::ProjectUpdateRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    if record.project != project || record.update != identity.record {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![
        entity(
            &record.update,
            "project_update",
            vec![
                analyzed_text(field::TEXT, record.body),
                exact_text(field::PROJECT, &project),
                exact_text(field::AUTHOR, record.author),
                exact_text(field::HEALTH, record.health),
                unsigned(field::CREATED_AT, record.timestamp),
            ],
        )?,
        relation("project", &record.update, &project, Some(&project))?,
    ])
}

fn extract_triage(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity_bytes = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let space = identity.owner;
    let space = mechanics::ids::SpaceId::parse(&space).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::space_triage_key(&space, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record =
        crate::v4::TriageRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?;
    let mut nodes = Vec::new();
    match record {
        crate::v4::TriageRecord::Submission(record) => {
            if record.triage != identity.record {
                return Err(Rejection::StateCorrupt);
            }
            nodes.push(entity(
                &record.triage,
                "triage",
                vec![
                    analyzed_text(field::TITLE, record.title),
                    analyzed_text(field::TEXT, record.body),
                    exact_text(field::AUTHOR, record.submitted_by),
                    unsigned(field::CREATED_AT, record.timestamp),
                ],
            )?);
        }
        crate::v4::TriageRecord::Decision(record) => {
            if record.decision != identity.record {
                return Err(Rejection::StateCorrupt);
            }
            let outcome = match record.outcome {
                crate::v4::TriageOutcome::Accepted => "accepted",
                crate::v4::TriageOutcome::Declined => "declined",
                crate::v4::TriageOutcome::Duplicate => "duplicate",
            };
            let id = format!("triage-decision:{}:{}", record.triage, record.decision);
            nodes.push(entity(
                &id,
                "triage_decision",
                vec![
                    exact_text(field::STATE, outcome),
                    exact_text(field::AUTHOR, record.decided_by),
                    analyzed_text(field::TEXT, record.note),
                    unsigned(field::CREATED_AT, record.timestamp),
                ],
            )?);
            nodes.push(relation("triage", &id, &record.triage, None)?);
            if let Some(project) = &record.project {
                nodes.push(relation("project", &id, project, Some(project))?);
            }
            if let Some(issue) = record.issue {
                nodes.push(relation("issue", &id, &issue, record.project.as_deref())?);
            }
        }
        crate::v4::TriageRecord::Resolution(record) => {
            if record.identity() != identity.record {
                return Err(Rejection::StateCorrupt);
            }
            let id = format!("triage-resolution:{}:{}", record.triage, record.decision);
            let decision = format!("triage-decision:{}:{}", record.triage, record.decision);
            nodes.push(entity(
                &id,
                "triage_resolution",
                vec![
                    exact_text(field::AUTHOR, record.resolved_by),
                    unsigned(field::CREATED_AT, record.timestamp),
                ],
            )?);
            nodes.push(relation("triage", &id, &record.triage, None)?);
            nodes.push(relation("decision", &id, &decision, None)?);
        }
    }
    Ok(nodes)
}

fn extract_comment(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity_bytes = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let issue = crate::ids::DocId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_comment_key(&issue, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let mut nodes = Vec::new();
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record =
        crate::v4::DiscussionRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?;
    let crate::v4::DiscussionRecord::Comment(comment) = record else {
        return Err(Rejection::StateCorrupt);
    };
    let Some(id) = comment.id else {
        return Err(Rejection::StateCorrupt);
    };
    if id != identity.record {
        return Err(Rejection::StateCorrupt);
    }
    let mut fields = vec![
        analyzed_text(field::TEXT, comment.b),
        exact_text(field::AUTHOR, comment.a),
        unsigned(field::CREATED_AT, comment.t),
        exact_text(field::SOURCE_ID, &identity.owner),
    ];
    if let Some(parent) = &comment.parent {
        fields.push(exact_text(field::TARGET_ID, parent));
    }
    nodes.push(entity(&id, "comment", fields)?);
    nodes.push(relation("issue", &id, &identity.owner, None)?);
    if let Some(parent) = comment.parent {
        nodes.push(relation("reply", &id, &parent, None)?);
    }
    Ok(nodes)
}

fn extract_reaction(body: &BodyKey, raw: &[u8]) -> Result<Vec<ExtractedNode>, Rejection> {
    let crate::v4::DiscussionRecord::Reaction(reaction) =
        crate::v4::DiscussionRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?
    else {
        return Err(Rejection::StateCorrupt);
    };
    let issue = crate::ids::DocId::parse(&reaction.issue).ok_or(Rejection::StateCorrupt)?;
    let identity = reaction.identity();
    if crate::v4::issue_reaction_key(&issue, &identity) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let id = format!("reaction:{identity}");
    Ok(vec![
        entity(
            &id,
            "reaction",
            vec![
                exact_text(field::STATE, if reaction.on { "on" } else { "off" }),
                exact_text(field::AUTHOR, &reaction.actor),
                exact_text(field::RELATION_KIND, &reaction.emoji),
                exact_text(field::SOURCE_ID, &reaction.comment),
                exact_text(field::TARGET_ID, &reaction.issue),
            ],
        )?,
        relation("reaction", &id, &reaction.comment, None)?,
    ])
}

fn extract_activity(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity_bytes = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let issue = crate::ids::DocId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_activity_key(&issue, &identity.record) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record =
        crate::v4::ActivityRecord::decode_canonical(raw).map_err(|_| Rejection::StateCorrupt)?;
    if record.issue != identity.owner {
        return Err(Rejection::StateCorrupt);
    }
    let event = record.event;
    let id = format!("activity:{}:{}", identity.owner, identity.record);
    Ok(vec![
        entity(
            &id,
            "activity",
            vec![
                exact_text(field::STATE, event.k),
                exact_text(field::AUTHOR, event.a),
                analyzed_text(field::TEXT, event.x),
                unsigned(field::CREATED_AT, event.t),
                exact_text(field::SOURCE_ID, &identity.owner),
            ],
        )?,
        relation("issue", &id, &identity.owner, None)?,
    ])
}

fn extract_issue_relation(body: &BodyKey, raw: &[u8]) -> Result<Vec<ExtractedNode>, Rejection> {
    let record = crate::v4::IssueRelationRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let identity = record.identity();
    let issue = crate::ids::DocId::parse(&record.issue).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::issue_relation_key(&issue, &identity) != *body {
        return Err(Rejection::StateCorrupt);
    }
    if !record.present {
        return Ok(Vec::new());
    }
    let target = if record.kind == "baseline" {
        serde_json::from_str::<crate::spec::BaselineRef>(&record.target)
            .map_err(|_| Rejection::StateCorrupt)?
            .baseline
    } else {
        record.target.clone()
    };
    Ok(vec![relation_with_identity(
        blake3::derive_key("lait.issues.issue-relation-node.v1", identity.as_bytes()),
        &record.kind,
        &record.issue,
        &target,
        Some(&record.project),
    )?])
}

fn extract_initiative(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let id = register(view, crate::v4::roots::IDENTITY);
    let Some(initiative) = crate::ids::InitiativeId::parse(&id) else {
        return Err(Rejection::StateCorrupt);
    };
    if crate::v4::initiative_key(&initiative) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let tombstone = matches!(
        register(view, crate::v4::roots::TOMBSTONE).as_str(),
        "1" | "true"
    );
    let record = crate::v4::InitiativeRecord {
        initiative: id.clone(),
        name: register(view, crate::v4::roots::NAME),
        description: text(view, crate::v4::roots::DESCRIPTION),
        owner: register(view, crate::v4::roots::OWNER),
        health: register(view, crate::v4::roots::HEALTH),
        target_date: optional_u64(view, crate::v4::roots::TARGET_DATE),
        tombstone,
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    let mut fields = vec![
        analyzed_text(field::TITLE, record.name),
        analyzed_text(field::TEXT, record.description),
        exact_text(field::HEALTH, record.health),
        boolean(field::TOMBSTONE, record.tombstone),
    ];
    if !record.owner.is_empty() {
        fields.push(exact_text(field::AUTHOR, record.owner));
    }
    if let Some(target) = record.target_date {
        fields.push(unsigned(field::TARGET_DATE, target));
    }
    Ok(vec![entity(&id, "initiative", fields)?])
}

fn extract_team(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let id = register(view, crate::v4::roots::IDENTITY);
    let Some(team) = crate::ids::TeamId::parse(&id) else {
        return Err(Rejection::StateCorrupt);
    };
    if crate::v4::team_key(&team) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let record = crate::v4::TeamRecord {
        team: id.clone(),
        name: register(view, crate::v4::roots::NAME),
        key: register(view, crate::v4::roots::KEY),
        icon: register(view, crate::v4::roots::ICON),
        lead: register(view, crate::v4::roots::LEAD),
        tombstone: matches!(
            register(view, crate::v4::roots::TOMBSTONE).as_str(),
            "1" | "true"
        ),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    Ok(vec![entity(
        &id,
        "team",
        vec![
            analyzed_text(field::TITLE, record.name.clone()),
            exact_text(field::EXACT_NAME, record.name.trim().to_ascii_lowercase()),
            exact_text(field::ENTITY_KEY, record.key),
            boolean(field::TOMBSTONE, record.tombstone),
        ],
    )?])
}

fn extract_entity_relation(body: &BodyKey, raw: &[u8]) -> Result<Vec<ExtractedNode>, Rejection> {
    let record = crate::v4::EntityRelationRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let identity = record.identity();
    if record.identity() != identity
        || crate::v4::entity_relation_key(&record.owner, &identity) != *body
    {
        return Err(Rejection::StateCorrupt);
    }
    if !record.present {
        return Ok(Vec::new());
    }
    let relation_kind = match record.kind.as_str() {
        "initiative_project" => "project",
        "team_member" => "member",
        _ => return Err(Rejection::StateCorrupt),
    };
    Ok(vec![relation_with_identity(
        blake3::derive_key("lait.issues.entity-relation-node.v1", identity.as_bytes()),
        relation_kind,
        &record.owner,
        &record.target,
        None,
    )?])
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = "prj_00000000000000000000000000";
    const ISSUE: &str = "iss_00000000000000000000000000";

    #[test]
    fn declaration_is_canonical_and_every_source_has_one_extractor() {
        let schemas = schemas();
        assert_eq!(schemas.len(), 1);
        let schema = schemas.first().unwrap();
        assert!(schema.validate().is_ok());
        let extractors = extractors();
        assert_eq!(extractors.len(), schema.sources.len());
        assert!(schema.sources.iter().all(|source| extractors
            .iter()
            .filter(|item| &item.source == source)
            .count()
            == 1));
        assert!(schema
            .sources
            .iter()
            .any(|source| { source.name.as_str() == crate::v4::WORKFLOW_REVISION_SCHEMA }));
        assert!(schema
            .sources
            .iter()
            .any(|source| { source.name.as_str() == crate::v4::GOVERNANCE_REVISION_SCHEMA }));
    }

    #[test]
    fn analyzer_is_normalized_bounded_and_deterministic() {
        let once = terms("CAFÉ cafe\u{301} roadmap—Roadmap");
        let twice = terms("CAFÉ cafe\u{301} roadmap—Roadmap");
        assert_eq!(once, twice);
        assert_eq!(once.len(), 2);
        assert!(once.iter().all(|term| term.len() <= MAX_TERM_BYTES));
    }

    #[test]
    fn relation_identity_is_typed_and_stable() {
        let a = relation_identity("blocks", "iss_a", "iss_b");
        assert_eq!(a, relation_identity("blocks", "iss_a", "iss_b"));
        assert_ne!(a, relation_identity("relates", "iss_a", "iss_b"));
        assert_ne!(a, relation_identity("blocks", "iss_b", "iss_a"));
    }

    #[test]
    fn issue_extraction_keeps_content_identity_meta_and_placement_independent() {
        let issue = crate::ids::DocId::parse(ISSUE).unwrap();
        let identity = crate::v4::IssueIdentityRecord {
            issue: ISSUE.into(),
            alias: crate::v4::IssueAliasCoordinate::for_issue(7, &issue).unwrap(),
        };
        let placement = crate::v4::IssuePlacementRecord {
            issue: ISSUE.into(),
            placement: crate::v4::BoardPlacement {
                project: PROJECT.into(),
                workflow_state: "in_progress".into(),
                position: "V".into(),
            },
        };
        let envelope_identity = crate::v4::RecordBodyIdentityRecord {
            owner: ISSUE.into(),
            record: "identity".into(),
        };
        let mut content = fabric::CollaborativeView::default();
        content
            .registers
            .insert(crate::v4::roots::ISSUE_ID.into(), ISSUE.as_bytes().to_vec());
        content
            .texts
            .insert("description".into(), "Find every linked issue".into());
        let mut identity_view = fabric::CollaborativeView::default();
        identity_view.registers.insert(
            crate::v4::roots::IDENTITY.into(),
            envelope_identity.encode_canonical().unwrap(),
        );
        identity_view.registers.insert(
            crate::v4::roots::RECORD.into(),
            identity.encode_canonical().unwrap(),
        );
        let mut meta = fabric::CollaborativeView::default();
        for (path, value) in [
            (crate::v4::roots::IDENTITY, ISSUE),
            (crate::v4::roots::TITLE, "Fast lookup"),
            (crate::v4::roots::PRIORITY, "none"),
            (crate::v4::roots::CREATED_AT, "1"),
            (crate::v4::roots::TOMBSTONE, "0"),
        ] {
            meta.registers
                .insert(path.into(), value.as_bytes().to_vec());
        }

        let nodes = [
            extract_issue(&contract::issue_key(ISSUE), &content).unwrap(),
            extract_issue_identity(&crate::v4::issue_identity_key(&issue), &identity_view).unwrap(),
            extract_issue_placement(
                &crate::v4::issue_placement_key(&issue),
                &placement.encode_canonical().unwrap(),
            )
            .unwrap(),
            extract_issue_meta(&crate::v4::issue_meta_key(&issue), &meta).unwrap(),
        ]
        .concat();
        assert_eq!(nodes.len(), 4);
        let identities: BTreeSet<_> = nodes.iter().map(|node| node.key.clone()).collect();
        assert_eq!(identities.len(), nodes.len());
        assert!(nodes
            .iter()
            .any(|node| node.key.node.as_bytes() == ISSUE.as_bytes()));
        assert!(nodes.iter().all(|node| node.edges.is_empty()));
        assert!(nodes.iter().any(|node| node.fields.iter().any(|field| {
            field.reference == field_ref(field::KIND)
                && field.value == Value::Text("issue_placement".into())
        })));
    }

    #[test]
    fn hierarchy_extraction_emits_normalized_reverse_adjacency() {
        let record = crate::v4::HierarchyRecord {
            project: PROJECT.into(),
            child: ISSUE.into(),
            parent: Some("iss_00000000000000000000000001".into()),
        };
        let project = ProjectId::parse(PROJECT).unwrap();
        let raw = crate::v4::TopologyRecord::Parent(record)
            .encode_canonical()
            .unwrap();
        let nodes =
            extract_hierarchy(&crate::v4::project_hierarchy_key(&project, ISSUE), &raw).unwrap();
        assert_eq!(nodes.len(), 1);
        let relation = nodes.first().unwrap();
        assert_eq!(relation.edges.len(), 2);
        assert!(relation.edges.iter().all(|edge| edge.targets.len() == 1));
    }
}
