//! Versioned application protocol carried inside an opaque [`Call`].

use runtime::world::call::{Access, Call, Code, Failure, Reply};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const OPERATION: &str = "issues.control";
pub const VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "at", rename_all = "snake_case")]
pub enum BoardPos {
    Top,
    Bottom,
    Before { reff: String },
    After { reff: String },
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

/// Issues-owned application requests.
///
/// The tagged JSON representation is also the Issues web-client contract. All
/// native product clients carry this type inside a [`Call`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IssuesRequest {
    /// Project the acting identity's inbox using the caller-local read
    /// watermark. Advancing that watermark remains a client-host facility.
    Inbox {
        #[serde(default)]
        watermark: u64,
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
    IssueGraph {
        reff: String,
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
    IssueView {
        reff: String,
    },
    List {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        filter: Filter,
    },
    Board {
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        project_hint: Option<String>,
    },
    History {
        reff: String,
    },
    ProjectNew {
        name: String,
        key: String,
        #[serde(default)]
        color: Option<String>,
    },
    ProjectList,
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
    InitiativeList,
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
    TeamList,
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
    TriageList,
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
    LabelList,
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
        #[serde(default)]
        since: u64,
    },
    RoleList,
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
    },
    SpecShow {
        spec: String,
    },
    SpecHistory {
        spec: String,
    },
    SpecReferences {
        #[serde(default)]
        project: Option<String>,
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
    BaselineList {
        #[serde(default)]
        project: Option<String>,
    },
    BaselineShow {
        baseline: String,
    },
    BaselineHistory {
        baseline: String,
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
    Ok {
        message: Option<String>,
    },
    Ref {
        reff: String,
    },
    Issue(Box<issues::dto::IssueView>),
    List {
        rows: Vec<issues::dto::Row>,
    },
    Board(Box<issues::dto::BoardView>),
    Graph(Box<issues::dto::GraphView>),
    Activity {
        events: Vec<issues::dto::ActivityEvent>,
        last: u64,
    },
    Inbox {
        entries: Vec<issues::dto::InboxEntry>,
        unread: u64,
    },
    AccessPlan {
        assignments: Vec<AccessAssignment>,
    },
    Projects {
        projects: Vec<issues::dto::ProjectDto>,
    },
    Updates {
        updates: Vec<issues::dto::ProjectUpdateDto>,
    },
    Labels {
        labels: Vec<issues::dto::LabelDto>,
    },
    Milestones {
        milestones: Vec<issues::dto::MilestoneDto>,
    },
    Cycles {
        cycles: Vec<issues::dto::CycleDto>,
    },
    Initiatives {
        initiatives: Vec<issues::dto::InitiativeDto>,
    },
    Teams {
        teams: Vec<issues::dto::TeamDto>,
    },
    TriageItems {
        items: Vec<issues::dto::TriageDto>,
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
        revisions: Vec<issues::spec::Revision>,
    },
    SpecReferences {
        references: Vec<issues::spec::SpecReference>,
    },
    BaselineRevisions {
        revisions: Vec<issues::spec::BaselineRevision>,
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
        specs: Vec<issues::spec::SpecView>,
    },
    Baseline(Box<issues::spec::BaselineView>),
    Baselines {
        baselines: Vec<issues::spec::BaselineView>,
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
    NotFound,
    Denied,
    Retry,
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
            Self::NotFound => world_interface::Failure::Invalid,
            Self::Denied => world_interface::Failure::Refusal,
            Self::Error | Self::Retry => world_interface::Failure::Operation,
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
        "not_found" => IssuesErrorKind::NotFound,
        "denied" => IssuesErrorKind::Denied,
        "retry" => IssuesErrorKind::Retry,
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

    pub fn retry(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
            error_kind: IssuesErrorKind::Retry,
        }
    }
}

impl IssuesRequest {
    pub fn access(&self) -> Access {
        use IssuesRequest::*;
        match self {
            Inbox { .. }
            | AccessPlan { .. }
            | IssueGraph { .. }
            | IssueView { .. }
            | List { .. }
            | Board { .. }
            | History { .. }
            | ProjectList
            | ProjectUpdates { .. }
            | MilestoneList { .. }
            | CycleList { .. }
            | InitiativeList
            | TeamList
            | TriageList
            | AttachmentGet { .. }
            | LabelList
            | Activity { .. }
            | RoleList
            | RoleShow { .. }
            | WorkflowShow { .. }
            | WorkflowValidate { .. }
            | SpecList { .. }
            | SpecShow { .. }
            | SpecHistory { .. }
            | SpecReferences { .. }
            | BaselineList { .. }
            | BaselineShow { .. }
            | BaselineHistory { .. }
            | Packet { .. } => Access::Query,
            IssueNew { .. }
            | IssueEdit { .. }
            | IssueTextSplice { .. }
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
            | SpecState { .. }
            | SpecResolve { .. }
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
            title: "Login is race-free".into(),
            text: String::new(),
            state: issues::spec::State::Draft,
            links: vec![],
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
        };
        let json = serde_json::to_value(&splice).unwrap();
        assert_eq!(json["cmd"], "issue_text_splice");
        assert_eq!(json["index"], 4);
        assert_eq!(json["delete"], 1);
        assert_eq!(json["insert"], "🙂");
        assert!(matches!(
            serde_json::from_value(json).unwrap(),
            IssuesRequest::IssueTextSplice { index: 4, delete: 1, insert, .. }
                if insert == "🙂"
        ));

        let checkpoint = serde_json::to_value(IssuesRequest::IssueTextCheckpoint {
            reff: "ORB-1".into(),
        })
        .unwrap();
        assert_eq!(checkpoint["cmd"], "issue_text_checkpoint");
    }
}
