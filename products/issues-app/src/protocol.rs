//! Versioned application protocol carried inside an opaque [`WorldCall`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use world_bridge::{WorldCall, WorldCallAccess, WorldCallError, WorldCallErrorCode, WorldReply};

pub const OPERATION: &str = "issues.control";
pub const VERSION: u32 = 1;

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
    #[serde(default)]
    pub all: bool,
}

/// Issues-owned application requests.
///
/// The tagged JSON representation is also the Issues web-client contract. All
/// native product clients carry this type inside a [`WorldCall`].
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum IssuesRequest {
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
        #[serde(default)]
        target: Option<String>,
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
        data_b64: String,
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
}

/// Issues-owned application responses.
///
/// The root control protocol may deserialize this JSON into its wider
/// compatibility response enum, but execution never depends on that enum.
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
        data_b64: String,
    },
    Text {
        text: String,
    },
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
}

impl IssuesRequest {
    pub fn access(&self) -> WorldCallAccess {
        use IssuesRequest::*;
        match self {
            IssueGraph { .. }
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
            | WorkflowValidate { .. } => WorldCallAccess::Query,
            _ => WorldCallAccess::Command,
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

pub fn encode_call(request: &IssuesRequest) -> Result<WorldCall, WorldCallError> {
    let payload = serde_json::to_vec(request).map_err(|error| {
        WorldCallError::new(
            WorldCallErrorCode::InvalidCall,
            format!("encode Issues request: {error}"),
        )
    })?;
    WorldCall::new(issues::contract::world_id(), OPERATION, VERSION, payload)
}

pub fn decode_call(call: &WorldCall) -> Result<IssuesRequest, WorldCallError> {
    validate_contract(call)?;
    serde_json::from_slice(call.payload()).map_err(|error| {
        WorldCallError::new(
            WorldCallErrorCode::InvalidCall,
            format!("decode Issues request: {error}"),
        )
    })
}

pub fn encode_reply(call: &WorldCall, response: &Value) -> WorldReply {
    match serde_json::to_vec(response) {
        Ok(payload) => WorldReply::ok(call, payload),
        Err(error) => WorldReply::error(
            call,
            WorldCallErrorCode::Internal,
            format!("encode Issues response: {error}"),
        ),
    }
}

pub fn decode_reply(call: &WorldCall, reply: WorldReply) -> Result<Value, WorldCallError> {
    reply.validate_for(call)?;
    let payload = reply.into_result()?;
    serde_json::from_slice(&payload).map_err(|error| {
        WorldCallError::new(
            WorldCallErrorCode::Internal,
            format!("decode Issues response: {error}"),
        )
    })
}

fn validate_contract(call: &WorldCall) -> Result<(), WorldCallError> {
    if call.world() != &issues::contract::world_id() {
        return Err(WorldCallError::new(
            WorldCallErrorCode::InvalidCall,
            format!(
                "Issues call addresses World {}, not {}",
                call.world(),
                issues::contract::world_id()
            ),
        ));
    }
    if call.operation() != OPERATION {
        return Err(WorldCallError::new(
            WorldCallErrorCode::UnsupportedOperation,
            format!("unsupported Issues operation '{}'", call.operation()),
        ));
    }
    if call.version() != VERSION {
        return Err(WorldCallError::new(
            WorldCallErrorCode::UnsupportedVersion,
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

    #[test]
    fn request_classification_is_package_owned() {
        assert_eq!(
            IssuesRequest::IssueView {
                reff: "ORB-1".into()
            }
            .access(),
            WorldCallAccess::Query
        );
        assert_eq!(
            IssuesRequest::IssueDone {
                reff: "ORB-1".into()
            }
            .access(),
            WorldCallAccess::Command
        );
    }
}
