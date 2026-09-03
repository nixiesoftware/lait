//! The Worker-side session host — the editor lane the viewer's
//! `workerSession.ts` adapter speaks to, answered over the same composed
//! engine the rpc lane drives. The frame vocabulary is OWNED by
//! `viewer/src/workerSession.ts` and mirrored here byte for byte: colon-tagged
//! frames (`session:open`…) under the `lait` tag, and a clone-safe mutation
//! outcome whose `errorKind` is camelCase — the client silently drops any
//! frame that does not match, so the serde renames below are load-bearing.
//!
//! What this host answers, and what it deliberately does not:
//!
//! - `mutate` mirrors the daemon's `socket_editor_rpc` allowlist verbatim
//!   (`issue_text_splice` | `issue_text_checkpoint` | `issue_view`, 403
//!   otherwise) and then runs the ordinary world seam — the same
//!   `BrowserEngine::world_rpc` path the rpc lane proved, raw operation
//!   envelope passed through intact (the unwrap lives client-side, once).
//! - `open` answers a `liveness: live` event: on this backend the "socket" is
//!   the engine in the same Worker, so reachable-at-all is live.
//! - `watch` is accepted and SILENT. `runtime::plane::live` does not exist on
//!   wasm32 — a tab has no transient table, no presence transport, no signal
//!   broadcast — so awareness (carets, typing, previews) structurally cannot
//!   cross here. The viewer treats silence on a watched question as
//!   "unchanged" and never gates `mutate` on it; peer edits still arrive
//!   through convergence and ring the doorbell. Synthesizing a local-only
//!   live view was considered and rejected: it would fabricate a presence
//!   table with no peers behind it.
//! - The `space` a mutate names is deliberately not enforced: one Space per
//!   tab, and a spelling mismatch here must never flip the viewer's
//!   wrong-head fallback toward an HTTP head that does not exist.
//!
//! A `sid` scopes every frame; a frame for an unknown or closed sid is
//! dropped, never an error — the client owns that contract (late frames are
//! ignored). One request can owe zero or several response frames, so the
//! entry point answers with a list.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::dispatch::BrowserEngine;

/// Frames the page sends — the mirror of `WorkerSessionRequest` in
/// `viewer/src/workerSession.ts`. `space` is accepted on the wire and
/// deliberately unread (see the module doc), so it is not declared.
#[derive(Debug, Deserialize)]
#[serde(tag = "lait")]
pub enum WorkerSessionRequest {
    #[serde(rename = "session:open")]
    Open { sid: u64 },
    #[serde(rename = "session:watch")]
    Watch {
        sid: u64,
        /// Held opaquely: the host has no live plane to evaluate a question
        /// against, and parsing it here would invent a schema nothing uses.
        question: serde_json::Value,
    },
    #[serde(rename = "session:mutate")]
    Mutate {
        sid: u64,
        rid: u64,
        request: serde_json::Value,
    },
    #[serde(rename = "session:close")]
    Close { sid: u64 },
}

/// Frames the host sends back — the mirror of `WorkerSessionResponse`.
#[derive(Debug, Serialize)]
#[serde(tag = "lait")]
pub enum WorkerSessionResponse {
    #[serde(rename = "session:event")]
    Event { sid: u64, event: serde_json::Value },
    #[serde(rename = "session:reply")]
    Reply {
        sid: u64,
        rid: u64,
        outcome: MutationOutcome,
    },
}

/// A mutation outcome in clone-safe data — no error type crosses, the client
/// rehydrates `SocketMutationError` from exactly these fields.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum MutationOutcome {
    Ok {
        ok: bool,
        status: u16,
        response: serde_json::Value,
    },
    Refused {
        ok: bool,
        status: u16,
        error: MutationError,
    },
}

#[derive(Debug, Serialize)]
pub struct MutationError {
    pub message: String,
    #[serde(rename = "errorKind")]
    pub error_kind: Option<String>,
}

impl MutationOutcome {
    fn accepted(status: u16, response: serde_json::Value) -> Self {
        Self::Ok {
            ok: true,
            status,
            response,
        }
    }
    fn refused(status: u16, message: &str) -> Self {
        Self::Refused {
            ok: false,
            status,
            error: MutationError {
                message: message.to_string(),
                error_kind: Some("error".to_string()),
            },
        }
    }
}

/// What phase 1 decided a frame needs. Split from the engine call so the
/// caller can release its borrow of the host before dispatching into the
/// engine — the same take-decide-drop discipline the handle's ring keeps.
pub enum Accepted {
    /// Answer these frames now; nothing engine-side to run.
    Respond(Vec<WorkerSessionResponse>),
    /// Run this editor request through the engine and reply to `rid`.
    Mutate {
        sid: u64,
        rid: u64,
        request: serde_json::Value,
    },
}

struct SessionState {
    /// The last watched question, held for the day a tab grows a live plane;
    /// today it only proves the watch was accepted.
    _question: Option<serde_json::Value>,
}

/// The open sessions behind one port. Single-threaded by construction — the
/// Worker calls in one frame at a time.
#[derive(Default)]
pub struct SessionHost {
    sessions: BTreeMap<u64, SessionState>,
}

impl SessionHost {
    /// Classify one frame against the session table. Everything that needs no
    /// engine work is answered here; a mutate for a live sid comes back as
    /// [`Accepted::Mutate`] for the caller to run after releasing this borrow.
    pub fn accept(&mut self, request: WorkerSessionRequest) -> Accepted {
        match request {
            WorkerSessionRequest::Open { sid } => {
                self.sessions.insert(sid, SessionState { _question: None });
                // On this backend the socket IS the engine in the same
                // Worker: reachable is live, and the client shows
                // "connecting" until told so.
                Accepted::Respond(vec![WorkerSessionResponse::Event {
                    sid,
                    event: serde_json::json!({"kind": "liveness", "liveness": "live"}),
                }])
            }
            WorkerSessionRequest::Watch { sid, question } => {
                if let Some(state) = self.sessions.get_mut(&sid) {
                    state._question = Some(question);
                }
                Accepted::Respond(Vec::new())
            }
            WorkerSessionRequest::Mutate { sid, rid, request } => {
                if self.sessions.contains_key(&sid) {
                    Accepted::Mutate { sid, rid, request }
                } else {
                    // A closed session's late mutate — dropped, never thrown;
                    // the client already rejected its own pending on close.
                    Accepted::Respond(Vec::new())
                }
            }
            WorkerSessionRequest::Close { sid } => {
                self.sessions.remove(&sid);
                Accepted::Respond(Vec::new())
            }
        }
    }
}

/// Run one accepted editor mutate through the engine and shape its reply
/// frame. The allowlist mirrors the daemon's `socket_editor_rpc` verbatim, so
/// the session lane cannot become a second, prompt-less RPC surface; past it,
/// the request runs the exact world seam the rpc lane proved, and the raw
/// operation envelope crosses intact for the client to unwrap.
pub fn mutate_reply(
    engine: &BrowserEngine,
    sid: u64,
    rid: u64,
    request: serde_json::Value,
) -> WorkerSessionResponse {
    let command = request.get("cmd").and_then(serde_json::Value::as_str);
    let outcome = if !matches!(
        command,
        Some("issue_text_splice" | "issue_text_checkpoint" | "issue_view")
    ) {
        MutationOutcome::refused(403, "the session socket accepts editor requests only")
    } else {
        match engine.world_rpc(request) {
            // A World signals a business REFUSAL — a conflict, a drifted splice,
            // an invalid request — in the reply BODY (`kind:"error"`), not as a
            // Rust `Err`, and the daemon carries it on a SUCCESSFUL frame:
            // `socket_editor_rpc` returns HTTP 200 for any executed reply, so
            // its socket sets `ok: status.is_success()` = true with the error
            // body inside (src/serve/socket.rs run_mutations, src/serve/mod.rs
            // world_rpc). The client resolves and inspects the body (the editor
            // re-reads and self-heals a drifted splice), so the tab MUST mirror
            // that: an executed reply is `ok:true`, whatever its body says. Only
            // an execution FAILURE (`Err`) is `ok:false` — the allowlist 403 is
            // the other, refused above before the World ran at all.
            Ok(body) => MutationOutcome::accepted(200, body),
            Err(failure) => MutationOutcome::refused(
                400,
                failure.diagnostic().unwrap_or("the World refused the call"),
            ),
        }
    };
    WorkerSessionResponse::Reply { sid, rid, outcome }
}
