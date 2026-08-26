//! The agent surface over HTTP, so a session is a connection rather than a
//! process an editor had to spawn.
//!
//! # What this replaces, and why
//!
//! `lait mcp` speaks stdio: the editor launches the process, and the two facts
//! that decide what the session *is* — which agent it acts as and which World
//! it speaks — are read once, from the environment, at construction. Changing
//! either means editing an editor's configuration file and restarting the
//! session. Every binding is therefore a per-client, per-project, per-World
//! file somebody has to manage, and every session start spawns a process that
//! builds a registry that launches every World's runner.
//!
//! A connection has none of those properties. The daemon is already long-lived
//! and identity-scoped; this hands an agent a way in.
//!
//! # Who may connect
//!
//! Two gates, and they answer different questions.
//!
//! **[`serve::auth::Guard`] answers "is this browser being used against us".**
//! The MCP specification requires a server to validate `Origin` on every
//! connection and to bind loopback when local, because a local HTTP port is
//! reachable by any page a victim visits: the page rebinds its own hostname to
//! 127.0.0.1 and drives the server cross-origin, enumerating tools and
//! exfiltrating their output. This is not hypothetical — it is the shape of
//! CVE-2026-63118 against another SDK's Streamable HTTP transport. lait already
//! had the mitigation for its own head, and reusing it is the point: one
//! allowlist, already tested, rather than a second one that drifts.
//!
//! **[`crate::agent_token`] answers "which agent is this".** A bearer
//! credential derived from the agent's seed, which the address book already
//! decided to sponsor. It confers no authority — standing is the Space's
//! answer, not this one's.
//!
//! Both must pass. Neither substitutes for the other: a valid token from a
//! rebound page is still a page driving the endpoint, and a same-origin caller
//! with no token is still nobody in particular.
//!
//! # Which agent a session acts as
//!
//! A first cut carried the identity in a task-local, set by the layer that
//! authenticated it and read by the SDK's handler factory. That was wrong, and
//! wrong in the way this project has been bitten by twice: every component
//! correct, the composition broken, and a symptom that names nothing.
//!
//! Tokio task-locals do not cross `tokio::spawn`, and rmcp spawns a session
//! worker — so every request after `initialize` runs outside the scope. The
//! read would return `None`, and `None` in `mcp::LaitMcp.act_as` means *the
//! primary identity*: the human whose machine hosts the daemon. An agent would
//! have silently acted as its sponsor. Nothing would have failed to compile and
//! nothing would have logged.
//!
//! So identity is not ambient. [`Session`] binds a session id to the agent that
//! opened it, established once when `initialize` is admitted and enforced on
//! every later request *before* the SDK sees it — because a session id rides in
//! a response header and is not a secret, while the token is.

use std::sync::Arc;

/// Everything the endpoint needs to decide who may connect.
#[derive(Clone)]
pub struct Access {
    /// This identity's home, where provisioned agents' seeds live.
    pub home: std::path::PathBuf,
    /// The rebinding allowlist this endpoint shares with the server hosting
    /// it — the allowlist alone, not the head's credential. This endpoint
    /// authenticates an agent, not the person who opened that window, and a
    /// struct holding a credential it never checks is an invitation to start
    /// checking it.
    pub guard: crate::serve::auth::OriginPolicy,
}

/// Why a connection was refused.
///
/// Separate variants because they are separate facts with separate remedies,
/// and because a caller that folds them together tells an attacker which half
/// it got right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// The `Origin`/`Host` allowlist refused: a page is being used against us.
    Origin,
    /// No credential was presented.
    Missing,
    /// A credential was presented and belongs to no agent this device sponsors.
    Unknown,
}

impl Refused {
    /// What the caller is told. Deliberately identical for `Missing` and
    /// `Unknown`: a presenter that has not authenticated must not learn whether
    /// a token merely *exists*, and the person debugging a real binding has the
    /// daemon's log, which does distinguish them.
    pub fn message(self) -> &'static str {
        match self {
            Refused::Origin => "Origin is cross-site; this endpoint is same-origin only",
            Refused::Missing | Refused::Unknown => {
                "present a sponsored agent's token as `Authorization: Bearer …`"
            }
        }
    }

    /// The status a caller sees. 403 for an origin refusal, matching what the
    /// MCP specification requires of an invalid `Origin`; 401 for a credential,
    /// which is the one a client can do something about.
    pub fn status(self) -> u16 {
        match self {
            Refused::Origin => 403,
            Refused::Missing | Refused::Unknown => 401,
        }
    }
}

impl Access {
    /// Decide whether one request may open or continue a session, and as whom.
    ///
    /// Origin first, deliberately. A rebound page presenting a stolen token
    /// must be refused as a page, not admitted as an agent — and refusing it
    /// before the credential is examined keeps the cheap check ahead of the
    /// linear one.
    pub fn admit(
        &self,
        host: Option<&str>,
        origin: Option<&str>,
        authorization: Option<&str>,
        fallback: Option<&str>,
    ) -> Result<String, Refused> {
        self.guard
            .check(host, origin)
            .map_err(|_| Refused::Origin)?;
        let presented =
            crate::agent_token::presented(authorization, fallback).ok_or(Refused::Missing)?;
        crate::agent_token::identify(&self.home, presented).ok_or(Refused::Unknown)
    }
}

tokio::task_local! {
    /// The agent the *current request* authenticated as.
    ///
    /// Read in exactly one place — [`current_agent`], called by the service
    /// factory — and captured into the handler it builds. It must never be read
    /// later.
    ///
    /// A first cut read it from handlers, which was wrong: tokio task-locals do
    /// not cross `tokio::spawn`, and rmcp spawns a session worker, so every
    /// request after `initialize` runs outside this scope. The read returned
    /// `None`, and `None` meant *the primary identity* — the human whose
    /// machine hosts the daemon. An agent would have silently acted as its
    /// sponsor with nothing failing to compile.
    ///
    /// The factory is different, and only the factory: it runs inside the
    /// request that created the session, before the worker is spawned. So this
    /// is sound exactly once, at construction, and the value is owned by the
    /// handler from then on.
    static REQUEST_AGENT: String;
}

/// The agent this request authenticated as.
///
/// For the service factory, at construction, and nothing else. `None` is not a
/// fallback to anybody — a caller that cannot name an agent must refuse.
pub fn current_agent() -> Option<String> {
    REQUEST_AGENT.try_with(|agent| agent.clone()).ok()
}

/// Gate one request, then run it with its agent established.
///
/// Origin first, then the credential, then the session's owner — each refusing
/// before the next is consulted, so a rebound page never reaches the token
/// check and a valid token never reaches somebody else's session.
pub async fn admit_request(
    access: Access,
    sessions: Sessions,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let headers = request.headers();
    let header = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    let agent = match access.admit(
        header("host"),
        header("origin"),
        header("authorization"),
        header("x-lait-agent-token"),
    ) {
        Ok(agent) => agent,
        Err(refused) => {
            tracing::warn!(reason = ?refused, "an agent session was refused");
            return refusal(refused);
        }
    };

    // A session id rides in a response header and lands in logs, so it is not a
    // secret. Presenting a valid token beside somebody else's session id must
    // not hand over their stream.
    if let Some(session) = header("mcp-session-id") {
        if !sessions.admits(session, &agent) {
            tracing::warn!(%agent, "an agent presented a session it did not open");
            return refusal(Refused::Unknown);
        }
    }

    let response = REQUEST_AGENT.scope(agent.clone(), next.run(request)).await;
    // Recorded from the response, because that is where a new session's id is
    // first stated — the initialize request could not have carried it.
    if let Some(session) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|value| value.to_str().ok())
    {
        sessions.opened(session, &agent);
    }
    response
}

fn refusal(refused: Refused) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::from_u16(refused.status())
            .unwrap_or(axum::http::StatusCode::FORBIDDEN),
        refused.message(),
    )
        .into_response()
}

/// Who opened which session.
///
/// lait owns this rather than the SDK, which has no notion of a principal.
/// `Access::admit` authenticates the *presenter*; without this, a second
/// sponsored agent could present its own valid token alongside the first
/// agent's session id and be handed that session's replayed stream. Two
/// sponsored agents on one device is the point of the address book, so this is
/// an ordinary configuration rather than an exotic one.
#[derive(Clone, Default)]
pub struct Sessions {
    bound: Arc<std::sync::Mutex<std::collections::HashMap<String, String>>>,
}

impl Sessions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `agent` opened `session`.
    pub fn opened(&self, session: &str, agent: &str) {
        if let Ok(mut bound) = self.bound.lock() {
            bound.insert(session.to_owned(), agent.to_owned());
        }
    }

    /// Whether `agent` may act on `session`.
    ///
    /// A session nobody recorded is refused rather than admitted: an id this
    /// device never issued is not one to resume, and treating unknown as
    /// allowed is how a session id becomes a credential.
    pub fn admits(&self, session: &str, agent: &str) -> bool {
        self.bound
            .lock()
            .ok()
            .and_then(|bound| bound.get(session).cloned())
            .is_some_and(|owner| owner == agent)
    }

    /// Forget a session, on an explicit DELETE or when it expires.
    pub fn closed(&self, session: &str) {
        if let Ok(mut bound) = self.bound.lock() {
            bound.remove(session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(home: &std::path::Path) -> Access {
        Access {
            home: home.to_path_buf(),
            guard: crate::serve::auth::Guard::new("head-token".into(), 7717).origin_policy(),
        }
    }

    fn provision(home: &std::path::Path, name: &str, seed: [u8; 32]) -> String {
        let dir = crate::registry::agents_base(home).join(name);
        std::fs::create_dir_all(&dir).expect("the agent directory");
        std::fs::write(
            dir.join("secret.key"),
            data_encoding::HEXLOWER.encode(&seed),
        )
        .expect("the seed");
        crate::agent_token::derive(&seed, 0)
    }

    #[test]
    fn a_sponsored_agent_with_its_token_is_admitted_as_itself() {
        let home = tempfile::tempdir().expect("a home");
        let token = provision(home.path(), "scribe", [5u8; 32]);
        let admitted = access(home.path()).admit(
            Some("127.0.0.1:7717"),
            None,
            Some(&format!("Bearer {token}")),
            None,
        );
        assert_eq!(admitted.as_deref(), Ok("scribe"));
    }

    /// The attack the MCP specification names, and the reason `Origin` is
    /// checked before anything else: a page that rebound its hostname to
    /// loopback is a page, whatever credential it managed to present.
    #[test]
    fn a_rebound_page_is_refused_even_holding_a_valid_token() {
        let home = tempfile::tempdir().expect("a home");
        let token = provision(home.path(), "scribe", [5u8; 32]);
        let refused = access(home.path()).admit(
            Some("127.0.0.1:7717"),
            Some("https://evil.example"),
            Some(&format!("Bearer {token}")),
            None,
        );
        assert_eq!(refused, Err(Refused::Origin));
        assert_eq!(Refused::Origin.status(), 403);
    }

    #[test]
    fn a_same_origin_caller_with_no_token_is_nobody() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [5u8; 32]);
        assert_eq!(
            access(home.path()).admit(Some("127.0.0.1:7717"), None, None, None),
            Err(Refused::Missing)
        );
    }

    /// A revoked agent's token identifies nobody, with nothing swept — the
    /// property `agent_token` is derived rather than stored for.
    #[test]
    fn a_token_for_an_agent_this_device_does_not_sponsor_is_refused() {
        let home = tempfile::tempdir().expect("a home");
        let token = provision(home.path(), "scribe", [5u8; 32]);
        std::fs::remove_dir_all(crate::registry::agents_base(home.path()).join("scribe"))
            .expect("the sponsor removes the agent");
        assert_eq!(
            access(home.path()).admit(
                Some("127.0.0.1:7717"),
                None,
                Some(&format!("Bearer {token}")),
                None
            ),
            Err(Refused::Unknown)
        );
    }

    /// Two refusals a presenter cannot tell apart. Whether a token *exists* is
    /// not something an unauthenticated caller gets to learn.
    #[test]
    fn a_missing_and_an_unknown_credential_read_identically_to_the_caller() {
        assert_eq!(Refused::Missing.message(), Refused::Unknown.message());
        assert_eq!(Refused::Missing.status(), Refused::Unknown.status());
        assert_ne!(Refused::Origin.message(), Refused::Missing.message());
    }

    /// The hole a task-local left. A second sponsored agent presenting its own
    /// valid token and the first agent's session id must not be handed that
    /// session — the id rides in a response header and lands in logs, so it is
    /// not a secret, while the token is.
    #[test]
    fn a_session_belongs_to_the_agent_that_opened_it() {
        let sessions = Sessions::new();
        sessions.opened("sess-1", "scribe");
        assert!(sessions.admits("sess-1", "scribe"));
        assert!(
            !sessions.admits("sess-1", "auditor"),
            "another sponsored agent holding a valid token of its own is still not this session"
        );
    }

    /// Unknown is refused, never admitted. Treating a session nobody recorded
    /// as allowed is how a session id quietly becomes a credential.
    #[test]
    fn a_session_this_device_never_issued_is_not_one_to_resume() {
        let sessions = Sessions::new();
        assert!(!sessions.admits("sess-never", "scribe"));
        sessions.opened("sess-1", "scribe");
        sessions.closed("sess-1");
        assert!(
            !sessions.admits("sess-1", "scribe"),
            "and a closed session does not linger"
        );
    }
}
