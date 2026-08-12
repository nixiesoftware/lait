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

use std::path::PathBuf;

use super::http::{post_json, Head};
use super::library::LaunchTicket;
use super::{Client, ClientError, ClientResult};

impl Client {
    /// The identity home this client's Orbits belong to.
    ///
    /// Resolved through the daemon client rather than read from configuration,
    /// so the head this starts serves the same identity the Library was read
    /// from. Two different answers here would produce a Library listing one
    /// person's Orbits and an `Open` that reached another's.
    pub fn identity_home(&self) -> ClientResult<PathBuf> {
        Ok(self.daemon()?.home().to_path_buf())
    }

    /// The head this client opens Worlds through, started if it is not up.
    ///
    /// Idempotent by the supervisor's own key: asking twice for one identity
    /// finds the head that is already running. That matters because the
    /// alternative is a port and a run credential per click.
    pub async fn head(&self) -> ClientResult<Head> {
        let home = self.identity_home()?;
        let facts = self.supervisor().start_identity_head(&home).await?;
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
    pub async fn open_world(&self, orbit: &str, entry_path: &str) -> ClientResult<LaunchTicket> {
        if orbit.trim().is_empty() {
            return Err(ClientError::invalid("an open needs an Orbit"));
        }
        if !entry_path.starts_with('/') {
            // The declared entry path is a World's own statement about itself.
            // A relative one is a declaration this client cannot act on, and
            // rewriting it into `/` would open the head somewhere the World
            // never named.
            return Err(ClientError::invalid(format!(
                "'{entry_path}' is not an entry path this client can open"
            )));
        }
        let head = self.head().await?;
        let ticket = self.mint(&head, orbit).await?;
        Self::launch_url(&head.base, entry_path, &ticket.secret, ticket.expires_at_ms)
    }

    /// Ask the head for one launch credential, scoped to `orbit`.
    async fn mint(&self, head: &Head, orbit: &str) -> ClientResult<Minted> {
        let reply = post_json(
            head,
            "/api/launch",
            &serde_json::json!({ "orbit": orbit.trim() }),
        )
        .await?;
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

struct Minted {
    secret: String,
    expires_at_ms: u64,
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

    /// Both refusals happen before anything is placed. Starting a head and
    /// *then* discovering there was nowhere to send it would leave a process
    /// running for a click that could never have worked — and the executable
    /// this supervisor would spawn does not exist, so a guard that ran late
    /// would fail with a message about a missing file instead of about the
    /// argument that was wrong.
    #[tokio::test]
    async fn a_launch_with_nowhere_to_land_is_refused_before_anything_is_placed() {
        let directory = tempfile::tempdir().expect("tempdir");
        let client = client(directory.path());

        for (orbit, entry) in [
            ("", "/"),
            ("   ", "/"),
            ("orb_one", "issues"),
            ("orb_one", ""),
        ] {
            let refused = client
                .open_world(orbit, entry)
                .await
                .expect_err("a launch with nothing to land on was accepted");
            assert_eq!(
                refused.code,
                super::super::error::ErrorCode::Invalid,
                "'{orbit}' at '{entry}' failed for the wrong reason: {refused}"
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
