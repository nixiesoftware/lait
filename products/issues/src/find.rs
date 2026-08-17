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
        ExtractedNode, ExtractionGrowth, ExtractionShape, Extractor, Field, FieldKind, FieldRef,
        Gate, GateRef, ModeSet, NodeId, NodeKey, OpSet, Schema, SchemaRef, SourceRef, Value,
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
    pub const KIND_PROJECT_POSITION: &str = "kind_project_position";
    pub const KIND_PROJECT_POSITION_DESC: &str = "kind_project_position_desc";
    pub const KIND_CREATED_DESC: &str = "kind_created_desc";
    pub const KIND_PROJECT_CREATED_DESC: &str = "kind_project_created_desc";
    pub const KIND_SOURCE_CREATED: &str = "kind_source_created";
    pub const KIND_SOURCE_CREATED_DESC: &str = "kind_source_created_desc";
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
    pub const BLOCK: &str = "block";
    pub const PROJECT_STATE_BLOCK_ORDER: &str = "project_state_block_order";
    pub const PROJECT_STATE_BLOCK_ORDER_DESC: &str = "project_state_block_order_desc";
    pub const PROJECT_STATE_BLOCK_MEMBER: &str = "project_state_block_member";
    pub const PROJECT_STATE_BLOCK_MEMBER_DESC: &str = "project_state_block_member_desc";
    /// Exact immutable workflow-transition id which currently owns an Issue's
    /// sole board placement. Bounded rank maintenance fences every overlay to
    /// this value, so a concurrent user move makes stale relabeling inert.
    pub const PLACEMENT_TRANSITION: &str = "placement_transition";
    pub const ALIAS_ORDINAL: &str = "alias_ordinal";
    pub const ALIAS_DISAMBIGUATOR: &str = "alias_disambiguator";
    pub const ALIAS_COORDINATE: &str = "alias_coordinate";
    pub const RELATION_KIND: &str = "relation_kind";
    pub const SOURCE_ID: &str = "source_id";
    pub const TARGET_ID: &str = "target_id";
    pub const GRAPH_SOURCE_ID: &str = "graph_source_id";
    pub const GRAPH_TARGET_ID: &str = "graph_target_id";
    /// Exact `(relation kind, source entity)` posting. This is the bounded
    /// audience/membership lookup surface; seeking `source_id` alone would
    /// visit every topology edge on a high-degree issue.
    pub const RELATION_SOURCE_KIND: &str = "relation_source_kind";
    /// Exact actor-prefixed, reverse-time activity coordinate used by the
    /// principal-pinned inbox. Prefix pagination stays inside one immutable
    /// World publication.
    pub const INBOX_ORDER: &str = "inbox_order";
    pub const DEVICE: &str = "device";
    pub const REVISION: &str = "revision";
    pub const HEAD_REVISIONS: &str = "head_revisions";
    pub const ISSUED_REVISIONS: &str = "issued_revisions";
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

fn migration_sources() -> Vec<SourceRef> {
    // Historical aggregate Bodies are raw inputs to the bounded migrator,
    // never corpus sources. In particular, declaring their unbounded
    // revision maps as extractors would make honest build admission
    // impossible and would revive the old coarse query path.
    let mut sources = vec![source(
        contract::ISSUE_SCHEMA,
        contract::ISSUE_SCHEMA_VERSION,
    )];
    sources.extend(
        crate::v4::PHYSICAL_SCHEMAS
            .iter()
            .map(|schema| source(schema.name(), crate::v4::SCHEMA_VERSION)),
    );
    sources.sort();
    sources.dedup();
    sources
}

/// Sources admitted by the preferred v4 implementation.  The long-lived
/// Issue Body remains the anchored description source at its current exact
/// binding; every other source is a v4 physical record.  Aggregate
/// Spec/Baseline Bodies and readable Issue predecessors belong exclusively to
/// the historical migrator package.
fn preferred_sources() -> Vec<SourceRef> {
    let mut sources = vec![source(
        contract::ISSUE_SCHEMA,
        contract::ISSUE_SCHEMA_VERSION,
    )];
    sources.extend(
        crate::v4::PHYSICAL_SCHEMAS
            .iter()
            .filter(|schema| schema.preferred())
            .map(|schema| source(schema.name(), crate::v4::SCHEMA_VERSION)),
    );
    sources.sort();
    sources.dedup();
    sources
}

pub fn schemas() -> Vec<Schema> {
    schemas_with_sources(migration_sources())
}

pub fn preferred_schemas() -> Vec<Schema> {
    schemas_with_sources(preferred_sources())
}

fn schemas_with_sources(sources: Vec<SourceRef>) -> Vec<Schema> {
    let schema = entity_schema_ref();
    let analyzer = analyzer_ref();
    let mut declaration = Schema {
        reference: schema.clone(),
        sources,
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
            (field::KIND_PROJECT_POSITION, FieldKind::Bytes, false),
            (field::KIND_PROJECT_POSITION_DESC, FieldKind::Bytes, false),
            (field::KIND_CREATED_DESC, FieldKind::Bytes, false),
            (field::KIND_PROJECT_CREATED_DESC, FieldKind::Bytes, false),
            (field::KIND_SOURCE_CREATED, FieldKind::Bytes, false),
            (field::KIND_SOURCE_CREATED_DESC, FieldKind::Bytes, false),
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
            (field::BLOCK, FieldKind::Text, false),
            (field::PROJECT_STATE_BLOCK_ORDER, FieldKind::Bytes, false),
            (
                field::PROJECT_STATE_BLOCK_ORDER_DESC,
                FieldKind::Bytes,
                false,
            ),
            (field::PROJECT_STATE_BLOCK_MEMBER, FieldKind::Bytes, false),
            (
                field::PROJECT_STATE_BLOCK_MEMBER_DESC,
                FieldKind::Bytes,
                false,
            ),
            (field::PLACEMENT_TRANSITION, FieldKind::Text, false),
            (field::ALIAS_ORDINAL, FieldKind::Unsigned, false),
            (field::ALIAS_DISAMBIGUATOR, FieldKind::Bytes, false),
            (field::ALIAS_COORDINATE, FieldKind::Text, false),
            (field::RELATION_KIND, FieldKind::Text, false),
            (field::SOURCE_ID, FieldKind::Text, false),
            (field::TARGET_ID, FieldKind::Text, false),
            (field::GRAPH_SOURCE_ID, FieldKind::Text, false),
            (field::GRAPH_TARGET_ID, FieldKind::Text, false),
            (field::RELATION_SOURCE_KIND, FieldKind::Bytes, false),
            (field::INBOX_ORDER, FieldKind::Bytes, false),
            (field::DEVICE, FieldKind::Text, false),
            (field::REVISION, FieldKind::Text, false),
            (field::HEAD_REVISIONS, FieldKind::Bytes, false),
            (field::ISSUED_REVISIONS, FieldKind::Bytes, false),
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
            packed_tokens: 8 * 1024 * 1024,
            wall_millis: 10_000,
        },
    };
    declaration = declaration.canonicalized();
    vec![declaration]
}

pub fn extractors() -> Vec<Extractor> {
    extractors_for(migration_sources())
}

pub fn preferred_extractors() -> Vec<Extractor> {
    extractors_for(preferred_sources())
}

fn extractors_for(sources: Vec<SourceRef>) -> Vec<Extractor> {
    sources
        .into_iter()
        .map(|source| Extractor {
            schema: entity_schema_ref(),
            semantic_digest: blake3::derive_key(
                "lait.issues.find.extractor.v1",
                &postcard::to_stdvec(&source).expect("canonical extractor source"),
            ),
            abi_version: runtime::find::EXTRACTOR_ABI_VERSION,
            shape: extraction_shape(&source),
            source,
        })
        .collect()
}

/// Enforceable maxima for one physical Body. These are deliberately grouped
/// by physical record family, not by observed averages. Large text is bounded
/// by the product's 1 MiB semantic ceiling; relation-heavy immutable revision
/// records use the exact collection ceilings in `spec`.
fn extraction_shape(source: &SourceRef) -> ExtractionShape {
    const KIB: u64 = 1_024;
    const MIB: u64 = 1_024 * KIB;
    let name = &source.name;
    if *name == schema_id(contract::ISSUE_SCHEMA)
        || *name == schema_id(crate::v4::SPACE_CONTENT_SCHEMA)
        || *name == schema_id(crate::v4::PROJECT_CONTENT_SCHEMA)
        || *name == schema_id(crate::v4::INITIATIVE_CONTENT_SCHEMA)
    {
        // One content node: retained text plus its bounded normalized terms.
        return ExtractionShape::new(
            1,
            4_128,
            4_128,
            2 * MIB + 16 * KIB,
            2 * MIB + 16 * KIB,
            3 * MIB,
        )
        .with_growth(ExtractionGrowth {
            base_nodes_per_body: 1,
            nodes_per_source_kib: 0,
            base_postings_per_body: 16,
            postings_per_source_kib: 512,
            base_variable_bytes_per_body: 4 * KIB,
            variable_bytes_per_source_byte: 3,
        });
    }
    if *name == schema_id(crate::v4::SPEC_REVISION_SCHEMA) {
        // entity + ownership + predecessors + links + Plan roots
        let nodes = 2
            + crate::spec::MAX_PREDECESSORS
            + crate::spec::MAX_LINKS
            + crate::spec::MAX_PLAN_ROOTS;
        return ExtractionShape::new(
            u32::try_from(nodes).expect("Spec extraction bound"),
            4_128,
            10_000,
            2 * MIB + 16 * KIB,
            4 * MIB,
            4 * MIB,
        )
        .with_growth(ExtractionGrowth {
            base_nodes_per_body: 2,
            nodes_per_source_kib: 100,
            base_postings_per_body: 32,
            postings_per_source_kib: 1_024,
            base_variable_bytes_per_body: 8 * KIB,
            variable_bytes_per_source_byte: 4,
        });
    }
    if *name == schema_id(crate::v4::BASELINE_REVISION_SCHEMA) {
        // entity + ownership + predecessors + members. Baseline titles are
        // capped at 256 bytes, so relation identifiers dominate each node.
        let nodes = 2 + crate::spec::MAX_PREDECESSORS + crate::spec::MAX_MEMBERS;
        return ExtractionShape::new(
            u32::try_from(nodes).expect("Baseline extraction bound"),
            512,
            20_000,
            8 * KIB,
            5 * MIB,
            2 * MIB,
        )
        .with_growth(ExtractionGrowth {
            base_nodes_per_body: 2,
            nodes_per_source_kib: 100,
            base_postings_per_body: 32,
            postings_per_source_kib: 64,
            base_variable_bytes_per_body: 8 * KIB,
            variable_bytes_per_source_byte: 4,
        });
    }
    if *name == schema_id(crate::v4::ISSUE_META_SCHEMA) {
        return ExtractionShape::new(
            u32::try_from(crate::v4::MAX_CONCURRENT_HEADS + 2)
                .expect("Issue head extraction bound"),
            2_080,
            3_200,
            16 * KIB,
            320 * KIB,
            256 * KIB,
        );
    }
    if *name == schema_id(crate::v4::BOARD_BLOCK_SCHEMA)
        || *name == schema_id(crate::v4::BOARD_LANE_SCHEMA)
    {
        // One canonical entity node. Collaborative sets are capped by
        // MAX_CONCURRENT_HEADS; conflicting structural heads stay one compact
        // typed-conflict projection rather than fanning out into candidates.
        return ExtractionShape::new(1, 32, 32, 8 * KIB, 8 * KIB, 32 * KIB);
    }
    if *name == schema_id(crate::v4::GOVERNANCE_HEADS_SCHEMA)
        || *name == schema_id(crate::v4::WORKFLOW_HEADS_SCHEMA)
        || *name == schema_id(crate::v4::SPEC_HEADS_SCHEMA)
        || *name == schema_id(crate::v4::BASELINE_HEADS_SCHEMA)
    {
        return ExtractionShape::new(
            u32::try_from(1 + 2 * crate::v4::MAX_CONCURRENT_HEADS)
                .expect("revision head extraction bound"),
            64,
            4_096,
            8 * KIB,
            1_100 * KIB,
            256 * KIB,
        );
    }
    if *name == schema_id(crate::v4::WORKFLOW_REVISION_SCHEMA) {
        // revision + two nodes/state + predecessor relations
        let nodes = 1 + 2 * crate::workflow::MAX_STATES + crate::workflow::MAX_PREDECESSORS;
        return ExtractionShape::new(
            u32::try_from(nodes).expect("workflow extraction bound"),
            192,
            200_000,
            4 * KIB,
            5 * MIB,
            3 * MIB,
        )
        .with_growth(ExtractionGrowth {
            base_nodes_per_body: 1,
            nodes_per_source_kib: 4,
            base_postings_per_body: 24,
            postings_per_source_kib: 256,
            base_variable_bytes_per_body: 4 * KIB,
            variable_bytes_per_source_byte: 6,
        });
    }
    if *name == schema_id(crate::v4::ISSUE_COMMENT_SCHEMA)
        || *name == schema_id(crate::v4::PROJECT_UPDATES_SCHEMA)
        || *name == schema_id(crate::v4::SPACE_TRIAGE_SCHEMA)
        || *name == schema_id(crate::v4::SPEC_OBSERVATION_SCHEMA)
    {
        return ExtractionShape::new(
            4,
            4_128,
            4_200,
            2 * MIB + 16 * KIB,
            2 * MIB + 64 * KIB,
            3 * MIB,
        )
        .with_growth(ExtractionGrowth {
            base_nodes_per_body: 1,
            nodes_per_source_kib: 1,
            base_postings_per_body: 20,
            postings_per_source_kib: 512,
            base_variable_bytes_per_body: 4 * KIB,
            variable_bytes_per_source_byte: 3,
        });
    }
    if *name == schema_id(crate::v4::ISSUE_ACTIVITY_SCHEMA) {
        // One activity entity, one issue edge, and at most one compact inbox
        // coordinate per bounded recipient. Event text lives only on the
        // activity entity; recipient nodes never duplicate the large value.
        let nodes = 2 + crate::contract::MAX_ISSUE_AUDIENCE;
        return ExtractionShape::new(
            u32::try_from(nodes).expect("activity extraction bound"),
            32,
            u64::try_from(nodes * 12).expect("activity postings bound"),
            2 * MIB + 16 * KIB,
            2 * MIB + 128 * KIB,
            3 * MIB,
        )
        .with_growth(ExtractionGrowth {
            base_nodes_per_body: 2,
            nodes_per_source_kib: 32,
            base_postings_per_body: 24,
            postings_per_source_kib: 384,
            base_variable_bytes_per_body: 8 * KIB,
            variable_bytes_per_source_byte: 3,
        });
    }
    if *name == schema_id(crate::v4::GOVERNANCE_REVISION_SCHEMA) {
        return ExtractionShape::new(
            1 + crate::roles::MAX_PREDECESSORS as u32,
            4_128,
            4_256,
            40 * KIB,
            96 * KIB,
            128 * KIB,
        );
    }
    if *name == schema_id(crate::v4::ISSUE_TRANSITION_SCHEMA) {
        // One transition entity, one issue edge, and one predecessor edge for
        // every observed concurrent head. `entity` adds ten canonical ordered
        // coordinates to the seven transition fields, so that node owns 35
        // postings (one schema row plus two per field); relation nodes own 19.
        let nodes = crate::v4::MAX_CONCURRENT_HEADS + 2;
        return ExtractionShape::new(
            u32::try_from(nodes).expect("transition extraction bound"),
            40,
            u64::try_from(35 + 19 * (nodes - 1)).expect("transition postings bound"),
            8 * KIB,
            80 * KIB,
            128 * KIB,
        );
    }
    if *name == schema_id(crate::v4::PROJECT_SCHEDULE_SCHEMA) {
        return ExtractionShape::new(2, 4_128, 4_144, 48 * KIB, 52 * KIB, 128 * KIB);
    }
    if *name == schema_id(crate::v4::PROJECT_META_SCHEMA)
        || *name == schema_id(crate::v4::SPACE_DIRECTORY_SCHEMA)
        || *name == schema_id(crate::v4::INITIATIVE_SCHEMA)
        || *name == schema_id(crate::v4::TEAM_SCHEMA)
        || *name == schema_id(crate::v4::LABEL_SCHEMA)
    {
        let nodes = if *name == schema_id(crate::v4::SPACE_DIRECTORY_SCHEMA) {
            1 + crate::roles::BUILT_IN_ROLE_IDS.len() as u32
        } else {
            2
        };
        return ExtractionShape::new(nodes, 2_080, 8_384, 16 * KIB, 64 * KIB, 128 * KIB);
    }
    if *name == schema_id(crate::v4::ISSUE_ATTACHMENT_SCHEMA) {
        return ExtractionShape::new(1, 128, 128, 8 * KIB, 8 * KIB, 64 * KIB);
    }
    if *name == schema_id(crate::v4::ISSUE_PLACEMENT_SCHEMA) {
        return ExtractionShape::new(1, 24, 24, 4 * KIB, 4 * KIB, 32 * KIB);
    }
    if *name == schema_id(crate::v4::ISSUE_IDENTITY_SCHEMA)
        || *name == schema_id(crate::v4::ISSUE_CHECK_SCHEMA)
    {
        return ExtractionShape::new(1, 20, 20, 4 * KIB, 4 * KIB, 32 * KIB);
    }
    if *name == schema_id(crate::v4::ISSUE_REACTION_SCHEMA) {
        return ExtractionShape::new(2, 20, 40, 4 * KIB, 8 * KIB, 32 * KIB);
    }
    if *name == schema_id(crate::v4::PROJECT_HIERARCHY_SCHEMA)
        || *name == schema_id(crate::v4::ISSUE_RELATION_SCHEMA)
        || *name == schema_id(crate::v4::ENTITY_RELATION_SCHEMA)
        || *name == schema_id(crate::v4::REVISION_ALIAS_SCHEMA)
    {
        return ExtractionShape::new(1, 16, 16, 4 * KIB, 4 * KIB, 32 * KIB);
    }
    // A declaration omission is a package bug. Keep the fallback small so a
    // newly introduced extractor cannot silently reserve tracker-scale memory
    // under an unrelated generic maximum; its focused test will fail shape
    // admission until this match is extended deliberately.
    ExtractionShape::new(2, 32, 64, 8 * KIB, 16 * KIB, 64 * KIB)
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
        // TITLE/TEXT are projection material. The synthetic SEARCH field owns
        // the one token copy so the corpus does not retain duplicate postings
        // for the same human text.
        terms: Vec::new(),
        value: Value::text(value),
        gate: None,
    }
}

fn analyzed_search(value: &str) -> ExtractedField {
    ExtractedField {
        reference: field_ref(field::SEARCH),
        terms: terms(value),
        // Search values are never packed; canonical TITLE/TEXT remain on the
        // node (or are hydrated from the source Body). Avoid retaining a
        // second full 1 MiB string solely to own term postings.
        value: Value::text(String::new()),
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

pub(crate) fn composite_prefix_upper(mut prefix: Vec<u8>) -> Option<Vec<u8>> {
    for index in (0..prefix.len()).rev() {
        if prefix[index] != u8::MAX {
            prefix[index] = prefix[index].saturating_add(1);
            prefix.truncate(index.saturating_add(1));
            return Some(prefix);
        }
    }
    None
}

pub(crate) fn board_block_order_key(
    project: &str,
    state: &str,
    order: &str,
    block: &str,
) -> Vec<u8> {
    composite_key([project, state, order, block])
}

pub(crate) fn board_block_order_desc_key(
    project: &str,
    state: &str,
    order: &str,
    block: &str,
) -> Vec<u8> {
    let mut encoded = composite_key([project, state]);
    encoded.extend(
        composite_key([order, block])
            .into_iter()
            .map(|byte| 0xff_u8 - byte),
    );
    encoded
}

pub(crate) fn board_block_member_key(
    project: &str,
    state: &str,
    block: &str,
    position: &str,
    issue: &str,
) -> Vec<u8> {
    composite_key([project, state, block, position, issue])
}

pub(crate) fn board_block_member_desc_key(
    project: &str,
    state: &str,
    block: &str,
    position: &str,
    issue: &str,
) -> Vec<u8> {
    let mut encoded = composite_key([project, state, block]);
    encoded.extend(
        composite_key([position, issue])
            .into_iter()
            .map(|byte| 0xff_u8 - byte),
    );
    encoded
}

pub(crate) fn entity_position_key(kind: &str, project: &str, position: &str, id: &str) -> Vec<u8> {
    composite_key([kind, project, position, id])
}

pub(crate) fn entity_position_desc_key(
    kind: &str,
    project: &str,
    position: &str,
    id: &str,
) -> Vec<u8> {
    let mut encoded = composite_key([kind, project]);
    encoded.extend(
        composite_key([position, id])
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

fn unsigned_field(fields: &[ExtractedField], name: &str) -> Option<u64> {
    fields.iter().find_map(|item| {
        (item.reference == field_ref(name))
            .then_some(&item.value)
            .and_then(|value| match value {
                Value::Unsigned(value) => Some(*value),
                _ => None,
            })
    })
}

pub(crate) fn created_desc_key(
    kind: &str,
    project: Option<&str>,
    created_at: u64,
    id: &str,
) -> Vec<u8> {
    let mut encoded = project.map_or_else(
        || composite_key([kind]),
        |project| composite_key([kind, project]),
    );
    encoded.extend_from_slice(&(!created_at).to_be_bytes());
    encoded.extend_from_slice(&composite_key([id]));
    encoded
}

pub(crate) fn source_created_key(
    kind: &str,
    source: &str,
    created_at: u64,
    id: &str,
    descending: bool,
) -> Vec<u8> {
    let mut encoded = composite_key([kind, source]);
    encoded.extend_from_slice(&if descending { !created_at } else { created_at }.to_be_bytes());
    encoded.extend_from_slice(&composite_key([id]));
    encoded
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
        fields.push(analyzed_search(&searchable));
    }
    if let Some(created_at) = unsigned_field(&fields, field::CREATED_AT) {
        fields.push(bytes(
            field::KIND_CREATED_DESC,
            created_desc_key(kind, None, created_at, id),
        ));
        if let Some(source) = text_field(&fields, field::SOURCE_ID).map(str::to_owned) {
            fields.push(bytes(
                field::KIND_SOURCE_CREATED,
                source_created_key(kind, &source, created_at, id, false),
            ));
            fields.push(bytes(
                field::KIND_SOURCE_CREATED_DESC,
                source_created_key(kind, &source, created_at, id, true),
            ));
        }
    }
    if let Some(project) = text_field(&fields, field::PROJECT).map(str::to_owned) {
        fields.push(bytes(
            field::KIND_PROJECT,
            composite_key([kind, project.as_str()]),
        ));
        if let Some(created_at) = unsigned_field(&fields, field::CREATED_AT) {
            fields.push(bytes(
                field::KIND_PROJECT_CREATED_DESC,
                created_desc_key(kind, Some(&project), created_at, id),
            ));
        }
        if let Some(position) = text_field(&fields, field::POSITION).map(str::to_owned) {
            fields.push(bytes(
                field::KIND_PROJECT_POSITION,
                entity_position_key(kind, &project, &position, id),
            ));
            fields.push(bytes(
                field::KIND_PROJECT_POSITION_DESC,
                entity_position_desc_key(kind, &project, &position, id),
            ));
        }
        if let Some(state) = text_field(&fields, field::STATE).map(str::to_owned) {
            fields.push(bytes(
                field::KIND_PROJECT_STATE,
                composite_key([kind, project.as_str(), state.as_str()]),
            ));
            if kind == "issue" {
                if let (Some(position), Some(block)) = (
                    text_field(&fields, field::POSITION).map(str::to_owned),
                    text_field(&fields, field::BLOCK).map(str::to_owned),
                ) {
                    fields.push(bytes(
                        field::PROJECT_STATE_BLOCK_MEMBER,
                        board_block_member_key(&project, &state, &block, &position, id),
                    ));
                    fields.push(bytes(
                        field::PROJECT_STATE_BLOCK_MEMBER_DESC,
                        board_block_member_desc_key(&project, &state, &block, &position, id),
                    ));
                }
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
        bytes(
            field::RELATION_SOURCE_KIND,
            composite_key([relation_kind, source]),
        ),
    ];
    if relation_kind == "parent" || crate::contract::LINK_KINDS.contains(&relation_kind) {
        fields.push(exact_text(field::GRAPH_SOURCE_ID, source));
        fields.push(exact_text(field::GRAPH_TARGET_ID, target));
    }
    if let Some(project) = project {
        fields.push(exact_text(field::PROJECT, project));
        fields.push(bytes(
            field::KIND_PROJECT,
            composite_key(["relation", project]),
        ));
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

fn tagged_relation(
    tag: &str,
    kind: &str,
    source: &str,
    target: &str,
    project: Option<&str>,
) -> Result<ExtractedNode, Rejection> {
    let mut node = relation(kind, source, target, project)?;
    node.fields.push(exact_text(field::ENTITY_KEY, tag));
    Ok(node)
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
    if extractor.schema != entity_schema_ref() || !migration_sources().contains(&extractor.source) {
        return Err(Rejection::ContractViolation);
    }
    let name = extractor.source.name.as_str();
    if name == crate::v4::ISSUE_PLACEMENT_SCHEMA {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_placement(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_ATTACHMENT_SCHEMA {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_attachment(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_CHECK_SCHEMA {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_check(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_REACTION_SCHEMA {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_reaction(body, &bytes)?));
    }
    if name == crate::v4::PROJECT_HIERARCHY_SCHEMA {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_hierarchy(body, &bytes)?));
    }
    if name == crate::v4::ISSUE_RELATION_SCHEMA {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_issue_relation(body, &bytes)?));
    }
    if name == crate::v4::ENTITY_RELATION_SCHEMA {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        return Ok(finish(ctx, body, extract_entity_relation(body, &bytes)?));
    }
    let mut owned_view;
    let guarded_view;
    let view: &fabric::CollaborativeView = if crate::v4::PHYSICAL_SCHEMAS
        .iter()
        .copied()
        .find(|schema| schema.name() == name)
        .is_some_and(crate::v4::PhysicalSchema::immutable)
    {
        let bytes = ctx.read_body(body)?.ok_or(Rejection::StateCorrupt)?;
        let physical = crate::v4::PHYSICAL_SCHEMAS
            .iter()
            .copied()
            .find(|schema| schema.name() == name)
            .ok_or(Rejection::ContractViolation)?;
        if crate::v4::immutable_record_key(physical, &bytes) != *body {
            return Err(Rejection::StateCorrupt);
        }
        let envelope = crate::v4::ImmutableRecordEnvelope::decode_canonical(&bytes)
            .map_err(|_| Rejection::StateCorrupt)?;
        owned_view = fabric::CollaborativeView::default();
        owned_view.registers.insert(
            crate::v4::roots::IDENTITY.into(),
            envelope
                .identity
                .encode_canonical()
                .map_err(|_| Rejection::StateCorrupt)?,
        );
        owned_view
            .registers
            .insert(crate::v4::roots::RECORD.into(), envelope.record);
        &owned_view
    } else {
        guarded_view = ctx
            .read_collaborative(body)?
            .ok_or(Rejection::StateCorrupt)?;
        &guarded_view
    };
    let nodes = match name {
        contract::ISSUE_SCHEMA => extract_issue(body, view)?,
        contract::SPEC_SCHEMA => extract_spec_marker(body, view)?,
        contract::BASELINE_SCHEMA => extract_baseline_marker(body, view)?,
        crate::v4::SPACE_DIRECTORY_SCHEMA => extract_space(body, view)?,
        crate::v4::SPACE_CONTENT_SCHEMA => extract_space_content(body, view)?,
        crate::v4::GOVERNANCE_REVISION_SCHEMA => extract_governance(body, view)?,
        crate::v4::GOVERNANCE_HEADS_SCHEMA => extract_governance_heads(body, view)?,
        crate::v4::PROJECT_META_SCHEMA => extract_project(body, view)?,
        crate::v4::PROJECT_CONTENT_SCHEMA => extract_project_content(body, view)?,
        crate::v4::WORKFLOW_REVISION_SCHEMA => extract_workflow(body, view)?,
        crate::v4::WORKFLOW_HEADS_SCHEMA => extract_workflow_heads(body, view)?,
        crate::v4::PROJECT_SCHEDULE_SCHEMA => extract_schedule(body, view)?,
        crate::v4::PROJECT_UPDATES_SCHEMA => extract_updates(body, view)?,
        crate::v4::SPACE_TRIAGE_SCHEMA => extract_triage(body, view)?,
        crate::v4::ISSUE_COMMENT_SCHEMA => extract_comment(body, view)?,
        crate::v4::ISSUE_ACTIVITY_SCHEMA => extract_activity(body, view)?,
        crate::v4::ISSUE_IDENTITY_SCHEMA => extract_issue_identity(body, view)?,
        crate::v4::ISSUE_META_SCHEMA => extract_issue_meta(body, view)?,
        crate::v4::ISSUE_TRANSITION_SCHEMA => extract_issue_transition(view)?,
        crate::v4::BOARD_BLOCK_SCHEMA => extract_board_block(body, view)?,
        crate::v4::BOARD_LANE_SCHEMA => extract_board_lane(body, view)?,
        crate::v4::INITIATIVE_SCHEMA => extract_initiative(body, view)?,
        crate::v4::INITIATIVE_CONTENT_SCHEMA => extract_initiative_content(body, view)?,
        crate::v4::TEAM_SCHEMA => extract_team(body, view)?,
        crate::v4::LABEL_SCHEMA => extract_label(body, view)?,
        crate::v4::REVISION_ALIAS_SCHEMA => extract_revision_alias(body, view)?,
        crate::v4::SPEC_REVISION_SCHEMA => extract_spec_revision(view)?,
        crate::v4::SPEC_HEADS_SCHEMA => extract_spec_heads(body, view)?,
        crate::v4::SPEC_OBSERVATION_SCHEMA => extract_spec_observation(view)?,
        crate::v4::BASELINE_REVISION_SCHEMA => extract_baseline_revision(view)?,
        crate::v4::BASELINE_HEADS_SCHEMA => extract_baseline_heads(body, view)?,
        _ => return Err(Rejection::ContractViolation),
    };
    Ok(finish(ctx, body, nodes))
}

fn extract_spec_revision(
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::SpecRevisionRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let revision = record.revision;
    let revision_id = revision.revision.clone();
    let mut nodes = vec![entity(
        &revision_id,
        revision.body.kind.as_str(),
        vec![
            analyzed_text(field::TITLE, &revision.body.title),
            analyzed_text(field::TEXT, &revision.body.text),
            exact_text(field::PROJECT, &revision.body.project),
            exact_text(field::STATE, revision.body.state.as_str()),
            exact_text(field::AUTHOR, &revision.body.author),
            unsigned(field::CREATED_AT, revision.body.ts),
            exact_text(field::REVISION, &revision_id),
            exact_text(field::SOURCE_ID, &revision.body.spec),
            exact_text(field::RELATION_KIND, "spec_revision"),
        ],
    )?];
    nodes.push(relation(
        "spec_revision",
        &revision.body.spec,
        &revision_id,
        Some(&revision.body.project),
    )?);
    for predecessor in revision.predecessors {
        nodes.push(relation(
            "predecessor",
            &revision_id,
            &predecessor,
            Some(&revision.body.project),
        )?);
    }
    for link in revision.body.links {
        let target = match &link.target {
            crate::spec::Target::Spec { revision, .. }
            | crate::spec::Target::Baseline { revision, .. } => revision.as_str(),
            crate::spec::Target::Issue { issue } => issue.as_str(),
        };
        nodes.push(tagged_relation(
            "spec_reference",
            link.rel.as_str(),
            &revision_id,
            target,
            Some(&revision.body.project),
        )?);
    }
    if let Some(plan) = revision.body.plan {
        for root in plan.roots {
            nodes.push(relation(
                "plan_root",
                &revision_id,
                &root,
                Some(&revision.body.project),
            )?);
        }
    }
    Ok(nodes)
}

fn extract_spec_heads(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let spec = register(view, crate::v4::roots::IDENTITY);
    let parsed = crate::ids::SpecId::parse(&spec).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::spec_heads_key(&parsed) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let project = register(view, crate::v4::roots::PROJECT);
    let kind = register(view, crate::v4::roots::KIND);
    crate::ids::ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
    let kind = crate::spec::Kind::parse(&kind).ok_or(Rejection::StateCorrupt)?;
    let head_count = view.sets.get(crate::v4::roots::HEADS).map_or(0, Vec::len);
    let issued_count = view
        .sets
        .get(crate::v4::roots::ISSUED_HEADS)
        .map_or(0, Vec::len);
    if head_count > crate::v4::MAX_CONCURRENT_HEADS
        || issued_count > crate::v4::MAX_CONCURRENT_HEADS
    {
        return Err(Rejection::LimitExceeded);
    }
    let mut heads = view
        .sets
        .get(crate::v4::roots::HEADS)
        .into_iter()
        .flatten()
        .map(|value| String::from_utf8(value.clone()).map_err(|_| Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    let mut issued = view
        .sets
        .get(crate::v4::roots::ISSUED_HEADS)
        .into_iter()
        .flatten()
        .map(|value| String::from_utf8(value.clone()).map_err(|_| Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    heads.sort();
    heads.dedup();
    issued.sort();
    issued.dedup();
    let mut nodes = vec![entity(
        &spec,
        "spec",
        vec![
            exact_text(field::PROJECT, &project),
            exact_text(field::ENTITY_KEY, kind.as_str()),
            exact_text(field::RELATION_KIND, "spec_document"),
            bytes(
                field::HEAD_REVISIONS,
                serde_json::to_vec(&heads).map_err(|_| Rejection::StateCorrupt)?,
            ),
            bytes(
                field::ISSUED_REVISIONS,
                serde_json::to_vec(&issued).map_err(|_| Rejection::StateCorrupt)?,
            ),
            boolean(field::CONFLICTED, heads.len() > 1 || issued.len() > 1),
        ],
    )?];
    for revision in view.sets.get(crate::v4::roots::HEADS).into_iter().flatten() {
        let revision = String::from_utf8(revision.clone()).map_err(|_| Rejection::StateCorrupt)?;
        crate::spec::decode_revision(&revision).ok_or(Rejection::StateCorrupt)?;
        nodes.push(entity(
            &format!("spec-head:{spec}:{revision}"),
            "spec_head",
            vec![
                exact_text(field::PROJECT, &project),
                exact_text(field::SOURCE_ID, &spec),
                exact_text(field::TARGET_ID, &revision),
                exact_text(field::REVISION, revision),
            ],
        )?);
    }
    for revision in view
        .sets
        .get(crate::v4::roots::ISSUED_HEADS)
        .into_iter()
        .flatten()
    {
        let revision = String::from_utf8(revision.clone()).map_err(|_| Rejection::StateCorrupt)?;
        crate::spec::decode_revision(&revision).ok_or(Rejection::StateCorrupt)?;
        nodes.push(entity(
            &format!("spec-issued:{spec}:{revision}"),
            "spec_issued",
            vec![
                exact_text(field::PROJECT, &project),
                exact_text(field::SOURCE_ID, &spec),
                exact_text(field::TARGET_ID, &revision),
                exact_text(field::REVISION, revision),
            ],
        )?);
    }
    Ok(nodes)
}

fn extract_baseline_revision(
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::BaselineRevisionRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let revision = record.revision;
    let id = revision.revision.clone();
    let mut nodes = vec![entity(
        &id,
        "baseline_revision",
        vec![
            analyzed_text(field::TITLE, &revision.body.name),
            exact_text(field::PROJECT, &revision.body.project),
            exact_text(field::STATE, revision.body.state.as_str()),
            exact_text(field::AUTHOR, &revision.body.author),
            unsigned(field::CREATED_AT, revision.body.ts),
            exact_text(field::REVISION, &id),
            exact_text(field::SOURCE_ID, &revision.body.baseline),
            exact_text(field::RELATION_KIND, "baseline_revision"),
        ],
    )?];
    nodes.push(relation(
        "baseline_revision",
        &revision.body.baseline,
        &id,
        Some(&revision.body.project),
    )?);
    for predecessor in revision.predecessors {
        nodes.push(relation(
            "predecessor",
            &id,
            &predecessor,
            Some(&revision.body.project),
        )?);
    }
    for member in revision.body.members {
        nodes.push(relation(
            "baseline_member",
            &id,
            &member.revision,
            Some(&revision.body.project),
        )?);
    }
    Ok(nodes)
}

fn extract_baseline_heads(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let baseline = register(view, crate::v4::roots::IDENTITY);
    let parsed = crate::ids::BaselineId::parse(&baseline).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::baseline_heads_key(&parsed) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let project = register(view, crate::v4::roots::PROJECT);
    crate::ids::ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
    let head_count = view.sets.get(crate::v4::roots::HEADS).map_or(0, Vec::len);
    let issued_count = view
        .sets
        .get(crate::v4::roots::ISSUED_HEADS)
        .map_or(0, Vec::len);
    if head_count > crate::v4::MAX_CONCURRENT_HEADS
        || issued_count > crate::v4::MAX_CONCURRENT_HEADS
    {
        return Err(Rejection::LimitExceeded);
    }
    let mut heads = view
        .sets
        .get(crate::v4::roots::HEADS)
        .into_iter()
        .flatten()
        .map(|value| String::from_utf8(value.clone()).map_err(|_| Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    let mut issued = view
        .sets
        .get(crate::v4::roots::ISSUED_HEADS)
        .into_iter()
        .flatten()
        .map(|value| String::from_utf8(value.clone()).map_err(|_| Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    heads.sort();
    heads.dedup();
    issued.sort();
    issued.dedup();
    let mut nodes = vec![entity(
        &baseline,
        "baseline",
        vec![
            exact_text(field::PROJECT, &project),
            exact_text(field::RELATION_KIND, "baseline_document"),
            bytes(
                field::HEAD_REVISIONS,
                serde_json::to_vec(&heads).map_err(|_| Rejection::StateCorrupt)?,
            ),
            bytes(
                field::ISSUED_REVISIONS,
                serde_json::to_vec(&issued).map_err(|_| Rejection::StateCorrupt)?,
            ),
            boolean(field::CONFLICTED, heads.len() > 1 || issued.len() > 1),
        ],
    )?];
    for revision in view.sets.get(crate::v4::roots::HEADS).into_iter().flatten() {
        let revision = String::from_utf8(revision.clone()).map_err(|_| Rejection::StateCorrupt)?;
        crate::spec::decode_revision(&revision).ok_or(Rejection::StateCorrupt)?;
        nodes.push(entity(
            &format!("baseline-head:{baseline}:{revision}"),
            "baseline_head",
            vec![
                exact_text(field::PROJECT, &project),
                exact_text(field::SOURCE_ID, &baseline),
                exact_text(field::TARGET_ID, &revision),
                exact_text(field::REVISION, revision),
            ],
        )?);
    }
    for revision in view
        .sets
        .get(crate::v4::roots::ISSUED_HEADS)
        .into_iter()
        .flatten()
    {
        let revision = String::from_utf8(revision.clone()).map_err(|_| Rejection::StateCorrupt)?;
        crate::spec::decode_revision(&revision).ok_or(Rejection::StateCorrupt)?;
        nodes.push(entity(
            &format!("baseline-issued:{baseline}:{revision}"),
            "baseline_issued",
            vec![
                exact_text(field::PROJECT, &project),
                exact_text(field::SOURCE_ID, &baseline),
                exact_text(field::TARGET_ID, &revision),
                exact_text(field::REVISION, revision),
            ],
        )?);
    }
    Ok(nodes)
}

fn extract_spec_observation(
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::SpecObservationRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    match record {
        crate::v4::SpecObservationRecord::Assert {
            project,
            observation,
        } => {
            let target = match &observation.target {
                crate::spec::Target::Spec { revision, .. }
                | crate::spec::Target::Baseline { revision, .. } => revision.as_str(),
                crate::spec::Target::Issue { issue } => issue.as_str(),
            };
            Ok(vec![entity(
                &observation.observation,
                "spec_observation_fact",
                vec![
                    exact_text(field::STATE, "assert"),
                    exact_text(field::PROJECT, project),
                    exact_text(field::SOURCE_ID, &observation.spec),
                    exact_text(field::TARGET_ID, target),
                    exact_text(field::RELATION_KIND, observation.rel.as_str()),
                    exact_text(field::AUTHOR, observation.observer),
                    analyzed_text(field::TEXT, observation.note),
                    unsigned(field::CREATED_AT, observation.ts),
                ],
            )?])
        }
        crate::v4::SpecObservationRecord::Retract {
            project,
            observation,
            spec,
            actor,
            timestamp,
        } => Ok(vec![entity(
            &format!("observation-retraction:{observation}"),
            "spec_observation_fact",
            vec![
                exact_text(field::STATE, "retract"),
                exact_text(field::PROJECT, project),
                exact_text(field::SOURCE_ID, spec),
                exact_text(field::TARGET_ID, observation),
                exact_text(field::AUTHOR, actor),
                unsigned(field::CREATED_AT, timestamp),
            ],
        )?]),
    }
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
    _body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
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

fn revision_heads(view: &fabric::CollaborativeView) -> Result<Vec<String>, Rejection> {
    let count = view.sets.get(crate::v4::roots::HEADS).map_or(0, Vec::len);
    if count > crate::v4::MAX_CONCURRENT_HEADS {
        return Err(Rejection::LimitExceeded);
    }
    let mut heads = view
        .sets
        .get(crate::v4::roots::HEADS)
        .into_iter()
        .flatten()
        .map(|value| String::from_utf8(value.clone()).map_err(|_| Rejection::StateCorrupt))
        .collect::<Result<Vec<_>, _>>()?;
    if heads.iter().any(|head| head.is_empty() || head.len() > 256) {
        return Err(Rejection::StateCorrupt);
    }
    heads.sort();
    heads.dedup();
    Ok(heads)
}

fn extract_governance_heads(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let role = register(view, crate::v4::roots::IDENTITY);
    if role.is_empty() || crate::v4::governance_heads_key(&role) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let heads = revision_heads(view)?;
    Ok(vec![entity(
        &format!("role:{role}"),
        "role_head",
        vec![
            exact_text(field::ENTITY_KEY, role),
            bytes(
                field::HEAD_REVISIONS,
                serde_json::to_vec(&heads).map_err(|_| Rejection::StateCorrupt)?,
            ),
            boolean(field::CONFLICTED, heads.len() != 1),
            boolean(field::TOMBSTONE, false),
        ],
    )?])
}

fn extract_workflow(
    _body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
    let project_id = ProjectId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
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

fn extract_workflow_heads(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let project = register(view, crate::v4::roots::IDENTITY);
    let project_id = ProjectId::parse(&project).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::workflow_heads_key(&project_id) != *body {
        return Err(Rejection::StateCorrupt);
    }
    let heads = revision_heads(view)?;
    Ok(vec![entity(
        &format!("workflow:{project}"),
        "workflow_head",
        vec![
            exact_text(field::PROJECT, &project),
            exact_text(field::SOURCE_ID, project),
            bytes(
                field::HEAD_REVISIONS,
                serde_json::to_vec(&heads).map_err(|_| Rejection::StateCorrupt)?,
            ),
            boolean(field::CONFLICTED, heads.len() != 1),
        ],
    )?])
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
    _body: &BodyKey,
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
    if envelope_identity.owner != record.issue || envelope_identity.record != "identity" {
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
            exact_text(
                field::ALIAS_COORDINATE,
                format!("{}-{}", record.alias.ordinal, record.alias.suffix()),
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
            exact_text(field::BLOCK, &record.placement.block),
            exact_text(field::POSITION, &record.placement.position),
        ],
    )?])
}

fn extract_issue_transition(
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let raw = view
        .registers
        .get(crate::v4::roots::RECORD)
        .ok_or(Rejection::StateCorrupt)?;
    let record = crate::v4::IssueTransitionRecord::decode_canonical(raw)
        .map_err(|_| Rejection::StateCorrupt)?;
    let transition = record
        .transition_id()
        .map_err(|_| Rejection::StateCorrupt)?;
    let transition_node = entity(
        &transition,
        "issue_transition",
        vec![
            exact_text(field::SOURCE_ID, record.issue.clone()),
            exact_text(field::PROJECT, record.placement.project.clone()),
            exact_text(field::STATE, record.placement.workflow_state.clone()),
            exact_text(field::BLOCK, record.placement.block.clone()),
            exact_text(field::POSITION, record.placement.position.clone()),
            exact_text(field::AUTHOR, record.actor.clone()),
            unsigned(field::CREATED_AT, record.timestamp),
        ],
    )?;
    let mut nodes = vec![transition_node];
    for predecessor in record.predecessors {
        nodes.push(relation(
            "transition_predecessor",
            &transition,
            &predecessor,
            Some(&record.placement.project),
        )?);
    }
    nodes.push(relation(
        "issue_transition",
        &record.issue,
        &transition,
        Some(&record.placement.project),
    )?);
    Ok(nodes)
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
    let mut heads = view
        .sets
        .get(crate::v4::roots::PLACEMENT_HEADS)
        .into_iter()
        .flatten()
        .map(|value| {
            crate::v4::IssueTransitionHead::decode_canonical(value)
                .map_err(|_| Rejection::StateCorrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if heads.len() > crate::v4::MAX_CONCURRENT_HEADS {
        return Err(Rejection::LimitExceeded);
    }
    heads.sort_by(|left, right| left.transition.cmp(&right.transition));
    heads.dedup_by(|left, right| left.transition == right.transition);
    if heads
        .iter()
        .any(|head| head.validate().is_err() || head.core.issue != issue)
    {
        return Err(Rejection::StateCorrupt);
    }
    let rank_overlay = view
        .registers
        .get(crate::v4::roots::RANK_OVERLAY)
        .filter(|bytes| !bytes.is_empty())
        .map(|bytes| {
            let overlay = crate::v4::IssueRankOverlay::decode_canonical(bytes)
                .map_err(|_| Rejection::StateCorrupt)?;
            overlay.validate().map_err(|_| Rejection::StateCorrupt)?;
            if overlay.issue != issue {
                return Err(Rejection::StateCorrupt);
            }
            Ok(overlay)
        })
        .transpose()?;
    let effective_placement = match heads.as_slice() {
        [head] => {
            let mut placement = head.core.placement.clone();
            if let Some(overlay) = &rank_overlay {
                if overlay.transition == head.transition
                    && overlay.project == placement.project
                    && overlay.workflow_state == placement.workflow_state
                {
                    placement.block.clone_from(&overlay.block);
                    placement.position.clone_from(&overlay.position);
                }
            }
            Some(placement)
        }
        _ => None,
    };
    fields.push(boolean(field::CONFLICTED, heads.len() > 1));
    if let Some(placement) = &effective_placement {
        fields.extend([
            exact_text(field::PROJECT, &placement.project),
            exact_text(field::STATE, &placement.workflow_state),
            exact_text(field::BLOCK, &placement.block),
            exact_text(field::POSITION, &placement.position),
        ]);
        if let [head] = heads.as_slice() {
            fields.push(exact_text(field::PLACEMENT_TRANSITION, &head.transition));
        }
    }
    let issue_node = entity(&issue, "issue", fields)?;
    let mut nodes = vec![issue_node];
    for head in &heads {
        nodes.push(relation(
            "transition_head",
            &issue,
            &head.transition,
            Some(&head.core.placement.project),
        )?);
    }
    Ok(nodes)
}

fn extract_board_block(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let mut heads = view
        .sets
        .get(crate::v4::roots::BLOCK_HEADS)
        .into_iter()
        .flatten()
        .map(|value| {
            crate::v4::BoardBlockHead::decode_canonical(value).map_err(|_| Rejection::StateCorrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    heads.sort_by(|left, right| left.revision.cmp(&right.revision));
    heads.dedup_by(|left, right| left.revision == right.revision);
    let first = heads.first().ok_or(Rejection::StateCorrupt)?;
    if heads.iter().any(|head| {
        head.validate().is_err()
            || head.core.project != first.core.project
            || head.core.workflow_state != first.core.workflow_state
            || head.core.block != first.core.block
    }) {
        return Err(Rejection::StateCorrupt);
    }
    let project = ProjectId::parse(&first.core.project).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::board_block_key(&project, &first.core.workflow_state, &first.core.block) != *body
    {
        return Err(Rejection::StateCorrupt);
    }
    let conflicted = heads.len() != 1;
    let mut fields = vec![
        exact_text(field::PROJECT, &first.core.project),
        exact_text(field::STATE, &first.core.workflow_state),
        exact_text(field::BLOCK, &first.core.block),
        exact_text(field::REVISION, &first.revision),
        boolean(field::CONFLICTED, conflicted),
    ];
    let mut order = first.core.order.clone();
    if !conflicted {
        if let Some(raw) = view
            .registers
            .get(crate::v4::roots::ORDER_OVERLAY)
            .filter(|raw| !raw.is_empty())
        {
            let overlay = crate::v4::BoardBlockOrderOverlay::decode_canonical(raw)
                .map_err(|_| Rejection::StateCorrupt)?;
            overlay.validate().map_err(|_| Rejection::StateCorrupt)?;
            if overlay.block_revision == first.revision {
                order = overlay.order;
            }
        }
    }
    // Even a conflicted block retains one deterministic index coordinate so
    // ordered traversal encounters the conflict and refuses it. Omitting the
    // posting would silently make the block (and its Issues) disappear.
    fields.push(exact_text(field::POSITION, &order));
    fields.push(bytes(
        field::PROJECT_STATE_BLOCK_ORDER,
        board_block_order_key(
            &first.core.project,
            &first.core.workflow_state,
            &order,
            &first.core.block,
        ),
    ));
    fields.push(bytes(
        field::PROJECT_STATE_BLOCK_ORDER_DESC,
        board_block_order_desc_key(
            &first.core.project,
            &first.core.workflow_state,
            &order,
            &first.core.block,
        ),
    ));
    Ok(vec![entity(&first.core.block, "board_block", fields)?])
}

fn extract_board_lane(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let mut heads = view
        .sets
        .get(crate::v4::roots::TOPOLOGY_HEADS)
        .into_iter()
        .flatten()
        .map(|value| {
            crate::v4::BoardTopologyHead::decode_canonical(value)
                .map_err(|_| Rejection::StateCorrupt)
        })
        .collect::<Result<Vec<_>, _>>()?;
    heads.sort_by(|left, right| left.transition.cmp(&right.transition));
    heads.dedup_by(|left, right| left.transition == right.transition);
    let first = heads.first().ok_or(Rejection::StateCorrupt)?;
    if heads.iter().any(|head| {
        head.validate().is_err()
            || head.core.project != first.core.project
            || head.core.workflow_state != first.core.workflow_state
    }) {
        return Err(Rejection::StateCorrupt);
    }
    let project = ProjectId::parse(&first.core.project).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::board_lane_key(&project, &first.core.workflow_state) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("lane:{}:{}", first.core.project, first.core.workflow_state),
        "board_lane",
        vec![
            exact_text(field::PROJECT, &first.core.project),
            exact_text(field::STATE, &first.core.workflow_state),
            boolean(field::CONFLICTED, heads.len() != 1),
        ],
    )?])
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
        description: String::new(),
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    let mut nodes = vec![entity(
        &id,
        "space",
        vec![analyzed_text(field::TITLE, record.name)],
    )?];
    for role in crate::roles::BUILT_IN_ROLE_IDS {
        let revision = crate::roles::built_in(role).ok_or(Rejection::StateCorrupt)?;
        let heads = vec![data_encoding::HEXLOWER.encode(&revision.revision_id)];
        nodes.push(entity(
            &format!("role:{role}"),
            "role_head",
            vec![
                exact_text(field::ENTITY_KEY, role),
                exact_text(field::STATE, "built_in"),
                bytes(
                    field::HEAD_REVISIONS,
                    serde_json::to_vec(&heads).map_err(|_| Rejection::StateCorrupt)?,
                ),
                boolean(field::CONFLICTED, false),
                boolean(field::TOMBSTONE, false),
            ],
        )?);
    }
    Ok(nodes)
}

fn extract_space_content(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let id = register(view, crate::v4::roots::IDENTITY);
    let space = mechanics::ids::SpaceId::parse(&id).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::space_content_key(&space) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("space-content:{id}"),
        "space_content",
        vec![
            exact_text(field::SOURCE_ID, id),
            analyzed_text(field::TEXT, text(view, crate::v4::roots::DESCRIPTION)),
        ],
    )?])
}

fn extract_revision_alias(
    _body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity)
        .map_err(|_| Rejection::StateCorrupt)?;
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
        description: String::new(),
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
        exact_text(field::PROJECT, &project),
        exact_text(field::HEALTH, meta.color),
        exact_text(field::AUTHOR, meta.lead),
        exact_text(field::SOURCE_ID, meta.team.clone()),
        boolean(field::ARCHIVED, meta.archived),
        boolean(field::TOMBSTONE, meta.tombstone),
    ];
    if let Some(start) = meta.start_date {
        fields.push(unsigned(field::CREATED_AT, start));
    }
    if let Some(target) = meta.target_date {
        fields.push(unsigned(field::TARGET_DATE, target));
    }
    let mut nodes = vec![entity(&project, "project", fields)?];
    if !meta.team.is_empty() {
        nodes.push(relation("team", &project, &meta.team, Some(&project))?);
    }
    Ok(nodes)
}

fn extract_project_content(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let id = register(view, crate::v4::roots::IDENTITY);
    let project = ProjectId::parse(&id).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::project_content_key(&project) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("project-content:{id}"),
        "project_content",
        vec![
            exact_text(field::PROJECT, id.clone()),
            exact_text(field::SOURCE_ID, id),
            analyzed_text(field::TEXT, text(view, crate::v4::roots::DESCRIPTION)),
        ],
    )?])
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
                analyzed_text(field::TITLE, name.clone()),
                exact_text(field::EXACT_NAME, name.trim().to_ascii_lowercase()),
                analyzed_text(field::TEXT, description),
                exact_text(field::PROJECT, &project),
                boolean(field::TOMBSTONE, tombstone),
            ];
            if !tombstone {
                fields.push(exact_text(field::POSITION, position));
            }
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
                    analyzed_text(field::TITLE, name.clone()),
                    exact_text(field::EXACT_NAME, name.trim().to_ascii_lowercase()),
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
    _body: &BodyKey,
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
    _body: &BodyKey,
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
                "triage_fact",
                vec![
                    exact_text(field::STATE, "submission"),
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
                "triage_fact",
                vec![
                    exact_text(field::STATE, "decision"),
                    exact_text(field::HEALTH, outcome),
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
                "triage_fact",
                vec![
                    exact_text(field::STATE, "resolution"),
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
    _body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity_bytes = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    let issue = crate::ids::DocId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
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
    _body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let identity_bytes = view
        .registers
        .get(crate::v4::roots::IDENTITY)
        .ok_or(Rejection::StateCorrupt)?;
    let identity = crate::v4::RecordBodyIdentityRecord::decode_canonical(identity_bytes)
        .map_err(|_| Rejection::StateCorrupt)?;
    crate::ids::DocId::parse(&identity.owner).ok_or(Rejection::StateCorrupt)?;
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
    let inbox_kind = event.inbox_kind().map(str::to_owned);
    let id = format!("activity:{}:{}", identity.owner, identity.record);
    let mut nodes = vec![
        entity(
            &id,
            "activity",
            vec![
                exact_text(field::STATE, event.k.as_str()),
                exact_text(field::AUTHOR, event.a.as_str()),
                analyzed_text(field::TEXT, event.x.as_str()),
                unsigned(field::CREATED_AT, event.t),
                exact_text(field::SOURCE_ID, &identity.owner),
            ],
        )?,
        relation("issue", &id, &identity.owner, None)?,
    ];
    if let Some(inbox_kind) = inbox_kind {
        let reverse_time = format!("{:020}", u64::MAX.saturating_sub(event.t));
        for recipient in record.recipients {
            let inbox_id = format!("inbox:{recipient}:{id}");
            nodes.push(entity(
                &inbox_id,
                "inbox",
                vec![
                    exact_text(field::STATE, inbox_kind.as_str()),
                    exact_text(field::AUTHOR, &event.a),
                    exact_text(field::DEVICE, &event.d),
                    unsigned(field::CREATED_AT, event.t),
                    exact_text(field::SOURCE_ID, &identity.owner),
                    bytes(
                        field::INBOX_ORDER,
                        composite_key([recipient.as_str(), reverse_time.as_str(), id.as_str()]),
                    ),
                ],
            )?);
        }
    }
    Ok(nodes)
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
        description: String::new(),
        owner: register(view, crate::v4::roots::OWNER),
        health: register(view, crate::v4::roots::HEALTH),
        target_date: optional_u64(view, crate::v4::roots::TARGET_DATE),
        tombstone,
    };
    record.validate().map_err(|_| Rejection::StateCorrupt)?;
    let mut fields = vec![
        analyzed_text(field::TITLE, record.name.clone()),
        exact_text(field::EXACT_NAME, record.name.trim().to_ascii_lowercase()),
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

fn extract_initiative_content(
    body: &BodyKey,
    view: &fabric::CollaborativeView,
) -> Result<Vec<ExtractedNode>, Rejection> {
    let id = register(view, crate::v4::roots::IDENTITY);
    let initiative = crate::ids::InitiativeId::parse(&id).ok_or(Rejection::StateCorrupt)?;
    if crate::v4::initiative_content_key(&initiative) != *body {
        return Err(Rejection::StateCorrupt);
    }
    Ok(vec![entity(
        &format!("initiative-content:{id}"),
        "initiative_content",
        vec![
            exact_text(field::SOURCE_ID, id),
            analyzed_text(field::TEXT, text(view, crate::v4::roots::DESCRIPTION)),
        ],
    )?])
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
            exact_text(field::HEALTH, record.icon),
            exact_text(field::AUTHOR, record.lead),
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
                block: crate::v4::board_seed_block_id(PROJECT, "in_progress"),
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
    fn concurrent_transition_heads_are_visible_and_inert_on_the_board() {
        let issue = crate::ids::DocId::parse(ISSUE).unwrap();
        let placement = |state: &str, position: &str| crate::v4::BoardPlacement {
            project: PROJECT.into(),
            workflow_state: state.into(),
            block: crate::v4::board_seed_block_id(PROJECT, state),
            position: position.into(),
        };
        let mut meta = fabric::CollaborativeView::default();
        for (path, value) in [
            (crate::v4::roots::IDENTITY, ISSUE),
            (crate::v4::roots::TITLE, "Concurrent move"),
            (crate::v4::roots::PRIORITY, "none"),
            (crate::v4::roots::CREATED_AT, "1"),
            (crate::v4::roots::TOMBSTONE, "0"),
        ] {
            meta.registers
                .insert(path.into(), value.as_bytes().to_vec());
        }
        let first = crate::v4::IssueTransitionHead {
            core: crate::v4::IssueTransitionCore {
                issue: ISSUE.into(),
                predecessors: Vec::new(),
                placement: placement("active", "F"),
                actor: "act_0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                timestamp: 1,
            },
            transition: String::new(),
        };
        let first = crate::v4::IssueTransitionHead {
            transition: first.core.transition_id().unwrap(),
            ..first
        };
        meta.sets.insert(
            crate::v4::roots::PLACEMENT_HEADS.into(),
            vec![first.encode_canonical().unwrap()],
        );

        let sole = extract_issue_meta(&crate::v4::issue_meta_key(&issue), &meta).unwrap();
        let sole_issue = sole
            .iter()
            .find(|node| node.key.node.as_bytes() == ISSUE.as_bytes())
            .unwrap();
        assert!(sole_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::PROJECT)
                && field.value == Value::Text(PROJECT.into())
        }));
        assert!(sole_issue
            .fields
            .iter()
            .any(|field| field.reference == field_ref(field::PROJECT_STATE_BLOCK_MEMBER)));

        // Rank maintenance is non-semantic and fenced to the exact placement
        // transition. The same final state results whether this old overlay
        // arrives before or after the successor move: once the head changes,
        // the overlay is inert and cannot pull the card back into its old
        // block during a concurrent split.
        let old_overlay = crate::v4::IssueRankOverlay {
            issue: ISSUE.into(),
            transition: first.transition.clone(),
            project: PROJECT.into(),
            workflow_state: "active".into(),
            block: crate::v4::board_seed_block_id(PROJECT, "active"),
            position: "Z".into(),
            maintenance: "split-maintenance".into(),
        };
        meta.registers.insert(
            crate::v4::roots::RANK_OVERLAY.into(),
            old_overlay.encode_canonical().unwrap(),
        );
        let maintained = extract_issue_meta(&crate::v4::issue_meta_key(&issue), &meta).unwrap();
        let maintained_issue = maintained
            .iter()
            .find(|node| node.key.node.as_bytes() == ISSUE.as_bytes())
            .unwrap();
        assert!(maintained_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::POSITION) && field.value == Value::Text("Z".into())
        }));

        let mut split_placement = placement("active", "U");
        split_placement.block = "a".repeat(64);
        let successor = crate::v4::IssueTransitionHead {
            core: crate::v4::IssueTransitionCore {
                issue: ISSUE.into(),
                predecessors: vec![first.transition.clone()],
                placement: split_placement.clone(),
                actor: "act_0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                timestamp: 2,
            },
            transition: String::new(),
        };
        let successor = crate::v4::IssueTransitionHead {
            transition: successor.core.transition_id().unwrap(),
            ..successor
        };
        meta.sets.insert(
            crate::v4::roots::PLACEMENT_HEADS.into(),
            vec![successor.encode_canonical().unwrap()],
        );
        let moved = extract_issue_meta(&crate::v4::issue_meta_key(&issue), &meta).unwrap();
        let moved_issue = moved
            .iter()
            .find(|node| node.key.node.as_bytes() == ISSUE.as_bytes())
            .unwrap();
        assert!(moved_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::POSITION) && field.value == Value::Text("U".into())
        }));
        assert!(!moved_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::POSITION) && field.value == Value::Text("Z".into())
        }));
        assert!(moved_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::BLOCK)
                && field.value == Value::Text(split_placement.block.clone().into())
        }));

        // Rebuild the same converged view in the opposite delivery order:
        // successor transition first, stale split-maintenance overlay second.
        // The overlay remains inert because it names the predecessor head.
        let mut successor_then_maintenance = meta.clone();
        successor_then_maintenance
            .registers
            .remove(crate::v4::roots::RANK_OVERLAY);
        successor_then_maintenance.sets.insert(
            crate::v4::roots::PLACEMENT_HEADS.into(),
            vec![successor.encode_canonical().unwrap()],
        );
        successor_then_maintenance.registers.insert(
            crate::v4::roots::RANK_OVERLAY.into(),
            old_overlay.encode_canonical().unwrap(),
        );
        let reverse = extract_issue_meta(
            &crate::v4::issue_meta_key(&issue),
            &successor_then_maintenance,
        )
        .unwrap();
        let reverse_issue = reverse
            .iter()
            .find(|node| node.key.node.as_bytes() == ISSUE.as_bytes())
            .unwrap();
        assert!(reverse_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::POSITION) && field.value == Value::Text("U".into())
        }));
        assert!(!reverse_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::POSITION) && field.value == Value::Text("Z".into())
        }));
        assert!(reverse_issue.fields.iter().any(|field| {
            field.reference == field_ref(field::BLOCK)
                && field.value == Value::Text(split_placement.block.clone().into())
        }));

        meta.registers.remove(crate::v4::roots::RANK_OVERLAY);
        meta.sets.insert(
            crate::v4::roots::PLACEMENT_HEADS.into(),
            vec![first.encode_canonical().unwrap()],
        );

        let second = crate::v4::IssueTransitionHead {
            core: crate::v4::IssueTransitionCore {
                issue: ISSUE.into(),
                predecessors: Vec::new(),
                placement: placement("done", "U"),
                actor: "act_0000000000000000000000000000000000000000000000000000000000000000"
                    .into(),
                timestamp: 2,
            },
            transition: String::new(),
        };
        let second = crate::v4::IssueTransitionHead {
            transition: second.core.transition_id().unwrap(),
            ..second
        };
        meta.sets
            .get_mut(crate::v4::roots::PLACEMENT_HEADS)
            .unwrap()
            .push(second.encode_canonical().unwrap());
        let conflicted = extract_issue_meta(&crate::v4::issue_meta_key(&issue), &meta).unwrap();
        let issue_node = conflicted
            .iter()
            .find(|node| node.key.node.as_bytes() == ISSUE.as_bytes())
            .unwrap();
        assert!(issue_node.fields.iter().any(|field| {
            field.reference == field_ref(field::CONFLICTED) && field.value == Value::Bool(true)
        }));
        assert!(!issue_node
            .fields
            .iter()
            .any(|field| field.reference == field_ref(field::PROJECT)));
        assert_eq!(conflicted.len(), 3, "issue plus two visible head facts");

        // A peer cannot forge current board placement merely by pairing an
        // authentic transition id with a different projection in IssueMeta.
        let mut forged = first.clone();
        forged.core.placement.workflow_state = "done".into();
        meta.sets.insert(
            crate::v4::roots::PLACEMENT_HEADS.into(),
            vec![serde_json::to_vec(&forged).unwrap()],
        );
        assert_eq!(
            extract_issue_meta(&crate::v4::issue_meta_key(&issue), &meta),
            Err(Rejection::StateCorrupt)
        );
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
