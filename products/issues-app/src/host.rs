//! Typed host capabilities requested by Issues client interfaces.
//!
//! These operations need facilities outside a World Session (the working tree,
//! local files, read watermarks, or Space authority). The product owns their
//! vocabulary and validation; the navigation shell only supplies the facility.

use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use world_interface::{
    ClientAccess, ClientHost, ClientInvocation, ClientInvocationKind, ClientOutput, Failure,
    HostAssignment, HostControlRequest, LocalInvocation, Presentation, PresentationFailure,
    PresentationOptions,
};

use crate::cli::{
    LOCAL_ACCESS, LOCAL_ATTACH, LOCAL_ATTACHMENT_GET, LOCAL_FOCUS, LOCAL_INBOX, LOCAL_NEW_START,
    LOCAL_WORK_STATE, LOCAL_WORLD_UPGRADE,
};
use crate::IssuesRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkStateAction {
    Start,
    Done,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessRequest {
    List {
        actor: Option<String>,
    },
    Grant {
        actor: String,
        role: String,
        project: Option<String>,
    },
    Revoke {
        grant_id: String,
    },
}

#[derive(Debug, Clone)]
pub enum IssuesHostRequest {
    Focus,
    NewStart(IssuesRequest),
    WorkState {
        action: WorkStateAction,
        reff: String,
        no_branch: bool,
    },
    Inbox {
        clear: bool,
    },
    WorldUpgrade,
    Access(AccessRequest),
    Attach {
        reff: String,
        file: String,
        comment: Option<String>,
    },
    AttachmentGet {
        reff: String,
        id: String,
        out: Option<String>,
    },
}

impl IssuesHostRequest {
    /// Classify the complete host operation, including caller-local effects.
    ///
    /// This must stay exhaustive: a new host capability cannot silently inherit
    /// command access or be mistaken for a read-only World query.
    pub fn access(&self) -> ClientAccess {
        match self {
            Self::Focus
            | Self::Inbox { clear: false }
            | Self::Access(AccessRequest::List { .. }) => ClientAccess::Query,
            Self::NewStart(_)
            | Self::WorkState { .. }
            | Self::Inbox { clear: true }
            | Self::WorldUpgrade
            | Self::Access(AccessRequest::Grant { .. } | AccessRequest::Revoke { .. })
            | Self::Attach { .. }
            | Self::AttachmentGet { .. } => ClientAccess::Command,
        }
    }
}

pub struct AccessGrantPlan {
    pub assignments: Vec<crate::AccessAssignment>,
}

/// Read the caller-local inbox watermark. Missing or malformed state simply
/// means the inbox is unread; it is never replicated truth.
pub fn read_inbox_watermark(home: &Path) -> u64 {
    std::fs::read_to_string(home.join("inbox-read.json"))
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Advance the caller-local inbox watermark after a successful projection.
pub fn write_inbox_watermark(home: &Path, timestamp: u64) -> std::io::Result<()> {
    std::fs::write(home.join("inbox-read.json"), timestamp.to_string())
}

pub fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccessRefusal {
    NotFound(String),
    Invalid(String),
}

impl std::fmt::Display for AccessRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(message) | Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AccessRefusal {}

/// Decode one package-emitted local invocation at the product/host boundary.
pub fn decode(operation: &str, input: Value) -> Result<IssuesHostRequest, Failure> {
    match operation {
        LOCAL_FOCUS => Ok(IssuesHostRequest::Focus),
        LOCAL_NEW_START => serde_json::from_value(input)
            .map(IssuesHostRequest::NewStart)
            .map_err(|error| Failure::new(format!("decode Issues new/start: {error}"))),
        LOCAL_WORK_STATE => {
            let action = match required(&input, "action")?.as_str() {
                "start" => WorkStateAction::Start,
                "done" => WorkStateAction::Done,
                "stop" => WorkStateAction::Stop,
                other => {
                    return Err(Failure::new(format!(
                        "unsupported Issues work-state action '{other}'"
                    )));
                }
            };
            Ok(IssuesHostRequest::WorkState {
                action,
                reff: required(&input, "reff")?,
                no_branch: input
                    .get("no_branch")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }
        LOCAL_INBOX => Ok(IssuesHostRequest::Inbox {
            clear: input.get("clear").and_then(Value::as_bool).unwrap_or(false),
        }),
        LOCAL_WORLD_UPGRADE => Ok(IssuesHostRequest::WorldUpgrade),
        LOCAL_ACCESS => {
            let action = input.get("action").and_then(Value::as_str).unwrap_or("ls");
            let request = match action {
                "access" | "ls" => AccessRequest::List {
                    actor: optional(&input, "actor"),
                },
                "grant" => AccessRequest::Grant {
                    actor: required(&input, "actor")?,
                    role: required(&input, "role")?,
                    project: optional(&input, "project"),
                },
                "revoke" => AccessRequest::Revoke {
                    grant_id: required(&input, "grant_id")?,
                },
                other => {
                    return Err(Failure::new(format!(
                        "unsupported Issues access action '{other}'"
                    )));
                }
            };
            Ok(IssuesHostRequest::Access(request))
        }
        LOCAL_ATTACH => Ok(IssuesHostRequest::Attach {
            reff: required(&input, "reff")?,
            file: required(&input, "file")?,
            comment: optional(&input, "comment"),
        }),
        LOCAL_ATTACHMENT_GET => Ok(IssuesHostRequest::AttachmentGet {
            reff: required(&input, "reff")?,
            id: required(&input, "id")?,
            out: optional(&input, "out"),
        }),
        other => Err(Failure::new(format!(
            "unsupported Issues host capability '{other}'"
        ))),
    }
}

/// Construct one package-owned local invocation with whole-operation policy.
pub fn invocation(operation: &str, input: Value) -> Result<ClientInvocation, Failure> {
    let request = decode(operation, input.clone())?;
    Ok(ClientInvocation::local(
        issues::contract::world_id(),
        operation,
        input,
        request.access(),
        None,
    ))
}

/// Construct one Issues World invocation with package-owned client policy.
pub fn world_invocation(request: IssuesRequest) -> Result<ClientInvocation, Failure> {
    let access = match request.access() {
        world_bridge::WorldCallAccess::Query => ClientAccess::Query,
        world_bridge::WorldCallAccess::Command => ClientAccess::Command,
    };
    let confirmation = request.destructive_question();
    let call = crate::encode_call(&request).map_err(|error| Failure::new(error.to_string()))?;
    Ok(ClientInvocation::world(call, access, confirmation))
}

/// Decode the Issues browser protocol behind its explicit World route.
///
/// Viewer-only host capabilities are adapted here; ordinary commands remain
/// the exact strict [`IssuesRequest`] schema carried by CLI and MCP.
pub fn parse_web(input: Value) -> Result<ClientInvocation, Failure> {
    let command = input
        .get("cmd")
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::new("Issues request is missing string field 'cmd'"))?;
    match command {
        "inbox" => {
            let clear = match input.get("clear") {
                None => false,
                Some(Value::Bool(clear)) => *clear,
                Some(_) => {
                    return Err(Failure::new("inbox 'clear' must be a boolean"));
                }
            };
            invocation(LOCAL_INBOX, json!({ "clear": clear }))
        }
        "access_list" => invocation(
            LOCAL_ACCESS,
            json!({
                "action": "ls",
                "actor": input.get("actor").and_then(Value::as_str),
            }),
        ),
        "access_grant" => invocation(
            LOCAL_ACCESS,
            json!({
                "action": "grant",
                "actor": required(&input, "actor")?,
                "role": required(&input, "role")?,
                "project": optional(&input, "project"),
            }),
        ),
        "access_revoke" => invocation(
            LOCAL_ACCESS,
            json!({
                "action": "revoke",
                "grant_id": required(&input, "grant_id")?,
            }),
        ),
        _ => {
            let request: IssuesRequest = serde_json::from_value(input)
                .map_err(|error| Failure::new(format!("bad Issues request: {error}")))?;
            world_invocation(request)
        }
    }
}

/// Execute one Issues-owned local operation through generic host facilities.
pub fn execute<'a>(
    host: &'a dyn ClientHost,
    local: LocalInvocation,
    options: PresentationOptions,
) -> world_interface::ClientFuture<'a, ClientOutput> {
    Box::pin(async move {
        let request = decode(&local.operation, local.input)?;
        match request {
            IssuesHostRequest::Focus => run_focus(host, options).await,
            IssuesHostRequest::NewStart(request) => run_new_start(host, request, options).await,
            IssuesHostRequest::WorkState {
                action,
                reff,
                no_branch,
            } => run_work_state(host, action, reff, no_branch, options).await,
            IssuesHostRequest::Inbox { clear } => run_inbox(host, clear, options).await,
            IssuesHostRequest::WorldUpgrade => {
                let value = host
                    .call_control(HostControlRequest::WorldActivate {
                        world: issues::contract::world_id(),
                    })
                    .await?;
                Ok(control_output(value, options))
            }
            IssuesHostRequest::Access(access) => run_access(host, access, options).await,
            IssuesHostRequest::Attach {
                reff,
                file,
                comment,
            } => run_attach(host, reff, file, comment, options).await,
            IssuesHostRequest::AttachmentGet { reff, id, out } => {
                run_attachment_get(host, reff, id, out, options).await
            }
        }
    })
}

/// Name what a destructive Issues command would destroy.
///
/// `destructive_question` is built at parse time and can only echo the selector
/// the user typed — and for `lait issues delete` that selector is usually a ref
/// inferred from the git branch, which makes "delete T-1?" a question nobody can
/// answer. Reading the title first is the difference between a prompt and a
/// coin flip, so it happens before anyone is asked.
///
/// Best-effort by construction: a failed read returns the declared question
/// rather than blocking the confirmation on a lookup that only adds detail.
pub fn confirmation<'a>(
    host: &'a dyn ClientHost,
    invocation: &'a ClientInvocation,
) -> world_interface::ClientFuture<'a, Option<String>> {
    Box::pin(async move {
        let Some(question) = invocation.confirmation_question() else {
            return Ok(None);
        };
        let ClientInvocationKind::World(call) = invocation.kind() else {
            return Ok(Some(question.to_string()));
        };
        let Ok(IssuesRequest::IssueDelete { reff }) = crate::decode_call(call) else {
            return Ok(Some(question.to_string()));
        };
        let titled = match call_issues(host, IssuesRequest::IssueView { reff }).await {
            Ok(crate::IssuesResponse::Issue(view)) => format!("{question}  {}", view.title),
            _ => question.to_string(),
        };
        Ok(Some(titled))
    })
}

async fn call_issues(
    host: &dyn ClientHost,
    request: IssuesRequest,
) -> Result<crate::IssuesResponse, Failure> {
    let call = crate::encode_call(&request).map_err(|error| Failure::new(error.to_string()))?;
    let reply = host.call_world(call.clone()).await?;
    let value =
        crate::decode_reply(&call, reply).map_err(|error| Failure::new(error.to_string()))?;
    serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("decode Issues response: {error}")))
}

fn issues_output(response: &crate::IssuesResponse, options: PresentationOptions) -> ClientOutput {
    ClientOutput::new(
        serde_json::to_value(response).unwrap_or(Value::Null),
        Some(crate::presentation::render(response, options)),
    )
}

async fn run_inbox(
    host: &dyn ClientHost,
    clear: bool,
    options: PresentationOptions,
) -> Result<ClientOutput, Failure> {
    let response = call_issues(
        host,
        IssuesRequest::Inbox {
            watermark: read_inbox_watermark(host.local_root()),
        },
    )
    .await?;
    if clear && matches!(&response, crate::IssuesResponse::Inbox { .. }) {
        write_inbox_watermark(host.local_root(), now_seconds())
            .map_err(|error| Failure::new(format!("advance Issues inbox watermark: {error}")))?;
    }
    Ok(issues_output(&response, options))
}

async fn run_focus(
    host: &dyn ClientHost,
    options: PresentationOptions,
) -> Result<ClientOutput, Failure> {
    let inbox = call_issues(
        host,
        IssuesRequest::Inbox {
            watermark: read_inbox_watermark(host.local_root()),
        },
    )
    .await?;
    if matches!(&inbox, crate::IssuesResponse::Error { .. }) {
        return Ok(issues_output(&inbox, options));
    }
    let mine = call_issues(
        host,
        IssuesRequest::List {
            project: None,
            filter: crate::Filter {
                mine: true,
                status: None,
                label: None,
                milestone: None,
                all: false,
            },
        },
    )
    .await?;
    if matches!(&mine, crate::IssuesResponse::Error { .. }) {
        return Ok(issues_output(&mine, options));
    }

    let value = json!({ "kind": "focus", "inbox": inbox, "mine": mine });
    let stdout = if options.json {
        format!(
            "{}\n{}\n",
            serde_json::to_string(&inbox).unwrap_or_else(|_| "{}".into()),
            serde_json::to_string(&mine).unwrap_or_else(|_| "{}".into())
        )
    } else {
        let mut text = String::new();
        if let crate::IssuesResponse::Inbox {
            entries, unread, ..
        } = &inbox
        {
            if *unread > 0 {
                let heads: Vec<_> = entries
                    .iter()
                    .take(3)
                    .map(|entry| format!("{} {}", inbox_line_verb(entry), entry.reff))
                    .collect();
                text.push_str(&format!("Inbox ({unread}): {}\n", heads.join(" · ")));
            }
        }
        match &mine {
            crate::IssuesResponse::List { rows } if rows.is_empty() => text.push_str(
                "nothing assigned to you — grab something: `lait issues ls`, or file one: \
                 `lait issues new \"...\"`\n",
            ),
            crate::IssuesResponse::List { rows } => {
                for row in rows {
                    text.push_str(&format!(
                        "  {}  {:<10}  {}\n",
                        row.reff, row.status, row.title
                    ));
                }
            }
            _ => {}
        }
        text
    };
    Ok(ClientOutput::new(
        value,
        Some(Presentation {
            stdout,
            stderr: String::new(),
            exit_code: 0,
            failure: None,
            failure_message: None,
        }),
    ))
}

async fn run_new_start(
    host: &dyn ClientHost,
    request: IssuesRequest,
    options: PresentationOptions,
) -> Result<ClientOutput, Failure> {
    let response = call_issues(host, request).await?;
    match response {
        crate::IssuesResponse::Ref { reff } => {
            let prefix = reff.clone();
            let mut output =
                run_work_state(host, WorkStateAction::Start, reff, false, options).await?;
            if !options.json {
                if let Some(presentation) = output.presentation.as_mut() {
                    presentation.stdout.insert_str(0, &format!("{prefix}\n"));
                }
            }
            Ok(output)
        }
        other => Ok(issues_output(&other, options)),
    }
}

async fn run_work_state(
    host: &dyn ClientHost,
    action: WorkStateAction,
    reff: String,
    no_branch: bool,
    options: PresentationOptions,
) -> Result<ClientOutput, Failure> {
    let request = match action {
        WorkStateAction::Start => IssuesRequest::IssueStart { reff },
        WorkStateAction::Done => IssuesRequest::IssueDone { reff },
        WorkStateAction::Stop => IssuesRequest::IssueStop { reff },
    };
    let starting = matches!(action, WorkStateAction::Start);
    let response = call_issues(host, request).await?;
    let crate::IssuesResponse::Issue(issue) = &response else {
        return Ok(issues_output(&response, options));
    };
    if options.json {
        return Ok(issues_output(&response, options));
    }

    let mut stdout = format!("{}\n", workstate_line(issue));
    let mut stderr = String::new();
    if starting {
        stdout = format!("{}  · you\n", workstate_line(issue));
        if !no_branch {
            match checkout_issue_branch(issue) {
                Some(Ok(message)) => stdout.push_str(&format!("{message}\n")),
                Some(Err(message)) => stderr.push_str(&format!("({message})\n")),
                None => {}
            }
        }
    }
    Ok(ClientOutput::new(
        serde_json::to_value(&response).unwrap_or(Value::Null),
        Some(Presentation {
            stdout,
            stderr,
            exit_code: 0,
            failure: None,
            failure_message: None,
        }),
    ))
}

async fn run_access(
    host: &dyn ClientHost,
    access: AccessRequest,
    options: PresentationOptions,
) -> Result<ClientOutput, Failure> {
    let control = match access {
        AccessRequest::List { actor } => HostControlRequest::AssignmentList { actor },
        AccessRequest::Revoke { grant_id } => HostControlRequest::AssignmentRevoke { grant_id },
        AccessRequest::Grant {
            actor,
            role,
            project,
        } => {
            let response = call_issues(host, IssuesRequest::AccessPlan { role, project }).await?;
            let crate::IssuesResponse::AccessPlan { assignments } = response else {
                return Ok(issues_output(&response, options));
            };
            HostControlRequest::AssignmentGrant {
                actor,
                assignments: assignments
                    .into_iter()
                    .map(|assignment| HostAssignment {
                        world: assignment.world,
                        capability: assignment.capability,
                        resource: assignment.resource,
                    })
                    .collect(),
            }
        }
    };
    let value = host.call_control(control).await?;
    Ok(control_output(value, options))
}

async fn run_attach(
    host: &dyn ClientHost,
    reff: String,
    path: String,
    comment: Option<String>,
    options: PresentationOptions,
) -> Result<ClientOutput, Failure> {
    // Two steps, and the order is the contract: the content is committed
    // first, then the issue names it. The substrate refuses a declaration whose
    // descriptor is not committed, so doing it the other way round does not
    // race — it simply fails.
    //
    // The file is never read into this process. `call_content` streams it, so
    // `lait issues attach` on a gigabyte costs a gigabyte of disk and a
    // quarter-megabyte of memory.
    let name = Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let stored = host
        .call_content(world_interface::HostContentRequest::Write {
            path: std::path::PathBuf::from(&path),
        })
        .await?;
    let content = stored
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| Failure::new("the content plane stored the file but did not name it"))?
        .to_string();
    let size = stored
        .get("size")
        .and_then(|s| s.as_u64())
        .unwrap_or_default();
    let response = call_issues(
        host,
        IssuesRequest::Attach {
            reff,
            mime: Some(mime_for(&name)),
            name,
            content,
            size,
            comment,
        },
    )
    .await?;
    Ok(issues_output(&response, options))
}

/// Where `attachment get` writes, given what the caller asked for and what the
/// attachment calls itself.
///
/// The stored display name is authored by whoever attached the file, which on a
/// synced issue is any peer — so it is a path supplied by a remote party, and a
/// path supplied by a remote party is not a path. Untreated it is an arbitrary
/// write triggered by a local user running an ordinary read command, and the
/// CLI's own output invites exactly that command.
///
/// `--out` passes through untouched. That one the caller typed, and a caller
/// naming a directory of their own is the feature.
fn destination_for(out: Option<String>, stored_name: &str) -> String {
    out.unwrap_or_else(|| world_interface::destination::sanitize_display_name(stored_name))
}

async fn run_attachment_get(
    host: &dyn ClientHost,
    reff: String,
    id: String,
    out: Option<String>,
    options: PresentationOptions,
) -> Result<ClientOutput, Failure> {
    let response = call_issues(host, IssuesRequest::AttachmentGet { reff, id }).await?;
    let crate::IssuesResponse::Attachment {
        name,
        content,
        data_b64,
        ..
    } = response
    else {
        return Ok(issues_output(&response, options));
    };
    let destination = destination_for(out, &name);
    // Which era this record is from decides how it is saved, and both are
    // permanent. An inline record is bytes in a Body and always will be — the
    // files are in the field, and a reader that refused them would lose them
    // rather than migrate them. A content record is streamed, so a large
    // attachment is never held in memory here.
    let written = match (content, data_b64) {
        (Some(content), _) => {
            let saved = host
                .call_content(world_interface::HostContentRequest::Read {
                    content,
                    destination: std::path::PathBuf::from(&destination),
                })
                .await?;
            saved
                .get("size")
                .and_then(|s| s.as_u64())
                .unwrap_or_default()
        }
        (None, Some(data_b64)) => {
            let bytes = data_encoding::BASE64
                .decode(data_b64.as_bytes())
                .map_err(|_| Failure::new("stored attachment did not decode"))?;
            std::fs::write(&destination, &bytes)
                .map_err(|error| Failure::new(format!("could not write {destination}: {error}")))?;
            bytes.len() as u64
        }
        (None, None) => {
            return Err(Failure::new(
                "this attachment record carries neither bytes nor a content id",
            ))
        }
    };
    let message = format!("saved {written} bytes to {destination}");
    let value = json!({ "kind": "ok", "message": message });
    let stdout = if options.json {
        format!(
            "{}\n",
            serde_json::to_string(&value).unwrap_or_else(|_| "{}".into())
        )
    } else {
        format!("{message}\n")
    };
    Ok(ClientOutput::new(
        value,
        Some(Presentation {
            stdout,
            stderr: String::new(),
            exit_code: 0,
            failure: None,
            failure_message: None,
        }),
    ))
}

fn control_output(value: Value, options: PresentationOptions) -> ClientOutput {
    let kind = value.get("kind").and_then(Value::as_str);
    let error_message = (kind == Some("error"))
        .then(|| {
            value
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .flatten();
    let (exit_code, failure) = match value.get("error_kind").and_then(Value::as_str) {
        Some("not_found") => (2, Some(PresentationFailure::InvalidRequest)),
        Some("denied") => (1, Some(PresentationFailure::InvalidRequest)),
        Some("error") => (1, Some(PresentationFailure::Internal)),
        _ => (0, None),
    };
    let (stdout, stderr) = if options.json {
        (
            format!(
                "{}\n",
                serde_json::to_string(&value).unwrap_or_else(|_| "{}".into())
            ),
            String::new(),
        )
    } else if let Some(message) = &error_message {
        (String::new(), format!("error: {message}\n"))
    } else if kind == Some("assignments") {
        let mut text = String::new();
        let rows = value
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if rows.is_empty() {
            text.push_str("(no effective assignments)\n");
        }
        for row in rows {
            let grant = row
                .get("grant_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let scope = row
                .get("resource")
                .and_then(Value::as_array)
                .map(|segments| {
                    segments
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .filter(|scope| !scope.is_empty())
                .unwrap_or_else(|| "space".into());
            text.push_str(&format!(
                "{}  {:<24} {:<28} {}\n",
                &grant[..12.min(grant.len())],
                row.get("capability")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
                scope,
                row.get("actor").and_then(Value::as_str).unwrap_or_default()
            ));
        }
        (text, String::new())
    } else {
        (
            format!(
                "{}\n",
                value.get("message").and_then(Value::as_str).unwrap_or("ok")
            ),
            String::new(),
        )
    };
    ClientOutput::new(
        value,
        Some(Presentation {
            stdout,
            stderr,
            exit_code,
            failure,
            failure_message: error_message,
        }),
    )
}

fn workstate_line(issue: &issues::dto::IssueView) -> String {
    let handle = issue.key_alias.as_deref().unwrap_or(&issue.reff);
    format!("{handle}  {}  {}", issue.title, issue.status)
}

fn checkout_issue_branch(issue: &issues::dto::IssueView) -> Option<Result<String, String>> {
    let in_repo = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !in_repo {
        return None;
    }
    let name = branch_name_for(issue);
    let created = Command::new("git")
        .args(["switch", "-c", &name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    let switched = created
        || Command::new("git")
            .args(["switch", &name])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    Some(if switched {
        Ok(format!(
            "{} branch '{name}'",
            if created {
                "switched to new"
            } else {
                "switched to"
            }
        ))
    } else {
        Err(format!(
            "could not create/switch branch '{name}' — continue manually"
        ))
    })
}

fn branch_name_for(issue: &issues::dto::IssueView) -> String {
    let handle = issue
        .key_alias
        .clone()
        .unwrap_or_else(|| issue.reff.clone())
        .to_ascii_lowercase();
    let mut slug = String::new();
    for character in issue.title.to_ascii_lowercase().chars() {
        if slug.len() >= 40 {
            break;
        }
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        handle
    } else {
        format!("{handle}-{slug}")
    }
}

fn inbox_line_verb(entry: &issues::dto::InboxEntry) -> String {
    let who = entry.actor_nick.clone().unwrap_or_else(|| "someone".into());
    match entry.kind.as_str() {
        "assigned" => format!("{who} assigned you"),
        "comment" => format!("{who} commented on"),
        _ => format!("{who} moved"),
    }
}

fn mime_for(name: &str) -> String {
    let extension = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// Expand one pinned Issues role into the exact authority assignments that the
/// Space host must commit.
pub fn plan_access_grant(
    session: &runtime::Session,
    role: &str,
    project: Option<&str>,
) -> Result<AccessGrantPlan, AccessRefusal> {
    let Some(view) = crate::projections::query_json(
        session,
        issues::contract::IssueQuery::RoleShow {
            role: role.to_string(),
        },
    ) else {
        return Err(AccessRefusal::NotFound(format!(
            "no role `{role}` in this space"
        )));
    };
    let conflicts = view["conflict_heads"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !conflicts.is_empty() {
        return Err(AccessRefusal::Invalid(format!(
            "role `{role}` has {} concurrent revision heads — resolve them with \
             `lait issues role resolve` before assigning",
            conflicts.len()
        )));
    }
    let Some(revision) = view.get("revision").filter(|revision| !revision.is_null()) else {
        return Err(AccessRefusal::NotFound(format!(
            "role `{role}` has no usable revision"
        )));
    };
    let body = &revision["body"];
    if body["tombstone"].as_bool() == Some(true) {
        return Err(AccessRefusal::Invalid(format!(
            "role `{role}` is tombstoned"
        )));
    }
    let scope_kind = body["scope_kind"].as_str().unwrap_or("space");
    let world = issues::contract::PRODUCT_WORLD;
    let resource = match (scope_kind, project) {
        ("space", None) => mechanics::demand::Resource::root(world),
        ("space", Some(_)) => {
            return Err(AccessRefusal::Invalid(
                "that is a Space role — it takes no --project".into(),
            ));
        }
        ("project", Some(selector)) => {
            let Some(snapshot) =
                crate::projections::query_json(session, issues::contract::IssueQuery::Snapshot)
            else {
                return Err(AccessRefusal::Invalid(
                    "the Issues catalog is unavailable".into(),
                ));
            };
            let projects = snapshot["catalog"]["projects"].as_object().cloned();
            let resolved = projects.and_then(|projects| {
                let upper = selector.to_ascii_uppercase();
                if projects.contains_key(selector) {
                    return Some(selector.to_string());
                }
                projects
                    .iter()
                    .find(|(_, metadata)| metadata["key"].as_str() == Some(upper.as_str()))
                    .map(|(id, _)| id.clone())
            });
            let Some(id) = resolved else {
                return Err(AccessRefusal::NotFound(format!(
                    "no project matches '{selector}'"
                )));
            };
            mechanics::demand::Resource::segments(world, [&id])
                .map_err(|error| AccessRefusal::Invalid(error.to_string()))?
        }
        ("project", None) => {
            return Err(AccessRefusal::Invalid(
                "that is a Project role — pass -p <project>".into(),
            ));
        }
        _ => {
            return Err(AccessRefusal::Invalid(
                "unrecognized Issues role scope".into(),
            ));
        }
    };
    let assignments: Vec<crate::AccessAssignment> = body["capabilities"]
        .as_array()
        .map(|capabilities| {
            capabilities
                .iter()
                .filter_map(Value::as_str)
                .map(|capability| crate::AccessAssignment {
                    world: world.to_string(),
                    capability: capability.to_string(),
                    resource: resource.segments.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    if assignments.is_empty() {
        return Err(AccessRefusal::Invalid(format!(
            "role `{role}` expands to no capabilities"
        )));
    }
    Ok(AccessGrantPlan { assignments })
}

fn required(value: &Value, field: &str) -> Result<String, Failure> {
    optional(value, field)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Failure::new(format!("Issues invocation is missing '{field}'")))
}

fn optional(value: &Value, field: &str) -> Option<String> {
    value.get(field).and_then(Value::as_str).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_peer_named_attachment_is_saved_beside_us_not_wherever_it_asked() {
        for hostile in [
            r"..\..\..\Users\Public\startup.bat",
            "../../../../etc/cron.d/evil",
            r"C:\Windows\System32\evil.dll",
        ] {
            let chosen = destination_for(None, hostile);
            let path = Path::new(&chosen);
            assert!(path.is_relative(), "{hostile:?} became {chosen:?}");
            assert_eq!(
                path.components().count(),
                1,
                "{hostile:?} became {chosen:?}, which is more than one component"
            );
        }
    }

    #[test]
    fn an_explicit_out_is_the_callers_own_choice_and_is_left_alone() {
        // The whole point of the flag: a path the person typed, including one
        // that walks up out of the working directory.
        let chosen = destination_for(Some("../saved/report.pdf".into()), "notes.txt");
        assert_eq!(chosen, "../saved/report.pdf");
    }

    #[test]
    fn an_ordinary_name_still_saves_under_its_own_name() {
        assert_eq!(destination_for(None, "report.pdf"), "report.pdf");
    }

    #[test]
    fn decodes_work_state_at_the_product_boundary() {
        let request = decode(
            LOCAL_WORK_STATE,
            json!({"action": "start", "reff": "ENG-7", "no_branch": true}),
        )
        .unwrap();
        assert!(matches!(
            request,
            IssuesHostRequest::WorkState {
                action: WorkStateAction::Start,
                ref reff,
                no_branch: true,
            } if reff == "ENG-7"
        ));
    }

    #[test]
    fn rejects_incomplete_access_grants_before_the_host_sees_them() {
        let error = decode(LOCAL_ACCESS, json!({"action": "grant", "actor": "alice"})).unwrap_err();
        assert!(error.to_string().contains("missing 'role'"));
    }

    #[test]
    fn web_inbox_clear_is_a_complete_command_not_a_world_query() {
        let read = parse_web(json!({"cmd": "inbox"})).unwrap();
        assert_eq!(read.access(), ClientAccess::Query);

        let clear = parse_web(json!({"cmd": "inbox", "clear": true})).unwrap();
        assert_eq!(clear.access(), ClientAccess::Command);
    }

    #[test]
    fn malformed_world_input_keeps_its_product_error() {
        let error = parse_web(json!({"cmd": "issue_new"})).unwrap_err();
        assert!(error.to_string().contains("bad Issues request"));
        assert!(error.to_string().contains("title"));
    }
}
