#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "v4 schema identifiers are compile-time constants and digest prefixes have fixed lengths"
)]
//! Physical vocabulary for the post-Catalog Issues representation.
//!
//! This module deliberately contains no read or write routing. It names the
//! Bodies the v4 implementation will own and freezes the small canonical
//! records that cross the durable/corpus boundary. The current implementation
//! can therefore register the schemas without partially adopting their
//! semantics; migration and query work can land against one reviewed shape.
//! [`RootSpec`] is the durable layout authority: a projection record that
//! contains several fields never implies those fields share one register.
//! In particular, overview prose remains a collaborative Text root.
//! A Plan deliberately has no physical schema here: it remains
//! [`crate::spec::Kind::Plan`] and uses the ordinary Spec lifecycle.

use replica::body::{
    BodyId, BodyKey, CollaborativeSchema, EncodingId, MutationModel, Schema, SchemaId,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::ids::{
    ActorId, CycleId, DocId, InitiativeId, LabelId, MilestoneId, ProjectId, TeamId, TriageId,
    UpdateId,
};

pub const SCHEMA_VERSION: u32 = 1;
// High-churn collaboration records are deliberately one record per Body.
// Body cardinality is the scalable dimension; an append-only "current
// segment" would merely move the coarse invalidation boundary.
pub const MIGRATION_AUDIT_RECORDS: u64 = 1_024;

pub const SPACE_DIRECTORY_SCHEMA: &str = "issues_space_directory";
pub const PROJECT_META_SCHEMA: &str = "issues_project_meta";
pub const PROJECT_SCHEDULE_SCHEMA: &str = "issues_project_schedule";
pub const PROJECT_HIERARCHY_SCHEMA: &str = "issues_project_hierarchy";
pub const PROJECT_UPDATES_SCHEMA: &str = "issues_project_updates";
pub const SPACE_TRIAGE_SCHEMA: &str = "issues_space_triage";
pub const ISSUE_COMMENT_SCHEMA: &str = "issues_issue_comment";
pub const ISSUE_REACTION_SCHEMA: &str = "issues_issue_reaction";
pub const ISSUE_ACTIVITY_SCHEMA: &str = "issues_issue_activity";
pub const ISSUE_RELATION_SCHEMA: &str = "issues_issue_relation";
pub const ISSUE_IDENTITY_SCHEMA: &str = "issues_issue_identity";
pub const ISSUE_META_SCHEMA: &str = "issues_issue_meta";
pub const ISSUE_PLACEMENT_SCHEMA: &str = "issues_issue_placement";
pub const ISSUE_ATTACHMENT_SCHEMA: &str = "issues_issue_attachment";
pub const ISSUE_CHECK_SCHEMA: &str = "issues_issue_check";
pub const INITIATIVE_SCHEMA: &str = "issues_initiative";
pub const TEAM_SCHEMA: &str = "issues_team";
pub const LABEL_SCHEMA: &str = "issues_label";
pub const ENTITY_RELATION_SCHEMA: &str = "issues_entity_relation";
pub const REVISION_ALIAS_SCHEMA: &str = "issues_revision_alias";
pub const GOVERNANCE_REVISION_SCHEMA: &str = "issues_governance_revision";
pub const WORKFLOW_REVISION_SCHEMA: &str = "issues_workflow_revision";

pub const SPACE_DIRECTORY_ENCODING: &str = "lait.issues.space-directory.v1";
pub const PROJECT_META_ENCODING: &str = "lait.issues.project-meta.v1";
pub const PROJECT_SCHEDULE_ENCODING: &str = "lait.issues.project-schedule.v1";
pub const PROJECT_HIERARCHY_ENCODING: &str = "lait.issues.project-hierarchy.v1";
pub const PROJECT_UPDATES_ENCODING: &str = "lait.issues.project-updates.v1";
pub const SPACE_TRIAGE_ENCODING: &str = "lait.issues.space-triage.v1";
pub const ISSUE_COMMENT_ENCODING: &str = "lait.issues.issue-comment.v1";
pub const ISSUE_REACTION_ENCODING: &str = "lait.issues.issue-reaction.v1";
pub const ISSUE_ACTIVITY_ENCODING: &str = "lait.issues.issue-activity.v1";
pub const ISSUE_RELATION_ENCODING: &str = "lait.issues.issue-relation.v1";
pub const ISSUE_IDENTITY_ENCODING: &str = "lait.issues.issue-identity.v1";
pub const ISSUE_META_ENCODING: &str = "lait.issues.issue-meta.v1";
pub const ISSUE_PLACEMENT_ENCODING: &str = "lait.issues.issue-placement.v1";
pub const ISSUE_ATTACHMENT_ENCODING: &str = "lait.issues.issue-attachment.v1";
pub const ISSUE_CHECK_ENCODING: &str = "lait.issues.issue-check.v1";
pub const INITIATIVE_ENCODING: &str = "lait.issues.initiative.v1";
pub const TEAM_ENCODING: &str = "lait.issues.team.v1";
pub const LABEL_ENCODING: &str = "lait.issues.label.v1";
pub const ENTITY_RELATION_ENCODING: &str = "lait.issues.entity-relation.v1";
pub const REVISION_ALIAS_ENCODING: &str = "lait.issues.revision-alias.v1";
pub const GOVERNANCE_REVISION_ENCODING: &str = "lait.issues.governance-revision.v1";
pub const WORKFLOW_REVISION_ENCODING: &str = "lait.issues.workflow-revision.v1";

/// Typed roots within each collaborative Body. A path is listed once here so
/// migration, extractors, and writers do not grow three spellings for it.
pub mod roots {
    pub const IDENTITY: &str = "identity";
    pub const NAME: &str = "name";
    pub const DESCRIPTION: &str = "description";
    /// One durable cursor for the crash-resumable v3 -> v4 rewrite. The cursor
    /// is metadata, never an ownership directory for live Issues.
    pub const MIGRATION: &str = "migration";
    /// Immutable administrative batch receipts. They make a completed
    /// migration auditable without retaining a second copy of product state.
    pub const MIGRATION_AUDIT: &str = "migration_audit";
    pub const KEY: &str = "key";
    pub const COLOR: &str = "color";
    pub const LEAD: &str = "lead";
    pub const START_DATE: &str = "start_date";
    pub const TARGET_DATE: &str = "target_date";
    pub const ARCHIVED: &str = "archived";
    pub const TOMBSTONE: &str = "tombstone";
    pub const OWNER: &str = "owner";
    pub const HEALTH: &str = "health";
    pub const ICON: &str = "icon";
    pub const RELATION: &str = "relation";
    pub const RECORD: &str = "record";
    pub const INITIATIVE: &str = "initiative";
    pub const TEAM: &str = "team";
    /// Additive v4 roots on the long-lived Issue Body. Identity/alias and the
    /// three board coordinates are each one atomic register value.
    pub const ISSUE_IDENTITY: &str = "v4_identity";
    pub const BOARD_PLACEMENT: &str = "v4_board_placement";
    pub const ISSUE_TOMBSTONE: &str = "v4_tombstone";
    /// Immutable self-coordinate on the anchored content Body. It exists only
    /// so that Body's extractor can name its content node; alias and placement
    /// truth live in their dedicated small Bodies.
    pub const ISSUE_ID: &str = "v4_issue_id";
    pub const PRIORITY: &str = "priority";
    pub const TITLE: &str = "title";
    pub const CREATED_BY: &str = "created_by";
    pub const CREATED_AT: &str = "created_at";
    pub const DUE_AT: &str = "due_at";
    pub const ESTIMATE: &str = "estimate";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RootAlgebra {
    Register,
    Text,
    Map,
    Set,
    List,
    Tree,
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootSpec {
    pub path: &'static str,
    pub algebra: RootAlgebra,
}

const SPACE_DIRECTORY_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::NAME,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::DESCRIPTION,
        algebra: RootAlgebra::Text,
    },
    RootSpec {
        path: roots::MIGRATION,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::MIGRATION_AUDIT,
        algebra: RootAlgebra::Log,
    },
];
const PROJECT_META_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::NAME,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::KEY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::COLOR,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::DESCRIPTION,
        algebra: RootAlgebra::Text,
    },
    RootSpec {
        path: roots::LEAD,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::START_DATE,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TARGET_DATE,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::ARCHIVED,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TEAM,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TOMBSTONE,
        algebra: RootAlgebra::Register,
    },
];
const PROJECT_SCHEDULE_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const PROJECT_HIERARCHY_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const PROJECT_UPDATES_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const SPACE_TRIAGE_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const ISSUE_ACTIVITY_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const ISSUE_RELATION_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RELATION,
        algebra: RootAlgebra::Register,
    },
];
const LABEL_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const ENTITY_RELATION_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const REVISION_RECORD_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::RECORD,
        algebra: RootAlgebra::Register,
    },
];
const INITIATIVE_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::NAME,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::DESCRIPTION,
        algebra: RootAlgebra::Text,
    },
    RootSpec {
        path: roots::OWNER,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::HEALTH,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TARGET_DATE,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TOMBSTONE,
        algebra: RootAlgebra::Register,
    },
];
const TEAM_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::NAME,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::KEY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::ICON,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::LEAD,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TOMBSTONE,
        algebra: RootAlgebra::Register,
    },
];

const ISSUE_META_ROOTS: &[RootSpec] = &[
    RootSpec {
        path: roots::IDENTITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TITLE,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::PRIORITY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::CREATED_BY,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::CREATED_AT,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::DUE_AT,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::ESTIMATE,
        algebra: RootAlgebra::Register,
    },
    RootSpec {
        path: roots::TOMBSTONE,
        algebra: RootAlgebra::Register,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PhysicalSchema {
    SpaceDirectory,
    ProjectMeta,
    ProjectSchedule,
    ProjectHierarchy,
    ProjectUpdates,
    SpaceTriage,
    IssueComment,
    IssueReaction,
    IssueActivity,
    IssueRelation,
    IssueIdentity,
    IssueMeta,
    IssuePlacement,
    IssueAttachment,
    IssueCheck,
    Initiative,
    Team,
    Label,
    EntityRelation,
    RevisionAlias,
    GovernanceRevision,
    WorkflowRevision,
}

pub const PHYSICAL_SCHEMAS: [PhysicalSchema; 22] = [
    PhysicalSchema::SpaceDirectory,
    PhysicalSchema::ProjectMeta,
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
    PhysicalSchema::IssuePlacement,
    PhysicalSchema::IssueAttachment,
    PhysicalSchema::IssueCheck,
    PhysicalSchema::Initiative,
    PhysicalSchema::Team,
    PhysicalSchema::Label,
    PhysicalSchema::EntityRelation,
    PhysicalSchema::RevisionAlias,
    PhysicalSchema::GovernanceRevision,
    PhysicalSchema::WorkflowRevision,
];

impl PhysicalSchema {
    pub const fn immutable(self) -> bool {
        matches!(
            self,
            Self::ProjectUpdates
                | Self::SpaceTriage
                | Self::IssueActivity
                | Self::RevisionAlias
                | Self::GovernanceRevision
                | Self::WorkflowRevision
                | Self::IssueIdentity
                | Self::IssueComment
        )
    }

    pub const fn atomic(self) -> bool {
        self.immutable()
            || matches!(
                self,
                Self::IssuePlacement
                    | Self::IssueAttachment
                    | Self::IssueCheck
                    | Self::IssueReaction
                    | Self::IssueRelation
                    | Self::ProjectHierarchy
                    | Self::EntityRelation
            )
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::SpaceDirectory => SPACE_DIRECTORY_SCHEMA,
            Self::ProjectMeta => PROJECT_META_SCHEMA,
            Self::ProjectSchedule => PROJECT_SCHEDULE_SCHEMA,
            Self::ProjectHierarchy => PROJECT_HIERARCHY_SCHEMA,
            Self::ProjectUpdates => PROJECT_UPDATES_SCHEMA,
            Self::SpaceTriage => SPACE_TRIAGE_SCHEMA,
            Self::IssueComment => ISSUE_COMMENT_SCHEMA,
            Self::IssueReaction => ISSUE_REACTION_SCHEMA,
            Self::IssueActivity => ISSUE_ACTIVITY_SCHEMA,
            Self::IssueRelation => ISSUE_RELATION_SCHEMA,
            Self::IssueIdentity => ISSUE_IDENTITY_SCHEMA,
            Self::IssueMeta => ISSUE_META_SCHEMA,
            Self::IssuePlacement => ISSUE_PLACEMENT_SCHEMA,
            Self::IssueAttachment => ISSUE_ATTACHMENT_SCHEMA,
            Self::IssueCheck => ISSUE_CHECK_SCHEMA,
            Self::Initiative => INITIATIVE_SCHEMA,
            Self::Team => TEAM_SCHEMA,
            Self::Label => LABEL_SCHEMA,
            Self::EntityRelation => ENTITY_RELATION_SCHEMA,
            Self::RevisionAlias => REVISION_ALIAS_SCHEMA,
            Self::GovernanceRevision => GOVERNANCE_REVISION_SCHEMA,
            Self::WorkflowRevision => WORKFLOW_REVISION_SCHEMA,
        }
    }

    pub const fn encoding(self) -> &'static str {
        match self {
            Self::SpaceDirectory => SPACE_DIRECTORY_ENCODING,
            Self::ProjectMeta => PROJECT_META_ENCODING,
            Self::ProjectSchedule => PROJECT_SCHEDULE_ENCODING,
            Self::ProjectHierarchy => PROJECT_HIERARCHY_ENCODING,
            Self::ProjectUpdates => PROJECT_UPDATES_ENCODING,
            Self::SpaceTriage => SPACE_TRIAGE_ENCODING,
            Self::IssueComment => ISSUE_COMMENT_ENCODING,
            Self::IssueReaction => ISSUE_REACTION_ENCODING,
            Self::IssueActivity => ISSUE_ACTIVITY_ENCODING,
            Self::IssueRelation => ISSUE_RELATION_ENCODING,
            Self::IssueIdentity => ISSUE_IDENTITY_ENCODING,
            Self::IssueMeta => ISSUE_META_ENCODING,
            Self::IssuePlacement => ISSUE_PLACEMENT_ENCODING,
            Self::IssueAttachment => ISSUE_ATTACHMENT_ENCODING,
            Self::IssueCheck => ISSUE_CHECK_ENCODING,
            Self::Initiative => INITIATIVE_ENCODING,
            Self::Team => TEAM_ENCODING,
            Self::Label => LABEL_ENCODING,
            Self::EntityRelation => ENTITY_RELATION_ENCODING,
            Self::RevisionAlias => REVISION_ALIAS_ENCODING,
            Self::GovernanceRevision => GOVERNANCE_REVISION_ENCODING,
            Self::WorkflowRevision => WORKFLOW_REVISION_ENCODING,
        }
    }

    pub const fn roots(self) -> &'static [RootSpec] {
        match self {
            Self::SpaceDirectory => SPACE_DIRECTORY_ROOTS,
            Self::ProjectMeta => PROJECT_META_ROOTS,
            Self::ProjectSchedule => PROJECT_SCHEDULE_ROOTS,
            Self::ProjectHierarchy => PROJECT_HIERARCHY_ROOTS,
            Self::ProjectUpdates => PROJECT_UPDATES_ROOTS,
            Self::SpaceTriage => SPACE_TRIAGE_ROOTS,
            Self::IssueComment | Self::IssueReaction => REVISION_RECORD_ROOTS,
            Self::IssueActivity => ISSUE_ACTIVITY_ROOTS,
            Self::IssueRelation => ISSUE_RELATION_ROOTS,
            Self::IssueMeta => ISSUE_META_ROOTS,
            Self::IssueIdentity
            | Self::IssuePlacement
            | Self::IssueAttachment
            | Self::IssueCheck => REVISION_RECORD_ROOTS,
            Self::Initiative => INITIATIVE_ROOTS,
            Self::Team => TEAM_ROOTS,
            Self::Label => LABEL_ROOTS,
            Self::EntityRelation => ENTITY_RELATION_ROOTS,
            Self::RevisionAlias | Self::GovernanceRevision | Self::WorkflowRevision => {
                REVISION_RECORD_ROOTS
            }
        }
    }

    fn domain(self) -> &'static [u8] {
        match self {
            Self::SpaceDirectory => b"lait/issues-v4/space-directory/1",
            Self::ProjectMeta => b"lait/issues-v4/project-meta/1",
            Self::ProjectSchedule => b"lait/issues-v4/project-schedule/1",
            Self::ProjectHierarchy => b"lait/issues-v4/project-hierarchy/1",
            Self::ProjectUpdates => b"lait/issues-v4/project-updates/1",
            Self::SpaceTriage => b"lait/issues-v4/space-triage/1",
            Self::IssueComment => b"lait/issues-v4/issue-comment/1",
            Self::IssueReaction => b"lait/issues-v4/issue-reaction/1",
            Self::IssueActivity => b"lait/issues-v4/issue-activity/1",
            Self::IssueRelation => b"lait/issues-v4/issue-relation/1",
            Self::IssueIdentity => b"lait/issues-v4/issue-identity/1",
            Self::IssueMeta => b"lait/issues-v4/issue-meta/1",
            Self::IssuePlacement => b"lait/issues-v4/issue-placement/1",
            Self::IssueAttachment => b"lait/issues-v4/issue-attachment/1",
            Self::IssueCheck => b"lait/issues-v4/issue-check/1",
            Self::Initiative => b"lait/issues-v4/initiative/1",
            Self::Team => b"lait/issues-v4/team/1",
            Self::Label => b"lait/issues-v4/label/1",
            Self::EntityRelation => b"lait/issues-v4/entity-relation/1",
            Self::RevisionAlias => b"lait/issues-v4/revision-alias/1",
            Self::GovernanceRevision => b"lait/issues-v4/governance-revision/1",
            Self::WorkflowRevision => b"lait/issues-v4/workflow-revision/1",
        }
    }

    pub fn declaration(self) -> Schema {
        Schema {
            id: SchemaId::parse(self.name()).expect("v4 schema id"),
            version: SCHEMA_VERSION,
            encoding: EncodingId::parse(self.encoding()).expect("v4 encoding id"),
            mutation: if self.atomic() {
                MutationModel::Atomic
            } else {
                MutationModel::Collaborative(CollaborativeSchema::default())
            },
            readable_predecessors: Vec::new(),
        }
    }
}

/// Decisive post-Catalog declarations. Legacy schemas are migration inputs,
/// never normal-path predecessors or a second durable query truth.
pub fn schemas() -> Vec<Schema> {
    PHYSICAL_SCHEMAS
        .iter()
        .copied()
        .map(PhysicalSchema::declaration)
        .collect()
}

fn body_id(schema: PhysicalSchema, coordinates: &[&str]) -> BodyId {
    let mut material = Vec::new();
    for coordinate in coordinates {
        let len = u64::try_from(coordinate.len()).unwrap_or(u64::MAX);
        material.extend_from_slice(&len.to_be_bytes());
        material.extend_from_slice(coordinate.as_bytes());
    }
    let digest = blake3::derive_key(
        std::str::from_utf8(schema.domain()).expect("ASCII v4 Body-id domain"),
        &material,
    );
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    BodyId::from_bytes(id)
}

fn key(schema: PhysicalSchema, coordinates: &[&str]) -> BodyKey {
    BodyKey::new(crate::contract::world_id(), body_id(schema, coordinates))
}

pub fn space_directory_key(space: &crate::ids::SpaceId) -> BodyKey {
    key(PhysicalSchema::SpaceDirectory, &[space.as_str()])
}

pub fn project_meta_key(project: &ProjectId) -> BodyKey {
    key(PhysicalSchema::ProjectMeta, &[project.as_str()])
}

pub fn project_schedule_key(project: &ProjectId, record: &str) -> BodyKey {
    key(PhysicalSchema::ProjectSchedule, &[project.as_str(), record])
}

pub fn project_hierarchy_key(project: &ProjectId, record: &str) -> BodyKey {
    key(
        PhysicalSchema::ProjectHierarchy,
        &[project.as_str(), record],
    )
}

pub fn project_updates_key(project: &ProjectId, update: &str) -> BodyKey {
    key(PhysicalSchema::ProjectUpdates, &[project.as_str(), update])
}

pub fn space_triage_key(space: &crate::ids::SpaceId, record: &str) -> BodyKey {
    key(PhysicalSchema::SpaceTriage, &[space.as_str(), record])
}

pub fn issue_comment_key(issue: &DocId, record: &str) -> BodyKey {
    key(PhysicalSchema::IssueComment, &[issue.as_str(), record])
}

pub fn issue_reaction_key(issue: &DocId, record: &str) -> BodyKey {
    key(PhysicalSchema::IssueReaction, &[issue.as_str(), record])
}

pub fn issue_activity_key(issue: &DocId, record: &str) -> BodyKey {
    key(PhysicalSchema::IssueActivity, &[issue.as_str(), record])
}

pub fn issue_relation_key(issue: &DocId, relation: &str) -> BodyKey {
    key(PhysicalSchema::IssueRelation, &[issue.as_str(), relation])
}

pub fn issue_identity_key(issue: &DocId) -> BodyKey {
    key(PhysicalSchema::IssueIdentity, &[issue.as_str()])
}

pub fn issue_meta_key(issue: &DocId) -> BodyKey {
    key(PhysicalSchema::IssueMeta, &[issue.as_str()])
}

pub fn issue_placement_key(issue: &DocId) -> BodyKey {
    key(PhysicalSchema::IssuePlacement, &[issue.as_str()])
}

pub fn issue_attachment_key(issue: &DocId, attachment: &str) -> BodyKey {
    key(
        PhysicalSchema::IssueAttachment,
        &[issue.as_str(), attachment],
    )
}

pub fn issue_check_key(issue: &DocId, run: &str) -> BodyKey {
    key(PhysicalSchema::IssueCheck, &[issue.as_str(), run])
}

pub fn initiative_key(initiative: &InitiativeId) -> BodyKey {
    key(PhysicalSchema::Initiative, &[initiative.as_str()])
}

pub fn team_key(team: &TeamId) -> BodyKey {
    key(PhysicalSchema::Team, &[team.as_str()])
}

pub fn label_key(label: &LabelId) -> BodyKey {
    key(PhysicalSchema::Label, &[label.as_str()])
}

pub fn entity_relation_key(owner: &str, relation: &str) -> BodyKey {
    key(PhysicalSchema::EntityRelation, &[owner, relation])
}

pub fn revision_alias_key(spec: &str, legacy_revision: &str) -> BodyKey {
    key(PhysicalSchema::RevisionAlias, &[spec, legacy_revision])
}

pub fn governance_revision_key(role: &str, revision: &str) -> BodyKey {
    key(PhysicalSchema::GovernanceRevision, &[role, revision])
}

pub fn workflow_revision_key(project: &ProjectId, revision: &str) -> BodyKey {
    key(
        PhysicalSchema::WorkflowRevision,
        &[project.as_str(), revision],
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalid {
    Field(&'static str),
    NonCanonical,
    Encoding,
}

pub trait CanonicalRecord: Sized + Serialize + DeserializeOwned {
    fn validate(&self) -> Result<(), Invalid>;

    fn encode_canonical(&self) -> Result<Vec<u8>, Invalid> {
        self.validate()?;
        let value = serde_json::to_value(self).map_err(|_| Invalid::Encoding)?;
        serde_json::to_vec(&value).map_err(|_| Invalid::Encoding)
    }

    fn decode_canonical(bytes: &[u8]) -> Result<Self, Invalid> {
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Invalid::Encoding)?;
        let encoded = value.encode_canonical()?;
        if encoded == bytes {
            Ok(value)
        } else {
            Err(Invalid::NonCanonical)
        }
    }
}

fn nonempty_bounded(value: &str, max: usize, field: &'static str) -> Result<(), Invalid> {
    if value.trim().is_empty() || value.len() > max {
        Err(Invalid::Field(field))
    } else {
        Ok(())
    }
}

fn optional_bounded(value: &str, max: usize, field: &'static str) -> Result<(), Invalid> {
    if value.len() > max {
        Err(Invalid::Field(field))
    } else {
        Ok(())
    }
}

fn canonical_actor(value: &str) -> bool {
    ActorId::parse(value).is_some_and(|actor| actor.as_str() == value)
}

fn token(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn sorted_unique(values: &[String]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn decode_digest(value: &str, field: &'static str) -> Result<[u8; 32], Invalid> {
    let decoded = data_encoding::HEXLOWER
        .decode(value.as_bytes())
        .map_err(|_| Invalid::Field(field))?;
    <[u8; 32]>::try_from(decoded.as_slice()).map_err(|_| Invalid::Field(field))
}

/// Identity of an entity-sized collection Body. `owner` scopes the record and
/// `record` is the stable entity/relation coordinate used in its Body key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordBodyIdentityRecord {
    pub owner: String,
    pub record: String,
}

impl CanonicalRecord for RecordBodyIdentityRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if self.owner.is_empty() || self.owner.len() > 128 || !token(&self.record, 192) {
            Err(Invalid::Field("record_body_identity"))
        } else {
            Ok(())
        }
    }
}

/// Canonical bytes of an immutable record-sized Body. Atomic schemas store
/// exactly this envelope, so identity and payload are one indivisible value
/// and an existing coordinate can be checked for equivocation byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImmutableRecordEnvelope {
    pub identity: RecordBodyIdentityRecord,
    pub record: Vec<u8>,
}

impl CanonicalRecord for ImmutableRecordEnvelope {
    fn validate(&self) -> Result<(), Invalid> {
        self.identity.validate()?;
        if self.record.is_empty() || self.record.len() > crate::contract::MAX_TEXT_BYTES {
            return Err(Invalid::Field("record"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceDirectoryRecord {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

pub const MIGRATION_V3_TO_V4: &str = "issues-v3-to-v4";

/// The only mutable migration coordinate. A batch advances `cursor` in the
/// same transaction as the Bodies it materializes; a crash therefore either
/// advances both or neither. `cursor` names the last completed canonical work
/// item, not an offset whose meaning could change while migration is running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationMarkerRecord {
    pub migration: String,
    pub source_version: u32,
    pub target_version: u32,
    /// The immutable interpretation used for every rewritten revision. A
    /// multi-batch migration crosses Manifest roots but must not change ids.
    pub publication: runtime::publication::PublicationId,
    pub batch: u64,
    #[serde(default)]
    pub cursor: String,
    #[serde(default)]
    pub complete: bool,
    pub actor: String,
    pub started_at: u64,
    pub updated_at: u64,
}

impl CanonicalRecord for MigrationMarkerRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if self.migration != MIGRATION_V3_TO_V4
            || self.source_version != 3
            || self.target_version != 4
            || self.publication.implementation_digest == [0; 32]
            || self.publication.extractor_schema_digest.digest() == [0; 32]
            || self.batch == 0
            || !canonical_actor(&self.actor)
            || self.started_at == 0
            || self.updated_at < self.started_at
            || self.cursor.len() > 512
            || (!self.complete && self.cursor.is_empty())
        {
            return Err(Invalid::Field("migration_marker"));
        }
        Ok(())
    }
}

/// Durable redirect from a pre-v4 exact revision coordinate to its canonical
/// v4 replacement. Readers follow this typed v4 record; no legacy decoder is
/// retained on the steady-state path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionAliasRecord {
    pub spec: String,
    pub legacy_revision: String,
    pub canonical_revision: String,
}

impl CanonicalRecord for RevisionAliasRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if crate::ids::SpecId::parse(&self.spec).is_none()
            || crate::spec::decode_revision(&self.legacy_revision).is_none()
            || crate::spec::decode_revision(&self.canonical_revision).is_none()
            || self.legacy_revision == self.canonical_revision
        {
            Err(Invalid::Field("revision_alias"))
        } else {
            Ok(())
        }
    }
}

/// Immutable receipt for one bounded migration transaction. `first` and
/// `last` are inclusive stable work keys. `operations` is recorded after
/// staging, so audit and the transaction it describes cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationAuditRecord {
    pub migration: String,
    pub batch: u64,
    pub actor: String,
    pub timestamp: u64,
    pub first: String,
    pub last: String,
    pub items: u32,
    pub operations: u32,
    #[serde(default)]
    pub complete: bool,
}

impl CanonicalRecord for MigrationAuditRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if self.migration != MIGRATION_V3_TO_V4
            || self.batch == 0
            || !canonical_actor(&self.actor)
            || self.timestamp == 0
            || self.items == 0
            || self.operations == 0
            || self.first.is_empty()
            || self.first.len() > 512
            || self.last.len() > 512
            || self.first > self.last
        {
            return Err(Invalid::Field("migration_audit"));
        }
        Ok(())
    }
}

impl CanonicalRecord for SpaceDirectoryRecord {
    fn validate(&self) -> Result<(), Invalid> {
        nonempty_bounded(&self.name, crate::contract::MAX_NAME_BYTES, "name")?;
        optional_bounded(
            &self.description,
            crate::contract::MAX_TEXT_BYTES,
            "description",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDirectoryEntry {
    pub project: String,
    pub key: String,
    #[serde(default)]
    pub tombstone: bool,
}

impl CanonicalRecord for ProjectDirectoryEntry {
    fn validate(&self) -> Result<(), Invalid> {
        if ProjectId::parse(&self.project).is_none()
            || self.key.is_empty()
            || self.key.len() > 8
            || !self.key.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            return Err(Invalid::Field("project"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelDirectoryEntry {
    pub label: String,
    pub name: String,
    pub color: String,
    #[serde(default)]
    pub tombstone: bool,
}

impl CanonicalRecord for LabelDirectoryEntry {
    fn validate(&self) -> Result<(), Invalid> {
        if LabelId::parse(&self.label).is_none() {
            return Err(Invalid::Field("label"));
        }
        nonempty_bounded(&self.name, crate::contract::MAX_NAME_BYTES, "name")?;
        optional_bounded(
            &self.color,
            crate::contract::MAX_PRESENTATION_TOKEN_BYTES,
            "color",
        )
    }
}

/// One exact role revision in the governance Body. Revision validity remains
/// owned by the existing role builder rather than duplicated here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernanceRevisionRecord {
    pub role: String,
    pub revision: crate::views::StoredRoleRevision,
}

impl CanonicalRecord for GovernanceRevisionRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if self.role != self.revision.body.role_id || !sorted_unique(&self.revision.predecessor_ids)
        {
            return Err(Invalid::Field("role"));
        }
        let revision_id = decode_digest(&self.revision.revision_id, "revision")?;
        let predecessors = self
            .revision
            .predecessor_ids
            .iter()
            .map(|value| decode_digest(value, "predecessor"))
            .collect::<Result<Vec<_>, _>>()?;
        let rebuilt = crate::roles::build_revision(self.revision.body.clone(), predecessors)
            .map_err(|_| Invalid::Field("revision"))?;
        if rebuilt.revision_id != revision_id
            || rebuilt
                .predecessor_ids
                .iter()
                .map(|digest| data_encoding::HEXLOWER.encode(digest))
                .collect::<Vec<_>>()
                != self.revision.predecessor_ids
        {
            return Err(Invalid::Field("revision"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMetaRecord {
    pub project: String,
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub lead: String,
    #[serde(default)]
    pub start_date: Option<u64>,
    #[serde(default)]
    pub target_date: Option<u64>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub team: String,
    #[serde(default)]
    pub tombstone: bool,
}

impl CanonicalRecord for ProjectMetaRecord {
    fn validate(&self) -> Result<(), Invalid> {
        ProjectDirectoryEntry {
            project: self.project.clone(),
            key: self.key.clone(),
            tombstone: self.tombstone,
        }
        .validate()?;
        nonempty_bounded(&self.name, crate::contract::MAX_NAME_BYTES, "name")?;
        optional_bounded(
            &self.color,
            crate::contract::MAX_PRESENTATION_TOKEN_BYTES,
            "color",
        )?;
        optional_bounded(
            &self.description,
            crate::contract::MAX_METADATA_DESCRIPTION_BYTES,
            "description",
        )?;
        if (!self.lead.is_empty() && !canonical_actor(&self.lead))
            || (!self.team.is_empty() && TeamId::parse(&self.team).is_none())
            || self
                .start_date
                .zip(self.target_date)
                .is_some_and(|(start, target)| target < start)
        {
            return Err(Invalid::Field("project_meta"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectWorkflowRevisionRecord {
    pub project: String,
    pub revision: crate::workflow::WorkflowRevision,
}

impl CanonicalRecord for ProjectWorkflowRevisionRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if ProjectId::parse(&self.project).is_none()
            || self.project != self.revision.body.project_id
            || !sorted_unique(&self.revision.predecessor_ids)
        {
            return Err(Invalid::Field("workflow"));
        }
        let predecessors: Vec<[u8; 32]> = self
            .revision
            .predecessor_ids
            .iter()
            .map(|value| decode_digest(value, "predecessor"))
            .collect::<Result<_, _>>()?;
        let rebuilt = crate::workflow::build_revision(self.revision.body.clone(), predecessors)
            .map_err(|_| Invalid::Field("workflow"))?;
        if rebuilt != self.revision {
            return Err(Invalid::Field("workflow"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScheduleRecord {
    Milestone {
        milestone: String,
        project: String,
        name: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        target_date: Option<u64>,
        position: String,
        #[serde(default)]
        tombstone: bool,
    },
    Cycle {
        cycle: String,
        project: String,
        name: String,
        #[serde(default)]
        start: u64,
        #[serde(default)]
        end: u64,
        #[serde(default)]
        tombstone: bool,
    },
}

impl CanonicalRecord for ScheduleRecord {
    fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Milestone {
                milestone,
                project,
                name,
                description,
                position,
                ..
            } => {
                if MilestoneId::parse(milestone).is_none()
                    || ProjectId::parse(project).is_none()
                    || !valid_position(position)
                {
                    return Err(Invalid::Field("milestone"));
                }
                nonempty_bounded(name, crate::contract::MAX_NAME_BYTES, "name")?;
                optional_bounded(description, crate::contract::MAX_TEXT_BYTES, "description")
            }
            Self::Cycle {
                cycle,
                project,
                name,
                start,
                end,
                ..
            } => {
                if CycleId::parse(cycle).is_none()
                    || ProjectId::parse(project).is_none()
                    || (*start != 0 && *end != 0 && end < start)
                {
                    return Err(Invalid::Field("cycle"));
                }
                nonempty_bounded(name, crate::contract::MAX_NAME_BYTES, "name")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HierarchyRecord {
    pub project: String,
    pub child: String,
    #[serde(default)]
    pub parent: Option<String>,
}

impl HierarchyRecord {
    /// Stable identity of the parent fact. Reparenting replaces its target
    /// without changing the relation node that reverse-adjacency postings name.
    pub fn relation_identity(&self) -> [u8; 32] {
        blake3::derive_key("lait.issues.parent-relation.v1", self.child.as_bytes())
    }
}

impl CanonicalRecord for HierarchyRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if ProjectId::parse(&self.project).is_none()
            || DocId::parse(&self.child).is_none()
            || self
                .parent
                .as_ref()
                .is_some_and(|parent| DocId::parse(parent).is_none() || parent == &self.child)
        {
            return Err(Invalid::Field("hierarchy"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLinkRecord {
    pub project: String,
    pub from: String,
    pub kind: String,
    pub to: String,
    /// Explicit presence is required during v3/v4 transitional reads: absence
    /// in this Body cannot distinguish "not migrated yet" from "removed".
    pub present: bool,
}

impl ProjectLinkRecord {
    pub fn relation_identity(&self) -> [u8; 32] {
        let mut material = Vec::new();
        for value in [&self.from, &self.kind, &self.to] {
            material
                .extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            material.extend_from_slice(value.as_bytes());
        }
        blake3::derive_key("lait.issues.project-link.v1", &material)
    }
}

impl CanonicalRecord for ProjectLinkRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if ProjectId::parse(&self.project).is_none()
            || DocId::parse(&self.from).is_none()
            || DocId::parse(&self.to).is_none()
            || self.from == self.to
            || !crate::contract::LINK_KINDS.contains(&self.kind.as_str())
        {
            Err(Invalid::Field("project_link"))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "record_kind",
    content = "record",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TopologyRecord {
    Parent(HierarchyRecord),
    Link(ProjectLinkRecord),
}

impl CanonicalRecord for TopologyRecord {
    fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Parent(record) => record.validate(),
            Self::Link(record) => record.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectUpdateRecord {
    pub update: String,
    pub project: String,
    pub author: String,
    pub timestamp: u64,
    pub body: String,
    #[serde(default)]
    pub health: String,
}

impl CanonicalRecord for ProjectUpdateRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if UpdateId::parse(&self.update).is_none()
            || ProjectId::parse(&self.project).is_none()
            || !canonical_actor(&self.author)
            || self.timestamp == 0
            || !matches!(
                self.health.as_str(),
                "" | "on_track" | "at_risk" | "off_track"
            )
        {
            return Err(Invalid::Field("update"));
        }
        nonempty_bounded(&self.body, crate::contract::MAX_TEXT_BYTES, "body")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageOutcome {
    Accepted,
    Declined,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageSubmissionRecord {
    pub triage: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub source: String,
    pub submitted_by: String,
    pub timestamp: u64,
}

impl CanonicalRecord for TriageSubmissionRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if TriageId::parse(&self.triage).is_none()
            || !canonical_actor(&self.submitted_by)
            || self.timestamp == 0
        {
            return Err(Invalid::Field("triage"));
        }
        nonempty_bounded(&self.title, crate::contract::MAX_TITLE_BYTES, "title")?;
        optional_bounded(&self.body, crate::contract::MAX_TEXT_BYTES, "body")?;
        optional_bounded(
            &self.source,
            crate::contract::MAX_PRESENTATION_TOKEN_BYTES,
            "source",
        )
    }
}

/// An immutable decision claim. Several claims may coexist after an offline
/// race; a separate resolution selects one instead of LWW-erasing evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageDecisionRecord {
    pub decision: String,
    pub triage: String,
    pub outcome: TriageOutcome,
    pub decided_by: String,
    pub timestamp: u64,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub issue: Option<String>,
    #[serde(default)]
    pub note: String,
}

impl CanonicalRecord for TriageDecisionRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if !token(&self.decision, 128)
            || TriageId::parse(&self.triage).is_none()
            || !canonical_actor(&self.decided_by)
            || self.timestamp == 0
            || self
                .project
                .as_ref()
                .is_some_and(|project| ProjectId::parse(project).is_none())
            || self
                .issue
                .as_ref()
                .is_some_and(|issue| DocId::parse(issue).is_none())
        {
            return Err(Invalid::Field("decision"));
        }
        let coordinates_match = match self.outcome {
            TriageOutcome::Accepted => self.project.is_some() && self.issue.is_some(),
            TriageOutcome::Duplicate => self.project.is_none() && self.issue.is_some(),
            TriageOutcome::Declined => self.project.is_none() && self.issue.is_none(),
        };
        if !coordinates_match {
            return Err(Invalid::Field("decision_coordinates"));
        }
        optional_bounded(&self.note, crate::contract::MAX_TEXT_BYTES, "note")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageResolutionRecord {
    pub triage: String,
    pub decision: String,
    pub resolved_by: String,
    pub timestamp: u64,
}

impl TriageResolutionRecord {
    pub fn identity(&self) -> String {
        data_encoding::HEXLOWER.encode(&blake3::derive_key(
            "lait.issues.triage-resolution.v1",
            format!("{}\0{}", self.triage, self.decision).as_bytes(),
        ))
    }
}

impl CanonicalRecord for TriageResolutionRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if TriageId::parse(&self.triage).is_none()
            || !token(&self.decision, 128)
            || !canonical_actor(&self.resolved_by)
            || self.timestamp == 0
        {
            return Err(Invalid::Field("resolution"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "record_kind",
    content = "record",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum TriageRecord {
    Submission(TriageSubmissionRecord),
    Decision(TriageDecisionRecord),
    Resolution(TriageResolutionRecord),
}

impl CanonicalRecord for TriageRecord {
    fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Submission(record) => record.validate(),
            Self::Decision(record) => record.validate(),
            Self::Resolution(record) => record.validate(),
        }
    }
}

fn valid_position(position: &str) -> bool {
    !position.is_empty()
        && position.len() <= 256
        && position.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Project, workflow state, and position are replaced as one value. There is
/// no intermediate generation where an Issue belongs to a new column with an
/// ordering coordinate from its old one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoardPlacement {
    pub project: String,
    pub workflow_state: String,
    pub position: String,
}

impl CanonicalRecord for BoardPlacement {
    fn validate(&self) -> Result<(), Invalid> {
        if ProjectId::parse(&self.project).is_none()
            || !token(&self.workflow_state, 64)
            || !valid_position(&self.position)
        {
            return Err(Invalid::Field("board_placement"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuePlacementRecord {
    pub issue: String,
    pub placement: BoardPlacement,
}

impl CanonicalRecord for IssuePlacementRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none() {
            return Err(Invalid::Field("issue_placement"));
        }
        self.placement.validate()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueMetaRecord {
    pub issue: String,
    pub title: String,
    pub priority: String,
    pub created_by: Option<String>,
    pub created_at: u64,
    pub due_at: Option<u64>,
    pub estimate: Option<u32>,
    pub tombstone: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueAttachmentRecord {
    pub issue: String,
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub by: String,
    pub timestamp: u64,
    pub comment: Option<String>,
    pub content: String,
    pub tombstone: bool,
}

impl CanonicalRecord for IssueAttachmentRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none()
            || crate::ids::AttachmentId::parse(&self.id).is_none()
            || self.name.is_empty()
            || self.name.len() > crate::contract::MAX_ATTACHMENT_NAME_BYTES
            || self.name.chars().any(char::is_control)
            || self.mime.len() > crate::contract::MAX_PRESENTATION_TOKEN_BYTES
            || self.size == 0
            || ActorId::parse(&self.by).is_none()
            || self.timestamp == 0
            || self
                .comment
                .as_deref()
                .is_some_and(|comment| !crate::contract::is_comment_id(comment))
            || self.content.len() != 64
            || !self.content.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(Invalid::Field("issue_attachment"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueCheckRecord {
    pub issue: String,
    pub run: String,
    pub check: crate::contract::CheckRecord,
}

impl CanonicalRecord for IssueCheckRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none()
            || !token(&self.run, 128)
            || !token(&self.check.spec, 128)
            || self.check.v == 0
            || !token(&self.check.build, 128)
            || self.check.source.len() != 64
            || !self
                .check
                .source
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
            || !matches!(self.check.state.as_str(), "started" | "accepted")
            || ActorId::parse(&self.check.by).is_none()
            || self.check.ts == 0
            || self.check.report.as_deref().is_some_and(|report| {
                report.len() != 64 || !report.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
        {
            return Err(Invalid::Field("issue_check"));
        }
        Ok(())
    }
}

impl CanonicalRecord for IssueMetaRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none()
            || !crate::contract::valid_title(&self.title)
            || crate::dto::Priority::parse(&self.priority).is_none()
            || self
                .created_by
                .as_deref()
                .is_some_and(|actor| ActorId::parse(actor).is_none())
            || self.due_at == Some(0)
            || self
                .estimate
                .is_some_and(|estimate| estimate > crate::contract::MAX_ESTIMATE)
        {
            return Err(Invalid::Field("issue_meta"));
        }
        Ok(())
    }
}

/// Stable, peer-safe alias identity stored on its Issue. `ordinal` preserves a
/// short human ordering hint; `disambiguator` prevents two offline allocations
/// of the same ordinal from changing one another's alias after convergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueAliasCoordinate {
    pub ordinal: u64,
    pub disambiguator: [u8; 16],
}

/// Durable identity and alias coordinates stored together on the Issue Body.
/// The Issue id is therefore available to a one-Body extractor without a
/// Catalog join, while an alias collision never renumbers either Issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueIdentityRecord {
    pub issue: String,
    pub alias: IssueAliasCoordinate,
}

impl CanonicalRecord for IssueIdentityRecord {
    fn validate(&self) -> Result<(), Invalid> {
        let issue = DocId::parse(&self.issue).ok_or(Invalid::Field("issue_identity"))?;
        let expected = IssueAliasCoordinate::for_issue(self.alias.ordinal, &issue)?;
        if self.alias != expected {
            return Err(Invalid::Field("alias_binding"));
        }
        Ok(())
    }
}

impl IssueAliasCoordinate {
    pub fn for_issue(ordinal: u64, issue: &DocId) -> Result<Self, Invalid> {
        if ordinal == 0 {
            return Err(Invalid::Field("alias"));
        }
        let digest =
            blake3::derive_key("lait.issues.alias-coordinate.v1", issue.as_str().as_bytes());
        let mut disambiguator = [0u8; 16];
        disambiguator.copy_from_slice(&digest[..16]);
        Ok(Self {
            ordinal,
            disambiguator,
        })
    }

    /// Allocate without a shared counter.  The ordinal is a stable 63-bit
    /// display hint derived from the Issue id; the independent 128-bit
    /// disambiguator remains the collision-proof identity component.
    pub fn deterministic_for_issue(issue: &DocId) -> Self {
        let digest = blake3::derive_key(
            "lait.issues.alias-coordinate.ordinal.v1",
            issue.as_str().as_bytes(),
        );
        let mut ordinal_bytes = [0u8; 8];
        ordinal_bytes.copy_from_slice(&digest[..8]);
        let high_bit_clear = u64::try_from(i64::MAX).unwrap_or(u64::MAX);
        let ordinal = (u64::from_be_bytes(ordinal_bytes) & high_bit_clear).max(1);
        match Self::for_issue(ordinal, issue) {
            Ok(coordinate) => coordinate,
            Err(_) => {
                let digest = blake3::derive_key(
                    "lait.issues.alias-coordinate.v1",
                    issue.as_str().as_bytes(),
                );
                let mut disambiguator = [0u8; 16];
                disambiguator.copy_from_slice(&digest[..16]);
                Self {
                    ordinal,
                    disambiguator,
                }
            }
        }
    }

    pub fn suffix(self) -> String {
        data_encoding::HEXLOWER.encode(&self.disambiguator)
    }

    /// Render an alias that is stable without consulting any collision group.
    /// The fixed disambiguator is intentionally always present: conditionally
    /// adding one after an offline collision would rename an existing Issue.
    pub fn render(self, project_key: &str) -> Result<String, Invalid> {
        if project_key.is_empty()
            || project_key.len() > 8
            || !project_key.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            return Err(Invalid::Field("project_key"));
        }
        self.validate()?;
        let suffix = self.suffix();
        let short = suffix.get(..8).unwrap_or(suffix.as_str());
        Ok(format!("{project_key}-{}-{short}", self.ordinal))
    }
}

impl CanonicalRecord for IssueAliasCoordinate {
    fn validate(&self) -> Result<(), Invalid> {
        if self.ordinal == 0 || self.disambiguator == [0u8; 16] {
            Err(Invalid::Field("alias"))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind {
    Comment,
    Reaction,
    Activity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SegmentDescriptor {
    pub issue: String,
    pub kind: SegmentKind,
    /// Stable record identity. For comments this is the comment id; for
    /// reactions it is a digest of (comment, emoji, actor); for activity it is
    /// derived from the Runtime request coordinate and Issue id.
    pub record: String,
}

impl CanonicalRecord for SegmentDescriptor {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none() || !token(&self.record, 160) {
            return Err(Invalid::Field("record_body"));
        }
        Ok(())
    }
}

/// One LWW reaction toggle. Its Body identity is the stable tuple, so turning
/// a reaction off never removes history or races a shared set mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactionRecord {
    pub issue: String,
    pub comment: String,
    pub emoji: String,
    pub actor: String,
    pub on: bool,
}

impl ReactionRecord {
    pub fn identity(&self) -> String {
        let mut material = Vec::new();
        for value in [&self.comment, &self.emoji, &self.actor] {
            let len = u64::try_from(value.len()).unwrap_or(u64::MAX);
            material.extend_from_slice(&len.to_be_bytes());
            material.extend_from_slice(value.as_bytes());
        }
        data_encoding::HEXLOWER.encode(&blake3::derive_key(
            "lait.issues.reaction-record.v1",
            &material,
        ))
    }
}

impl CanonicalRecord for ReactionRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none()
            || !crate::contract::is_comment_id(&self.comment)
            || !crate::contract::is_reaction_emoji(&self.emoji)
            || ActorId::parse(&self.actor).is_none()
        {
            return Err(Invalid::Field("reaction"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "record_kind",
    content = "record",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum DiscussionRecord {
    Comment(crate::contract::StoredComment),
    Reaction(ReactionRecord),
}

impl CanonicalRecord for DiscussionRecord {
    fn validate(&self) -> Result<(), Invalid> {
        match self {
            Self::Comment(comment) => {
                if !canonical_actor(&comment.a)
                    || comment.t == 0
                    || comment.b.is_empty()
                    || comment.b.len() > crate::contract::MAX_TEXT_BYTES
                    || comment
                        .id
                        .as_deref()
                        .is_none_or(|id| !crate::contract::is_comment_id(id))
                    || comment
                        .parent
                        .as_deref()
                        .is_some_and(|id| !crate::contract::is_comment_id(id))
                {
                    Err(Invalid::Field("comment"))
                } else {
                    Ok(())
                }
            }
            Self::Reaction(record) => record.validate(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityRecord {
    pub issue: String,
    pub event: crate::contract::IssueEvent,
}

impl CanonicalRecord for ActivityRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none()
            || self.event.k.is_empty()
            || self.event.k.len() > 64
            || !canonical_actor(&self.event.a)
            || self.event.t == 0
            || self.event.x.len() > crate::contract::MAX_TEXT_BYTES
            || self.event.c.len() > crate::contract::MAX_NAME_BYTES
        {
            Err(Invalid::Field("activity"))
        } else {
            Ok(())
        }
    }
}

/// One independently edited enrichment edge. Set-like relationships are
/// keyed by the full tuple; singleton memberships (milestone/cycle/baseline)
/// are keyed by `(issue, kind)` so replacement is one atomic register write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssueRelationRecord {
    pub issue: String,
    pub project: String,
    pub kind: String,
    pub target: String,
    pub present: bool,
}

impl IssueRelationRecord {
    pub fn identity(&self) -> String {
        let singleton = matches!(self.kind.as_str(), "milestone" | "cycle" | "baseline");
        let mut material = Vec::new();
        for value in [&self.issue, &self.kind] {
            material
                .extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            material.extend_from_slice(value.as_bytes());
        }
        if !singleton {
            material.extend_from_slice(
                &u64::try_from(self.target.len())
                    .unwrap_or(u64::MAX)
                    .to_be_bytes(),
            );
            material.extend_from_slice(self.target.as_bytes());
        }
        data_encoding::HEXLOWER.encode(&blake3::derive_key(
            "lait.issues.issue-relation.v1",
            &material,
        ))
    }
}

impl CanonicalRecord for IssueRelationRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if DocId::parse(&self.issue).is_none()
            || ProjectId::parse(&self.project).is_none()
            || !matches!(
                self.kind.as_str(),
                "assignee" | "follower" | "label" | "milestone" | "cycle" | "baseline"
            )
            || self.target.is_empty()
            || self.target.len() > 256
        {
            return Err(Invalid::Field("issue_relation"));
        }
        Ok(())
    }
}

/// One independently edited membership fact outside the Issue enrichment
/// plane. Its stable tuple owns one LWW register Body; entity Bodies never grow
/// with project or actor membership cardinality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityRelationRecord {
    pub owner: String,
    pub kind: String,
    pub target: String,
    pub present: bool,
}

impl EntityRelationRecord {
    pub fn identity(&self) -> String {
        let mut material = Vec::new();
        for value in [&self.owner, &self.kind, &self.target] {
            material
                .extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            material.extend_from_slice(value.as_bytes());
        }
        data_encoding::HEXLOWER.encode(&blake3::derive_key(
            "lait.issues.entity-relation.v1",
            &material,
        ))
    }
}

impl CanonicalRecord for EntityRelationRecord {
    fn validate(&self) -> Result<(), Invalid> {
        let valid = match self.kind.as_str() {
            "initiative_project" => {
                InitiativeId::parse(&self.owner).is_some()
                    && ProjectId::parse(&self.target).is_some()
            }
            "team_member" => {
                TeamId::parse(&self.owner).is_some() && ActorId::parse(&self.target).is_some()
            }
            _ => false,
        };
        if valid {
            Ok(())
        } else {
            Err(Invalid::Field("entity_relation"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InitiativeRecord {
    pub initiative: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub health: String,
    #[serde(default)]
    pub target_date: Option<u64>,
    #[serde(default)]
    pub tombstone: bool,
}

impl CanonicalRecord for InitiativeRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if InitiativeId::parse(&self.initiative).is_none()
            || (!self.owner.is_empty() && !canonical_actor(&self.owner))
            || !matches!(
                self.health.as_str(),
                "" | "on_track" | "at_risk" | "off_track"
            )
        {
            return Err(Invalid::Field("initiative"));
        }
        nonempty_bounded(&self.name, crate::contract::MAX_NAME_BYTES, "name")?;
        optional_bounded(
            &self.description,
            crate::contract::MAX_METADATA_DESCRIPTION_BYTES,
            "description",
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TeamRecord {
    pub team: String,
    pub name: String,
    pub key: String,
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub lead: String,
    #[serde(default)]
    pub tombstone: bool,
}

impl CanonicalRecord for TeamRecord {
    fn validate(&self) -> Result<(), Invalid> {
        if TeamId::parse(&self.team).is_none()
            || self.key.is_empty()
            || self.key.len() > 8
            || !self.key.bytes().all(|byte| byte.is_ascii_uppercase())
            || (!self.lead.is_empty() && !canonical_actor(&self.lead))
        {
            return Err(Invalid::Field("team"));
        }
        nonempty_bounded(&self.name, crate::contract::MAX_NAME_BYTES, "name")?;
        optional_bounded(
            &self.icon,
            crate::contract::MAX_PRESENTATION_TOKEN_BYTES,
            "icon",
        )
    }
}

/// Compact, generation-local corpus coordinates. Durable identities are
/// interned once and mapped to these fixed-width values.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeIx(pub u32);

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StringIx(pub u32);

pub mod issue_flags {
    pub const TOMBSTONED: u16 = 1 << 0;
    pub const ARCHIVED_PROJECT: u16 = 1 << 1;
    pub const HAS_BASELINE: u16 = 1 << 2;
    pub const HAS_DISCUSSION_OVERFLOW: u16 = 1 << 3;
    pub const HAS_ACTIVITY_OVERFLOW: u16 = 1 << 4;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusIssueRecord {
    pub project: NodeIx,
    pub workflow_state: StringIx,
    pub position: StringIx,
    pub alias_ordinal: u64,
    pub alias_disambiguator: [u8; 16],
    pub flags: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusScheduleRecord {
    pub project: NodeIx,
    pub name: StringIx,
    pub target_or_start: u64,
    pub end: u64,
    pub total: u32,
    pub done: u32,
    pub flags: u16,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorpusRecordAddress {
    pub owner: NodeIx,
    pub record: StringIx,
    pub kind: u8,
}

/// Validate that a list intended for a set-like root is already canonical.
pub fn validate_sorted_unique_ids(values: &[String]) -> Result<(), Invalid> {
    if !sorted_unique(values) || values.iter().any(|value| value.is_empty()) {
        return Err(Invalid::Field("ids"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const PROJECT: &str = "prj_00000000000000000000000000";
    const ISSUE: &str = "iss_00000000000000000000000000";
    const ACTOR: &str = "act_0000000000000000000000000000000000000000000000000000000000000000";

    #[test]
    fn schema_vocabulary_is_unique_and_canonical() {
        let declarations = schemas();
        assert_eq!(declarations.len(), PHYSICAL_SCHEMAS.len());
        let ids: BTreeSet<_> = declarations
            .iter()
            .map(|schema| schema.id.as_str())
            .collect();
        assert_eq!(ids.len(), declarations.len());
        assert!(declarations
            .iter()
            .zip(PHYSICAL_SCHEMAS)
            .all(|(declaration, physical)| {
                declaration.version == SCHEMA_VERSION
                    && declaration.readable_predecessors.is_empty()
                    && matches!(
                        (&declaration.mutation, physical.atomic()),
                        (MutationModel::Atomic, true) | (MutationModel::Collaborative(_), false)
                    )
            }));
        assert!(PHYSICAL_SCHEMAS
            .iter()
            .all(|schema| !schema.name().contains("plan")));
        assert_eq!(
            crate::spec::Kind::parse("plan"),
            Some(crate::spec::Kind::Plan)
        );
        for schema in PHYSICAL_SCHEMAS {
            let paths: BTreeSet<_> = schema.roots().iter().map(|root| root.path).collect();
            assert_eq!(paths.len(), schema.roots().len(), "{:?}", schema);
        }
    }

    #[test]
    fn body_coordinates_are_domain_separated() {
        let project = ProjectId::parse(PROJECT).unwrap();
        let meta = project_meta_key(&project);
        let workflow = workflow_revision_key(&project, &"11".repeat(32));
        let schedule = project_schedule_key(&project, "mil_00000000000000000000000000");
        assert_ne!(meta, workflow);
        assert_ne!(workflow, schedule);
        assert_eq!(meta, project_meta_key(&project));
        let one = "upd_00000000000000000000000000";
        let two = "upd_00000000000000000000000001";
        assert_ne!(
            project_updates_key(&project, one),
            project_updates_key(&project, two)
        );
        assert_ne!(
            project_hierarchy_key(&project, ISSUE),
            project_hierarchy_key(&project, "iss_00000000000000000000000001")
        );
    }

    #[test]
    fn board_placement_is_one_canonical_value() {
        let placement = BoardPlacement {
            project: PROJECT.into(),
            workflow_state: "in_progress".into(),
            position: "V0z".into(),
        };
        let bytes = placement.encode_canonical().unwrap();
        assert_eq!(
            String::from_utf8(bytes.clone()).unwrap(),
            format!(
                "{{\"position\":\"V0z\",\"project\":\"{PROJECT}\",\"workflow_state\":\"in_progress\"}}"
            )
        );
        assert_eq!(BoardPlacement::decode_canonical(&bytes), Ok(placement));
        assert_eq!(
            BoardPlacement::decode_canonical(
                format!(
                    "{{ \"position\":\"V0z\",\"project\":\"{PROJECT}\",\"workflow_state\":\"in_progress\"}}"
                )
                .as_bytes()
            ),
            Err(Invalid::NonCanonical)
        );
    }

    #[test]
    fn embedded_revision_claims_rebuild_to_their_content_ids() {
        let role = crate::roles::built_in("lait.viewer").unwrap();
        let governance = GovernanceRevisionRecord {
            role: role.body.role_id.clone(),
            revision: crate::views::StoredRoleRevision {
                revision_id: data_encoding::HEXLOWER.encode(&role.revision_id),
                predecessor_ids: role
                    .predecessor_ids
                    .iter()
                    .map(|digest| data_encoding::HEXLOWER.encode(digest))
                    .collect(),
                body: role.body,
            },
        };
        assert!(governance.validate().is_ok());

        let workflow = crate::workflow::build_revision(
            crate::workflow::default_workflow_body(PROJECT),
            Vec::new(),
        )
        .unwrap();
        assert!(ProjectWorkflowRevisionRecord {
            project: PROJECT.into(),
            revision: workflow,
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn alias_coordinates_do_not_drift_when_ordinals_collide() {
        let issue = DocId::parse(ISSUE).unwrap();
        let other = DocId::parse("iss_00000000000000000000000001").unwrap();
        let a = IssueAliasCoordinate::for_issue(12, &issue).unwrap();
        let again = IssueAliasCoordinate::for_issue(12, &issue).unwrap();
        let b = IssueAliasCoordinate::for_issue(12, &other).unwrap();
        assert_eq!(a, again);
        assert_ne!(a, b);
        assert_eq!(a.suffix().len(), 32);
        assert!(IssueIdentityRecord {
            issue: ISSUE.into(),
            alias: a,
        }
        .validate()
        .is_ok());
        assert_eq!(
            IssueIdentityRecord {
                issue: ISSUE.into(),
                alias: b,
            }
            .validate(),
            Err(Invalid::Field("alias_binding"))
        );
    }

    #[test]
    fn triage_decisions_preserve_races_as_distinct_claims() {
        let accepted = TriageDecisionRecord {
            decision: "decision_1".into(),
            triage: "trg_00000000000000000000000000".into(),
            outcome: TriageOutcome::Accepted,
            decided_by: ACTOR.into(),
            timestamp: 1,
            project: Some(PROJECT.into()),
            issue: Some(ISSUE.into()),
            note: String::new(),
        };
        assert!(accepted.validate().is_ok());
        let mut invalid = accepted.clone();
        invalid.issue = None;
        assert_eq!(
            invalid.validate(),
            Err(Invalid::Field("decision_coordinates"))
        );
    }

    #[test]
    fn collaboration_records_have_stable_single_record_bodies() {
        let descriptor = SegmentDescriptor {
            issue: ISSUE.into(),
            kind: SegmentKind::Comment,
            record: "cmt_00000000000000000000000000".into(),
        };
        assert!(descriptor.validate().is_ok());
        let key = issue_comment_key(&DocId::parse(ISSUE).unwrap(), &descriptor.record);
        assert_eq!(
            key,
            issue_comment_key(&DocId::parse(ISSUE).unwrap(), &descriptor.record)
        );
        assert_ne!(
            key,
            issue_reaction_key(&DocId::parse(ISSUE).unwrap(), &descriptor.record)
        );
        let mut invalid = descriptor;
        invalid.record.clear();
        assert_eq!(invalid.validate(), Err(Invalid::Field("record_body")));
    }

    #[test]
    fn relation_nodes_have_stable_direct_routing() {
        let parent = HierarchyRecord {
            project: PROJECT.into(),
            child: ISSUE.into(),
            parent: Some("iss_00000000000000000000000001".into()),
        };
        assert!(parent.validate().is_ok());
        assert_eq!(
            project_hierarchy_key(&ProjectId::parse(PROJECT).unwrap(), &parent.child),
            project_hierarchy_key(&ProjectId::parse(PROJECT).unwrap(), ISSUE)
        );

        let link = ProjectLinkRecord {
            project: PROJECT.into(),
            from: ISSUE.into(),
            kind: "blocks".into(),
            to: "iss_00000000000000000000000001".into(),
            present: true,
        };
        assert!(link.validate().is_ok());
        assert_eq!(link.relation_identity(), link.relation_identity());
        let coordinate = data_encoding::HEXLOWER.encode(&link.relation_identity());
        assert_eq!(coordinate.len(), 64);
    }

    #[test]
    fn compact_records_do_not_embed_product_strings() {
        assert!(std::mem::size_of::<CorpusIssueRecord>() <= 48);
        assert!(std::mem::size_of::<CorpusScheduleRecord>() <= 40);
        assert!(std::mem::size_of::<CorpusRecordAddress>() <= 12);
    }

    #[test]
    fn unknown_fields_fail_closed() {
        let bytes = format!(
            "{{\"position\":\"V\",\"project\":\"{PROJECT}\",\"workflow_state\":\"backlog\",\"surprise\":true}}"
        );
        assert_eq!(
            BoardPlacement::decode_canonical(bytes.as_bytes()),
            Err(Invalid::Encoding)
        );
    }
}
