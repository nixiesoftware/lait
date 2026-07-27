//! Issues-owned MCP tools.

use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use world_interface::{CliInvocation, InterfaceError, McpTool};

use crate::cli::{LOCAL_ACCESS, LOCAL_INBOX};
use crate::{encode_call, BoardPos, Filter, IssuesRequest};

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct EmptyArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueNewArgs {
    title: String,
    #[serde(default)]
    project: Option<String>,
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct InboxArgs {
    #[serde(default)]
    clear: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RefArgs {
    reff: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueEditArgs {
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueMoveArgs {
    reff: String,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    position: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AssignArgs {
    reff: String,
    who: Vec<String>,
    #[serde(default)]
    remove: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LabelArgs {
    reff: String,
    #[serde(default)]
    add: Vec<String>,
    #[serde(default)]
    remove: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CommentArgs {
    reff: String,
    body: String,
    #[serde(default)]
    reply_to: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReactArgs {
    reff: String,
    comment: String,
    emoji: String,
    #[serde(default)]
    remove: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LinkArgs {
    reff: String,
    kind: String,
    target: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ParentArgs {
    reff: String,
    #[serde(default)]
    parent: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ListArgs {
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    mine: bool,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BoardArgs {
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectNewArgs {
    name: String,
    key: String,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LabelNewArgs {
    name: String,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ActivityArgs {
    #[serde(default)]
    since: u64,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RoleShowArgs {
    role: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RoleCreateArgs {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    project: Option<String>,
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RoleEditArgs {
    role: String,
    expect_revision: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    capabilities: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RoleDeleteArgs {
    role: String,
    expect_revision: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RoleResolveArgs {
    role: String,
    expect_heads: Vec<String>,
    body_json: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AccessListArgs {
    #[serde(default)]
    actor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AccessGrantArgs {
    actor: String,
    role: String,
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AccessRevokeArgs {
    grant_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkflowShowArgs {
    project: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkflowValidateArgs {
    body_json: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WorkflowSetArgs {
    project: String,
    expect_heads: Vec<String>,
    body_json: String,
}

pub fn tools() -> Vec<McpTool> {
    vec![
        tool::<IssueNewArgs>(
            "new",
            "Create an issue. Returns the resolved canonical handle.",
            issue_new,
        ),
        tool::<RefArgs>(
            "start",
            "Assign yourself and move an issue to its active state.",
            issue_start,
        ),
        tool::<RefArgs>("done", "Move an issue to its done state.", issue_done),
        tool::<RefArgs>(
            "stop",
            "Return an issue to backlog and unassign yourself.",
            issue_stop,
        ),
        tool::<InboxArgs>("inbox", "Read or clear the durable inbox.", inbox),
        tool::<IssueEditArgs>("edit", "Edit issue fields.", issue_edit),
        tool::<IssueMoveArgs>(
            "move",
            "Move an issue to another project or board position.",
            issue_move,
        ),
        tool::<AssignArgs>("assign", "Add or remove issue assignees.", assign),
        tool::<LabelArgs>("label", "Add or remove labels on an issue.", label),
        tool::<CommentArgs>("comment", "Append an immutable comment.", comment),
        tool::<ReactArgs>("react", "Toggle a reaction on a comment.", react),
        tool::<RefArgs>("delete", "Tombstone an issue.", issue_delete),
        tool::<RefArgs>("restore", "Restore a deleted issue.", issue_restore),
        tool::<LinkArgs>("link", "Link two issues.", issue_link),
        tool::<LinkArgs>("unlink", "Remove an issue link.", issue_unlink),
        tool::<ParentArgs>("parent", "Set or clear an issue parent.", issue_parent),
        tool::<RefArgs>("graph", "Read an issue graph neighborhood.", issue_graph),
        tool::<RefArgs>("view", "Read a full issue.", issue_view),
        tool::<ListArgs>("list", "List issue rows.", list),
        tool::<BoardArgs>("board", "Render a project board.", board),
        tool::<RefArgs>("history", "Read an issue's history.", history),
        tool::<ProjectNewArgs>("project_new", "Create a project.", project_new),
        tool::<EmptyArgs>("project_list", "List projects.", project_list),
        tool::<LabelNewArgs>("label_new", "Create a label.", label_new),
        tool::<EmptyArgs>("label_list", "List labels.", label_list),
        tool::<ActivityArgs>("activity", "Read recent IssuesWorld transitions.", activity),
        tool::<EmptyArgs>("role_list", "List role definitions.", role_list),
        tool::<RoleShowArgs>("role_show", "Read one role definition.", role_show),
        tool::<RoleCreateArgs>("role_create", "Create a custom role.", role_create),
        tool::<RoleEditArgs>("role_edit", "Edit a custom role.", role_edit),
        tool::<RoleDeleteArgs>("role_delete", "Delete a custom role.", role_delete),
        tool::<RoleResolveArgs>(
            "role_resolve",
            "Resolve concurrent custom-role heads.",
            role_resolve,
        ),
        tool::<AccessListArgs>(
            "access_list",
            "List effective scoped assignments.",
            access_list,
        ),
        tool::<AccessGrantArgs>(
            "access_grant",
            "Grant a pinned role to an actor.",
            access_grant,
        ),
        tool::<AccessRevokeArgs>(
            "access_revoke",
            "Revoke an effective assignment.",
            access_revoke,
        ),
        tool::<WorkflowShowArgs>("workflow_show", "Read a project's workflow.", workflow_show),
        tool::<WorkflowValidateArgs>(
            "workflow_validate",
            "Validate a canonical workflow body.",
            workflow_validate,
        ),
        tool::<WorkflowSetArgs>(
            "workflow_set",
            "Replace a project's workflow.",
            workflow_set,
        ),
    ]
}

fn tool<T: JsonSchema>(
    name: &'static str,
    description: &'static str,
    call: fn(Value) -> Result<CliInvocation, InterfaceError>,
) -> McpTool {
    McpTool::new(name, description, schema::<T>, call)
}

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("Issues MCP schemas are JSON serializable")
}

fn args<T: DeserializeOwned>(input: Value) -> Result<T, InterfaceError> {
    serde_json::from_value(input)
        .map_err(|error| InterfaceError::new(format!("invalid tool arguments: {error}")))
}

fn world(request: IssuesRequest) -> Result<CliInvocation, InterfaceError> {
    encode_call(&request)
        .map(CliInvocation::World)
        .map_err(|error| InterfaceError::new(error.to_string()))
}

fn local(operation: &str, input: Value) -> Result<CliInvocation, InterfaceError> {
    Ok(CliInvocation::Local {
        operation: operation.to_string(),
        input,
    })
}

fn issue_new(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: IssueNewArgs = args(input)?;
    world(IssuesRequest::IssueNew {
        title: a.title,
        project: a.project,
        project_hint: None,
        assignees: a.assignees,
        priority: a.priority,
        labels: a.labels,
        body: a.body,
        due: a.due,
        estimate: a.estimate,
    })
}

fn issue_start(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueStart { reff: a.reff })
}

fn issue_done(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueDone { reff: a.reff })
}

fn issue_stop(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueStop { reff: a.reff })
}

fn inbox(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: InboxArgs = args(input)?;
    local(LOCAL_INBOX, json!({ "clear": a.clear }))
}

fn issue_edit(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: IssueEditArgs = args(input)?;
    world(IssuesRequest::IssueEdit {
        reff: a.reff,
        title: a.title,
        status: a.status,
        priority: a.priority,
        description: a.description,
        due: a.due,
        estimate: a.estimate,
    })
}

fn issue_move(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: IssueMoveArgs = args(input)?;
    world(IssuesRequest::IssueMove {
        reff: a.reff,
        project: a.project,
        pos: a.position.as_deref().and_then(parse_position),
    })
}

fn assign(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: AssignArgs = args(input)?;
    world(IssuesRequest::Assign {
        reff: a.reff,
        who: a.who,
        add: !a.remove,
    })
}

fn label(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: LabelArgs = args(input)?;
    world(IssuesRequest::Label {
        reff: a.reff,
        add: a.add,
        remove: a.remove,
    })
}

fn comment(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: CommentArgs = args(input)?;
    world(IssuesRequest::Comment {
        reff: a.reff,
        body: a.body,
        reply_to: a.reply_to,
    })
}

fn react(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: ReactArgs = args(input)?;
    world(IssuesRequest::React {
        reff: a.reff,
        comment: a.comment,
        emoji: a.emoji,
        on: !a.remove,
    })
}

fn issue_delete(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueDelete { reff: a.reff })
}

fn issue_restore(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueRestore { reff: a.reff })
}

fn issue_link(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: LinkArgs = args(input)?;
    world(IssuesRequest::IssueLink {
        reff: a.reff,
        kind: a.kind,
        target: a.target,
    })
}

fn issue_unlink(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: LinkArgs = args(input)?;
    world(IssuesRequest::IssueUnlink {
        reff: a.reff,
        kind: a.kind,
        target: a.target,
    })
}

fn issue_parent(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: ParentArgs = args(input)?;
    world(IssuesRequest::IssueParent {
        reff: a.reff,
        parent: a.parent,
    })
}

fn issue_graph(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueGraph { reff: a.reff })
}

fn issue_view(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueView { reff: a.reff })
}

fn list(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: ListArgs = args(input)?;
    world(IssuesRequest::List {
        project: a.project,
        filter: Filter {
            mine: a.mine,
            status: a.status,
            label: a.label,
            all: a.all,
        },
    })
}

fn board(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: BoardArgs = args(input)?;
    world(IssuesRequest::Board {
        project: a.project,
        project_hint: None,
    })
}

fn history(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::History { reff: a.reff })
}

fn project_new(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: ProjectNewArgs = args(input)?;
    world(IssuesRequest::ProjectNew {
        name: a.name,
        key: a.key,
        color: a.color,
    })
}

fn project_list(input: Value) -> Result<CliInvocation, InterfaceError> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::ProjectList)
}

fn label_new(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: LabelNewArgs = args(input)?;
    world(IssuesRequest::LabelNew {
        name: a.name,
        color: a.color,
    })
}

fn label_list(input: Value) -> Result<CliInvocation, InterfaceError> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::LabelList)
}

fn activity(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: ActivityArgs = args(input)?;
    world(IssuesRequest::Activity { since: a.since })
}

fn role_list(input: Value) -> Result<CliInvocation, InterfaceError> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::RoleList)
}

fn role_show(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RoleShowArgs = args(input)?;
    world(IssuesRequest::RoleShow { role: a.role })
}

fn role_create(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RoleCreateArgs = args(input)?;
    world(IssuesRequest::RoleCreate {
        name: a.name,
        description: a.description,
        project: a.project,
        capabilities: a.capabilities,
    })
}

fn role_edit(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RoleEditArgs = args(input)?;
    world(IssuesRequest::RoleEdit {
        role: a.role,
        expect_revision: a.expect_revision,
        name: a.name,
        description: a.description,
        capabilities: a.capabilities,
    })
}

fn role_delete(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RoleDeleteArgs = args(input)?;
    world(IssuesRequest::RoleDelete {
        role: a.role,
        expect_revision: a.expect_revision,
    })
}

fn role_resolve(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: RoleResolveArgs = args(input)?;
    world(IssuesRequest::RoleResolve {
        role: a.role,
        expect_heads: a.expect_heads,
        body_json: a.body_json,
    })
}

fn access_list(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: AccessListArgs = args(input)?;
    local(LOCAL_ACCESS, json!({ "action": "ls", "actor": a.actor }))
}

fn access_grant(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: AccessGrantArgs = args(input)?;
    local(
        LOCAL_ACCESS,
        json!({
            "action": "grant",
            "actor": a.actor,
            "role": a.role,
            "project": a.project,
        }),
    )
}

fn access_revoke(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: AccessRevokeArgs = args(input)?;
    local(
        LOCAL_ACCESS,
        json!({ "action": "revoke", "grant_id": a.grant_id }),
    )
}

fn workflow_show(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: WorkflowShowArgs = args(input)?;
    world(IssuesRequest::WorkflowShow { project: a.project })
}

fn workflow_validate(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: WorkflowValidateArgs = args(input)?;
    world(IssuesRequest::WorkflowValidate {
        body_json: a.body_json,
    })
}

fn workflow_set(input: Value) -> Result<CliInvocation, InterfaceError> {
    let a: WorkflowSetArgs = args(input)?;
    world(IssuesRequest::WorkflowSet {
        project: a.project,
        expect_heads: a.expect_heads,
        body_json: a.body_json,
    })
}

fn parse_position(value: &str) -> Option<BoardPos> {
    match value {
        "top" => Some(BoardPos::Top),
        "bottom" => Some(BoardPos::Bottom),
        value => value
            .strip_prefix("before:")
            .map(|reff| BoardPos::Before { reff: reff.into() })
            .or_else(|| {
                value
                    .strip_prefix("after:")
                    .map(|reff| BoardPos::After { reff: reff.into() })
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_are_package_local_and_emit_world_calls() {
        let tools = tools();
        assert_eq!(tools.len(), 38);
        assert!(tools.iter().all(|tool| !tool.name().starts_with("issues_")));
        let invocation = tools
            .iter()
            .find(|tool| tool.name() == "view")
            .unwrap()
            .call(json!({ "reff": "ENG-1" }))
            .unwrap();
        assert!(matches!(invocation, CliInvocation::World(_)));
    }
}
