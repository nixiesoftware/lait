//! Versioned application protocol carried inside an opaque [`Call`].

use runtime::world::call::{Access, Call, Code, Failure, Reply};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const OPERATION: &str = "issues.control";
pub const VERSION: u32 = 2;

/// Portable semantic publication coordinate. All three digests are mandatory:
/// a Manifest root alone does not identify the World implementation or the
/// extractor contract that gave the corpus its meaning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PublicationCoordinate {
    pub manifest_root: String,
    pub implementation_digest: String,
    pub extractor_schema_digest: String,
}

impl PublicationCoordinate {
    pub fn from_id(value: &runtime::publication::PublicationId) -> Self {
        Self {
            manifest_root: data_encoding::HEXLOWER.encode(&value.manifest_root),
            implementation_digest: data_encoding::HEXLOWER.encode(&value.implementation_digest),
            extractor_schema_digest: data_encoding::HEXLOWER
                .encode(&value.extractor_schema_digest.digest()),
        }
    }

    pub fn parse(&self) -> Option<runtime::publication::PublicationId> {
        fn digest(value: &str) -> Option<[u8; 32]> {
            let bytes = data_encoding::HEXLOWER.decode(value.as_bytes()).ok()?;
            bytes.as_slice().try_into().ok()
        }
        Some(runtime::publication::PublicationId::new(
            digest(&self.manifest_root)?,
            digest(&self.implementation_digest)?,
            runtime::publication::ExtractorSchemaDigest::from_digest(digest(
                &self.extractor_schema_digest,
            )?),
        ))
    }
}

/// Exact Station-local read image used only for short-lived reconciliation.
/// Unlike `PublicationCoordinate`, this must never be persisted in durable
/// product state because materialization ids are local to one activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorldPublicationCoordinate {
    pub publication: PublicationCoordinate,
    pub materialization: u64,
}

impl WorldPublicationCoordinate {
    pub fn from_id(value: &runtime::publication::WorldPublicationId) -> Self {
        Self {
            publication: PublicationCoordinate::from_id(&value.publication),
            materialization: value.materialization.get(),
        }
    }

    pub fn parse(&self) -> Option<runtime::publication::WorldPublicationId> {
        Some(runtime::publication::WorldPublicationId::new(
            self.publication.parse()?,
            runtime::publication::MaterializationId::from_u64(self.materialization)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum BoardPos {
    Top,
    Bottom,
    Before { reff: String },
    After { reff: String },
}

/// One Unicode-scalar edit in an atomic legacy-document upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DocumentSplice {
    pub index: u64,
    pub delete: u64,
    pub insert: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Filter {
    #[serde(default)]
    pub mine: bool,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Milestone name or `mls_` id, resolved within the listed project.
    /// Meaningless without a project — a milestone belongs to exactly one, so a
    /// filter with no project to resolve against is refused rather than guessed.
    #[serde(default)]
    pub milestone: Option<String>,
    #[serde(default)]
    pub all: bool,
}

/// One generic Space-authority assignment produced by an Issues role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccessAssignment {
    pub world: String,
    pub capability: String,
    #[serde(default)]
    pub resource: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ChangeProject {
    Existing { project: String },
    Created { operation: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ChangeLabel {
    Existing { label: String },
    Created { operation: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ChangeOperation {
    ProjectCreate {
        name: String,
        key: String,
        color: String,
    },
    SpecCreate {
        project: ChangeProject,
        kind: issues::spec::Kind,
        title: String,
        text: String,
        #[serde(default)]
        links: Vec<issues::spec::Link>,
    },
    IssueCreate {
        project: ChangeProject,
        title: String,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default)]
        assignees: Vec<String>,
        #[serde(default)]
        labels: Vec<ChangeLabel>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        due: Option<u64>,
        #[serde(default)]
        estimate: Option<u32>,
    },
    IssueBoard {
        issue: String,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        position: Option<ChangePosition>,
    },
    IssuePatch {
        issue: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        due: Option<u64>,
        #[serde(default)]
        clear_due: bool,
        #[serde(default)]
        estimate: Option<u32>,
        #[serde(default)]
        clear_estimate: bool,
        #[serde(default)]
        assignees: Option<Vec<String>>,
        #[serde(default)]
        labels: Option<Vec<ChangeLabel>>,
    },
    IssueWork {
        issue: String,
        action: ChangeWorkAction,
    },
    IssueTombstone {
        issue: String,
        on: bool,
    },
    IssueComment {
        issue: String,
        body: String,
        #[serde(default)]
        parent: Option<String>,
    },
    IssueCommentAt {
        issue: String,
        body: String,
        field: String,
        start: u64,
        #[serde(default)]
        end: Option<u64>,
        #[serde(default)]
        parent: Option<String>,
        source: WorldPublicationCoordinate,
    },
    IssueReaction {
        issue: String,
        comment: String,
        emoji: String,
        on: bool,
    },
    IssueLink {
        issue: String,
        kind: String,
        target: String,
        on: bool,
    },
    IssueParent {
        issue: String,
        #[serde(default)]
        parent: Option<String>,
    },
    IssueMove {
        issue: String,
        #[serde(default)]
        project: Option<ChangeProject>,
        #[serde(default)]
        position: Option<ChangePosition>,
    },
    IssueMilestone {
        issue: String,
        #[serde(default)]
        milestone: Option<String>,
    },
    LabelCreate {
        name: String,
        color: String,
    },
    LabelEdit {
        label: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    LabelDelete {
        label: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChangeWorkAction {
    Start,
    Done,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum ChangePosition {
    Top,
    Bottom,
    Before { issue: String },
    After { issue: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeEffect {
    pub operation: u16,
    pub kind: String,
    pub id: String,
}

/// The durable acknowledgement for one Issues operation.
///
/// `operation` is the signed Runtime action's persistent RequestId, not a
/// browser- or adapter-local correlation token. `accepted` therefore begins
/// only after Replica durability. The exact publication is the coordinate a
/// live consumer must observe and refresh before advancing the same operation
/// to `committed` in its rendered view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationReceipt {
    pub operation: String,
    pub phase: OperationPhase,
    pub publication: runtime::publication::WorldPublicationId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationPhase {
    Accepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationReadiness {
    Absent,
    Ready,
    Building,
    Capacity,
    ImplementationUnavailable,
    GenerationUnavailable,
    Unavailable,
}

/// Issues-owned application requests.
///
/// The tagged JSON representation is also the Issues web-client contract. All
/// native product clients carry this type inside a [`Call`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IssuesRequest {
    /// Atomically lower dependent project/Spec creation through one product
    /// planner and one Runtime publication.
    ChangeSet {
        /// Optional caller-generated 128-bit idempotency coordinate. Browser
        /// feedback supplies it so `sending` already names the exact signed
        /// operation; server/agent adapters may omit it and receive the minted
        /// durable id in the accepted envelope.
        #[serde(default)]
        operation: Option<String>,
        /// Retry-stable authored time. Replaying an operation must replay this
        /// value so the signed semantic intent stays byte-identical.
        #[serde(default)]
        timestamp: Option<u64>,
        operations: Vec<ChangeOperation>,
    },
    /// Read one exact ChangeSet receipt without invoking or resubmitting it.
    OperationStatus {
        operation: String,
        timestamp: u64,
        operations: Vec<ChangeOperation>,
    },
    /// Project the acting identity's inbox using the caller-local read
    /// watermark. Advancing that watermark remains a client-host facility.
    Inbox {
        #[serde(default)]
        watermark: u64,
        #[serde(default)]
        page: issues::contract::PageRequest,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
    },
    /// Resolve a pinned Issues role into generic Mechanics assignments. The
    /// client host commits the returned plan through root Space authority.
    AccessPlan {
        role: String,
        #[serde(default)]
        project: Option<String>,
    },
    IssueNew {
        title: String,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        project_hint: Option<String>,
        #[serde(default)]
        assignees: Vec<String>,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        labels: Vec<String>,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        due: Option<String>,
        #[serde(default)]
        estimate: Option<u32>,
    },
    IssueEdit {
        reff: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        due: Option<String>,
        #[serde(default)]
        estimate: Option<String>,
    },
    /// Apply one Unicode-scalar splice to the collaborative issue body.
    ///
    /// This is the live-editor write path. Unlike `IssueEdit.description`, it
    /// preserves the user's local operation so concurrent insertions can be
    /// merged by the text CRDT instead of being collapsed into competing
    /// whole-document replacements.
    IssueTextSplice {
        reff: String,
        index: u64,
        delete: u64,
        insert: String,
        /// Scalar length of the document these offsets were measured against.
        /// The World refuses when it disagrees with what it holds.
        #[serde(default)]
        base_len: Option<u64>,
    },
    /// Upgrade a legacy issue body without exposing either storage language in
    /// the user interface. `expected` prevents a concurrent edit being lost.
    IssueDocumentUpgrade {
        reff: String,
        expected: String,
        splices: Vec<DocumentSplice>,
    },
    /// Record one human-readable history entry after a burst of live splices.
    /// This is deliberately separate from replication: its quiet window must
    /// never delay text reaching peers.
    IssueTextCheckpoint {
        reff: String,
    },
    IssueMove {
        reff: String,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        pos: Option<BoardPos>,
    },
    Assign {
        reff: String,
        who: Vec<String>,
        #[serde(default = "default_true")]
        add: bool,
    },
    Label {
        reff: String,
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
    },
    Comment {
        reff: String,
        body: String,
        #[serde(default)]
        reply_to: Option<String>,
    },
    /// Comment on a span of an issue's collaborative text.
    ///
    /// A separate verb from [`IssuesRequest::Comment`] for the same reason
    /// `IssueIntent::CommentAt` is separate from `IssueIntent::Comment`: the
    /// span carries preconditions a plain comment has none of, and `comment`'s
    /// field set is the wire form clients already write.
    CommentAt {
        reff: String,
        body: String,
        /// The collaborative text field the span lies in — `description`.
        field: String,
        /// The span's start, in Unicode scalar offsets. A browser client
        /// counts a `string` in UTF-16 code units and must convert; the two
        /// disagree on every astral character.
        start: u64,
        /// The span's end. Absent names a position rather than a span.
        #[serde(default)]
        end: Option<u64>,
        #[serde(default)]
        reply_to: Option<String>,
        /// Exact rendered source whose scalar coordinates are being named.
        source: WorldPublicationCoordinate,
    },
    React {
        reff: String,
        comment: String,
        emoji: String,
        #[serde(default = "default_true")]
        on: bool,
    },
    IssueDelete {
        reff: String,
    },
    IssueRestore {
        reff: String,
    },
    IssueLink {
        reff: String,
        kind: String,
        target: String,
    },
    IssueUnlink {
        reff: String,
        kind: String,
        target: String,
    },
    IssueParent {
        reff: String,
        #[serde(default)]
        parent: Option<String>,
    },
    /// Deterministic Issue morphology for a Plan seed. `publication` selects
    /// one exact semantic corpus; absence means the authority-active current
    /// publication. `page` is bounded and artifact-pinned by its cursor.
    Geometry {
        project: String,
        #[serde(default)]
        roots: Vec<String>,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
        #[serde(default)]
        #[schemars(with = "Option<serde_json::Value>")]
        page: Option<issues::geometry::GeometryPageRequest>,
    },
    IssueStart {
        reff: String,
    },
    IssueDone {
        reff: String,
    },
    IssueStop {
        reff: String,
    },
    /// Start the Issues-owned verification of one pinned repository source.
    Verify {
        reff: String,
        source: String,
        build: String,
    },
    /// Accept one returned verification report into issue truth.
    AcceptCheck {
        reff: String,
        run: String,
        attempt: String,
        report: String,
        verdict: String,
        #[serde(default)]
        move_to_done: bool,
    },
    IssueView {
        reff: String,
    },
    /// Bounded issue summary plus first pages of every enrichment section.
    IssueDetail {
        reff: String,
        /// Exact World publication to render while reconciling a durable
        /// operation. Omitted only for an ordinary current-view open.
        #[serde(default)]
        publication: Option<WorldPublicationCoordinate>,
    },
    List {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        filter: Filter,
        page: issues::contract::PageRequest,
    },
    Board {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        project_hint: Option<String>,
        page: issues::contract::PageRequest,
    },
    History {
        reff: String,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
        page: issues::contract::PageRequest,
    },
    IssueRelations {
        reff: String,
        direction: issues::dto::RelationDirection,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
        page: issues::contract::PageRequest,
    },
    IssueComments {
        reff: String,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
        page: issues::contract::PageRequest,
    },
    IssueReactions {
        reff: String,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
        page: issues::contract::PageRequest,
    },
    IssueAttachments {
        reff: String,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
        page: issues::contract::PageRequest,
    },
    IssueChecks {
        reff: String,
        #[serde(default)]
        publication: Option<PublicationCoordinate>,
        page: issues::contract::PageRequest,
    },
    ProjectNew {
        name: String,
        key: String,
        #[serde(default)]
        color: Option<String>,
    },
    ProjectList {
        page: issues::contract::PageRequest,
    },
    ProjectEdit {
        project: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        color: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        lead: Option<String>,
        #[serde(default)]
        start: Option<String>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        archived: Option<bool>,
        #[serde(default)]
        team: Option<String>,
    },
    ProjectDelete {
        project: String,
    },
    Follow {
        reff: String,
        #[serde(default = "default_true")]
        on: bool,
    },
    MilestoneList {
        project: String,
        page: issues::contract::PageRequest,
    },
    MilestoneSet {
        project: String,
        #[serde(default)]
        milestone: Option<String>,
        #[serde(default)]
        name: Option<String>,
        /// The milestone's prose body. Absent leaves it untouched; `""` clears.
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        target: Option<String>,
        /// Where to place it in the project's manual order — `Before`/`After`
        /// name another milestone of the same project. Absent leaves an existing
        /// milestone where it is and appends a new one.
        #[serde(default)]
        pos: Option<BoardPos>,
        #[serde(default)]
        remove: bool,
    },
    IssueMilestone {
        reff: String,
        #[serde(default)]
        milestone: Option<String>,
    },
    CycleList {
        project: String,
        page: issues::contract::PageRequest,
    },
    CycleSet {
        project: String,
        #[serde(default)]
        cycle: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        start: Option<String>,
        #[serde(default)]
        end: Option<String>,
        #[serde(default)]
        remove: bool,
    },
    IssueCycle {
        reff: String,
        #[serde(default)]
        cycle: Option<String>,
    },
    InitiativeList {
        page: issues::contract::PageRequest,
    },
    InitiativeSet {
        #[serde(default)]
        initiative: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        owner: Option<String>,
        #[serde(default)]
        health: Option<String>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        add_projects: Vec<String>,
        #[serde(default)]
        remove_projects: Vec<String>,
        #[serde(default)]
        remove: bool,
    },
    TeamList {
        page: issues::contract::PageRequest,
    },
    TeamSet {
        #[serde(default)]
        team: Option<String>,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        icon: Option<String>,
        #[serde(default)]
        lead: Option<String>,
        #[serde(default)]
        add_members: Vec<String>,
        #[serde(default)]
        remove_members: Vec<String>,
        #[serde(default)]
        remove: bool,
    },
    TriageList {
        page: issues::contract::PageRequest,
    },
    TriageSubmit {
        title: String,
        #[serde(default)]
        body: Option<String>,
        #[serde(default)]
        source: Option<String>,
    },
    TriageDecide {
        id: String,
        outcome: String,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        note: Option<String>,
    },
    Attach {
        reff: String,
        name: String,
        #[serde(default)]
        mime: Option<String>,
        /// The content id this attachment names, already on the content plane.
        content: String,
        /// Plaintext bytes, as the uploader saw them.
        size: u64,
        #[serde(default)]
        comment: Option<String>,
    },
    Detach {
        reff: String,
        id: String,
    },
    AttachmentGet {
        reff: String,
        id: String,
    },
    ProjectUpdates {
        project: String,
        page: issues::contract::PageRequest,
    },
    ProjectUpdatePost {
        project: String,
        body: String,
        #[serde(default)]
        health: Option<String>,
    },
    LabelNew {
        name: String,
        #[serde(default)]
        color: Option<String>,
    },
    LabelList {
        page: issues::contract::PageRequest,
    },
    /// Hydrate one label at an exact retained World publication. Used by the
    /// shared operation registry before it retires label optimism.
    LabelShow {
        label: String,
        publication: WorldPublicationCoordinate,
    },
    LabelEdit {
        label: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        color: Option<String>,
    },
    LabelDelete {
        label: String,
    },
    SpaceRename {
        name: String,
    },
    SpaceDescribe {
        description: String,
    },
    Activity {
        page: issues::contract::PageRequest,
    },
    RoleList {
        page: issues::contract::PageRequest,
    },
    RoleShow {
        role: String,
    },
    RoleCreate {
        name: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        project: Option<String>,
        capabilities: Vec<String>,
    },
    RoleEdit {
        role: String,
        expect_revision: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        capabilities: Option<Vec<String>>,
    },
    RoleDelete {
        role: String,
        expect_revision: String,
    },
    RoleResolve {
        role: String,
        expect_heads: Vec<String>,
        body_json: String,
    },
    WorkflowShow {
        project: String,
    },
    WorkflowValidate {
        body_json: String,
    },
    WorkflowSet {
        project: String,
        expect_heads: Vec<String>,
        body_json: String,
    },
    SpecList {
        #[serde(default)]
        project: Option<String>,
        page: issues::contract::PageRequest,
    },
    SpecShow {
        spec: String,
    },
    SpecHistory {
        spec: String,
        page: issues::contract::PageRequest,
    },
    SpecReferences {
        #[serde(default)]
        project: Option<String>,
        page: issues::contract::PageRequest,
    },
    SpecNew {
        project: String,
        kind: issues::spec::Kind,
        title: String,
        #[serde(default)]
        text: String,
        #[serde(default)]
        links: Vec<issues::spec::Link>,
    },
    SpecRevise {
        spec: String,
        expected: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        links: Option<Vec<issues::spec::Link>>,
        /// Omitted preserves the Plan structure; `null` removes it.
        #[serde(default)]
        plan: Option<Option<issues::spec::PlanData>>,
    },
    /// Upgrade one legacy Spec body without exposing either document language
    /// in the user interface. The immutable successor keeps the same state.
    SpecDocumentUpgrade {
        spec: String,
        expected: String,
        text: String,
    },
    SpecState {
        spec: String,
        expected: String,
        state: issues::spec::State,
    },
    SpecResolve {
        spec: String,
        expected_heads: Vec<String>,
        body_json: String,
    },
    SpecObservations {
        #[serde(default)]
        project: Option<String>,
        page: issues::contract::PageRequest,
    },
    SpecObserve {
        spec: String,
        rel: issues::spec::Rel,
        target: issues::spec::Target,
        #[serde(default)]
        note: String,
    },
    SpecRetract {
        spec: String,
        observation: String,
    },
    BaselineList {
        #[serde(default)]
        project: Option<String>,
        page: issues::contract::PageRequest,
    },
    BaselineShow {
        baseline: String,
    },
    BaselineHistory {
        baseline: String,
        page: issues::contract::PageRequest,
    },
    BaselineNew {
        project: String,
        name: String,
        members: Vec<issues::spec::SpecRef>,
    },
    BaselineRevise {
        baseline: String,
        expected: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        members: Option<Vec<issues::spec::SpecRef>>,
    },
    BaselineState {
        baseline: String,
        expected: String,
        state: issues::spec::State,
    },
    BaselineResolve {
        baseline: String,
        expected_heads: Vec<String>,
        body_json: String,
    },
    IssueBaseline {
        reff: String,
        #[serde(default)]
        baseline: Option<issues::spec::BaselineRef>,
    },
    Packet {
        reff: String,
    },
}

/// Issues-owned application responses.
///
/// Client packages decode and present this schema directly; the root control
/// protocol neither mirrors nor interprets these product variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IssuesResponse {
    /// A product result paired with the durable Runtime receipt that produced
    /// it. All write access paths use this same envelope; agents do not receive
    /// a weaker or differently-shaped acknowledgement than the viewer.
    Operation {
        receipt: OperationReceipt,
        response: Box<IssuesResponse>,
    },
    ChangeSet {
        results: Vec<ChangeEffect>,
    },
    OperationStatus {
        operation: String,
        readiness: OperationReadiness,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        publication: Option<runtime::publication::WorldPublicationId>,
        #[serde(default)]
        results: Vec<ChangeEffect>,
    },
    Ok {
        message: Option<String>,
    },
    Ref {
        reff: String,
    },
    Check {
        reff: String,
        run: String,
    },
    Issue(Box<issues::dto::IssueView>),
    IssueDetail(Box<issues::contract::IssueDetailProjection>),
    List {
        page: issues::contract::Page<issues::dto::Row>,
    },
    Board(Box<issues::dto::BoardPage>),
    Geometry(Box<issues::contract::GeometryProjection>),
    Activity {
        page: issues::contract::Page<issues::dto::ActivityEvent>,
    },
    Relations {
        page: issues::contract::Page<issues::dto::IssueRelationDto>,
    },
    Comments {
        page: issues::contract::Page<issues::dto::CommentDto>,
    },
    Reactions {
        page: issues::contract::Page<issues::v4::ReactionRecord>,
    },
    Attachments {
        page: issues::contract::Page<issues::dto::AttachmentMetaDto>,
    },
    Checks {
        page: issues::contract::Page<issues::dto::CheckDto>,
    },
    Inbox {
        page: issues::contract::Page<issues::dto::InboxEntry>,
        /// Rows newer than the caller-local watermark in this page. The
        /// protocol does not mislabel this bounded value as a whole-inbox
        /// total when continuation remains.
        unread_on_page: u64,
    },
    AccessPlan {
        assignments: Vec<AccessAssignment>,
    },
    Projects {
        page: issues::contract::Page<issues::dto::ProjectDto>,
    },
    Updates {
        page: issues::contract::Page<issues::dto::ProjectUpdateDto>,
    },
    Labels {
        page: issues::contract::Page<issues::dto::LabelDto>,
    },
    Label {
        publication: runtime::publication::WorldPublicationId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<issues::dto::LabelDto>,
    },
    Roles {
        page: issues::contract::Page<issues::contract::RoleProjection>,
    },
    Milestones {
        page: issues::contract::Page<issues::dto::MilestoneDto>,
    },
    Cycles {
        page: issues::contract::Page<issues::dto::CycleDto>,
    },
    Initiatives {
        page: issues::contract::Page<issues::dto::InitiativeDto>,
    },
    Teams {
        page: issues::contract::Page<issues::dto::TeamDto>,
    },
    TriageItems {
        page: issues::contract::Page<issues::v4::TriageRecord>,
    },
    Attachment {
        name: String,
        mime: String,
        /// The content id, when this record was written after the cutover.
        ///
        /// Exactly one of `content` and `data_b64` is present, and which one
        /// says which era the record is from. Both are optional rather than an
        /// enum because this type is decoded by clients that predate the split
        /// and must not fail on a shape they do not recognise.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        /// The inline payload, for records written before the cutover.
        ///
        /// Read-only and permanent. Those records are in Bodies in the field;
        /// a reader that dropped this would lose the files rather than migrate
        /// them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_b64: Option<String>,
        #[serde(default)]
        size: u64,
    },
    Text {
        text: String,
    },
    SpecRevisions {
        page: issues::contract::Page<issues::spec::Revision>,
    },
    SpecReferences {
        page: issues::contract::Page<issues::spec::SpecReferenceFact>,
    },
    SpecObservations {
        page: issues::contract::Page<issues::v4::SpecObservationRecord>,
    },
    BaselineRevisions {
        page: issues::contract::Page<issues::spec::BaselineRevision>,
    },
    /// Named rather than flattened, unlike its Baseline and Packet neighbours.
    /// This enum is internally tagged on `kind` and a `SpecView` carries a
    /// `kind` of its own, so a newtype variant would emit the key twice: Rust
    /// reads the first occurrence and survives, `JSON.parse` keeps the last and
    /// decodes the reply as a Spec *kind* it has never heard of. A named payload
    /// is the only shape where both readers agree.
    Spec {
        spec: Box<issues::spec::SpecView>,
    },
    Specs {
        page: issues::contract::Page<issues::spec::SpecSummary>,
    },
    Baseline(Box<issues::spec::BaselineView>),
    Baselines {
        page: issues::contract::Page<issues::spec::BaselineSummary>,
    },
    Packet(Box<issues::spec::Packet>),
    Error {
        message: String,
        #[serde(default)]
        error_kind: IssuesErrorKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssuesErrorKind {
    #[default]
    Error,
    Invalid,
    NotFound,
    Denied,
    Retry,
    /// Durability may have succeeded but no terminal receipt can currently be
    /// proven. A caller preserves optimism and reconciles by operation id; it
    /// must not blindly replay as though nothing landed.
    Indeterminate,
}

impl IssuesErrorKind {
    /// How a head that speaks a typed protocol should classify this answer.
    ///
    /// The distinction is whether the caller can act on it. A missing thing or
    /// a refused write is the caller's problem and names its own remedy — the
    /// first wall a freshly-sponsored agent hits is `Denied`, and its message
    /// says what standing is missing. Reporting either as an internal fault
    /// throws that message away and leaves the caller nothing to do.
    pub const fn failure(self) -> world_interface::Failure {
        match self {
            Self::Invalid | Self::NotFound => world_interface::Failure::invalid(),
            Self::Denied => world_interface::Failure::refusal(),
            Self::Error | Self::Retry | Self::Indeterminate => {
                world_interface::Failure::operation()
            }
        }
    }
}

/// Read a failure out of an Issues answer that was delivered successfully.
///
/// Both shapes a client operation can answer with — an [`IssuesResponse`] and
/// the host's control JSON — tag themselves the same way, so this is one peek
/// at two fields rather than a decode of either. That matters: it runs on every
/// answer, and nearly every answer is fine.
pub fn classify_failure(value: &Value) -> Option<(world_interface::Failure, String)> {
    let kind = match value.get("error_kind").and_then(Value::as_str)? {
        "invalid" => IssuesErrorKind::Invalid,
        "not_found" => IssuesErrorKind::NotFound,
        "denied" => IssuesErrorKind::Denied,
        "retry" => IssuesErrorKind::Retry,
        "indeterminate" => IssuesErrorKind::Indeterminate,
        _ => IssuesErrorKind::Error,
    };
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("the Issues request failed")
        .to_string();
    Some((kind.failure(), message))
}

impl IssuesResponse {
    pub fn err(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_kind: IssuesErrorKind::Error,
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_kind: IssuesErrorKind::NotFound,
        }
    }

    pub fn denied(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_kind: IssuesErrorKind::Denied,
        }
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_kind: IssuesErrorKind::Invalid,
        }
    }

    pub fn retry(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_kind: IssuesErrorKind::Retry,
        }
    }

    pub fn indeterminate(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_kind: IssuesErrorKind::Indeterminate,
        }
    }
}

impl IssuesRequest {
    pub fn access(&self) -> Access {
        use IssuesRequest::*;
        match self {
            Inbox { .. }
            | AccessPlan { .. }
            | Geometry { .. }
            | IssueView { .. }
            | IssueDetail { .. }
            | List { .. }
            | Board { .. }
            | History { .. }
            | IssueRelations { .. }
            | IssueComments { .. }
            | IssueReactions { .. }
            | IssueAttachments { .. }
            | IssueChecks { .. }
            | ProjectList { .. }
            | ProjectUpdates { .. }
            | MilestoneList { .. }
            | CycleList { .. }
            | InitiativeList { .. }
            | TeamList { .. }
            | TriageList { .. }
            | AttachmentGet { .. }
            | LabelList { .. }
            | LabelShow { .. }
            | Activity { .. }
            | RoleList { .. }
            | RoleShow { .. }
            | WorkflowShow { .. }
            | WorkflowValidate { .. }
            | SpecList { .. }
            | SpecShow { .. }
            | SpecHistory { .. }
            | SpecReferences { .. }
            | SpecObservations { .. }
            | BaselineList { .. }
            | BaselineShow { .. }
            | BaselineHistory { .. }
            | Packet { .. }
            | OperationStatus { .. } => Access::Query,
            ChangeSet { .. }
            | IssueNew { .. }
            | IssueEdit { .. }
            | IssueTextSplice { .. }
            | IssueDocumentUpgrade { .. }
            | IssueTextCheckpoint { .. }
            | IssueMove { .. }
            | Assign { .. }
            | Label { .. }
            | Comment { .. }
            | CommentAt { .. }
            | React { .. }
            | IssueDelete { .. }
            | IssueRestore { .. }
            | IssueLink { .. }
            | IssueUnlink { .. }
            | IssueParent { .. }
            | IssueStart { .. }
            | IssueDone { .. }
            | IssueStop { .. }
            | Verify { .. }
            | AcceptCheck { .. }
            | ProjectNew { .. }
            | ProjectEdit { .. }
            | ProjectDelete { .. }
            | Follow { .. }
            | MilestoneSet { .. }
            | IssueMilestone { .. }
            | CycleSet { .. }
            | IssueCycle { .. }
            | InitiativeSet { .. }
            | TeamSet { .. }
            | TriageSubmit { .. }
            | TriageDecide { .. }
            | Attach { .. }
            | Detach { .. }
            | ProjectUpdatePost { .. }
            | LabelNew { .. }
            | LabelEdit { .. }
            | LabelDelete { .. }
            | SpaceRename { .. }
            | SpaceDescribe { .. }
            | RoleCreate { .. }
            | RoleEdit { .. }
            | RoleDelete { .. }
            | RoleResolve { .. }
            | WorkflowSet { .. }
            | SpecNew { .. }
            | SpecRevise { .. }
            | SpecDocumentUpgrade { .. }
            | SpecState { .. }
            | SpecResolve { .. }
            | SpecObserve { .. }
            | SpecRetract { .. }
            | BaselineNew { .. }
            | BaselineRevise { .. }
            | BaselineState { .. }
            | BaselineResolve { .. }
            | IssueBaseline { .. } => Access::Command,
        }
    }

    /// The confirmation question for a destructive Issues command.
    pub fn destructive_question(&self) -> Option<String> {
        match self {
            Self::IssueDelete { reff } => Some(format!("delete {reff}?")),
            _ => None,
        }
    }
}

pub fn encode_call(request: &IssuesRequest) -> Result<Call, Failure> {
    let payload = serde_json::to_vec(request).map_err(|error| {
        Failure::new(Code::InvalidCall, format!("encode Issues request: {error}"))
    })?;
    Call::new(issues::contract::world_id(), OPERATION, VERSION, payload)
}

pub fn decode_call(call: &Call) -> Result<IssuesRequest, Failure> {
    validate_contract(call)?;
    serde_json::from_slice(call.payload())
        .map_err(|error| Failure::new(Code::InvalidCall, format!("decode Issues request: {error}")))
}

pub fn encode_reply(call: &Call, response: &Value) -> Reply {
    match serde_json::to_vec(response) {
        Ok(payload) => Reply::ok(call, payload),
        Err(error) => Reply::error(
            call,
            Code::Internal,
            format!("encode Issues response: {error}"),
        ),
    }
}

pub fn decode_reply(call: &Call, reply: Reply) -> Result<Value, Failure> {
    reply.validate_for(call)?;
    let payload = reply.into_result()?;
    serde_json::from_slice(&payload)
        .map_err(|error| Failure::new(Code::Internal, format!("decode Issues response: {error}")))
}

fn validate_contract(call: &Call) -> Result<(), Failure> {
    if call.world() != &issues::contract::world_id() {
        return Err(Failure::new(
            Code::InvalidCall,
            format!(
                "Issues call addresses World {}, not {}",
                call.world(),
                issues::contract::world_id()
            ),
        ));
    }
    if call.operation() != OPERATION {
        return Err(Failure::new(
            Code::UnsupportedOperation,
            format!("unsupported Issues operation '{}'", call.operation()),
        ));
    }
    if call.version() != VERSION {
        return Err(Failure::new(
            Code::UnsupportedVersion,
            format!(
                "unsupported Issues protocol version {}; expected {VERSION}",
                call.version()
            ),
        ));
    }
    Ok(())
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_round_trips_without_root_control_types() {
        let request = IssuesRequest::IssueNew {
            title: "Carve the product".into(),
            project: Some("ORB".into()),
            project_hint: None,
            assignees: vec!["@me".into()],
            priority: Some("high".into()),
            labels: vec!["architecture".into()],
            body: None,
            due: None,
            estimate: Some(3),
        };
        let call = encode_call(&request).unwrap();
        assert_eq!(call.world(), &issues::contract::world_id());
        assert_eq!(call.operation(), OPERATION);
        assert!(matches!(
            decode_call(&call).unwrap(),
            IssuesRequest::IssueNew { title, .. } if title == "Carve the product"
        ));
    }

    /// A `Spec` reply must carry exactly one `kind`, and it must be the tag.
    ///
    /// `IssuesResponse` is internally tagged on `kind`, and a `SpecView` has a
    /// `kind` field of its own — a newtype variant would flatten the view beside
    /// the tag and emit the key twice. Rust survives that by reading the first
    /// occurrence; `JSON.parse` keeps the last, so the browser would decode the
    /// reply as a Spec *kind* and never recognise the response at all. The
    /// variant therefore names its payload, and this pins that it stays named.
    #[test]
    fn spec_replies_do_not_collide_with_the_response_tag() {
        let body = issues::spec::Body {
            spec: "spc_01JV0IUE".into(),
            project: "prj_01JUM4INOC41PRQOF2B082EB87".into(),
            kind: issues::spec::Kind::Requirement,
            publication: runtime::publication::PublicationId::new(
                [1; 32],
                [2; 32],
                runtime::publication::ExtractorSchemaDigest::from_digest([3; 32]),
            ),
            title: "Login is race-free".into(),
            text: String::new(),
            state: issues::spec::State::Draft,
            links: vec![],
            plan: None,
            author: "act_1".into(),
            ts: 1,
        };
        let view = issues::spec::SpecView {
            spec: body.spec.clone(),
            project: body.project.clone(),
            kind: body.kind,
            title: body.title.clone(),
            state: body.state,
            revision: "rev_1".into(),
            heads: vec!["rev_1".into()],
            issued: vec![],
            body,
        };
        let text = serde_json::to_string(&IssuesResponse::Spec {
            spec: Box::new(view),
        })
        .unwrap();
        // The reply's own keys are the tag and the payload, and nothing else.
        // Flattening would put the view's nine fields up here beside a second
        // `kind`, which is exactly the shape a browser cannot read.
        assert_eq!(text.matches("\"kind\":\"spec\"").count(), 1, "{text}");
        let json: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json.as_object().unwrap().len(), 2, "{text}");
        assert_eq!(json["kind"], "spec");
        assert_eq!(json["spec"]["kind"], "requirement");
    }

    #[test]
    fn request_classification_is_package_owned() {
        assert_eq!(
            IssuesRequest::IssueView {
                reff: "ORB-1".into()
            }
            .access(),
            Access::Query
        );
        assert_eq!(
            IssuesRequest::IssueDone {
                reff: "ORB-1".into()
            }
            .access(),
            Access::Command
        );
        assert_eq!(
            IssuesRequest::IssueTextSplice {
                reff: "ORB-1".into(),
                index: 4,
                delete: 1,
                insert: "🙂".into(),
                base_len: None,
            }
            .access(),
            Access::Command
        );
    }

    #[test]
    fn live_text_commands_keep_unicode_scalar_coordinates_on_the_wire() {
        let splice = IssuesRequest::IssueTextSplice {
            reff: "ORB-1".into(),
            index: 4,
            delete: 1,
            insert: "🙂".into(),
            base_len: Some(12),
        };
        let json = serde_json::to_value(&splice).unwrap();
        assert_eq!(json["cmd"], "issue_text_splice");
        assert_eq!(json["index"], 4);
        assert_eq!(json["delete"], 1);
        assert_eq!(json["insert"], "🙂");
        // The document the offsets were measured against travels with them.
        // Without it the World has no way to tell an offset that means what the
        // caller thought from one that addresses different text entirely.
        assert_eq!(json["base_len"], 12);
        assert!(matches!(
            serde_json::from_value(json).unwrap(),
            IssuesRequest::IssueTextSplice { index: 4, delete: 1, insert, base_len: Some(12), .. }
                if insert == "🙂"
        ));

        // A client that predates the fence still parses, and is fenced by
        // nothing — which is the point of the option, and the reason it must
        // become required once no such client remains.
        let unfenced: IssuesRequest = serde_json::from_value(serde_json::json!({
            "cmd": "issue_text_splice",
            "reff": "ORB-1",
            "index": 4,
            "delete": 1,
            "insert": "x",
        }))
        .expect("an older client's splice still parses");
        assert!(matches!(
            unfenced,
            IssuesRequest::IssueTextSplice { base_len: None, .. }
        ));

        let checkpoint = serde_json::to_value(IssuesRequest::IssueTextCheckpoint {
            reff: "ORB-1".into(),
        })
        .unwrap();
        assert_eq!(checkpoint["cmd"], "issue_text_checkpoint");

        let upgrade = serde_json::to_value(IssuesRequest::IssueDocumentUpgrade {
            reff: "ORB-1".into(),
            expected: "# old".into(),
            splices: vec![DocumentSplice {
                index: 0,
                delete: 1,
                insert: "=".into(),
            }],
        })
        .unwrap();
        assert_eq!(upgrade["cmd"], "issue_document_upgrade");
        assert_eq!(upgrade["splices"][0]["index"], 0);
        assert_eq!(upgrade["splices"][0]["insert"], "=");

        let spec_upgrade = serde_json::to_value(IssuesRequest::SpecDocumentUpgrade {
            spec: "spc_00000000000000000000000000".into(),
            expected: "spr_0000000000000000000000000000000000000000000000000000".into(),
            text: "// lait-document:1\n= Current".into(),
        })
        .unwrap();
        assert_eq!(spec_upgrade["cmd"], "spec_document_upgrade");
        assert_eq!(spec_upgrade["text"], "// lait-document:1\n= Current");
    }
}
