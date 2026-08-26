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
//! `StreamableHttpService` builds its handler through a factory taking no
//! arguments, so the handler cannot read the request that caused it. The agent
//! identity therefore travels in a task-local, set by the layer that
//! authenticated it and read by the factory inside the same request.
//!
//! It is worth being plain that this is a workaround for the SDK's shape rather
//! than a design: the alternative is a service per agent mounted at a path per
//! agent, which leaks agent names into URLs and has to be rebuilt whenever
//! somebody is sponsored. The task-local keeps the authenticated identity and
//! its use in one request, and [`agent_for_session`] is the only way to read
//! it.

use std::sync::Arc;

use crate::agent_token::Reach;

tokio::task_local! {
    /// The agent this request authenticated as, between the layer that proved
    /// it and the factory that builds a handler for it.
    static SESSION_AGENT: String;
}

/// The agent the current request authenticated as, or `None` outside one.
pub fn agent_for_session() -> Option<String> {
    SESSION_AGENT.try_with(|agent| agent.clone()).ok()
}

/// Everything the endpoint needs to decide who may connect.
#[derive(Clone)]
pub struct Access {
    /// This identity's home, where provisioned agents' seeds live.
    pub home: std::path::PathBuf,
    /// The head credential and origin policy this endpoint shares with the
    /// server hosting it.
    pub guard: Arc<crate::serve::auth::Guard>,
    /// Where a credential may be presented from.
    pub reach: Reach,
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
            .check_origin(host, origin)
            .map_err(|_| Refused::Origin)?;
        let presented =
            crate::agent_token::presented(authorization, fallback).ok_or(Refused::Missing)?;
        crate::agent_token::identify(&self.home, presented).ok_or(Refused::Unknown)
    }

    /// Where a listener for this endpoint belongs.
    pub fn bind_address(&self) -> &'static str {
        self.reach.bind_address()
    }
}

/// Run `work` with `agent` established as the session's identity.
///
/// The only way the task-local is set, so every path that establishes an
/// identity goes through the one that authenticated it.
pub async fn as_agent<F, T>(agent: String, work: F) -> T
where
    F: std::future::Future<Output = T>,
{
    SESSION_AGENT.scope(agent, work).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access(home: &std::path::Path) -> Access {
        Access {
            home: home.to_path_buf(),
            guard: Arc::new(crate::serve::auth::Guard::new("head-token".into(), 7717)),
            reach: Reach::Loopback,
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
        crate::agent_token::derive(&seed)
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

    #[tokio::test]
    async fn the_session_identity_is_readable_only_inside_the_request_that_proved_it() {
        assert!(agent_for_session().is_none(), "nothing outside a request");
        let seen = as_agent("scribe".into(), async { agent_for_session() }).await;
        assert_eq!(seen.as_deref(), Some("scribe"));
        assert!(agent_for_session().is_none(), "and nothing after it");
    }

    #[test]
    fn loopback_is_what_this_build_binds() {
        assert_eq!(
            access(&std::path::PathBuf::from("/tmp")).bind_address(),
            "127.0.0.1"
        );
    }
}
