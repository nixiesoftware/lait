//! `Open` — the handoff, end to end.
//!
//! Four things have to line up, and each of them lives where it does for a
//! reason:
//!
//! 1. **A head has to be running for this identity.** The Library is passive and
//!    starts nothing; `Open` is the act that places. This is the only call in
//!    the client that starts a process on purpose.
//! 2. **The head mints the credential.** Redemption consumes, so only the
//!    process that will be presented with a ticket can spend one. A client
//!    minting its own would be issuing credentials against a store nothing
//!    checks.
//! 3. **The World declares where the ticket lands.** A row whose World declares
//!    no entry path cannot be opened, and says so rather than guessing `/`.
//! 4. **The browser is the person's.** Nothing here draws a World, and nothing
//!    here holds a handle to what it launched.

use super::http::{post_json, Head};
use super::library::LaunchTicket;
use super::{Client, ClientError, ClientResult};

impl Client {
    /// The head this client opens Worlds through, started if it is not up.
    ///
    /// Started against the *identity* this client is bound to — the same one
    /// the Library was read from — and against nothing at all when that is the
    /// ordinary per-user identity, because there is no path that means "the
    /// ordinary one".
    ///
    /// This was wrong once and the failure was silent in the worst way. Handing
    /// the head `daemon::Client::home()` looks right — it is where this client
    /// talks to its daemon — but that is the daemon's own directory beneath the
    /// identity, so the head came up serving a self-contained identity nobody
    /// had ever used. It started, it announced an address, it minted a ticket,
    /// and it listed no Spaces. Every part worked.
    ///
    /// Idempotent by the supervisor's own key: asking twice for one identity
    /// finds the head that is already running. That matters because the
    /// alternative is a port and a run credential per click.
    /// `world` is the mount this head will serve, and it is what makes two
    /// Worlds two heads.
    ///
    /// Mandatory, and it was an `Option` once. "Unspecified" is a question, and a
    /// question cannot be a map key: one caller passing `None` and another
    /// passing the same World by name produced two keys, so one World got two
    /// heads and stopping either left the row saying Running. Whoever knows which
    /// build this is resolves it; by the time it reaches here it is an answer.
    pub async fn head(&self, world: &str) -> ClientResult<Head> {
        let facts = self
            .supervisor()
            .start_identity_head(self.identity(), world)
            .await?;
        let url = facts.url.as_deref().ok_or_else(|| {
            ClientError::internal("the head came up without announcing an address")
        })?;
        Head::from_ready_url(url)
    }

    /// Open one World, and hand the person's browser the result.
    ///
    /// Returns what it launched rather than nothing: a surface that says *where*
    /// it sent the browser is the difference between "did that work" and a
    /// window that may or may not have appeared behind another one.
    pub async fn open_world(&self, world: &str, entry_path: &str) -> ClientResult<LaunchTicket> {
        if !entry_path.starts_with('/') {
            // The declared entry path is a World's own statement about itself.
            // A relative one is a declaration this client cannot act on, and
            // rewriting it into `/` would open the head somewhere the World
            // never named.
            return Err(ClientError::invalid(format!(
                "'{entry_path}' is not an entry path this client can open"
            )));
        }
        // No Orbit is named and none is placed. Selecting a Space is the
        // destination's act — the head's front page carries the selector, and
        // choosing there is what attaches a daemon. A client that placed an
        // Orbit here would be pre-answering a question the person is about to
        // be asked.
        // This World's head, not whichever one happened to be up. That is the
        // difference between opening Issues and opening "the head", and it is
        // what lets stopping one say something true about one World.
        let head = self.head(world).await?;
        let ticket = self.mint(&head).await?;
        Self::launch_url(&head.base, entry_path, &ticket.secret, ticket.expires_at_ms)
    }

    /// Ask the head for one launch credential.
    ///
    /// Public because it is the half of `Open` that can be driven without
    /// starting a browser, and therefore the half a test can prove end to end
    /// against a real head. `open_world` is the same thing with the handoff on
    /// the end.
    pub async fn mint(&self, head: &Head) -> ClientResult<Minted> {
        let reply = post_json(head, "/api/launch", &serde_json::json!({})).await?;
        let secret = reply
            .get("ticket")
            .and_then(serde_json::Value::as_str)
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| ClientError::internal("the head minted no credential"))?
            .to_owned();
        // An expiry the head did not state is carried as unknown rather than as
        // zero or as now-plus-a-guess. It is only ever shown to a person, and
        // "expires at the epoch" is worse than saying nothing.
        let expires_at_ms = reply
            .get("expiresAtMs")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default();
        Ok(Minted {
            secret,
            expires_at_ms,
        })
    }
}

/// A credential the head minted, before it is composed into a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Minted {
    pub secret: String,
    pub expires_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client over a supervisor that has nothing to spawn. Enough to reach
    /// the guards, and deliberately not enough to start anything — which is
    /// what makes "refused before a head exists" a property this test can
    /// actually observe rather than assert.
    fn client(root: &std::path::Path) -> Client {
        let supervisor = lait_workbench::Supervisor::new(
            root.to_path_buf(),
            root.join(if cfg!(windows) { "lait.exe" } else { "lait" }),
        )
        .expect("a supervisor over an empty root");
        Client::over(supervisor, Some(root.to_path_buf()))
    }

    /// The refusal happens before anything is started. Starting a head and
    /// *then* discovering there was nowhere to send it would leave a process
    /// running for a click that could never have worked — and the executable
    /// this supervisor would spawn does not exist, so a guard that ran late
    /// would fail with a message about a missing file instead of about the
    /// argument that was wrong.
    #[tokio::test]
    async fn a_launch_with_nowhere_to_land_is_refused_before_anything_is_placed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let client = client(directory.path());

        for entry in ["issues", ""] {
            let refused = client
                .open_world("issues", entry)
                .await
                .expect_err("a launch with nothing to land on was accepted");
            assert_eq!(
                refused.code,
                super::super::error::ErrorCode::Invalid,
                "'{entry}' failed for the wrong reason: {refused}"
            );
        }

        assert!(
            client.supervisor().list_heads().is_empty(),
            "a refused launch started a head anyway"
        );
    }

    /// The composed URL is what a browser is actually handed, and it has to
    /// carry the credential to the path the World declared. This is the seam
    /// between minting and opening, and it is pure.
    #[test]
    fn the_composed_url_lands_on_the_declared_entry_carrying_the_ticket() {
        let launch = Client::launch_url("http://127.0.0.1:7717", "/spaces/x", "abc", 42)
            .expect("a launch url");
        assert_eq!(launch.url, "http://127.0.0.1:7717/spaces/x?ticket=abc");
        assert_eq!(launch.expires_at_ms, 42);
    }
}
