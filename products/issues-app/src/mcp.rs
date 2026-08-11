#![allow(
    clippy::expect_used,
    reason = "MCP schemas use derived serialization over static bounded tool descriptors"
)]
//! Issues-owned MCP tools.

use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use world_interface::{ClientInvocation, Failure, McpTool};

use crate::host::{LOCAL_ACCESS, LOCAL_ATTACH, LOCAL_ATTACHMENT_GET, LOCAL_INBOX};
use crate::{BoardPos, Filter, IssuesRequest};

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
struct CommentAtArgs {
    reff: String,
    body: String,
    /// The collaborative text field the span lies in — `description`.
    field: String,
    /// The span's start, counted in Unicode scalars. An agent reading the issue
    /// as JSON counts the same way; a browser client counts UTF-16 code units
    /// and must convert.
    start: u64,
    /// The span's end. Absent names a position rather than a span.
    #[serde(default)]
    end: Option<u64>,
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
    /// Milestone name or `mls_` id. Requires `project` — a milestone belongs to
    /// exactly one, so there is nothing to resolve the name against without it.
    #[serde(default)]
    milestone: Option<String>,
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
    /// Opaque resume token from a previous call's `last`; omit for the whole
    /// feed.
    #[serde(default)]
    since: Option<String>,
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

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectArgs {
    #[serde(default)]
    project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpecArgs {
    spec: String,
}

/// Unlike [`ProjectArgs`], the project is required: a dependency graph is a
/// property of one project, and there is no sensible whole-space default.
#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectGraphArgs {
    project: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpecNewArgs {
    project: String,
    kind: issues::spec::Kind,
    title: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    links: Vec<issues::spec::Link>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpecObserveArgs {
    /// The Spec the note is filed against.
    spec: String,
    rel: issues::spec::Rel,
    target: issues::spec::Target,
    /// Why you think so. An observation with no argument behind it is a claim
    /// nobody can weigh.
    #[serde(default)]
    note: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpecRetractArgs {
    spec: String,
    observation: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpecReviseArgs {
    spec: String,
    expected: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    links: Option<Vec<issues::spec::Link>>,
    #[serde(default)]
    plan: Option<Option<issues::spec::PlanData>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpecStateArgs {
    spec: String,
    expected: String,
    state: issues::spec::State,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ResolveArgs {
    id: String,
    expected_heads: Vec<String>,
    body_json: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BaselineArgs {
    baseline: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BaselineNewArgs {
    project: String,
    name: String,
    members: Vec<issues::spec::SpecRef>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BaselineReviseArgs {
    baseline: String,
    expected: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    members: Option<Vec<issues::spec::SpecRef>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BaselineStateArgs {
    baseline: String,
    expected: String,
    state: issues::spec::State,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueBaselineArgs {
    reff: String,
    #[serde(default)]
    baseline: Option<issues::spec::BaselineRef>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AttachFileArgs {
    reff: String,
    /// Path on the machine this tool runs on.
    file: String,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AttachmentSaveArgs {
    reff: String,
    id: String,
    /// Where to write it. Defaults to the attachment's own name, sanitized.
    #[serde(default)]
    out: Option<String>,
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
        tool::<CommentAtArgs>(
            "comment_at",
            "Comment on a span of an issue's description.",
            comment_at,
        ),
        tool::<ReactArgs>("react", "Toggle a reaction on a comment.", react),
        tool::<RefArgs>("delete", "Tombstone an issue.", issue_delete),
        tool::<RefArgs>("restore", "Restore a deleted issue.", issue_restore),
        tool::<LinkArgs>("link", "Link two issues.", issue_link),
        tool::<LinkArgs>("unlink", "Remove an issue link.", issue_unlink),
        tool::<ParentArgs>("parent", "Set or clear an issue parent.", issue_parent),
        tool::<RefArgs>("graph", "Read an issue graph neighborhood.", issue_graph),
        tool::<ProjectGraphArgs>(
            "project_graph",
            "Read a project's whole dependency graph — every blocks/relates edge and parent link between its live issues, in one call. Use this to work out what is unblocked and in what order work has to happen; `graph` answers the same question for a single issue.",
            project_graph,
        ),
        tool::<RefArgs>("view", "Read a full issue.", issue_view),
        tool::<ListArgs>("list", "List issue rows.", list),
        tool::<BoardArgs>("board", "Render a project board.", board),
        tool::<RefArgs>("history", "Read an issue's history.", history),
        tool::<EmptyArgs>(
            "structure_status",
            "Audit current Blueprint records that still depend on compatibility readers.",
            structure_status,
        ),
        tool::<EmptyArgs>(
            "structure_migrate",
            "Materialize current Blueprint topology and Spec heads into native structures.",
            structure_migrate,
        ),
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
        tool::<ProjectArgs>("spec_list", "List Specs, optionally by project.", spec_list),
        tool::<SpecArgs>("spec_show", "Read one versioned Spec.", spec_show),
        tool::<ProjectArgs>(
            "spec_links",
            "Every typed link asserted in scope, with the standing of the revision asserting it.",
            spec_links,
        ),
        tool::<SpecArgs>(
            "spec_history",
            "Every revision of one Spec, oldest first, with its predecessors.",
            spec_history,
        ),
        tool::<SpecNewArgs>("spec_new", "Create a draft Spec.", spec_new),
        tool::<SpecReviseArgs>("spec_revise", "Create a draft Spec successor.", spec_revise),
        tool::<SpecStateArgs>(
            "spec_state",
            "Review, issue, or withdraw a Spec head.",
            spec_state,
        ),
        tool::<ResolveArgs>(
            "spec_resolve",
            "Resolve concurrent Spec heads.",
            spec_resolve,
        ),
        tool::<ProjectArgs>(
            "spec_observations",
            "Every observation filed in scope — notes about the graph that bind \
             nobody's document and never govern anything.",
            spec_observations,
        ),
        tool::<SpecObserveArgs>(
            "spec_observe",
            "Note something about this document and another — a conflict, a \
             dependency, coverage nobody had connected. Not a claim the document \
             makes: it enters no revision, is not issued with it, and never \
             reaches an issue's packet. Assert it as a link instead when the \
             document itself should say it.",
            spec_observe,
        ),
        tool::<SpecRetractArgs>(
            "spec_retract",
            "Withdraw one observation. Your own needs write; anyone else's needs \
             the project's issuing capability.",
            spec_retract,
        ),
        tool::<ProjectArgs>(
            "baseline_list",
            "List Baselines, optionally by project.",
            baseline_list,
        ),
        tool::<BaselineArgs>("baseline_show", "Read one Baseline.", baseline_show),
        tool::<BaselineArgs>(
            "baseline_history",
            "Every revision of one Baseline, oldest first.",
            baseline_history,
        ),
        tool::<BaselineNewArgs>(
            "baseline_new",
            "Create a draft Baseline of exact Spec revisions.",
            baseline_new,
        ),
        tool::<BaselineReviseArgs>(
            "baseline_revise",
            "Create a draft Baseline successor.",
            baseline_revise,
        ),
        tool::<BaselineStateArgs>(
            "baseline_state",
            "Review, issue, or withdraw a Baseline head.",
            baseline_state,
        ),
        tool::<ResolveArgs>(
            "baseline_resolve",
            "Resolve concurrent Baseline heads.",
            baseline_resolve,
        ),
        tool::<IssueBaselineArgs>(
            "issue_baseline",
            "Pin or clear an exact issued Baseline on an Issue.",
            issue_baseline,
        ),
        tool::<RefArgs>(
            "packet",
            "Read the effective deterministic Spec packet for an Issue.",
            packet,
        ),
        tool::<AttachFileArgs>(
            "attach_file",
            "Attach a file from this machine's filesystem to an issue. The file \
             is streamed onto the content plane, never read into memory.",
            attach_file,
        ),
        tool::<AttachmentSaveArgs>(
            "attachment_save",
            "Save one of an issue's attachments to a local path.",
            attachment_save,
        ),
    ]
}

fn tool<T: JsonSchema>(
    name: &'static str,
    description: &'static str,
    call: fn(Value) -> Result<ClientInvocation, Failure>,
) -> McpTool {
    McpTool::new(name, description, schema::<T>, call)
}

fn schema<T: JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T))
        .expect("Issues MCP schemas are JSON serializable")
}

fn args<T: DeserializeOwned>(input: Value) -> Result<T, Failure> {
    serde_json::from_value(input)
        .map_err(|error| Failure::new(format!("invalid tool arguments: {error}")))
}

fn world(request: IssuesRequest) -> Result<ClientInvocation, Failure> {
    crate::host::world_invocation(request)
}

fn local(operation: &str, input: Value) -> Result<ClientInvocation, Failure> {
    crate::host::invocation(operation, input)
}

fn issue_new(input: Value) -> Result<ClientInvocation, Failure> {
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

fn issue_start(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueStart { reff: a.reff })
}

fn issue_done(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueDone { reff: a.reff })
}

fn issue_stop(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueStop { reff: a.reff })
}

fn inbox(input: Value) -> Result<ClientInvocation, Failure> {
    let a: InboxArgs = args(input)?;
    local(LOCAL_INBOX, json!({ "clear": a.clear }))
}

fn issue_edit(input: Value) -> Result<ClientInvocation, Failure> {
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

fn issue_move(input: Value) -> Result<ClientInvocation, Failure> {
    let a: IssueMoveArgs = args(input)?;
    world(IssuesRequest::IssueMove {
        reff: a.reff,
        project: a.project,
        pos: a.position.as_deref().and_then(parse_position),
    })
}

fn assign(input: Value) -> Result<ClientInvocation, Failure> {
    let a: AssignArgs = args(input)?;
    world(IssuesRequest::Assign {
        reff: a.reff,
        who: a.who,
        add: !a.remove,
    })
}

fn label(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LabelArgs = args(input)?;
    world(IssuesRequest::Label {
        reff: a.reff,
        add: a.add,
        remove: a.remove,
    })
}

fn comment(input: Value) -> Result<ClientInvocation, Failure> {
    let a: CommentArgs = args(input)?;
    world(IssuesRequest::Comment {
        reff: a.reff,
        body: a.body,
        reply_to: a.reply_to,
    })
}

fn comment_at(input: Value) -> Result<ClientInvocation, Failure> {
    let a: CommentAtArgs = args(input)?;
    world(IssuesRequest::CommentAt {
        reff: a.reff,
        body: a.body,
        field: a.field,
        start: a.start,
        end: a.end,
        reply_to: a.reply_to,
    })
}

fn react(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ReactArgs = args(input)?;
    world(IssuesRequest::React {
        reff: a.reff,
        comment: a.comment,
        emoji: a.emoji,
        on: !a.remove,
    })
}

fn issue_delete(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueDelete { reff: a.reff })
}

fn issue_restore(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueRestore { reff: a.reff })
}

fn issue_link(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LinkArgs = args(input)?;
    world(IssuesRequest::IssueLink {
        reff: a.reff,
        kind: a.kind,
        target: a.target,
    })
}

fn issue_unlink(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LinkArgs = args(input)?;
    world(IssuesRequest::IssueUnlink {
        reff: a.reff,
        kind: a.kind,
        target: a.target,
    })
}

fn issue_parent(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ParentArgs = args(input)?;
    world(IssuesRequest::IssueParent {
        reff: a.reff,
        parent: a.parent,
    })
}

fn issue_graph(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueGraph { reff: a.reff })
}

fn project_graph(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ProjectGraphArgs = args(input)?;
    world(IssuesRequest::ProjectGraph { project: a.project })
}

fn issue_view(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueView { reff: a.reff })
}

fn list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ListArgs = args(input)?;
    world(IssuesRequest::List {
        project: a.project,
        filter: Filter {
            mine: a.mine,
            status: a.status,
            label: a.label,
            milestone: a.milestone,
            all: a.all,
        },
    })
}

fn board(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BoardArgs = args(input)?;
    world(IssuesRequest::Board {
        project: a.project,
        project_hint: None,
    })
}

fn history(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::History { reff: a.reff })
}

fn structure_status(input: Value) -> Result<ClientInvocation, Failure> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::StructureStatus)
}

fn structure_migrate(input: Value) -> Result<ClientInvocation, Failure> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::StructureMigrate)
}

fn project_new(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ProjectNewArgs = args(input)?;
    world(IssuesRequest::ProjectNew {
        name: a.name,
        key: a.key,
        color: a.color,
    })
}

fn project_list(input: Value) -> Result<ClientInvocation, Failure> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::ProjectList)
}

fn label_new(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LabelNewArgs = args(input)?;
    world(IssuesRequest::LabelNew {
        name: a.name,
        color: a.color,
    })
}

fn label_list(input: Value) -> Result<ClientInvocation, Failure> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::LabelList)
}

fn activity(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ActivityArgs = args(input)?;
    world(IssuesRequest::Activity { since: a.since })
}

fn role_list(input: Value) -> Result<ClientInvocation, Failure> {
    let _: EmptyArgs = args(input)?;
    world(IssuesRequest::RoleList)
}

fn role_show(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RoleShowArgs = args(input)?;
    world(IssuesRequest::RoleShow { role: a.role })
}

fn role_create(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RoleCreateArgs = args(input)?;
    world(IssuesRequest::RoleCreate {
        name: a.name,
        description: a.description,
        project: a.project,
        capabilities: a.capabilities,
    })
}

fn role_edit(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RoleEditArgs = args(input)?;
    world(IssuesRequest::RoleEdit {
        role: a.role,
        expect_revision: a.expect_revision,
        name: a.name,
        description: a.description,
        capabilities: a.capabilities,
    })
}

fn role_delete(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RoleDeleteArgs = args(input)?;
    world(IssuesRequest::RoleDelete {
        role: a.role,
        expect_revision: a.expect_revision,
    })
}

fn role_resolve(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RoleResolveArgs = args(input)?;
    world(IssuesRequest::RoleResolve {
        role: a.role,
        expect_heads: a.expect_heads,
        body_json: a.body_json,
    })
}

fn access_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: AccessListArgs = args(input)?;
    local(LOCAL_ACCESS, json!({ "action": "ls", "actor": a.actor }))
}

fn access_grant(input: Value) -> Result<ClientInvocation, Failure> {
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

fn access_revoke(input: Value) -> Result<ClientInvocation, Failure> {
    let a: AccessRevokeArgs = args(input)?;
    local(
        LOCAL_ACCESS,
        json!({ "action": "revoke", "grant_id": a.grant_id }),
    )
}

fn workflow_show(input: Value) -> Result<ClientInvocation, Failure> {
    let a: WorkflowShowArgs = args(input)?;
    world(IssuesRequest::WorkflowShow { project: a.project })
}

fn workflow_validate(input: Value) -> Result<ClientInvocation, Failure> {
    let a: WorkflowValidateArgs = args(input)?;
    world(IssuesRequest::WorkflowValidate {
        body_json: a.body_json,
    })
}

fn workflow_set(input: Value) -> Result<ClientInvocation, Failure> {
    let a: WorkflowSetArgs = args(input)?;
    world(IssuesRequest::WorkflowSet {
        project: a.project,
        expect_heads: a.expect_heads,
        body_json: a.body_json,
    })
}

fn spec_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ProjectArgs = args(input)?;
    world(IssuesRequest::SpecList { project: a.project })
}

fn spec_show(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecArgs = args(input)?;
    world(IssuesRequest::SpecShow { spec: a.spec })
}

fn spec_links(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ProjectArgs = args(input)?;
    world(IssuesRequest::SpecReferences { project: a.project })
}

fn spec_history(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecArgs = args(input)?;
    world(IssuesRequest::SpecHistory { spec: a.spec })
}

fn spec_observations(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ProjectArgs = args(input)?;
    world(IssuesRequest::SpecObservations { project: a.project })
}

fn spec_observe(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecObserveArgs = args(input)?;
    world(IssuesRequest::SpecObserve {
        spec: a.spec,
        rel: a.rel,
        target: a.target,
        note: a.note,
    })
}

fn spec_retract(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecRetractArgs = args(input)?;
    world(IssuesRequest::SpecRetract {
        spec: a.spec,
        observation: a.observation,
    })
}

fn spec_new(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecNewArgs = args(input)?;
    world(IssuesRequest::SpecNew {
        project: a.project,
        kind: a.kind,
        title: a.title,
        text: a.text,
        links: a.links,
    })
}

fn spec_revise(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecReviseArgs = args(input)?;
    world(IssuesRequest::SpecRevise {
        spec: a.spec,
        expected: a.expected,
        title: a.title,
        text: a.text,
        links: a.links,
        plan: a.plan,
    })
}

fn spec_state(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecStateArgs = args(input)?;
    world(IssuesRequest::SpecState {
        spec: a.spec,
        expected: a.expected,
        state: a.state,
    })
}

fn spec_resolve(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ResolveArgs = args(input)?;
    world(IssuesRequest::SpecResolve {
        spec: a.id,
        expected_heads: a.expected_heads,
        body_json: a.body_json,
    })
}

fn baseline_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ProjectArgs = args(input)?;
    world(IssuesRequest::BaselineList { project: a.project })
}

fn baseline_show(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BaselineArgs = args(input)?;
    world(IssuesRequest::BaselineShow {
        baseline: a.baseline,
    })
}

fn baseline_history(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BaselineArgs = args(input)?;
    world(IssuesRequest::BaselineHistory {
        baseline: a.baseline,
    })
}

fn baseline_new(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BaselineNewArgs = args(input)?;
    world(IssuesRequest::BaselineNew {
        project: a.project,
        name: a.name,
        members: a.members,
    })
}

fn baseline_revise(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BaselineReviseArgs = args(input)?;
    world(IssuesRequest::BaselineRevise {
        baseline: a.baseline,
        expected: a.expected,
        name: a.name,
        members: a.members,
    })
}

fn baseline_state(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BaselineStateArgs = args(input)?;
    world(IssuesRequest::BaselineState {
        baseline: a.baseline,
        expected: a.expected,
        state: a.state,
    })
}

fn baseline_resolve(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ResolveArgs = args(input)?;
    world(IssuesRequest::BaselineResolve {
        baseline: a.id,
        expected_heads: a.expected_heads,
        body_json: a.body_json,
    })
}

fn issue_baseline(input: Value) -> Result<ClientInvocation, Failure> {
    let a: IssueBaselineArgs = args(input)?;
    world(IssuesRequest::IssueBaseline {
        reff: a.reff,
        baseline: a.baseline,
    })
}

fn packet(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::Packet { reff: a.reff })
}

/// Attaching from a path is a LOCAL operation, not the World `attach` command:
/// the World call takes bytes that are already on the content plane, and
/// getting them there is what needs a filesystem. Same for saving one back.
fn attach_file(input: Value) -> Result<ClientInvocation, Failure> {
    let a: AttachFileArgs = args(input)?;
    local(
        LOCAL_ATTACH,
        json!({ "reff": a.reff, "file": a.file, "comment": a.comment }),
    )
}

fn attachment_save(input: Value) -> Result<ClientInvocation, Failure> {
    let a: AttachmentSaveArgs = args(input)?;
    local(
        LOCAL_ATTACHMENT_GET,
        json!({ "reff": a.reff, "id": a.id, "out": a.out }),
    )
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

    /// The command tags the wire protocol defines, read out of the type rather
    /// than out of a list beside it.
    fn protocol_command_tags() -> Vec<String> {
        let schema = serde_json::to_value(schemars::schema_for!(IssuesRequest))
            .expect("the request schema is JSON serializable");
        schema["oneOf"]
            .as_array()
            .expect("an internally tagged enum schemas as a oneOf")
            .iter()
            .map(|variant| {
                variant["properties"]["cmd"]["const"]
                    .as_str()
                    .expect("every variant pins its own cmd tag")
                    .to_string()
            })
            .collect()
    }

    /// The smallest instance a schema's own required fields accept.
    fn minimal_instance(schema: &Value) -> Value {
        /// The required fields of one object schema, each filled in.
        fn object(root: &Value, schema: &Value) -> Value {
            let properties = &schema["properties"];
            let required = schema["required"].as_array().cloned().unwrap_or_default();
            Value::Object(
                required
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|name| (name.to_string(), placeholder(root, &properties[name], name)))
                    .collect(),
            )
        }

        fn placeholder(root: &Value, schema: &Value, name: &str) -> Value {
            let schema = schema["$ref"]
                .as_str()
                .and_then(|reff| reff.strip_prefix('#'))
                .and_then(|pointer| root.pointer(pointer))
                .unwrap_or(schema);
            // A pinned tag (`"kind": "spec"`) is its own smallest instance.
            if !schema["const"].is_null() {
                return schema["const"].clone();
            }
            if let Some(value) = schema["enum"].as_array().and_then(|values| values.first()) {
                return value.clone();
            }
            // A tagged enum with fields — `Target`, and anything shaped like it.
            // The smallest instance is its first variant's, which is the same
            // question one level down rather than a new one.
            if let Some(variant) = schema["oneOf"].as_array().and_then(|values| values.first()) {
                return object(root, variant);
            }
            match schema["type"].as_str() {
                Some("string") => json!("x"),
                Some("integer" | "number") => json!(0),
                Some("boolean") => json!(false),
                Some("array") => json!([]),
                Some("object") => object(root, schema),
                other => panic!("no placeholder for a required `{name}` of type {other:?}"),
            }
        }

        object(schema, schema)
    }

    /// The command tag every tool actually emits, taken from the call it makes.
    fn tags_reachable_through_tools() -> std::collections::BTreeSet<String> {
        let mut tags = std::collections::BTreeSet::new();
        for tool in tools() {
            let invocation = tool
                .call(minimal_instance(&tool.schema()))
                .unwrap_or_else(|error| {
                    panic!("tool `{}` rejects its own schema: {error}", tool.name())
                });
            if let world_interface::ClientInvocationKind::World(call) = invocation.into_kind() {
                let request = crate::decode_call(&call).expect("a tool emits its own protocol");
                let encoded = serde_json::to_value(&request).expect("request json");
                tags.insert(
                    encoded["cmd"]
                        .as_str()
                        .expect("a request carries its cmd tag")
                        .to_string(),
                );
            }
        }
        tags
    }

    /// The commands no tool emits, as of this build.
    ///
    /// Two kinds, and both are named one by one rather than skipped by shape.
    /// `inbox`, `access_plan`, `attach` and `attachment_get` are driven through
    /// a LOCAL invocation — `attach_file` and `attachment_save` are their tools
    /// — so the World call a tool ends up making is not the one it returns. The
    /// text splice, checkpoint, and document-upgrade commands are transport
    /// primitives for the live web editor, not semantic agent actions. The rest have no agent
    /// surface at all: they shipped on the web client and were never given a
    /// tool.
    ///
    /// Writing them out is what makes the guard work. A command added after this
    /// list is not on it, so it must arrive with a tool or fail the build — and
    /// a tool added for one of these forces its removal from the list.
    const WITHOUT_A_TOOL: &[&str] = &[
        "access_plan",
        "attach",
        "attachment_get",
        "cycle_list",
        "cycle_set",
        "detach",
        "follow",
        "geometry",
        "inbox",
        "initiative_list",
        "initiative_set",
        "issue_cycle",
        "issue_document_upgrade",
        "issue_milestone",
        "issue_text_checkpoint",
        "issue_text_splice",
        "label_delete",
        "label_edit",
        "milestone_list",
        "milestone_set",
        "project_delete",
        "project_edit",
        "project_update_post",
        "project_updates",
        "space_describe",
        "space_rename",
        "spec_document_upgrade",
        "team_list",
        "team_set",
        "triage_decide",
        "triage_list",
        "triage_submit",
    ];

    /// Every command on the wire protocol is reachable through a tool, or is
    /// written down as one that is not.
    ///
    /// Derived from [`IssuesRequest`] itself. A list of expected command names
    /// kept beside the enum cannot fail when a variant is added, which is the
    /// one event a parity guard exists for — so the tags come out of the type's
    /// own schema, and every tool is called to see which tag it really emits.
    #[test]
    fn every_protocol_command_is_reachable_through_a_tool() {
        let reachable = tags_reachable_through_tools();
        let defined = protocol_command_tags();
        let missing: Vec<&String> = defined
            .iter()
            .filter(|tag| !reachable.contains(*tag) && !WITHOUT_A_TOOL.contains(&tag.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "these commands are on the wire protocol with no MCP tool behind them — the \
             agent surface drifted from the command surface: {missing:?}"
        );
        for tag in WITHOUT_A_TOOL {
            assert!(
                defined.iter().any(|command| command == tag),
                "`{tag}` is listed as having no tool but is not a command"
            );
            assert!(
                !reachable.contains(*tag),
                "`{tag}` has a tool now and must come off the list"
            );
        }
    }

    #[test]
    fn tools_are_package_local_and_emit_world_calls() {
        let tools = tools();
        assert_eq!(tools.len(), 64);
        assert!(tools.iter().all(|tool| !tool.name().starts_with("issues_")));
        let invocation = tools
            .iter()
            .find(|tool| tool.name() == "view")
            .unwrap()
            .call(json!({ "reff": "ENG-1" }))
            .unwrap();
        assert!(matches!(
            invocation.into_kind(),
            world_interface::ClientInvocationKind::World(_)
        ));
    }
}
