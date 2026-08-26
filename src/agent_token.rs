//! The credential an agent presents to reach this device over HTTP.
//!
//! # Why an agent needs one at all
//!
//! An agent surface reached over stdio needs no credential: the editor spawned
//! the process, the process inherited the environment, and the operating system
//! decided who could do that. A long-lived endpoint has none of those and has
//! to be told who is calling.
//!
//! # Derived, never stored
//!
//! This is not a secret minted beside an agent and filed next to it. It is
//! derived from the agent's own seed, which means three things fall out rather
//! than needing to be built:
//!
//! - **Revoking the agent revokes the token.** Removing an agent removes its
//!   seed, and a token that cannot be re-derived cannot be verified. There is
//!   no second list to keep in step, and no window where a revoked agent still
//!   holds a live credential because something forgot to sweep.
//! - **There is nothing extra to leak.** A device already holding the seed can
//!   act as the agent outright; the token grants nothing it did not.
//! - **It is stable.** The same agent presents the same token across restarts,
//!   which is the whole point — a binding written once keeps working, and
//!   nobody is sent to re-copy a string because a daemon restarted.
//!
//! # What it is not
//!
//! It is not authority. It says *which agent is calling* and nothing else:
//! what that agent may do is its standing in the Space, which the address book
//! and its sponsor decide and which this cannot widen. An agent presenting a
//! valid token and holding no membership is exactly as unable to act as one
//! presenting nothing — it simply gets told which agent it is while being
//! refused.
//!
//! # Reach
//!
//! Loopback today, and the reason is written down here rather than assumed at
//! the callsite: a bearer credential on a loopback socket is guarded by the
//! operating system's process boundary, and the same credential on a network
//! socket is guarded by nothing until there is transport security under it.
//! [`Reach`] exists so that widening it is a decision somebody makes in one
//! place, against this paragraph, rather than a bind address quietly changing.

use std::path::Path;

use anyhow::Result;

/// The domain separator. Present so this derivation can never collide with
/// another use of the same seed — a signature, a device key — and so that
/// changing what a token means is a change to a string that is searchable.
const PURPOSE: &[u8] = b"lait/agent-mcp-token/v1";

/// Where an agent's credential may be presented from.
///
/// Loopback is the only variant this build honours. It is an enum rather than
/// a bool because the second case is not "loopback plus", it is a different
/// threat model — a bearer token on a network socket needs transport security
/// and an answer for every device that can reach the port, and neither exists
/// yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// This machine only, over 127.0.0.1.
    Loopback,
}

impl Reach {
    /// What a listener should bind. One place, so widening reach is one edit
    /// with this module's reasoning next to it.
    pub fn bind_address(self) -> &'static str {
        match self {
            Reach::Loopback => "127.0.0.1",
        }
    }
}

/// The token for one co-located agent, derived from its seed.
pub fn derive(seed: &[u8; 32]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(PURPOSE);
    hasher.update(seed);
    hasher.finalize().to_hex().to_string()
}

/// The token for a provisioned agent by name, or `None` when no such agent is
/// provisioned under this home.
///
/// `None` is "there is no such agent", which is the same answer a wrong name
/// gets. A caller must not distinguish them for a presenter it has not
/// authenticated yet.
pub fn for_agent(home: &Path, name: &str) -> Option<String> {
    let path = seed_path(home, name).ok()?;
    let hex = std::fs::read_to_string(path).ok()?;
    let mut seed = [0u8; 32];
    let decoded = data_encoding::HEXLOWER.decode(hex.trim().as_bytes()).ok()?;
    if decoded.len() != seed.len() {
        return None;
    }
    seed.copy_from_slice(&decoded);
    Some(derive(&seed))
}

/// Which provisioned agent, if any, a presented token belongs to.
///
/// Every provisioned agent is re-derived and compared in constant time, so a
/// caller learns which agent is calling without the token ever being stored to
/// compare against. The cost is linear in agents on a device, which is a
/// handful — this is not a directory of users.
pub fn identify(home: &Path, presented: &str) -> Option<String> {
    let presented = presented.trim();
    if presented.is_empty() {
        return None;
    }
    for name in provisioned(home) {
        if let Some(expected) = for_agent(home, &name) {
            if constant_time_eq(expected.as_bytes(), presented.as_bytes()) {
                return Some(name);
            }
        }
    }
    None
}

/// Every agent name provisioned under this home.
pub fn provisioned(home: &Path) -> Vec<String> {
    let base = crate::registry::agents_base(home);
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| seed_path(home, name).map(|p| p.is_file()).unwrap_or(false))
        .collect();
    names.sort();
    names
}

fn seed_path(home: &Path, name: &str) -> Result<std::path::PathBuf> {
    // Re-proved here rather than trusted from the caller: this joins a name
    // into a path, and a name carrying a separator would read somebody else's
    // seed.
    if name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        || name.starts_with('.')
    {
        anyhow::bail!("'{name}' is not a plain agent name");
    }
    Ok(crate::registry::agents_base(home)
        .join(name)
        .join("secret.key"))
}

/// Compare without leaking where two byte strings first differ.
///
/// Hand-rolled rather than pulled in: it is six lines, and the alternative is a
/// dependency whose whole job is these six lines.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Read the token a caller presented, from either header a client may use.
///
/// `Authorization: Bearer …` is what an MCP client sends. `X-Lait-Agent-Token`
/// exists for callers that cannot set `Authorization` and is checked second, so
/// the standard spelling wins when both are present.
pub fn presented<'a>(authorization: Option<&'a str>, fallback: Option<&'a str>) -> Option<&'a str> {
    if let Some(value) = authorization {
        if let Some(rest) = value
            .strip_prefix("Bearer ")
            .or_else(|| value.strip_prefix("bearer "))
        {
            return Some(rest.trim());
        }
    }
    fallback.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provision(home: &Path, name: &str, seed: [u8; 32]) {
        let path = seed_path(home, name).expect("a plain name");
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("the agent directory");
        std::fs::write(&path, data_encoding::HEXLOWER.encode(&seed)).expect("the seed");
    }

    #[test]
    fn a_token_is_stable_for_one_agent_and_differs_between_agents() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        provision(home.path(), "auditor", [9u8; 32]);

        let first = for_agent(home.path(), "scribe").expect("a token");
        assert_eq!(
            first,
            for_agent(home.path(), "scribe").expect("the same token"),
            "a binding written once keeps working across restarts"
        );
        assert_ne!(first, for_agent(home.path(), "auditor").expect("a token"));
    }

    /// The property the whole derivation exists for. There is no second list to
    /// sweep, so there is no window where a removed agent still holds a live
    /// credential.
    #[test]
    fn removing_an_agent_revokes_its_token_with_nothing_to_sweep() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        let token = for_agent(home.path(), "scribe").expect("a token");
        assert_eq!(identify(home.path(), &token).as_deref(), Some("scribe"));

        std::fs::remove_dir_all(crate::registry::agents_base(home.path()).join("scribe"))
            .expect("the sponsor removes the agent");
        assert!(
            identify(home.path(), &token).is_none(),
            "the token cannot be re-derived, so it cannot be verified"
        );
    }

    #[test]
    fn an_unknown_token_identifies_nobody() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        assert!(identify(home.path(), "not-a-token").is_none());
        assert!(identify(home.path(), "").is_none());
        assert!(identify(home.path(), "   ").is_none());
    }

    /// A name reaches this from a request. One carrying a separator would read
    /// a seed that is not the caller's to ask about.
    #[test]
    fn a_name_that_is_not_a_plain_segment_reads_no_seed() {
        let home = tempfile::tempdir().expect("a home");
        provision(home.path(), "scribe", [7u8; 32]);
        assert!(seed_path(home.path(), "../scribe").is_err());
        assert!(seed_path(home.path(), "a/b").is_err());
        assert!(seed_path(home.path(), "").is_err());
        assert!(for_agent(home.path(), "../scribe").is_none());
    }

    #[test]
    fn the_standard_header_wins_and_a_blank_one_is_not_a_token() {
        assert_eq!(presented(Some("Bearer abc"), None), Some("abc"));
        assert_eq!(presented(Some("Bearer abc"), Some("def")), Some("abc"));
        assert_eq!(presented(None, Some("def")), Some("def"));
        assert_eq!(presented(None, Some("   ")), None);
        assert_eq!(presented(Some("Basic abc"), None), None);
    }

    /// The derivation is domain separated, so this token can never be the same
    /// bytes as another use of the same seed.
    #[test]
    fn the_derivation_is_domain_separated_from_the_raw_seed() {
        let seed = [3u8; 32];
        let token = derive(&seed);
        assert_ne!(token, data_encoding::HEXLOWER.encode(&seed));
        assert_ne!(token, blake3::hash(&seed).to_hex().to_string());
    }
}
