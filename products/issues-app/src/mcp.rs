#![allow(
    clippy::expect_used,
    reason = "MCP schemas use derived serialization over static bounded tool descriptors"
)]
//! Issues-owned MCP tools.

use schemars::JsonSchema;
use serde::{
    de::{DeserializeOwned, Error as _},
    Deserialize, Deserializer, Serialize,
};
use serde_json::{json, Value};
use world_interface::{ClientInvocation, Failure, McpTool};

use crate::host::{LOCAL_ACCESS, LOCAL_ATTACH, LOCAL_ATTACHMENT_GET, LOCAL_INBOX, LOCAL_WORK};
use crate::{BoardPos, Filter, IssuesRequest};

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct EmptyArgs {}

/// A first page needs no arguments. Requiring one made `{}` a tool error
/// ("missing field `page_size`") for every list an agent opens without an
/// opinion about size, which is most of them.
fn default_page_size() -> u32 {
    issues::contract::DEFAULT_PAGE_SIZE
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PageArgs {
    /// Maximum rows in this response (1..=1000). Omit for the default page.
    #[serde(default = "default_page_size")]
    #[schemars(range(min = 1, max = 1000))]
    page_size: u32,
    /// Opaque continuation emitted by the preceding page.
    #[serde(default)]
    cursor: Option<String>,
}

impl PageArgs {
    fn into_request(self) -> issues::contract::PageRequest {
        issues::contract::PageRequest {
            limit: self.page_size,
            cursor: self.cursor,
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PageOnlyArgs {
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RefPageArgs {
    reff: String,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum IssueDetailSection {
    Comments,
    Reactions,
    Attachments,
    Checks,
    OutgoingRelations,
    IncomingRelations,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueDetailPageArgs {
    reff: String,
    section: IssueDetailSection,
    /// Portable publication coordinate returned by the first detail response.
    /// It keeps hydration on the same implementation/extractor semantics.
    publication: McpPublicationId,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct McpPublicationId {
    #[schemars(length(min = 32, max = 32))]
    manifest_root: Vec<u8>,
    #[schemars(length(min = 32, max = 32))]
    implementation_digest: Vec<u8>,
    #[schemars(length(min = 32, max = 32))]
    extractor_schema_digest: Vec<u8>,
}

impl McpPublicationId {
    fn coordinate(self) -> Result<crate::PublicationCoordinate, Failure> {
        let digest = |name: &str, bytes: Vec<u8>| {
            if bytes.len() != 32 {
                return Err(Failure::new(format!(
                    "{name} must contain exactly 32 bytes"
                )));
            }
            Ok(data_encoding::HEXLOWER.encode(&bytes))
        };
        Ok(crate::PublicationCoordinate {
            manifest_root: digest("manifest_root", self.manifest_root)?,
            implementation_digest: digest("implementation_digest", self.implementation_digest)?,
            extractor_schema_digest: digest(
                "extractor_schema_digest",
                self.extractor_schema_digest,
            )?,
        })
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectPageArgs {
    project: String,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct OptionalProjectPageArgs {
    #[serde(default)]
    project: Option<String>,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SpecPageArgs {
    spec: String,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BaselinePageArgs {
    baseline: String,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ChangeSetArgs {
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    timestamp: Option<u64>,
    #[schemars(length(min = 1, max = 64))]
    operations: Vec<crate::ChangeOperation>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct OperationStatusArgs {
    operation: String,
    timestamp: u64,
    operations: Vec<crate::ChangeOperation>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueNewArgs {
    title: String,
    project: String,
    #[serde(default)]
    assignees: Vec<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    parent: Option<String>,
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
    #[serde(flatten)]
    page: PageArgs,
    /// Exact coordinate returned by the preceding inbox page.
    #[serde(default)]
    publication: Option<McpPublicationId>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RefArgs {
    reff: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssuesSearchArgs {
    #[serde(default)]
    text: Option<String>,
    /// Product entity kinds (for example issue, project, spec, plan_revision,
    /// comment). Several kinds use a bounded union and intentionally do not
    /// promise a continuation cursor until Runtime has Merge continuation.
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    tombstone: Option<bool>,
    /// 1..=10,000 rows; defaults to 50.
    #[serde(default)]
    limit: Option<u32>,
    /// Opaque base64url continuation returned by the prior Find answer.
    #[serde(default)]
    cursor: Option<String>,
    /// Bounded product field names. Raw schema/DAG coordinates are never
    /// accepted from an MCP caller.
    #[serde(default)]
    fields: Vec<String>,
}

macro_rules! exact_hex {
    ($name:ident, $bytes:expr, $encoded:expr, $pattern:literal) => {
        #[derive(Debug, Serialize, JsonSchema)]
        #[serde(transparent)]
        struct $name(
            #[schemars(length(min = $encoded, max = $encoded), regex(pattern = $pattern))] String,
        );

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                let valid = value.len() == $encoded
                    && data_encoding::HEXLOWER
                        .decode(value.as_bytes())
                        .is_ok_and(|bytes| bytes.len() == $bytes);
                if !valid {
                    return Err(D::Error::custom(format!(
                        "expected {} lowercase hex characters",
                        $encoded
                    )));
                }
                Ok(Self(value))
            }
        }

        impl $name {
            fn into_string(self) -> String {
                self.0
            }
        }
    };
}

exact_hex!(Hex16Bytes, 16, 32, "^[0-9a-f]{32}$");
exact_hex!(Hex32Bytes, 32, 64, "^[0-9a-f]{64}$");

/// Issues-owned lifecycle controls. The tagged shape makes `checkpoint`
/// required only for resume and keeps watch heads out of unrelated actions.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "lowercase")]
enum WorkArgs {
    Inspect {
        run: Hex16Bytes,
    },
    Watch {
        run: Hex16Bytes,
        /// Causal heads from the preceding inspect/watch response.
        #[serde(default)]
        heads: Vec<Hex32Bytes>,
    },
    Cancel {
        run: Hex16Bytes,
    },
    Continue {
        run: Hex16Bytes,
    },
    Resume {
        run: Hex16Bytes,
        /// Content id of an exact committed checkpoint on this Run.
        checkpoint: Hex32Bytes,
    },
}

#[derive(Debug, Deserialize, JsonSchema)]
struct VerifyArgs {
    reff: String,
    /// Pinned repository ContentRef (64 lowercase hex).
    source: Hex32Bytes,
    /// Exact caller-selected Build id. Omit to use the first-party runner-local
    /// verifier (the check records package_filled). That verifier binds the
    /// pinned source; it does not compile or isolate. Execution begins only
    /// when the named Build is installed locally.
    #[serde(default)]
    build: Option<Hex32Bytes>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum VerdictArg {
    Pass,
    Fail,
}

impl VerdictArg {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
struct AcceptCheckArgs {
    reff: String,
    run: Hex16Bytes,
    attempt: Hex16Bytes,
    /// Returned report ContentRef (64 lowercase hex).
    report: Hex32Bytes,
    /// Product verdict: `pass` or `fail`.
    verdict: VerdictArg,
    #[serde(default)]
    move_to_done: bool,
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
    /// Exact `publication` returned by `issue_detail`; stale coordinates are
    /// refused instead of being reinterpreted against current text.
    source: crate::protocol::WorldPublicationCoordinate,
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
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BoardArgs {
    #[serde(default)]
    project: Option<String>,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectNewArgs {
    name: String,
    key: String,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ProjectEditArgs {
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
    /// Team name, key, or `tm_` id. `"none"` clears.
    #[serde(default)]
    team: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MilestoneListArgs {
    project: String,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MilestoneSetArgs {
    project: String,
    /// Name or `mls_` id of an existing milestone. Omit to create.
    #[serde(default)]
    milestone: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    target: Option<String>,
    /// `top`, `bottom`, `before:<milestone>`, or `after:<milestone>`.
    #[serde(default)]
    pos: Option<String>,
    #[serde(default)]
    remove: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueMilestoneArgs {
    reff: String,
    /// Milestone name or `mls_` id. `"none"` or omit to clear.
    #[serde(default)]
    milestone: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CycleListArgs {
    project: String,
    #[serde(flatten)]
    page: PageArgs,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CycleSetArgs {
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct IssueCycleArgs {
    reff: String,
    #[serde(default)]
    cycle: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TeamSetArgs {
    /// Name, key, or `tm_` id. Omit to create.
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct InitiativeSetArgs {
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
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LabelNewArgs {
    name: String,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LabelEditArgs {
    label: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LabelDeleteArgs {
    label: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ActivityArgs {
    #[serde(flatten)]
    page: PageArgs,
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
    /// The grant ids to revoke as one all-or-nothing set — a role grant's
    /// whole expansion, as `access_list` groups it, or a single id.
    #[schemars(length(min = 1))]
    grant_ids: Vec<String>,
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

/// Protocol commands this World designed out of the agent surface.
///
/// Two kinds, and both are named one by one rather than skipped by shape.
/// `inbox`, `access_plan`, `attach` and `attachment_get` are driven through
/// a LOCAL invocation — `attach_file` and `attachment_save` are their tools
/// — so the World call a tool ends up making is not the one it returns. The
/// text splice, checkpoint, and document-upgrade commands are transport
/// primitives for the live web editor, not semantic agent actions.
/// `geometry` is compiled Blueprint output — connected components, layers,
/// residual loci — derived from parent/blocks/metadata at one World
/// generation. It is how the viewer draws a Plan. It is not an authored
/// object and must not become an agent verb; an agent asserts relations
/// and lets the picture follow.
/// The rest have no agent surface at all: they shipped on the web client
/// and were never given a tool.
///
/// Writing them out is what makes the guard work. A command added after this
/// list is not on it, so it must arrive with a tool or fail the build — and
/// a tool added for one of these forces its removal from the list.
pub const WITHOUT_A_TOOL: &[&str] = &[
    "access_plan",
    "attach",
    "attachment_get",
    "comment",
    "comment_at",
    "detach",
    "follow",
    "geometry",
    "inbox",
    "issue_attachments",
    "issue_checks",
    "issue_delete",
    "issue_document_upgrade",
    "issue_link",
    "issue_milestone",
    "issue_move",
    "issue_new",
    "issue_parent",
    "issue_reactions",
    "issue_relations",
    "issue_restore",
    "issue_text_checkpoint",
    "issue_text_splice",
    "issue_unlink",
    "issue_view",
    "label_delete",
    "label_edit",
    "label_new",
    "label_show",
    "project_delete",
    "project_update_post",
    "project_updates",
    "react",
    "space_describe",
    "space_rename",
    "spec_document_upgrade",
    "triage_decide",
    "triage_list",
    "triage_submit",
];

pub fn tools() -> Vec<McpTool> {
    vec![
        tool::<IssuesSearchArgs>(
            "issues_search",
            "Search Issues, projects, Plans/Specs and related records through the shared pinned publication. Supports product filters, bounded fields, and Runtime cursors without exposing the raw Find DAG.",
            issues_search,
        ),
        tool::<ChangeSetArgs>(
            "issues_change_set",
            "Atomically create dependent Issues records in one publication. Operations are ordered; later Specs/Plans may reference an earlier project_create by operation ordinal. The planner enforces product limits, preconditions, and retry-stable identities.",
            issues_change_set,
        ),
        tool::<OperationStatusArgs>(
            "issues_operation_status",
            "Read a ChangeSet's durable receipt and local publication readiness without resubmitting it.",
            issues_operation_status,
        ),
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
        tool::<WorkArgs>(
            "work",
            "Inspect or control durable Runtime work through the Issues-owned vocabulary. Starting work remains an Issues World action; this route has no raw Start operation.",
            work,
        ),
        tool::<VerifyArgs>(
            "verify",
            "Start a durable issue verification Run against one pinned repository ContentRef. Returns the stable Run id. The local Station may perform an Attempt after commit when a matching handler is installed. The first-party runner-local verifier binds the pinned source; it does not compile the repository or sandbox that runner. Omit build to select that Build — the check then records package_filled.",
            verify,
        ),
        tool::<AcceptCheckArgs>(
            "accept_check",
            "Attach one returned verification report and accept its Runtime Outcome through the Issues World validator.",
            accept_check,
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
        tool::<RefArgs>(
            "view",
            "Read an issue summary plus bounded first pages of comments, reactions, attachments, checks, and each relation direction; continue large sections with their cursors.",
            issue_view,
        ),
        tool::<IssueDetailPageArgs>(
            "view_page",
            "Continue exactly one issue-detail section at the publication returned by view; never drains or crosses into a newer publication.",
            issue_view_page,
        ),
        tool::<ListArgs>("list", "List issue rows.", list),
        tool::<BoardArgs>("board", "Render a project board.", board),
        tool::<RefPageArgs>("history", "Read one page of issue history.", history),
        tool::<ProjectNewArgs>("project_new", "Create a project.", project_new),
        tool::<PageOnlyArgs>("project_list", "List one page of projects.", project_list),
        tool::<ProjectEditArgs>(
            "project_edit",
            "Edit a project: name, color, description, lead, dates, archive, \
             or move it onto a team. Pass team=\"none\" to clear the team.",
            project_edit,
        ),
        tool::<MilestoneListArgs>(
            "milestone_list",
            "List a project's milestones.",
            milestone_list,
        ),
        tool::<MilestoneSetArgs>(
            "milestone_set",
            "Create, edit, reorder, or remove a project milestone. Omit \
             milestone to create; pass milestone (name or mls_ id) to edit; \
             remove=true tombstones it.",
            milestone_set,
        ),
        tool::<IssueMilestoneArgs>(
            "issue_milestone",
            "Assign or clear an issue's milestone. Pass milestone=\"none\" to \
             clear. The milestone must belong to the issue's project.",
            issue_milestone,
        ),
        tool::<CycleListArgs>("cycle_list", "List a project's cycles.", cycle_list),
        tool::<CycleSetArgs>(
            "cycle_set",
            "Create, edit, or remove a project cycle.",
            cycle_set,
        ),
        tool::<IssueCycleArgs>(
            "issue_cycle",
            "Assign or clear an issue's cycle. Pass cycle=\"none\" to clear.",
            issue_cycle,
        ),
        tool::<PageOnlyArgs>("team_list", "List one page of teams.", team_list),
        tool::<TeamSetArgs>(
            "team_set",
            "Create, edit, or remove a team. Omit team to create; pass team \
             (name, key, or tm_ id) to edit; remove=true tombstones it.",
            team_set,
        ),
        tool::<PageOnlyArgs>(
            "initiative_list",
            "List one page of initiatives.",
            initiative_list,
        ),
        tool::<InitiativeSetArgs>(
            "initiative_set",
            "Create, edit, or remove an initiative, including its project membership.",
            initiative_set,
        ),
        tool::<LabelNewArgs>("label_new", "Create a label.", label_new),
        tool::<LabelEditArgs>("label_edit", "Edit a label.", label_edit),
        tool::<LabelDeleteArgs>("label_delete", "Delete a label.", label_delete),
        tool::<PageOnlyArgs>("label_list", "List one page of labels.", label_list),
        tool::<ActivityArgs>("activity", "Read recent IssuesWorld transitions.", activity),
        tool::<PageArgs>("role_list", "List role definitions.", role_list),
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
            "Revoke effective assignments by grant id, as one all-or-nothing set. \
             Pass `grant_ids` — the ids access_list groups under one role grant \
             — or a single `grant_id`. Assignments whose origin is founder, \
             admission, membership or sponsorship are the base role: change \
             the role on Members instead of revoking them here.",

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
        tool::<OptionalProjectPageArgs>(
            "spec_list",
            "List one page of Specs, optionally by project.",
            spec_list,
        ),
        tool::<SpecArgs>("spec_show", "Read one versioned Spec.", spec_show),
        tool::<OptionalProjectPageArgs>(
            "spec_links",
            "One page of typed links asserted in scope, with the standing of each asserting revision.",
            spec_links,
        ),
        tool::<SpecPageArgs>(
            "spec_history",
            "One exact-publication page of Spec revisions and predecessors.",
            spec_history,
        ),
        tool::<SpecNewArgs>(
            "spec_new",
            "Create a draft Spec. kind is one of goal, requirement, plan, \
             design, order, guide, proof, verdict, waiver, record. A plan \
             Spec's optional plan field seeds which issue roots Blueprint \
             compiles; it stores no phases or completion. Do not put order in \
             prose — use issues_parent and issues_link kind=blocks.",
            spec_new,
        ),
        tool::<SpecReviseArgs>(
            "spec_revise",
            "Create a draft successor. Never rewrite an issued revision — \
             draft a successor and issue that.",
            spec_revise,
        ),
        tool::<SpecStateArgs>(
            "spec_state",
            "Move a Spec head: draft→review (spec.write / contributor), \
             review→issued or issued→withdrawn (spec.issue or space.admin). \
             Issuing makes governing truth. expected is the current head \
             revision from spec_show.",
            spec_state,
        ),
        tool::<ResolveArgs>(
            "spec_resolve",
            "Resolve concurrent Spec heads.",
            spec_resolve,
        ),
        tool::<OptionalProjectPageArgs>(
            "spec_observations",
            "One page of observations filed in scope — notes about the graph that bind \
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
        tool::<OptionalProjectPageArgs>(
            "baseline_list",
            "List one page of Baselines, optionally by project.",
            baseline_list,
        ),
        tool::<BaselineArgs>("baseline_show", "Read one Baseline.", baseline_show),
        tool::<BaselinePageArgs>(
            "baseline_history",
            "One exact-publication page of Baseline revisions.",
            baseline_history,
        ),
        tool::<BaselineNewArgs>(
            "baseline_new",
            "Create a draft Baseline of exact *issued* Spec revisions. \
             members are {spec, revision} pairs; a review or draft head is \
             refused. Issue each Spec first, then baseline, then \
             baseline_state to issued, then issue_baseline to pin it on work.",
            baseline_new,
        ),
        tool::<BaselineReviseArgs>(
            "baseline_revise",
            "Create a draft Baseline successor.",
            baseline_revise,
        ),
        tool::<BaselineStateArgs>(
            "baseline_state",
            "Move a Baseline head: draft→review (baseline.write / \
             contributor), review→issued (baseline.issue or space.admin). \
             Issuing freezes the named set of Spec revisions.",
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

fn issues_search(input: Value) -> Result<ClientInvocation, Failure> {
    use runtime::find as find_api;

    let a: IssuesSearchArgs = args(input)?;
    let page_size = a.limit.unwrap_or(50);
    if !(1..=find_api::MAX_PAGE_SIZE).contains(&page_size) {
        return Err(Failure::new("limit must be between 1 and 10000"));
    }
    let text = a.text.filter(|value| !value.trim().is_empty());
    if a.kinds.len() > 16 {
        return Err(Failure::new("at most 16 kinds may be searched together"));
    }
    let mut kinds = a.kinds;
    kinds.sort();
    kinds.dedup();
    if kinds.iter().any(|kind| {
        kind.is_empty()
            || kind.len() > 64
            || !kind
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    }) {
        return Err(Failure::new("kinds must be lowercase product tokens"));
    }

    let allowed_fields = [
        issues::find::field::ID,
        issues::find::field::KIND,
        issues::find::field::TITLE,
        issues::find::field::TEXT,
        issues::find::field::PROJECT,
        issues::find::field::STATE,
        issues::find::field::PRIORITY,
        issues::find::field::AUTHOR,
        issues::find::field::CREATED_AT,
        issues::find::field::DUE_AT,
        issues::find::field::HEALTH,
        issues::find::field::TOMBSTONE,
        issues::find::field::REVISION,
        issues::find::field::HEAD,
        issues::find::field::ISSUED,
        issues::find::field::CONFLICTED,
        issues::find::field::SOURCE_ID,
        issues::find::field::TARGET_ID,
        issues::find::field::RELATION_KIND,
    ];
    let requested = if a.fields.is_empty() {
        vec![
            issues::find::field::ID.into(),
            issues::find::field::KIND.into(),
            issues::find::field::TITLE.into(),
            issues::find::field::PROJECT.into(),
            issues::find::field::STATE.into(),
            issues::find::field::PRIORITY.into(),
            issues::find::field::TOMBSTONE.into(),
        ]
    } else {
        a.fields
    };
    if requested.len() > 20
        || requested
            .iter()
            .any(|field| !allowed_fields.contains(&field.as_str()))
    {
        return Err(Failure::new("fields contains an unsupported product field"));
    }
    let mut packed = requested
        .iter()
        .map(|field| issues::find::field_ref(field))
        .collect::<Vec<_>>();
    packed.sort();
    packed.dedup();

    let cursor = a
        .cursor
        .map(|encoded| {
            data_encoding::BASE64URL_NOPAD
                .decode(encoded.as_bytes())
                .map_err(|_| Failure::new("cursor is not canonical base64url"))
                .and_then(|bytes| {
                    find_api::Cursor::new(bytes)
                        .map_err(|_| Failure::new("cursor is not Runtime-issued"))
                })
        })
        .transpose()?;
    if cursor.is_some() && kinds.len() > 1 {
        return Err(Failure::new(
            "multi-kind union continuation is not available; narrow to one kind",
        ));
    }

    let candidate_bound = u64::from(page_size).saturating_mul(64).min(100_000);
    let bound = find_api::Bound {
        decoded_bodies: 1,
        postings_read: candidate_bound.saturating_mul(4),
        edges_visited: 1,
        nodes_visited: candidate_bound,
        paths_retained: 1,
        candidates_per_branch: candidate_bound,
        score_evaluations: candidate_bound,
        projected_bytes: u64::from(page_size).saturating_mul(64 * 1024),
        packed_tokens: u64::from(page_size).saturating_mul(512),
        wall_millis: 5_000,
    };
    let branch_kinds = if kinds.is_empty() {
        vec![None]
    } else {
        kinds.into_iter().map(Some).collect()
    };
    let mut steps = Vec::new();
    let mut outputs = Vec::new();
    let mut next_id = 1u32;
    for kind in branch_kinds {
        let seek_id = find_api::StepId::new(next_id)
            .ok_or_else(|| Failure::new("search plan exceeds the bounded DAG"))?;
        next_id = next_id
            .checked_add(1)
            .ok_or_else(|| Failure::new("search plan exceeds the bounded DAG"))?;
        let seek = if let Some(text) = &text {
            find_api::Seek::Term {
                field: issues::find::field_ref(issues::find::field::SEARCH),
                text: text.clone(),
                kind: find_api::Term::Token,
            }
        } else if let Some(kind) = &kind {
            find_api::Seek::Field(find_api::Predicate {
                field: issues::find::field_ref(issues::find::field::KIND),
                test: find_api::Test::Equal,
                value: find_api::Atom::Text(kind.clone()),
            })
        } else {
            find_api::Seek::Source
        };
        steps.push(find_api::Step {
            id: seek_id,
            input: Vec::new(),
            op: find_api::Op::Seek(seek),
            bound,
        });
        let mut predicates = Vec::new();
        if text.is_some() {
            if let Some(kind) = kind {
                predicates.push(find_api::Predicate {
                    field: issues::find::field_ref(issues::find::field::KIND),
                    test: find_api::Test::Equal,
                    value: find_api::Atom::Text(kind),
                });
            }
        }
        for (field, value) in [
            (issues::find::field::PROJECT, a.project.clone()),
            (issues::find::field::STATE, a.state.clone()),
            (issues::find::field::PRIORITY, a.priority.clone()),
        ] {
            if let Some(value) = value {
                predicates.push(find_api::Predicate {
                    field: issues::find::field_ref(field),
                    test: find_api::Test::Equal,
                    value: find_api::Atom::Text(value),
                });
            }
        }
        if let Some(tombstone) = a.tombstone {
            predicates.push(find_api::Predicate {
                field: issues::find::field_ref(issues::find::field::TOMBSTONE),
                test: find_api::Test::Equal,
                value: find_api::Atom::Bool(tombstone),
            });
        }
        if predicates.is_empty() {
            outputs.push(seek_id);
        } else {
            predicates.sort();
            let keep_id = find_api::StepId::new(next_id)
                .ok_or_else(|| Failure::new("search plan exceeds the bounded DAG"))?;
            next_id = next_id
                .checked_add(1)
                .ok_or_else(|| Failure::new("search plan exceeds the bounded DAG"))?;
            steps.push(find_api::Step {
                id: keep_id,
                input: vec![seek_id],
                op: find_api::Op::Keep(find_api::Keep { predicates }),
                bound,
            });
            outputs.push(keep_id);
        }
    }
    let output = if outputs.len() == 1 {
        outputs
            .first()
            .copied()
            .ok_or_else(|| Failure::new("search plan has no output"))?
    } else {
        let merge_id = find_api::StepId::new(next_id)
            .ok_or_else(|| Failure::new("search plan exceeds the bounded DAG"))?;
        next_id = next_id
            .checked_add(1)
            .ok_or_else(|| Failure::new("search plan exceeds the bounded DAG"))?;
        steps.push(find_api::Step {
            id: merge_id,
            input: outputs,
            op: find_api::Op::Merge(find_api::Merge {
                method: find_api::MergeMethod::Union,
            }),
            bound,
        });
        merge_id
    };
    let pack_id = find_api::StepId::new(next_id)
        .ok_or_else(|| Failure::new("search plan exceeds the bounded DAG"))?;
    steps.push(find_api::Step {
        id: pack_id,
        input: vec![output],
        op: find_api::Op::Pack(find_api::Pack { fields: packed }),
        bound,
    });
    Ok(ClientInvocation::find_presented(
        issues::contract::world_id(),
        find_api::Query {
            schema: issues::find::entity_schema_ref(),
            publication: None,
            mode: find_api::Mode::Exact,
            steps,
            output: pack_id,
            bound,
            page_size,
            cursor,
        },
        present_issues_search,
    ))
}

fn issues_change_set(input: Value) -> Result<ClientInvocation, Failure> {
    let args: ChangeSetArgs = args(input)?;
    if args.operations.is_empty()
        || args.operations.len() > issues::contract::CHANGE_SET_MAX_OPERATIONS
    {
        return Err(Failure::invalid());
    }
    world(IssuesRequest::ChangeSet {
        operation: Some(args.operation.unwrap_or_else(|| {
            data_encoding::HEXLOWER.encode(&runtime::world::RequestId::mint().as_bytes())
        })),
        timestamp: Some(
            args.timestamp
                .unwrap_or_else(mechanics::wallclock::now_secs),
        ),
        operations: args.operations,
    })
}

fn issues_operation_status(input: Value) -> Result<ClientInvocation, Failure> {
    let args: OperationStatusArgs = args(input)?;
    world(IssuesRequest::OperationStatus {
        operation: args.operation,
        timestamp: args.timestamp,
        operations: args.operations,
    })
}

fn single_change(operation: crate::protocol::ChangeOperation) -> Result<ClientInvocation, Failure> {
    world(IssuesRequest::ChangeSet {
        operation: Some(
            data_encoding::HEXLOWER.encode(&runtime::world::RequestId::mint().as_bytes()),
        ),
        timestamp: Some(mechanics::wallclock::now_secs()),
        operations: vec![operation],
    })
}

/// Present Runtime's exact Find answer using Issues' public query vocabulary.
///
/// Runtime cursors are opaque bytes by design; byte-array JSON would expose an
/// accidental transport representation and contradict the base64url cursor
/// accepted by [`IssuesSearchArgs`]. Rows similarly become product field maps
/// rather than leaking schema references and corpus node coordinates.
fn present_issues_search(answer: runtime::find::Answer) -> Result<Value, Failure> {
    let publication = answer.coordinates().world_publication();
    let mut items = Vec::with_capacity(answer.rows().len());
    for row in answer.rows() {
        let mut item = serde_json::Map::new();
        for field in &row.fields {
            item.insert(
                field.reference.name.as_str().to_owned(),
                present_find_value(&field.value),
            );
        }
        items.push(Value::Object(item));
    }

    let mut envelope = serde_json::Map::new();
    envelope.insert(
        "publication".into(),
        json!({
            "manifest_root": data_encoding::HEXLOWER.encode(
                &publication.publication.manifest_root
            ),
            "implementation_digest": data_encoding::HEXLOWER.encode(
                &publication.publication.implementation_digest
            ),
            "extractor_schema_digest": data_encoding::HEXLOWER.encode(
                &publication.publication.extractor_schema_digest.digest()
            ),
            "materialization": publication.materialization.get(),
        }),
    );
    envelope.insert("items".into(), Value::Array(items));
    envelope.insert(
        "next_cursor".into(),
        answer
            .next_cursor()
            .map(|cursor| Value::String(data_encoding::BASE64URL_NOPAD.encode(cursor.as_bytes())))
            .unwrap_or(Value::Null),
    );
    if let Some(total) = answer.matched_total() {
        envelope.insert("total".into(), Value::Number(total.into()));
    }
    Ok(Value::Object(envelope))
}

fn present_find_value(value: &runtime::find::Value) -> Value {
    match value {
        runtime::find::Value::Bool(value) => Value::Bool(*value),
        runtime::find::Value::Signed(value) => Value::Number((*value).into()),
        runtime::find::Value::Unsigned(value) => Value::Number((*value).into()),
        runtime::find::Value::Bytes(value) => {
            Value::String(data_encoding::BASE64URL_NOPAD.encode(value))
        }
        runtime::find::Value::Text(value) => Value::String(value.to_string()),
    }
}

fn issue_new(input: Value) -> Result<ClientInvocation, Failure> {
    let a: IssueNewArgs = args(input)?;
    let due = match a.due.as_deref() {
        None | Some("none") => None,
        Some(value) => Some(crate::router::parse_due(value).ok_or_else(Failure::invalid)?),
    };
    single_change(crate::protocol::ChangeOperation::IssueCreate {
        project: crate::protocol::ChangeProject::Existing { project: a.project },
        title: a.title,
        priority: a.priority,
        status: a.status,
        parent: a.parent,
        assignees: a.assignees,
        labels: a
            .labels
            .into_iter()
            .map(|label| crate::protocol::ChangeLabel::Existing { label })
            .collect(),
        body: a.body,
        due,
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

fn work(input: Value) -> Result<ClientInvocation, Failure> {
    let a: WorkArgs = args(input)?;
    let input = match a {
        WorkArgs::Inspect { run } => {
            json!({"action": "inspect", "run": run.into_string()})
        }
        WorkArgs::Watch { run, heads } => json!({
            "action": "watch",
            "run": run.into_string(),
            "heads": heads.into_iter().map(Hex32Bytes::into_string).collect::<Vec<_>>(),
        }),
        WorkArgs::Cancel { run } => {
            json!({"action": "cancel", "run": run.into_string()})
        }
        WorkArgs::Continue { run } => {
            json!({"action": "continue", "run": run.into_string()})
        }
        WorkArgs::Resume { run, checkpoint } => json!({
            "action": "resume",
            "run": run.into_string(),
            "checkpoint": checkpoint.into_string(),
        }),
    };
    local(LOCAL_WORK, input)
}

fn verify(input: Value) -> Result<ClientInvocation, Failure> {
    let a: VerifyArgs = args(input)?;
    world(IssuesRequest::Verify {
        reff: a.reff,
        source: a.source.into_string(),
        // Empty means omitted: the router fills the package-selected Build and records
        // package_filled. Filling here would look like a caller-named Build.
        build: a.build.map(Hex32Bytes::into_string).unwrap_or_default(),
    })
}

fn accept_check(input: Value) -> Result<ClientInvocation, Failure> {
    let a: AcceptCheckArgs = args(input)?;
    world(IssuesRequest::AcceptCheck {
        reff: a.reff,
        run: a.run.into_string(),
        attempt: a.attempt.into_string(),
        report: a.report.into_string(),
        verdict: a.verdict.as_str().to_owned(),
        move_to_done: a.move_to_done,
    })
}

fn inbox(input: Value) -> Result<ClientInvocation, Failure> {
    let a: InboxArgs = args(input)?;
    let publication = a
        .publication
        .map(McpPublicationId::coordinate)
        .transpose()?;
    local(
        LOCAL_INBOX,
        json!({
            "clear": a.clear,
            "page": a.page.into_request(),
            "publication": publication,
        }),
    )
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
    single_change(crate::protocol::ChangeOperation::IssueMove {
        issue: a.reff,
        project: a
            .project
            .map(|project| crate::protocol::ChangeProject::Existing { project }),
        position: a.position.as_deref().and_then(parse_change_position),
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
    single_change(crate::protocol::ChangeOperation::IssueComment {
        issue: a.reff,
        body: a.body,
        parent: a.reply_to,
    })
}

fn comment_at(input: Value) -> Result<ClientInvocation, Failure> {
    let a: CommentAtArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueCommentAt {
        issue: a.reff,
        body: a.body,
        field: a.field,
        start: a.start,
        end: a.end,
        parent: a.reply_to,
        source: a.source,
    })
}

fn react(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ReactArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueReaction {
        issue: a.reff,
        comment: a.comment,
        emoji: a.emoji,
        on: !a.remove,
    })
}

fn issue_delete(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueTombstone {
        issue: a.reff,
        on: true,
    })
}

fn issue_restore(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueTombstone {
        issue: a.reff,
        on: false,
    })
}

fn issue_link(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LinkArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueLink {
        issue: a.reff,
        kind: a.kind,
        target: a.target,
        on: true,
    })
}

fn issue_unlink(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LinkArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueLink {
        issue: a.reff,
        kind: a.kind,
        target: a.target,
        on: false,
    })
}

fn issue_parent(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ParentArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueParent {
        issue: a.reff,
        parent: a.parent,
    })
}

fn issue_view(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefArgs = args(input)?;
    world(IssuesRequest::IssueDetail {
        reff: a.reff,
        publication: None,
    })
}

fn issue_view_page(input: Value) -> Result<ClientInvocation, Failure> {
    let a: IssueDetailPageArgs = args(input)?;
    let page = a.page.into_request();
    let publication = Some(a.publication.coordinate()?);
    let request = match a.section {
        IssueDetailSection::Comments => IssuesRequest::IssueComments {
            reff: a.reff,
            publication,
            page,
        },
        IssueDetailSection::Reactions => IssuesRequest::IssueReactions {
            reff: a.reff,
            publication,
            page,
        },
        IssueDetailSection::Attachments => IssuesRequest::IssueAttachments {
            reff: a.reff,
            publication,
            page,
        },
        IssueDetailSection::Checks => IssuesRequest::IssueChecks {
            reff: a.reff,
            publication,
            page,
        },
        IssueDetailSection::OutgoingRelations => IssuesRequest::IssueRelations {
            reff: a.reff,
            direction: issues::dto::RelationDirection::Out,
            publication,
            page,
        },
        IssueDetailSection::IncomingRelations => IssuesRequest::IssueRelations {
            reff: a.reff,
            direction: issues::dto::RelationDirection::In,
            publication,
            page,
        },
    };
    world(request)
}

fn list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ListArgs = args(input)?;
    let page = a.page.into_request();
    world(IssuesRequest::List {
        project: a.project,
        filter: Filter {
            mine: a.mine,
            status: a.status,
            label: a.label,
            milestone: a.milestone,
            all: a.all,
        },
        page,
    })
}

fn board(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BoardArgs = args(input)?;
    world(IssuesRequest::Board {
        project: a.project,
        project_hint: None,
        page: a.page.into_request(),
    })
}

fn history(input: Value) -> Result<ClientInvocation, Failure> {
    let a: RefPageArgs = args(input)?;
    world(IssuesRequest::History {
        reff: a.reff,
        publication: None,
        page: a.page.into_request(),
    })
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
    let a: PageOnlyArgs = args(input)?;
    world(IssuesRequest::ProjectList {
        page: a.page.into_request(),
    })
}

fn project_edit(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ProjectEditArgs = args(input)?;
    world(IssuesRequest::ProjectEdit {
        project: a.project,
        name: a.name,
        color: a.color,
        description: a.description,
        lead: a.lead,
        start: a.start,
        target: a.target,
        archived: a.archived,
        team: a.team,
    })
}

fn milestone_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: MilestoneListArgs = args(input)?;
    world(IssuesRequest::MilestoneList {
        project: a.project,
        page: a.page.into_request(),
    })
}

fn milestone_set(input: Value) -> Result<ClientInvocation, Failure> {
    let a: MilestoneSetArgs = args(input)?;
    world(IssuesRequest::MilestoneSet {
        project: a.project,
        milestone: a.milestone,
        name: a.name,
        description: a.description,
        target: a.target,
        pos: a.pos.as_deref().and_then(parse_position),
        remove: a.remove,
    })
}

fn issue_milestone(input: Value) -> Result<ClientInvocation, Failure> {
    let a: IssueMilestoneArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::IssueMilestone {
        issue: a.reff,
        milestone: a.milestone.filter(|milestone| milestone != "none"),
    })
}

fn cycle_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: CycleListArgs = args(input)?;
    world(IssuesRequest::CycleList {
        project: a.project,
        page: a.page.into_request(),
    })
}

fn cycle_set(input: Value) -> Result<ClientInvocation, Failure> {
    let a: CycleSetArgs = args(input)?;
    world(IssuesRequest::CycleSet {
        project: a.project,
        cycle: a.cycle,
        name: a.name,
        start: a.start,
        end: a.end,
        remove: a.remove,
    })
}

fn issue_cycle(input: Value) -> Result<ClientInvocation, Failure> {
    let a: IssueCycleArgs = args(input)?;
    world(IssuesRequest::IssueCycle {
        reff: a.reff,
        cycle: a.cycle,
    })
}

fn team_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: PageOnlyArgs = args(input)?;
    world(IssuesRequest::TeamList {
        page: a.page.into_request(),
    })
}

fn team_set(input: Value) -> Result<ClientInvocation, Failure> {
    let a: TeamSetArgs = args(input)?;
    world(IssuesRequest::TeamSet {
        team: a.team,
        name: a.name,
        key: a.key,
        icon: a.icon,
        lead: a.lead,
        add_members: a.add_members,
        remove_members: a.remove_members,
        remove: a.remove,
    })
}

fn initiative_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: PageOnlyArgs = args(input)?;
    world(IssuesRequest::InitiativeList {
        page: a.page.into_request(),
    })
}

fn initiative_set(input: Value) -> Result<ClientInvocation, Failure> {
    let a: InitiativeSetArgs = args(input)?;
    world(IssuesRequest::InitiativeSet {
        initiative: a.initiative,
        name: a.name,
        description: a.description,
        owner: a.owner,
        health: a.health,
        target: a.target,
        add_projects: a.add_projects,
        remove_projects: a.remove_projects,
        remove: a.remove,
    })
}

fn label_new(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LabelNewArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::LabelCreate {
        name: a.name,
        color: a.color.unwrap_or_else(|| "gray".into()),
    })
}

fn label_edit(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LabelEditArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::LabelEdit {
        label: a.label,
        name: a.name,
        color: a.color,
    })
}

fn label_delete(input: Value) -> Result<ClientInvocation, Failure> {
    let a: LabelDeleteArgs = args(input)?;
    single_change(crate::protocol::ChangeOperation::LabelDelete { label: a.label })
}

fn label_list(input: Value) -> Result<ClientInvocation, Failure> {
    let a: PageOnlyArgs = args(input)?;
    world(IssuesRequest::LabelList {
        page: a.page.into_request(),
    })
}

fn activity(input: Value) -> Result<ClientInvocation, Failure> {
    let a: ActivityArgs = args(input)?;
    world(IssuesRequest::Activity {
        page: a.page.into_request(),
    })
}

fn role_list(input: Value) -> Result<ClientInvocation, Failure> {
    let page: PageArgs = args(input)?;
    world(IssuesRequest::RoleList {
        page: page.into_request(),
    })
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
        json!({ "action": "revoke", "grant_ids": a.grant_ids }),
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
    let a: OptionalProjectPageArgs = args(input)?;
    world(IssuesRequest::SpecList {
        project: a.project,
        page: a.page.into_request(),
    })
}

fn spec_show(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecArgs = args(input)?;
    world(IssuesRequest::SpecShow { spec: a.spec })
}

fn spec_links(input: Value) -> Result<ClientInvocation, Failure> {
    let a: OptionalProjectPageArgs = args(input)?;
    world(IssuesRequest::SpecReferences {
        project: a.project,
        page: a.page.into_request(),
    })
}

fn spec_history(input: Value) -> Result<ClientInvocation, Failure> {
    let a: SpecPageArgs = args(input)?;
    world(IssuesRequest::SpecHistory {
        spec: a.spec,
        page: a.page.into_request(),
    })
}

fn spec_observations(input: Value) -> Result<ClientInvocation, Failure> {
    let a: OptionalProjectPageArgs = args(input)?;
    world(IssuesRequest::SpecObservations {
        project: a.project,
        page: a.page.into_request(),
    })
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
    let a: OptionalProjectPageArgs = args(input)?;
    world(IssuesRequest::BaselineList {
        project: a.project,
        page: a.page.into_request(),
    })
}

fn baseline_show(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BaselineArgs = args(input)?;
    world(IssuesRequest::BaselineShow {
        baseline: a.baseline,
    })
}

fn baseline_history(input: Value) -> Result<ClientInvocation, Failure> {
    let a: BaselinePageArgs = args(input)?;
    world(IssuesRequest::BaselineHistory {
        baseline: a.baseline,
        page: a.page.into_request(),
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

fn parse_change_position(value: &str) -> Option<crate::protocol::ChangePosition> {
    parse_position(value).map(|position| match position {
        BoardPos::Top => crate::protocol::ChangePosition::Top,
        BoardPos::Bottom => crate::protocol::ChangePosition::Bottom,
        BoardPos::Before { reff } => crate::protocol::ChangePosition::Before { issue: reff },
        BoardPos::After { reff } => crate::protocol::ChangePosition::After { issue: reff },
    })
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
                Some("string") => {
                    let length = schema["minLength"]
                        .as_u64()
                        .and_then(|length| usize::try_from(length).ok())
                        .unwrap_or(1);
                    json!("0".repeat(length))
                }
                Some("integer" | "number") => {
                    schema.get("minimum").cloned().unwrap_or_else(|| json!(0))
                }
                Some("boolean") => json!(false),
                Some("array") => {
                    let length = schema["minItems"]
                        .as_u64()
                        .and_then(|length| usize::try_from(length).ok())
                        .unwrap_or(0);
                    let item = &schema["items"];
                    Value::Array(
                        (0..length)
                            .map(|_| placeholder(root, item, "array item"))
                            .collect(),
                    )
                }
                Some("object") => object(root, schema),
                other => panic!("no placeholder for a required `{name}` of type {other:?}"),
            }
        }

        let selected = schema["oneOf"]
            .as_array()
            .and_then(|variants| variants.first())
            .unwrap_or(schema);
        object(schema, selected)
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
        world_interface::agent_surface_coverage(&defined, &reachable, WITHOUT_A_TOOL)
            .check()
            .unwrap_or_else(|error| {
                panic!("the agent surface drifted from the command surface: {error:?}")
            });
    }

    #[test]
    fn tools_are_package_local_and_emit_world_calls() {
        let tools = tools();
        // 80 rather than 81: `geometry` is not one of them. It is compiled
        // Blueprint output and is named in `WITHOUT_A_TOOL` for that reason.
        assert_eq!(tools.len(), 80);
        let qualified: Vec<_> = tools
            .iter()
            .filter(|tool| tool.name().starts_with("issues_"))
            .map(|tool| tool.name())
            .collect();
        assert_eq!(
            qualified,
            [
                "issues_search",
                "issues_change_set",
                "issues_operation_status",
            ]
        );
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

    #[test]
    fn search_owns_a_product_presenter_and_never_exposes_the_raw_find_dag() {
        let invocation = issues_search(json!({
            "kinds": ["issue"],
            "limit": 25,
            "fields": ["id", "title"]
        }))
        .expect("friendly search compiles");
        let world_interface::ClientInvocationKind::Find { query, presenter } =
            invocation.into_kind()
        else {
            panic!("issues_search did not compile to Runtime Find")
        };
        assert_eq!(query.page_size, 25);
        assert!(
            presenter.is_some(),
            "raw Runtime JSON would leak cursor bytes"
        );
    }

    #[test]
    fn search_values_use_client_scalars() {
        assert_eq!(
            present_find_value(&runtime::find::Value::text("Plan title")),
            json!("Plan title")
        );
        assert_eq!(
            present_find_value(&runtime::find::Value::bytes([0xfb, 0xff])),
            json!("-_8")
        );
        assert_eq!(
            present_find_value(&runtime::find::Value::Unsigned(42)),
            json!(42)
        );
    }

    #[test]
    fn lifecycle_tool_schema_requires_action_specific_exact_ids() {
        let tools = tools();
        let work = tools.iter().find(|tool| tool.name() == "work").unwrap();
        work.call(json!({
            "action": "continue",
            "run": "11".repeat(16),
        }))
        .expect("continue has no caller-supplied scheduling coordinates");

        let missing_checkpoint = work
            .call(json!({
                "action": "resume",
                "run": "11".repeat(16),
            }))
            .err()
            .expect("resume must require a checkpoint");
        assert!(
            missing_checkpoint
                .diagnostic()
                .is_some_and(|message| message.contains("missing field `checkpoint`")),
            "{missing_checkpoint:?}"
        );

        let malformed_head = work
            .call(json!({
                "action": "watch",
                "run": "11".repeat(16),
                "heads": ["short"],
            }))
            .err()
            .expect("watch heads are exact event ids");
        assert!(
            malformed_head
                .diagnostic()
                .is_some_and(|message| message.contains("expected 64 lowercase hex characters")),
            "{malformed_head:?}"
        );

        let accept = tools
            .iter()
            .find(|tool| tool.name() == "accept_check")
            .unwrap();
        let invalid_verdict = accept
            .call(json!({
                "reff": "ENG-1",
                "run": "11".repeat(16),
                "attempt": "22".repeat(16),
                "report": "33".repeat(32),
                "verdict": "maybe",
            }))
            .err()
            .expect("verdict is pass or fail");
        assert!(
            invalid_verdict
                .diagnostic()
                .is_some_and(|message| message.contains("unknown variant `maybe`")),
            "{invalid_verdict:?}"
        );
    }

    /// MCP clients validate `inputSchema.type == "object"` on every tool and
    /// refuse the entire tool list when one fails — a reconnect that fetches
    /// zero tools, not one broken tool. `work`'s tagged union is the shape
    /// that regressed.
    #[test]
    fn every_tool_publishes_an_object_input_schema() {
        for tool in tools() {
            assert_eq!(
                tool.schema()["type"],
                "object",
                "tool '{}' publishes a non-object input schema",
                tool.name()
            );
        }
    }
}
