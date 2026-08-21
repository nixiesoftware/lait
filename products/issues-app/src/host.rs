#![allow(
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "attachment sizes report a bounded length as u64; projection reads index serde_json::Value, whose Index impl yields Null rather than panicking"
)]
//! Typed host capabilities requested by Issues client interfaces.
//!
//! These operations need facilities outside a World Session — local files, read
//! watermarks, Space authority, the reviewed-implementation switch. The product
//! owns their vocabulary and validation; the navigation shell only supplies the
//! facility.

use std::path::Path;

use runtime::world::call::Access;
use serde_json::{json, Value};
use world_interface::{
    ClientAccess, ClientHost, ClientInvocation, ClientInvocationKind, Failure, HostAssignment,
    HostControlRequest, LocalInvocation,
};

use crate::IssuesRequest;

// The operation-name vocabulary of the local-invocation plane. It lives beside
// [`decode`], the only code that interprets it: the names and the match that
// reads them are one thing, and their previous home in a CLI parser was an
// accident of which head happened to be written first.
pub const LOCAL_INBOX: &str = "issues.inbox";
pub const LOCAL_ATTACH: &str = "issues.attach";
pub const LOCAL_ATTACHMENT_GET: &str = "issues.attachment_get";
pub const LOCAL_ACCESS: &str = "issues.access";
pub const LOCAL_WORK: &str = "issues.work";

#[derive(Debug, Clone)]
pub enum IssuesWorkAction {
    Inspect,
    Watch {
        known_heads: Vec<runtime::exec::EventId>,
    },
    Cancel,
    Continue,
    Resume {
        checkpoint: replica::content::ContentRef,
    },
}

#[derive(Debug, Clone)]
pub struct IssuesWorkRequest {
    pub run: runtime::exec::RunId,
    pub action: IssuesWorkAction,
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
    Inbox {
        clear: bool,
        page: issues::contract::PageRequest,
        publication: Option<crate::PublicationCoordinate>,
    },
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
    Work(IssuesWorkRequest),
}

impl IssuesHostRequest {
    /// Classify the complete host operation, including caller-local effects.
    ///
    /// This must stay exhaustive: a new host capability cannot silently inherit
    /// command access or be mistaken for a read-only World query.
    pub fn access(&self) -> ClientAccess {
        match self {
            Self::Inbox { clear: false, .. } | Self::Access(AccessRequest::List { .. }) => {
                ClientAccess::Query
            }
            Self::Work(IssuesWorkRequest {
                action: IssuesWorkAction::Inspect | IssuesWorkAction::Watch { .. },
                ..
            }) => ClientAccess::Query,
            Self::Inbox { clear: true, .. }
            | Self::Access(AccessRequest::Grant { .. } | AccessRequest::Revoke { .. })
            | Self::Attach { .. }
            | Self::AttachmentGet { .. }
            | Self::Work(_) => ClientAccess::Command,
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

/// Unix seconds now. Delegates to `mechanics::wallclock` so tests can
/// freeze it, and so the pre-epoch decision lives in one place rather
/// than in four copies of this function.
pub fn now_seconds() -> u64 {
    mechanics::wallclock::now_secs()
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
        LOCAL_INBOX => Ok(IssuesHostRequest::Inbox {
            clear: input.get("clear").and_then(Value::as_bool).unwrap_or(false),
            page: input
                .get("page")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| Failure::new(format!("decode inbox page: {error}")))?
                .unwrap_or_default(),
            publication: input
                .get("publication")
                .filter(|value| !value.is_null())
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|error| Failure::new(format!("decode inbox publication: {error}")))?,
        }),
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
        LOCAL_WORK => {
            let run = runtime::exec::RunId::from_bytes(hex::<16>(&required(&input, "run")?)?);
            let action = match required(&input, "action")?.as_str() {
                "inspect" => IssuesWorkAction::Inspect,
                "watch" => {
                    let mut known_heads = input
                        .get("heads")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|head| {
                            head.as_str()
                                .ok_or_else(|| Failure::new("Work heads must be strings"))
                                .and_then(hex::<32>)
                                .map(runtime::exec::EventId::from_bytes)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    known_heads.sort_unstable();
                    known_heads.dedup();
                    IssuesWorkAction::Watch { known_heads }
                }
                "cancel" => IssuesWorkAction::Cancel,
                "continue" => IssuesWorkAction::Continue,
                "resume" => IssuesWorkAction::Resume {
                    checkpoint: replica::content::ContentRef {
                        content_id: hex::<32>(&required(&input, "checkpoint")?)?,
                    },
                },
                other => {
                    return Err(Failure::new(format!(
                        "unsupported Issues Work action '{other}'"
                    )));
                }
            };
            Ok(IssuesHostRequest::Work(IssuesWorkRequest { run, action }))
        }
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
///
/// The access class is read off the request this call is encoded from — the
/// same [`IssuesRequest::access`] that `IssuesCallHandler::access` runs on the
/// daemon's side of the boundary, so the head's copy and the daemon's cannot
/// describe the same bytes differently. It is here for *head* policy only; the
/// daemon's own classification is what authorization consults, and it is
/// derived after the call arrives rather than taken from anything that
/// travelled with it.
pub fn world_invocation(request: IssuesRequest) -> Result<ClientInvocation, Failure> {
    let confirmation = request.destructive_question();
    let access = match request.access() {
        Access::Query => ClientAccess::Query,
        Access::Command => ClientAccess::Command,
    };
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
            invocation(
                LOCAL_INBOX,
                json!({
                    "clear": clear,
                    "page": input.get("page").cloned().unwrap_or_else(|| json!({})),
                    "publication": input.get("publication").cloned(),
                }),
            )
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
        "work" => invocation(LOCAL_WORK, input),
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
) -> world_interface::ClientFuture<'a, Value> {
    Box::pin(async move {
        let request = decode(&local.operation, local.input)?;
        match request {
            IssuesHostRequest::Inbox {
                clear,
                page,
                publication,
            } => run_inbox(host, clear, page, publication).await,
            IssuesHostRequest::Access(access) => run_access(host, access).await,
            IssuesHostRequest::Attach {
                reff,
                file,
                comment,
            } => run_attach(host, reff, file, comment).await,
            IssuesHostRequest::AttachmentGet { reff, id, out } => {
                run_attachment_get(host, reff, id, out).await
            }
            IssuesHostRequest::Work(request) => run_work(host, request).await,
        }
    })
}

async fn run_work(host: &dyn ClientHost, request: IssuesWorkRequest) -> Result<Value, Failure> {
    let world = issues::contract::world_id();
    let request = match request.action {
        IssuesWorkAction::Inspect => runtime::exec::WorkRequest::Inspect {
            world,
            run: request.run,
        },
        IssuesWorkAction::Watch { known_heads } => runtime::exec::WorkRequest::Watch {
            world,
            run: request.run,
            known_heads,
        },
        IssuesWorkAction::Cancel => runtime::exec::WorkRequest::Cancel {
            world,
            run: request.run,
        },
        IssuesWorkAction::Continue => runtime::exec::WorkRequest::Retry {
            world,
            run: request.run,
        },
        IssuesWorkAction::Resume { checkpoint } => runtime::exec::WorkRequest::Resume {
            world,
            run: request.run,
            checkpoint,
        },
    };
    let value = host.call_work(request).await?;
    if crate::classify_failure(&value).is_some() {
        return Ok(value);
    }
    let reply = serde_json::from_value(value)
        .map_err(|error| Failure::new(format!("decode Runtime Work reply: {error}")))?;
    Ok(work_output(reply))
}

fn work_output(reply: runtime::exec::WorkReply) -> Value {
    match reply {
        runtime::exec::WorkReply::Unchanged { world, run, heads } => json!({
            "kind": "work_unchanged",
            "world": world.as_str(),
            "run": hex_bytes(run.as_bytes()),
            "heads": heads.into_iter().map(|head| hex_bytes(head.as_bytes())).collect::<Vec<_>>(),
        }),
        runtime::exec::WorkReply::State(state) => json!({
            "kind": "work",
            "world": state.world.as_str(),
            "run": hex_bytes(state.run.as_bytes()),
            "spec": { "name": state.spec.name.as_str(), "version": state.spec.version },
            "build": hex_bytes(state.build.as_bytes()),
            "heads": state.heads.into_iter().map(|head| hex_bytes(head.as_bytes())).collect::<Vec<_>>(),
            "event_count": state.event_count,
            "unresolved": state.unresolved,
            "cancel_asked": state.cancel_asked.into_iter().map(|event| hex_bytes(event.as_bytes())).collect::<Vec<_>>(),
            "attempts": state.attempts.into_iter().map(|attempt| json!({
                "attempt": hex_bytes(attempt.attempt.as_bytes()),
                "station": attempt.station.to_string(),
                "build": hex_bytes(attempt.build.as_bytes()),
                "offer": attempt.offer.map(|id| hex_bytes(id.as_bytes())),
                "began": attempt.began.into_iter().map(|event| hex_bytes(event.as_bytes())).collect::<Vec<_>>(),
                "checkpoints": attempt.checkpoints.into_iter().map(|fact| json!({
                    "event": hex_bytes(fact.event.as_bytes()),
                    "content": hex_bytes(fact.checkpoint.content.content_id),
                    "build": hex_bytes(fact.checkpoint.build.as_bytes()),
                    "sequence": fact.checkpoint.sequence,
                })).collect::<Vec<_>>(),
                "returned": attempt.returned.into_iter().map(|fact| json!({
                    "event": hex_bytes(fact.event.as_bytes()),
                    "terminal": match fact.terminal {
                        runtime::exec::TerminalClass::Succeeded => "succeeded",
                        runtime::exec::TerminalClass::ApplicationFailed => "application_failed",
                    },
                    "output": fact
                        .output_content
                        .into_iter()
                        .map(|content| hex_bytes(content.content_id))
                        .collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "failed": attempt.failed.into_iter().map(|fact| json!({
                    "event": hex_bytes(fact.event.as_bytes()),
                    "class": match fact.class {
                        runtime::exec::FailureClass::Handler => "handler",
                        runtime::exec::FailureClass::Backend => "backend",
                        runtime::exec::FailureClass::Protocol => "protocol",
                        runtime::exec::FailureClass::Deadline => "deadline",
                        runtime::exec::FailureClass::Fence => "fence",
                        runtime::exec::FailureClass::Unknown => "unknown",
                    },
                })).collect::<Vec<_>>(),
                "cancelled": attempt.cancelled.into_iter().map(|event| hex_bytes(event.as_bytes())).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "accepted": state.accepted.into_iter().map(|fact| json!({
                "event": hex_bytes(fact.event.as_bytes()),
                "attempt": hex_bytes(fact.attempt.as_bytes()),
            })).collect::<Vec<_>>(),
            "rejected": state.rejected.into_iter().map(|fact| json!({
                "event": hex_bytes(fact.event.as_bytes()),
                "attempt": hex_bytes(fact.attempt.as_bytes()),
            })).collect::<Vec<_>>(),
        }),
    }
}

fn hex<const N: usize>(value: &str) -> Result<[u8; N], Failure> {
    let encoded_len = N.saturating_mul(2);
    data_encoding::HEXLOWER
        .decode(value.as_bytes())
        .ok()
        .and_then(|bytes| <[u8; N]>::try_from(bytes.as_slice()).ok())
        .ok_or_else(|| Failure::new(format!("expected {encoded_len} lowercase hex characters")))
}

fn hex_bytes<const N: usize>(value: [u8; N]) -> String {
    data_encoding::HEXLOWER.encode(&value)
}

/// Name what a destructive Issues command would destroy.
///
/// `destructive_question` is built at parse time and can only echo the selector
/// the caller sent, which makes "delete T-1?" a question nobody can answer —
/// nothing about a ref says which issue it is. Reading the title first is the
/// difference between a prompt and a coin flip, so it happens before anyone is
/// asked.
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

fn issues_output(response: &crate::IssuesResponse) -> Value {
    serde_json::to_value(response).unwrap_or(Value::Null)
}

async fn run_inbox(
    host: &dyn ClientHost,
    clear: bool,
    page: issues::contract::PageRequest,
    publication: Option<crate::PublicationCoordinate>,
) -> Result<Value, Failure> {
    let response = call_issues(
        host,
        IssuesRequest::Inbox {
            watermark: read_inbox_watermark(host.local_root()),
            page,
            publication,
        },
    )
    .await?;
    if clear && matches!(&response, crate::IssuesResponse::Inbox { .. }) {
        write_inbox_watermark(host.local_root(), now_seconds())
            .map_err(|error| Failure::new(format!("advance Issues inbox watermark: {error}")))?;
    }
    Ok(issues_output(&response))
}

async fn run_access(host: &dyn ClientHost, access: AccessRequest) -> Result<Value, Failure> {
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
                return Ok(issues_output(&response));
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
    host.call_control(control).await
}

async fn run_attach(
    host: &dyn ClientHost,
    reff: String,
    path: String,
    comment: Option<String>,
) -> Result<Value, Failure> {
    // Two steps, and the order is the contract: the content is committed
    // first, then the issue names it. The substrate refuses a declaration whose
    // descriptor is not committed, so doing it the other way round does not
    // race — it simply fails.
    //
    // The file is never read into this process. `call_content` streams it, so
    // attaching a gigabyte costs a gigabyte of disk and a quarter-megabyte of
    // memory.
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
    Ok(issues_output(&response))
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
/// An explicit `out` passes through untouched. That one the caller typed, and a
/// caller naming a directory of their own is the feature.
fn destination_for(out: Option<String>, stored_name: &str) -> String {
    out.unwrap_or_else(|| world_interface::destination::sanitize_display_name(stored_name))
}

async fn run_attachment_get(
    host: &dyn ClientHost,
    reff: String,
    id: String,
    out: Option<String>,
) -> Result<Value, Failure> {
    let response = call_issues(host, IssuesRequest::AttachmentGet { reff, id }).await?;
    let crate::IssuesResponse::Attachment {
        name,
        content,
        data_b64,
        ..
    } = response
    else {
        return Ok(issues_output(&response));
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
    Ok(json!({
        "kind": "ok",
        "message": format!("saved {written} bytes to {destination}"),
        "path": destination,
        "size": written,
    }))
}

/// The type an attachment is recorded under, from its name.
///
/// The extension and nothing else. This runs before a byte is read and its
/// answer is product metadata, not a claim about the bytes — `serve::content`
/// serves every attachment as `application/octet-stream` with `nosniff`
/// regardless, so a wrong guess here misnames a row and cannot mis-render one.
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
        // Video and audio were absent entirely, so an `.mp4` attached as
        // `application/octet-stream` — the system had no opinion about video at
        // the one boundary that names a file. It is also the boundary where a
        // catalog gets derived, and a row that cannot say it is a film is a
        // poor place to start.
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
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
            "role `{role}` has {} concurrent revision heads — resolve them under \
             Settings → Workflow (or with the `issues_role_resolve` tool) before assigning",
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
        ("space", None) => mechanics::authorization::Resource::root(world),
        ("space", Some(_)) => {
            return Err(AccessRefusal::Invalid(
                "that is a Space role — it takes no --project".into(),
            ));
        }
        ("project", Some(selector)) => {
            let Some(value) = crate::projections::query_json(
                session,
                issues::contract::IssueQuery::Resolve {
                    entity: issues::contract::ResolveEntity::Project,
                    selector: selector.to_string(),
                    project: None,
                },
            ) else {
                return Err(AccessRefusal::NotFound(format!(
                    "no project matches '{selector}'"
                )));
            };
            let resolved: issues::contract::ResolvedEntity = serde_json::from_value(value)
                .map_err(|_| {
                    AccessRefusal::Invalid("the Issues selector reply is invalid".into())
                })?;
            mechanics::authorization::Resource::segments(world, [&resolved.id])
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
    let capabilities = body["capabilities"].as_array().cloned().unwrap_or_default();
    if capabilities.is_empty() || capabilities.len() > issues::roles::MAX_CAPABILITIES {
        return Err(AccessRefusal::Invalid(format!(
            "role `{role}` exceeds the bounded capability registry"
        )));
    }
    let assignments: Vec<crate::AccessAssignment> = capabilities
        .iter()
        .filter_map(Value::as_str)
        .map(|capability| crate::AccessAssignment {
            world: world.to_string(),
            capability: capability.to_string(),
            resource: resource.segments.clone(),
        })
        .collect();
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
    use std::sync::Mutex;

    use super::*;

    struct WorkHost {
        request: Mutex<Option<runtime::exec::WorkRequest>>,
    }

    impl ClientHost for WorkHost {
        fn local_root(&self) -> &Path {
            Path::new(".")
        }

        fn call_world<'a>(
            &'a self,
            _call: runtime::world::call::Call,
        ) -> world_interface::ClientFuture<'a, runtime::world::call::Reply> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_work<'a>(
            &'a self,
            request: runtime::exec::WorkRequest,
        ) -> world_interface::ClientFuture<'a, Value> {
            *self.request.lock().expect("request") = Some(request.clone());
            Box::pin(async move {
                serde_json::to_value(runtime::exec::WorkReply::Unchanged {
                    world: request.world().clone(),
                    run: request.run(),
                    heads: Vec::new(),
                })
                .map_err(|error| Failure::new(error.to_string()))
            })
        }

        fn call_control<'a>(
            &'a self,
            _request: HostControlRequest,
        ) -> world_interface::ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_content<'a>(
            &'a self,
            _request: world_interface::HostContentRequest,
        ) -> world_interface::ClientFuture<'a, Value> {
            Box::pin(async { Err(Failure::refusal()) })
        }

        fn call_identity<'a>(
            &'a self,
            _handles: Vec<world_interface::PresentationHandle>,
        ) -> world_interface::ClientFuture<'a, world_interface::PresentationResolution> {
            Box::pin(async { Ok(world_interface::PresentationResolution::unavailable()) })
        }
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        let waker = std::task::Waker::noop();
        let mut context = std::task::Context::from_waker(waker);
        match future.as_mut().poll(&mut context) {
            std::task::Poll::Ready(value) => value,
            std::task::Poll::Pending => panic!("test Work host must not pend"),
        }
    }

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
    fn decodes_a_local_file_attach_at_the_product_boundary() {
        let request = decode(
            LOCAL_ATTACH,
            json!({"reff": "ENG-7", "file": "notes.txt", "comment": "see this"}),
        )
        .unwrap();
        assert!(matches!(
            request,
            IssuesHostRequest::Attach {
                ref reff,
                ref file,
                comment: Some(ref comment),
            } if reff == "ENG-7" && file == "notes.txt" && comment == "see this"
        ));
    }

    #[test]
    fn rejects_incomplete_access_grants_before_the_host_sees_them() {
        let error = decode(LOCAL_ACCESS, json!({"action": "grant", "actor": "alice"})).unwrap_err();
        assert_eq!(error.kind(), world_interface::FailureKind::Invalid);
    }

    #[test]
    fn web_inbox_clear_is_a_complete_command_not_a_world_query() {
        let read = parse_web(json!({"cmd": "inbox"})).unwrap();
        assert_eq!(read.access(), ClientAccess::Query);

        let clear = parse_web(json!({"cmd": "inbox", "clear": true})).unwrap();
        assert_eq!(clear.access(), ClientAccess::Command);
    }

    /// A head that serves reads only has to be able to tell a read from a
    /// write, and every World request classifies itself — otherwise "refuse
    /// what is not provably a query" refuses the entire product.
    #[test]
    fn a_world_call_classifies_itself_for_head_policy() {
        let read = parse_web(json!({"cmd": "issue_view", "reff": "ENG-1"})).unwrap();
        assert_eq!(read.access(), ClientAccess::Query);
        let board = parse_web(json!({"cmd": "board", "page": {}})).unwrap();
        assert_eq!(board.access(), ClientAccess::Query);

        let write = parse_web(json!({"cmd": "issue_start", "reff": "ENG-1"})).unwrap();
        assert_eq!(write.access(), ClientAccess::Command);
    }

    #[test]
    fn migration_is_not_a_public_tracker_command() {
        let error = parse_web(json!({"cmd": "world_upgrade"})).unwrap_err();
        assert_eq!(error.kind(), world_interface::FailureKind::Invalid);
    }

    #[test]
    fn malformed_world_input_is_a_typed_invalid_operation() {
        let error = parse_web(json!({"cmd": "issue_new"})).unwrap_err();
        assert_eq!(error.kind(), world_interface::FailureKind::Invalid);
    }

    #[test]
    fn issues_owns_the_control_vocabulary_and_calls_typed_work_without_root_protocol() {
        let run = runtime::exec::RunId::from_bytes([0x71; 16]);
        let host = WorkHost {
            request: Mutex::new(None),
        };
        let output = block_on(run_work(
            &host,
            IssuesWorkRequest {
                run,
                action: IssuesWorkAction::Cancel,
            },
        ))
        .unwrap();
        assert_eq!(output["kind"], "work_unchanged");
        assert!(matches!(
            host.request.lock().expect("request").as_ref(),
            Some(runtime::exec::WorkRequest::Cancel { world, run: actual })
                if world == &issues::contract::world_id() && actual == &run
        ));

        let inspect = invocation(
            LOCAL_WORK,
            json!({"action": "inspect", "run": hex_bytes(run.as_bytes())}),
        )
        .unwrap();
        assert_eq!(inspect.access(), ClientAccess::Query);
        let cancel = invocation(
            LOCAL_WORK,
            json!({"action": "cancel", "run": hex_bytes(run.as_bytes())}),
        )
        .unwrap();
        assert_eq!(cancel.access(), ClientAccess::Command);
        assert!(decode(
            LOCAL_WORK,
            json!({"action": "start", "run": hex_bytes(run.as_bytes())})
        )
        .is_err());
    }
    /// A film is recorded as a film.
    ///
    /// Video and audio were absent from this table entirely, so every `.mp4`
    /// attached as `application/octet-stream`. It is the boundary that names a
    /// file and the boundary where a catalog gets derived, and a row that cannot
    /// say it is a film is a poor place to start.
    #[test]
    fn an_attachment_is_named_by_its_extension_including_the_moving_kinds() {
        assert_eq!(mime_for("ribbon-cutting.mp4"), "video/mp4");
        assert_eq!(
            mime_for("CLIP.MOV"),
            "video/quicktime",
            "case is not meaning"
        );
        assert_eq!(mime_for("talk.m4a"), "audio/mp4");
        assert_eq!(mime_for("lobby.webm"), "video/webm");
        // The ones that were already right stay right.
        assert_eq!(mime_for("plan.pdf"), "application/pdf");
        assert_eq!(mime_for("shot.png"), "image/png");
        // An unknown extension, and a name with none at all, both fall back
        // rather than guessing.
        assert_eq!(mime_for("archive.wat"), "application/octet-stream");
        assert_eq!(mime_for("README"), "application/octet-stream");
    }
}
